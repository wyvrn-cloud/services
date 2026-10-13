use std::sync::Arc;

use anyhow::Context;
use tokio::sync::Notify;
use wyvrn_community::{
    community::Community,
    config::Config,
    server::{self, AppState},
    store::{Role, Store},
};
use wyvrn_service::now;

const USAGE: &str = "\
wyvrn-community [--config <path>] [serve]              run the server
wyvrn-community [--config <path>] members              everyone let in
wyvrn-community [--config <path>] add <did> [admin]    let someone in, or make them an admin
wyvrn-community [--config <path>] remove <did>         take someone out
wyvrn-community [--config <path>] channels             every channel

The config path is --config, else $COMMUNITY_CONFIG, else config/default.toml.
Only someone let in is answered. A DID is the ID their app shows for them.";

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
        None => std::env::var("COMMUNITY_CONFIG").unwrap_or_else(|_| "config/default.toml".to_string()),
    };
    let config = Config::load(&config_path)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["serve"] => serve(config).await,
        ["members"] => {
            for member in open_store(&config)?.members()? {
                println!("{}\t{}\t{}\t{}", member.role.as_str(), if member.joined { "joined" } else { "not joined" }, member.name.unwrap_or_default(), member.did);
            }
            Ok(())
        }
        ["add", did] | ["add", did, "member"] => add(&config, did, Role::Member),
        ["add", did, "admin"] => add(&config, did, Role::Admin),
        ["remove", did] => {
            let store = open_store(&config)?;
            if store.member(did)?.is_some_and(|member| member.role == Role::Admin) && store.admin_count()? <= 1 {
                anyhow::bail!("that is the only admin: make someone else one first");
            }
            println!("{}", if store.remove_member(did)? { "taken out" } else { "they weren't in" });
            Ok(())
        }
        ["channels"] => {
            for channel in open_store(&config)?.channels()? {
                println!("{}\t{}\t{}\t{}", channel.id, if channel.closed { "closed" } else { "open" }, channel.latest_seq, channel.name);
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
    // database (COMMUNITY_DATABASE_URL or DATABASE_URL: mysql://user:password@host/database).
    let location = wyvrn_service::db::Location::from_env("COMMUNITY", &config.store.path)?;
    Store::open(&location).with_context(|| format!("store {location}"))
}

fn add(config: &Config, did: &str, role: Role) -> anyhow::Result<()> {
    if !did.starts_with("did:") {
        anyhow::bail!("{did} isn't a DID");
    }
    let store = open_store(config)?;
    if store.is_superseded(did)? {
        anyhow::bail!("that DID has been replaced by another");
    }
    store.set_member(did, role, now())?;
    println!("let in as {}", role.as_str());
    Ok(())
}

async fn serve(config: Config) -> anyhow::Result<()> {
    wyvrn_service::logging::init();

    // Kept with the store: in the database, for a service that has one.
    let location = wyvrn_service::db::Location::from_env("COMMUNITY", &config.store.path)?;
    let identity = wyvrn_service::identity::load_or_generate(&location, &config.identity.path, "community")?;
    let (agent, did_document) = server::build_agent(&config, identity)?;
    if let Some(mediator) = &config.mediator {
        wyvrn_service::mediator::register(&agent, mediator).await?;
    }
    let community = Community { store: open_store(&config)?, config: config.community.clone() };
    community.admit_configured(now())?;
    let members = community.store.members()?;
    tracing::info!(did = %agent.did(), url = %config.server.public_url, name = %config.community.name, members = members.len(), channels = community.store.channels()?.len(), "community server ready");
    if members.is_empty() {
        tracing::warn!("nobody is let in yet: set COMMUNITY_ADMINS, or run `wyvrn-community add <did> admin`");
    }

    let state = Arc::new(AppState { agent, community, did_document, resolver: didcomm_quickstart::default_resolver(), deliver: Notify::new() });
    tokio::spawn(server::run_deliveries(state.clone()));
    tokio::spawn(server::run_pruning(state.clone()));
    if let Some(mediator) = &config.mediator {
        let service = Arc::new(server::CommunityService(state.clone()));
        tokio::spawn(wyvrn_service::mediator::run_pickups(service, mediator.poll_every()));
    }
    wyvrn_service::http::serve(&config.server.bind, server::router(state)).await
}
