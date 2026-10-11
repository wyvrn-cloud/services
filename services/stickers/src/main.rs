use std::sync::Arc;

use anyhow::Context;
use didcomm_agent::Identity;
use wyvrn_service::now;
use wyvrn_stickers::{
    config::Config,
    processor::Processor,
    server::{self, AppState},
    store::{pack_scope, Store},
};

const USAGE: &str = "\
wyvrn-stickers [--config <path>] [serve]         run the service
wyvrn-stickers [--config <path>] packs           every pack it hosts
wyvrn-stickers [--config <path>] remove <name>   take a pack down

The config path is --config, else $STICKERS_CONFIG, else config/default.toml.
A pack that is taken down is gone for everyone, and its name stays taken.";

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
        None => std::env::var("STICKERS_CONFIG").unwrap_or_else(|_| "config/default.toml".to_string()),
    };
    let config = Config::load(&config_path)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["serve"] => serve(config).await,
        ["packs"] => packs(&config),
        ["remove", name] => remove(&config, name),
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

fn packs(config: &Config) -> anyhow::Result<()> {
    let store = open_store(config)?;
    for pack in store.packs(None)? {
        let count = store.stickers(&pack_scope(&pack.name))?.len();
        let state = if pack.removed { "removed" } else if pack.reuse { "open" } else { "private" };
        println!("{}\t{}\t{}\t{count}\t{}\t{}", pack.name, pack.kind.as_str(), state, pack.title, pack.owner);
    }
    Ok(())
}

fn remove(config: &Config, name: &str) -> anyhow::Result<()> {
    let store = open_store(config)?;
    if store.pack(name)?.is_none() {
        anyhow::bail!("there is no pack called {name}");
    }
    if store.remove_pack(name, now())? {
        println!("{name} is taken down");
    } else {
        println!("{name} was taken down already");
    }
    Ok(())
}

async fn serve(config: Config) -> anyhow::Result<()> {
    wyvrn_service::logging::init();

    let identity = Identity::load_or_generate(&config.identity.path)
        .with_context(|| format!("identity file {}", config.identity.path.display()))?;
    let (agent, did_document) = server::build_agent(&config, identity)?;
    if let Some(mediator) = &config.mediator {
        wyvrn_service::mediator::register(&agent, mediator).await?;
    }
    let processor = Processor::new(open_store(&config)?, config.service.clone(), &config.server.public_url);
    tracing::info!(did = %agent.did(), url = %config.server.public_url, packs = processor.store.packs(None)?.len(), "sticker service ready");

    let state = Arc::new(AppState { agent, processor, did_document, resolver: didcomm_quickstart::default_resolver() });
    tokio::spawn(server::run_pruning(state.clone()));
    if let Some(mediator) = &config.mediator {
        let service = Arc::new(server::StickerService(state.clone()));
        tokio::spawn(wyvrn_service::mediator::run_pickups(service, mediator.poll_every()));
    }
    wyvrn_service::http::serve(&config.server.bind, server::router(state)).await
}
