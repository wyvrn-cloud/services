use std::sync::Arc;

use anyhow::Context;
use tokio::sync::Notify;
use wyvrn_announce::{
    channel::Channel,
    config::Config,
    server::{self, AppState},
    store::{Role, Store},
};

const USAGE: &str = "\
wyvrn-announce [--config <path>] [serve]       run the channel
wyvrn-announce [--config <path>] subscribers   list everyone the channel knows
wyvrn-announce [--config <path>] admin list
wyvrn-announce [--config <path>] admin add <did>
wyvrn-announce [--config <path>] admin remove <did>

The config path is --config, else $ANNOUNCE_CONFIG, else config/default.toml.";

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
        None => std::env::var("ANNOUNCE_CONFIG").unwrap_or_else(|_| "config/default.toml".to_string()),
    };
    let config = Config::load(&config_path)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["serve"] => serve(config).await,
        ["subscribers"] => list(&config, false),
        ["admin", "list"] => list(&config, true),
        ["admin", "add", did] => set_role(&config, did, Role::Admin),
        ["admin", "remove", did] => set_role(&config, did, Role::Subscriber),
        ["help" | "--help" | "-h"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => anyhow::bail!("unrecognized arguments\n\n{USAGE}"),
    }
}

fn open_store(config: &Config) -> anyhow::Result<Store> {
    // In the file the configuration names, unless the environment names a
    // database (ANNOUNCE_DATABASE_URL or DATABASE_URL: mysql://user:password@host/database).
    let location = wyvrn_service::db::Location::from_env("ANNOUNCE", &config.store.path)?;
    Store::open(&location).with_context(|| format!("store {location}"))
}

fn list(config: &Config, admins_only: bool) -> anyhow::Result<()> {
    for member in open_store(config)?.members()? {
        if admins_only && member.role != Role::Admin {
            continue;
        }
        println!(
            "{}\t{}\t{}\t{}",
            member.role.as_str(),
            if member.subscribed { "subscribed" } else { "not subscribed" },
            member.name.as_deref().unwrap_or("-"),
            member.did,
        );
    }
    Ok(())
}

/// Changes a role straight in the store. A running server sees it at once; the
/// member's app learns of it the next time it starts (it subscribes afresh then).
fn set_role(config: &Config, did: &str, role: Role) -> anyhow::Result<()> {
    let store = open_store(config)?;
    if store.is_superseded(did)? {
        anyhow::bail!("{did} has been replaced by another DID; use the one it became (see `subscribers`)");
    }
    if role == Role::Subscriber {
        let admins = store.members()?.into_iter().filter(|m| m.role == Role::Admin).count();
        let is_admin = store.member(did)?.is_some_and(|m| m.role == Role::Admin);
        if is_admin && admins <= 1 {
            anyhow::bail!("{did} is the channel's only admin; add another first");
        }
    }
    store.set_role(did, role, server::now())?;
    println!("{did} is now {}", if role == Role::Admin { "an admin" } else { "not an admin" });
    Ok(())
}

async fn serve(config: Config) -> anyhow::Result<()> {
    wyvrn_service::logging::init();

    let channel = Channel { store: open_store(&config)?, info: server::channel_info(&config)? };
    channel.name_admins(&config.channel.admins, server::now())?;

    // Kept with the store: in the database, for a service that has one.
    let location = wyvrn_service::db::Location::from_env("ANNOUNCE", &config.store.path)?;
    let identity = wyvrn_service::identity::load_or_generate(&location, &config.identity.path, "announce")?;
    let (agent, did_document) = server::build_agent(&config, identity)?;
    // Behind a mediator, the channel's DID is the one that names it as the way in.
    if let Some(mediator) = &config.mediator {
        wyvrn_service::mediator::register(&agent, mediator).await?;
    }
    tracing::info!(
        did = %agent.did(),
        url = %config.server.public_url,
        name = %channel.info.name,
        posts = channel.store.latest_seq()?,
        subscribers = channel.store.subscriber_count()?,
        "channel ready"
    );

    let state = Arc::new(AppState {
        agent,
        channel,
        did_document,
        resolver: didcomm_quickstart::default_resolver(),
        admins: config.channel.admins.clone(),
        deliver: Notify::new(),
        reacted: Default::default(),
        react: Notify::new(),
    });
    tokio::spawn(server::run_deliveries(state.clone()));
    tokio::spawn(server::run_reaction_notices(state.clone()));
    if let Some(mediator) = &config.mediator {
        let service = Arc::new(server::ChannelService(state.clone()));
        tokio::spawn(wyvrn_service::mediator::run_pickups(service, mediator.poll_every()));
    }
    wyvrn_service::http::serve(&config.server.bind, server::router(state)).await
}
