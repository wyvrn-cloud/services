//! Running a community server: what `wyvrn-community` and `wyvrn-claude` both
//! start (the latter with an assistant answering in its channels).

use std::sync::Arc;

use anyhow::Context;
use wyvrn_service::now;

use crate::{
    assistant::Assistant,
    community::Community,
    config::Config,
    server::{self, AppState},
    store::Store,
};

pub fn open_store(config: &Config) -> anyhow::Result<Store> {
    // In the file the configuration names, unless the environment names a
    // database (COMMUNITY_DATABASE_URL or DATABASE_URL: mysql://user:password@host/database).
    let location = wyvrn_service::db::Location::from_env("COMMUNITY", &config.store.path)?;
    Store::open(&location).with_context(|| format!("store {location}"))
}


/// Serves `config`'s community until stopped, with `assistant` answering in its
/// channels where there is one.
pub async fn serve(config: Config, assistant: Option<Assistant>) -> anyhow::Result<()> {
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
    let mut community = Community::new(open_store(&config)?, config.community.clone(), Some(kept))?.with_voice(config.voice.clone());
    community.assistant = assistant.as_ref().map(Assistant::listing);
    if let Some(files) = &config.files {
        community.files = Some(crate::files::Files::open(files, &config.server.public_url).context("the files members attach")?);
    }
    if let Some(document) = &did_document {
        document.changed(&community.changed());
    }
    community.admit_configured(now())?;
    let members = community.store.members()?;
    tracing::info!(did = %agent.did(), url = %config.server.public_url, name = %config.community.name, members = members.len(), channels = community.store.channels()?.len(), "community server ready");
    if members.is_empty() {
        tracing::warn!("nobody is let in yet: set COMMUNITY_ADMINS, or run `wyvrn-community add <did> admin`");
    }

    let state = AppState::new(agent, Default::default(), community, did_document, didcomm_quickstart::default_resolver());
    let state = Arc::new(match assistant {
        Some(assistant) => {
            tracing::info!(name = %assistant.config.name, model = %assistant.config.model, "an assistant answers in every channel");
            state.with_assistant(assistant)
        }
        None => state,
    });
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
    tokio::spawn(server::run_polls(state.clone()));
    if let Some(mediator) = &config.mediator {
        let service = Arc::new(server::CommunityService(state.clone()));
        tokio::spawn(wyvrn_service::mediator::run_pickups(service, mediator.poll_every()));
    }
    wyvrn_service::http::serve(&config.server.bind, server::router(state)).await
}
