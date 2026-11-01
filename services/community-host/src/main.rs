use std::sync::Arc;

use anyhow::Context;
use wyvrn_community_host::{
    admin::SEALED_FOR,
    config::{Config, Limits},
    registry::{Registry, State, Storage},
    server::{self, AppState},
    tenants::Host,
};
use wyvrn_service::now;

const USAGE: &str = "\
wyvrn-community-host [--config <path>] [serve]                    run the host
wyvrn-community-host [--config <path>] list                       every community
wyvrn-community-host [--config <path>] create <name> <owner-did> [<storage>] [<title>]
                                                                  make one (storage: sqlite, mysql-db, mysql-shared)
wyvrn-community-host [--config <path>] suspend|resume <name>      stop answering for one, or start again
wyvrn-community-host [--config <path>] delete|restore <name>      remove one (kept 30 days), or bring it back
wyvrn-community-host [--config <path>] limits <name> <members> <channels>
                                                                  what one may hold

The config path is --config, else $COMMUNITY_HOST_CONFIG, else config/default.toml.
A change made here while the host runs is taken up when the community is next opened.";

fn registry(config: &Config) -> anyhow::Result<Registry> {
    // In the file the configuration names, unless the environment names a database
    // (COMMUNITY_HOST_DATABASE_URL or DATABASE_URL).
    let location = wyvrn_service::db::Location::from_env("COMMUNITY_HOST", &config.store.path)?;
    Registry::open(&location).with_context(|| format!("store {location}"))
}

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
        None => std::env::var("COMMUNITY_HOST_CONFIG").unwrap_or_else(|_| "config/default.toml".to_string()),
    };
    let config = Config::load(&config_path)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let state_of = |name: &str, state: State, sealed: Option<i64>| -> anyhow::Result<()> {
        let registry = registry(&config)?;
        anyhow::ensure!(registry.set_state(name, state, sealed)?, "there is no community called {name}");
        println!("{name} is {}", state.as_str());
        Ok(())
    };
    match args.as_slice() {
        [] | ["serve"] => serve(config).await,
        ["list"] => {
            for record in registry(&config)?.all()? {
                println!("{}\t{}\t{}\t{}\t{}", record.name, record.state.as_str(), record.storage.as_str(), record.owner, record.title);
            }
            Ok(())
        }
        ["create", name, owner, rest @ ..] => {
            anyhow::ensure!(owner.starts_with("did:"), "{owner} isn't a DID");
            let storage = match rest.first() {
                Some(kind) => Storage::parse(kind).with_context(|| format!("{kind}: sqlite, mysql-db or mysql-shared"))?,
                None => Storage::parse(&config.storage.default).context("storage.default: sqlite, mysql-db or mysql-shared")?,
            };
            let title = rest.get(1).copied().unwrap_or(name);
            let record = registry(&config)?.create(name, title, storage, owner, "the command line", now())?;
            println!("{} is made, at {}c/{}/ (opened by the host to take up its address)", record.name, config.server.public_url, record.name);
            Ok(())
        }
        ["suspend", name] => state_of(name, State::Suspended, None),
        ["resume", name] | ["restore", name] => state_of(name, State::Active, None),
        ["delete", name] => state_of(name, State::Deleted, Some(now() + SEALED_FOR)),
        ["limits", name, members, channels] => {
            let registry = registry(&config)?;
            let record = registry.get(name)?.with_context(|| format!("there is no community called {name}"))?;
            let limits = Limits { members: members.parse()?, channels: channels.parse()?, ..record.limits.unwrap_or(config.limits) };
            registry.set_limits(name, Some(&limits))?;
            println!("{name} may hold {members} members and {channels} channels");
            Ok(())
        }
        ["help" | "--help" | "-h"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => anyhow::bail!("unrecognized arguments\n\n{USAGE}"),
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    wyvrn_service::logging::init();
    let location = wyvrn_service::db::Location::from_env("COMMUNITY_HOST", &config.store.path)?;
    let identity = wyvrn_service::identity::load_or_generate(&location, &config.identity.path, "community-host")?;
    let (agent, did_document) = server::build_agent(&config, identity)?;
    let registry = registry(&config)?;
    let communities = registry.all()?.len();
    let bind = config.server.bind.clone();
    if config.host.admins.is_empty() {
        tracing::warn!("nobody can make communities from the app: set COMMUNITY_HOST_ADMINS, or use `wyvrn-community-host create`");
    }
    let host = Host::new(config, registry);
    tracing::info!(did = %agent.did(), url = %host.config.server.public_url, communities, "community host ready");
    tokio::spawn(host.clone().run_housekeeping());
    // Owners' servers taking their communities over (export.rs).
    tokio::spawn(host.clone().watch_hand_overs(Arc::from(didcomm_quickstart::default_resolver())));
    let state = Arc::new(AppState { agent, host, did_document, resolver: didcomm_quickstart::default_resolver(), admin: Default::default() });
    wyvrn_service::http::serve(&bind, server::router(state)).await
}
