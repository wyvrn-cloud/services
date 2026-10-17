use std::sync::Arc;

use anyhow::Context;
use wyvrn_files::{
    config::Config,
    host::Host,
    server::{self, AppState},
    store::Store,
};
use wyvrn_service::now;

const USAGE: &str = "\
wyvrn-files [--config <path>] [serve]         run the service
wyvrn-files [--config <path>] files           every file it holds
wyvrn-files [--config <path>] remove <id>     remove a file
wyvrn-files [--config <path>] prune           remove what is past its time now

The config path is --config, else $FILES_CONFIG, else config/default.toml.";

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
        None => std::env::var("FILES_CONFIG").unwrap_or_else(|_| "config/default.toml".to_string()),
    };
    let config = Config::load(&config_path)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["serve"] => serve(config).await,
        ["files"] => files(&config),
        ["remove", id] => remove(&config, id),
        ["prune"] => prune(&config),
        ["help" | "--help" | "-h"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => anyhow::bail!("unrecognized arguments\n\n{USAGE}"),
    }
}

fn open_store(config: &Config) -> anyhow::Result<Store> {
    // In the file the configuration names, unless the environment names a
    // database (FILES_DATABASE_URL or DATABASE_URL: mysql://user:password@host/database).
    let location = wyvrn_service::db::Location::from_env("FILES", &config.store.path)?;
    Store::open(&location, &config.store.dir).with_context(|| format!("store {location}"))
}

fn files(config: &Config) -> anyhow::Result<()> {
    let store = open_store(config)?;
    let host = Host::new(store, config.service.clone(), &config.server.public_url);
    // Every owner's files: the store lists by owner, so ask it for each.
    let mut owners: Vec<String> = Vec::new();
    for id in std::fs::read_dir(&config.store.dir)?.flatten().filter_map(|entry| entry.file_name().into_string().ok()) {
        if let Some(file) = host.store.file(&id)? {
            if !owners.contains(&file.owner) {
                owners.push(file.owner);
            }
        }
    }
    for owner in owners {
        for file in host.store.files_of(&owner)? {
            println!("{}\t{}\t{}\t{}/{}\t{}\t{}", file.id, file.state.as_str(), file.name, host.store.received(&file.id), file.byte_count, file.expires, file.owner);
        }
    }
    Ok(())
}

fn remove(config: &Config, id: &str) -> anyhow::Result<()> {
    let store = open_store(config)?;
    if store.delete(id)? {
        println!("{id} is removed");
    } else {
        println!("there is no file {id}");
    }
    Ok(())
}

fn prune(config: &Config) -> anyhow::Result<()> {
    let store = open_store(config)?;
    println!("{} removed", store.prune(now())?);
    Ok(())
}

async fn serve(config: Config) -> anyhow::Result<()> {
    wyvrn_service::logging::init();

    // Kept with the store: in the database, for a service that has one.
    let location = wyvrn_service::db::Location::from_env("FILES", &config.store.path)?;
    let identity = wyvrn_service::identity::load_or_generate(&location, &config.identity.path, "files")?;
    let (agent, did_document) = server::build_agent(&config, identity)?;
    if let Some(mediator) = &config.mediator {
        wyvrn_service::mediator::register(&agent, mediator).await?;
    }
    let host = Host::new(open_store(&config)?, config.service.clone(), &config.server.public_url);
    tracing::info!(did = %agent.did(), url = %config.server.public_url, uploaders = host.config.uploaders.len(), "files service ready");

    let state = Arc::new(AppState::new(agent, host, did_document));
    // A public address at a DID host, if it is set to hold one (FILES_PUBLIC_ADDRESS):
    // taken up once the service is serving, and kept up to date from then on.
    if let (Some(address), Some(document)) = (wyvrn_service::hosted_address::Address::from_env("FILES")?, state.did_document.clone()) {
        let asking = state.clone();
        let ask: wyvrn_service::hosted_address::Ask = Box::new(move |to, message| {
            let asking = asking.clone();
            // As its did:web, which the host set the name aside for, whatever
            // DID the service speaks as to everyone else.
            Box::pin(async move { Ok(asking.agent.request_as(asking.agent.base_did(), &to, &message).await?.message) })
        });
        let kept = wyvrn_service::hosted_address::Kept::open(&location, "files")?;
        let speaking = state.clone();
        let held: wyvrn_service::hosted_address::Held = Box::new(move |did, address| {
            let speaking = speaking.clone();
            Box::pin(async move { speaking.voice.take_up(&speaking.agent, &did, &address, wyvrn_service::now()).await })
        });
        tokio::spawn(wyvrn_service::hosted_address::Holding::new(address, kept, document, ask)?.when_held(held).run());
    }
    tokio::spawn(server::run_pruning(state.clone()));
    if let Some(mediator) = &config.mediator {
        let service = Arc::new(server::FilesService(state.clone()));
        tokio::spawn(wyvrn_service::mediator::run_pickups(service, mediator.poll_every()));
    }
    wyvrn_service::http::serve(&config.server.bind, server::router(state)).await
}
