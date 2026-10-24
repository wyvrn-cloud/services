use std::sync::Arc;

use anyhow::Context;
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
    // What an admin has changed of its name, description and picture is kept with
    // the rest, and laid over the settings.
    let kept = wyvrn_service::profile::Kept::open(&location, "community")?;
    let community = Community::new(open_store(&config)?, config.community.clone(), Some(kept))?.with_voice(config.voice.clone());
    if let Some(document) = &did_document {
        document.changed(&community.changed());
    }
    community.admit_configured(now())?;
    let members = community.store.members()?;
    tracing::info!(did = %agent.did(), url = %config.server.public_url, name = %config.community.name, members = members.len(), channels = community.store.channels()?.len(), "community server ready");
    if members.is_empty() {
        tracing::warn!("nobody is let in yet: set COMMUNITY_ADMINS, or run `wyvrn-community add <did> admin`");
    }

    let state = Arc::new(AppState::new(agent, Default::default(), community, did_document, didcomm_quickstart::default_resolver()));
    // A public address at a DID host, if it is set to hold one (COMMUNITY_PUBLIC_ADDRESS):
    // taken up once the service is serving, and kept up to date from then on.
    if let (Some(address), Some(document)) = (wyvrn_service::hosted_address::Address::from_env("COMMUNITY")?, state.did_document.clone()) {
        let asking = state.clone();
        let ask: wyvrn_service::hosted_address::Ask = Box::new(move |to, message| {
            let asking = asking.clone();
            // As its did:web, which the host set the name aside for, whatever
            // DID the service speaks as to everyone else.
            Box::pin(async move { Ok(asking.agent.request_as(asking.agent.base_did(), &to, &message).await?.message) })
        });
        let kept = wyvrn_service::hosted_address::Kept::open(&location, "community")?;
        let speaking = state.clone();
        let held: wyvrn_service::hosted_address::Held = Box::new(move |did, address| {
            let speaking = speaking.clone();
            Box::pin(async move { speaking.voice.take_up(&speaking.agent, &did, &address, wyvrn_service::now()).await })
        });
        tokio::spawn(wyvrn_service::hosted_address::Holding::new(address, kept, document, ask)?.when_held(held).run());
    }
    tokio::spawn(server::run_deliveries(state.clone()));
    tokio::spawn(server::run_pruning(state.clone()));
    tokio::spawn(server::run_voice_sweep(state.clone()));
    if let Some(mediator) = &config.mediator {
        let service = Arc::new(server::CommunityService(state.clone()));
        tokio::spawn(wyvrn_service::mediator::run_pickups(service, mediator.poll_every()));
    }
    wyvrn_service::http::serve(&config.server.bind, server::router(state)).await
}
