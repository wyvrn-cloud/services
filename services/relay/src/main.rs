use std::sync::Arc;

use wyvrn_relay::{config::Config, relay::Relay, server, store::Store};

const USAGE: &str = "\
wyvrn-relay [--config <path>] [serve]         run the relay
wyvrn-relay [--config <path>] grants          every DID granted, and when
wyvrn-relay [--config <path>] release <did>   end a DID's grant

The config path is --config, else $RELAY_CONFIG, else config/default.toml.";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = match args.iter().position(|arg| arg == "--config") {
        Some(at) if at + 1 < args.len() => {
            let path = args.remove(at + 1);
            args.remove(at);
            path
        }
        Some(_) => anyhow::bail!("--config needs a path\n\n{USAGE}"),
        None => std::env::var("RELAY_CONFIG").unwrap_or_else(|_| "config/default.toml".to_string()),
    };
    let config = Config::load(&config_path)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["serve"] => serve(config).await,
        ["grants"] => {
            for (did, granted) in open_store(&config)?.grants()? {
                println!("{granted}\t{did}");
            }
            Ok(())
        }
        ["release", did] => {
            if open_store(&config)?.release(did)? {
                println!("{did} is released");
            } else {
                println!("{did} had no grant");
            }
            Ok(())
        }
        ["help" | "--help" | "-h"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => anyhow::bail!("unrecognized arguments\n\n{USAGE}"),
    }
}

fn open_store(config: &Config) -> anyhow::Result<Store> {
    // In the file the configuration names, unless the environment names a
    // database (RELAY_DATABASE_URL or DATABASE_URL).
    let location = wyvrn_service::db::Location::from_env("RELAY", &config.store.path)?;
    Store::open(&location)
}

async fn serve(config: Config) -> anyhow::Result<()> {
    wyvrn_service::logging::init();
    let location = wyvrn_service::db::Location::from_env("RELAY", &config.store.path)?;
    let identity = wyvrn_service::identity::load_or_generate(&location, &config.identity.path, "relay")?;
    let (agent, did_document) = server::build_agent(&config, identity)?;
    let relay = Relay::new(config.relay.clone(), open_store(&config)?)?;
    tracing::info!(did = %agent.did(), url = %config.server.public_url, carries = relay.config.carries.as_str(), open = relay.config.open, "relay ready");
    let state = Arc::new(server::AppState { agent, relay, did_document });
    wyvrn_service::http::serve(&config.server.bind, server::router(state)).await
}
