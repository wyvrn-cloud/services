use std::sync::Arc;

use anyhow::Context;
use didcomm_agent::Identity;
use wyvrn_did_host::{
    config::Config,
    host::Host,
    names::Name,
    server::{self, AppState},
    store::Store,
};
use wyvrn_service::now;

const USAGE: &str = "\
wyvrn-did-host [--config <path>] [serve]             run the host
wyvrn-did-host [--config <path>] names               every name the host knows of
wyvrn-did-host [--config <path>] assign <name> <did> set a name aside for a DID to claim
wyvrn-did-host [--config <path>] assign <name> none  set a name aside for nobody
wyvrn-did-host [--config <path>] free <name>         end what is hosted under a name

The config path is --config, else $DID_HOST_CONFIG, else config/default.toml.
`assign` and `free` are what an admin's messages do, for whoever is at the machine.";

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
        None => std::env::var("DID_HOST_CONFIG").unwrap_or_else(|_| "config/default.toml".to_string()),
    };
    let config = Config::load(&config_path)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["serve"] => serve(config).await,
        ["names"] => names(&config),
        ["assign", name, holder] => assign(&config, name, (*holder != "none").then_some(*holder)),
        ["free", name] => free(&config, name),
        ["help" | "--help" | "-h"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => anyhow::bail!("unrecognized arguments\n\n{USAGE}"),
    }
}

fn open_store(config: &Config) -> anyhow::Result<Store> {
    Store::open(&config.store.path).with_context(|| format!("store {}", config.store.path.display()))
}

fn host(config: &Config) -> anyhow::Result<Host> {
    Ok(Host::new(open_store(config)?, config.host.clone(), didcomm_quickstart::default_resolver()))
}

fn name(config: &Config, text: &str) -> anyhow::Result<Name> {
    Name::parse(text, config.host.max_labels).with_context(|| format!("{text:?} is not a name this host allows"))
}

fn names(config: &Config) -> anyhow::Result<()> {
    let host = host(config)?;
    for record in host.store.all()? {
        let state = match Name::parse(&record.name, usize::MAX) {
            Some(name) => host.state_of(&name, Some(&record), now()).as_str(),
            None => "?",
        };
        println!(
            "{}\t{}\t{}\t{}",
            record.name,
            state,
            record.did.as_deref().unwrap_or("-"),
            record.assigned.as_deref().map(|did| format!("for {did}")).unwrap_or_default(),
        );
    }
    Ok(())
}

fn assign(config: &Config, text: &str, holder: Option<&str>) -> anyhow::Result<()> {
    let name = name(config, text)?;
    let store = open_store(config)?;
    if store.get(&name.as_did_path())?.is_some_and(|r| r.did.is_some()) {
        anyhow::bail!("a DID is hosted under {name}; free it first");
    }
    store.assign(&name.as_did_path(), holder)?;
    println!("{name} is set aside for {}", holder.unwrap_or("nobody"));
    Ok(())
}

fn free(config: &Config, text: &str) -> anyhow::Result<()> {
    let name = name(config, text)?;
    let store = open_store(config)?;
    let was = store.get(&name.as_did_path())?.and_then(|r| r.did);
    store.free(&name.as_did_path())?;
    println!("{name} is free{}", was.map(|did| format!(" (it was {did})")).unwrap_or_default());
    Ok(())
}

async fn serve(config: Config) -> anyhow::Result<()> {
    wyvrn_service::logging::init();

    let identity = Identity::load_or_generate(&config.identity.path)
        .with_context(|| format!("identity file {}", config.identity.path.display()))?;
    let (agent, did_document) = server::build_agent(&config, identity)?;
    let host = host(&config)?;
    tracing::info!(
        did = %agent.did(),
        domain = %config.host.domain,
        names = host.store.all()?.len(),
        admins = config.host.admins.len(),
        "host ready"
    );

    let state = Arc::new(AppState { agent, host, did_document });
    tokio::spawn(server::run_staleness_checks(state.clone()));
    wyvrn_service::http::serve(&config.server.bind, server::router(state)).await
}
