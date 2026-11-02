//! Running a community server: what `wyvrn-community` and `wyvrn-claude` both
//! start (the latter with an assistant answering in its channels), and what
//! `wyvrn-community-host` opens once for each community it keeps ([`open`]).

use std::sync::Arc;

use anyhow::Context;
use wyvrn_service::db::Location;
use wyvrn_service::hosted_address::{Address, Holding};
use wyvrn_service::now;

use crate::{
    assistant::Assistant,
    community::Community,
    config::Config,
    server::{self, AppState},
    store::Store,
};

/// Where each of a community's stores is. The community's own also keeps its
/// keys (for a database), what an admin changed of its profile, and its public
/// address's log; its packs and its files' records are kept apart, since their
/// tables share names with its own.
#[derive(Debug, Clone)]
pub struct Stores {
    pub community: Location,
    pub stickers: Option<Location>,
    pub files: Option<Location>,
}

impl Stores {
    /// Where the configuration and the environment say, for a community that is
    /// the only one its process runs: `COMMUNITY_DATABASE_URL` (or `DATABASE_URL`),
    /// `COMMUNITY_STICKERS_DATABASE_URL` and `COMMUNITY_FILES_DATABASE_URL`, each
    /// else its file.
    pub fn from_env(config: &Config) -> anyhow::Result<Self> {
        Ok(Self {
            community: Location::from_env("COMMUNITY", &config.store.path)?,
            stickers: config.stickers.as_ref().map(|stickers| Location::from_env("COMMUNITY_STICKERS", &stickers.path)).transpose()?,
            files: config.files.as_ref().map(|files| Location::from_env("COMMUNITY_FILES", &files.dir.join("files.sqlite"))).transpose()?,
        })
    }
}

pub fn open_store(config: &Config) -> anyhow::Result<Store> {
    // In the file the configuration names, unless the environment names a
    // database (COMMUNITY_DATABASE_URL or DATABASE_URL: mysql://user:password@host/database).
    let location = Location::from_env("COMMUNITY", &config.store.path)?;
    Store::open(&location).with_context(|| format!("store {location}"))
}

/// How a community is opened beside its configuration and its stores.
#[derive(Default)]
pub struct Opening {
    /// The start of the environment settings that may say who it is
    /// (`COMMUNITY`: `COMMUNITY_PROFILE_NAME`, …); none where one process keeps
    /// many communities, which the environment can't speak for one by one.
    pub profile_env: Option<&'static str>,
    /// The public address it holds at a DID host, if any.
    pub address: Option<Address>,
    /// What its voice participants' tokens start with (see [`crate::voice::Rooms`]).
    pub voice_tokens: String,
    /// An assistant answering in its channels.
    pub assistant: Option<Assistant>,
    /// Pages its host adds for its owner alone (see `AppState::owner_pages`).
    pub owner_pages: Option<Arc<dyn wyvrn_service::admin::Pages>>,
}

/// A community, opened: its state, which [`server::router`] serves, and its hold
/// on its public address, to be run for as long as it is open.
pub struct Opened {
    pub state: Arc<AppState>,
    pub holding: Option<Holding>,
}

