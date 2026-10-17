use std::sync::Arc;

use anyhow::Context;
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
    // In the file the configuration names, unless the environment names a
    // database (STICKERS_DATABASE_URL or DATABASE_URL: mysql://user:password@host/database).
    let location = wyvrn_service::db::Location::from_env("STICKERS", &config.store.path)?;
    Store::open(&location).with_context(|| format!("store {location}"))
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

    // Kept with the store: in the database, for a service that has one.
    let location = wyvrn_service::db::Location::from_env("STICKERS", &config.store.path)?;
    let identity = wyvrn_service::identity::load_or_generate(&location, &config.identity.path, "stickers")?;
    let (agent, did_document) = server::build_agent(&config, identity)?;
    if let Some(mediator) = &config.mediator {
        wyvrn_service::mediator::register(&agent, mediator).await?;
    }
    let processor = Processor::new(open_store(&config)?, config.service.clone(), &config.server.public_url);
    tracing::info!(did = %agent.did(), url = %config.server.public_url, packs = processor.store.packs(None)?.len(), "sticker service ready");

    let state = Arc::new(AppState { agent, voice: Default::default(), processor, did_document, resolver: didcomm_quickstart::default_resolver(), admin: Default::default() });
    // A public address at a DID host, if it is set to hold one (STICKERS_PUBLIC_ADDRESS):
    // taken up once the service is serving, and kept up to date from then on.
    if let (Some(address), Some(document)) = (wyvrn_service::hosted_address::Address::from_env("STICKERS")?, state.did_document.clone()) {
        let asking = state.clone();
        let ask: wyvrn_service::hosted_address::Ask = Box::new(move |to, message| {
            let asking = asking.clone();
            // As its did:web, which the host set the name aside for, whatever
            // DID the service speaks as to everyone else.
            Box::pin(async move { Ok(asking.agent.request_as(asking.agent.base_did(), &to, &message).await?.message) })
        });
        let kept = wyvrn_service::hosted_address::Kept::open(&location, "stickers")?;
        let speaking = state.clone();
        let held: wyvrn_service::hosted_address::Held = Box::new(move |did, address| {
            let speaking = speaking.clone();
            Box::pin(async move { speaking.voice.take_up(&speaking.agent, &did, &address, wyvrn_service::now()).await })
        });
        tokio::spawn(wyvrn_service::hosted_address::Holding::new(address, kept, document, ask)?.when_held(held).run());
    }
    tokio::spawn(server::run_pruning(state.clone()));
    if let Some(mediator) = &config.mediator {
        let service = Arc::new(server::StickerService(state.clone()));
        tokio::spawn(wyvrn_service::mediator::run_pickups(service, mediator.poll_every()));
    }
    wyvrn_service::http::serve(&config.server.bind, server::router(state)).await
}