/// Opens `config`'s community from `stores`, ready to be served. Nothing here
/// reads the environment, and nothing is started: the caller runs the holding,
/// and the background work (`server::deliver_pending`, `polls_once`, `sweep_once`,
/// `prune_once`), as it sees fit.
pub async fn open(config: &Config, stores: &Stores, opening: Opening) -> anyhow::Result<Opened> {
    let location = &stores.community;
    // Kept with the store: in the database, for a service that has one.
    let identity = wyvrn_service::identity::load_or_generate(location, &config.identity.path, "community")?;
    let (agent, did_document) = server::build_agent_for(config, identity, opening.profile_env)?;
    // What an admin has changed of its name, description and picture is kept with
    // the rest, and laid over the settings.
    let kept = wyvrn_service::profile::Kept::open(location, "community")?;
    let store = Store::open(location).with_context(|| format!("store {location}"))?;
    // On the web: on for a community made now, off for one from before (until an
    // admin turns it on). Settled before anyone is let in.
    let web = crate::web::Web::new(config)?.map(Arc::new);
    if web.is_some() {
        crate::web::settle_new(&store)?;
    }
    let mut community = Community::new(store, config.community.clone(), Some(kept))?.with_voice(config.voice.clone());
    community.web_url = web.as_ref().map(|web| web.url.to_string());
    community.public_url = config.server.public_url.clone();
    community.assistant = opening.assistant.as_ref().map(Assistant::listing);
    if let Some(stickers) = &stores.stickers {
        community.expressions = Some(crate::expressions::Expressions::open(stickers, &config.server.public_url).context("the community's packs")?);
    }
    if let (Some(files), Some(location)) = (&config.files, &stores.files) {
        community.files = Some(crate::files::Files::open(files, location, &config.server.public_url).context("the files members attach")?);
    }
    if let Some(document) = &did_document {
        document.changed(&community.changed());
    }
    community.admit_configured(now())?;

    let mut state = AppState::new(agent, Default::default(), community, did_document, didcomm_quickstart::default_resolver());
    state.rooms = crate::voice::Rooms::with_token_prefix(&opening.voice_tokens);
    state.owner_pages = opening.owner_pages;
    state.web = web;
    let state = Arc::new(match opening.assistant {
        Some(assistant) => {
            tracing::info!(name = %assistant.config.name, model = %assistant.config.model, "an assistant answers in every channel");
            state.with_assistant(assistant)
        }
        None => state,
    });
    let holding = match (opening.address, state.did_document.clone()) {
        (Some(address), Some(document)) => Some(holding(&state, address, document, location)?),
        _ => None,
    };
    Ok(Opened { state, holding })
}

/// The community's hold on `address`: asked of its DID host as its `did:web`, and
/// spoken as once held.
fn holding(state: &Arc<AppState>, address: Address, document: wyvrn_service::address::Published, location: &Location) -> anyhow::Result<Holding> {
    let asking = state.clone();
    let ask: wyvrn_service::hosted_address::Ask = Box::new(move |to, message| {
        let asking = asking.clone();
        // As its did:web, which the host set the name aside for, whatever
        // DID the service speaks as to everyone else.
        Box::pin(async move { Ok(asking.agent.request_as(asking.agent.base_did(), &to, &message).await?.message) })
    });
    let kept = wyvrn_service::hosted_address::Kept::open(location, "community")?;
    let speaking = state.clone();
    let held: wyvrn_service::hosted_address::Held = Box::new(move |did, address| {
        let speaking = speaking.clone();
        Box::pin(async move { speaking.voice.take_up(&speaking.agent, &did, &address, wyvrn_service::now()).await })
    });
    Ok(Holding::new(address, kept, document, ask)?.when_held(held))
}

/// Serves `config`'s community until stopped, with `assistant` answering in its
/// channels where there is one.
pub async fn serve(config: Config, assistant: Option<Assistant>) -> anyhow::Result<()> {
    wyvrn_service::logging::init();

    let stores = Stores::from_env(&config)?;
    // A public address at a DID host, if it is set to hold one (COMMUNITY_PUBLIC_ADDRESS):
    // taken up once the service is serving, and kept up to date from then on.
    let address = Address::from_env("COMMUNITY")?;
    let opening = Opening { profile_env: Some("COMMUNITY"), address, voice_tokens: String::new(), assistant, owner_pages: None };
    let Opened { state, holding } = open(&config, &stores, opening).await?;
    if let Some(mediator) = &config.mediator {
        wyvrn_service::mediator::register(&state.agent, mediator).await?;
    }
    let members = state.community.store.members()?;
    tracing::info!(did = %state.agent.did(), url = %config.server.public_url, name = %config.community.name, members = members.len(), channels = state.community.store.channels()?.len(), "community server ready");
    if members.is_empty() {
        tracing::warn!("nobody is let in yet: set COMMUNITY_ADMINS, or run `wyvrn-community add <did> admin`");
    }
    if let Some(holding) = holding {
        tokio::spawn(holding.run());
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
