//! The HTTP side: the endpoint members' messages come to, and the sending of
//! what is posted to everyone who has joined.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{
        ws::{rejection::WebSocketUpgradeRejection, Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tokio::sync::mpsc;
use didcomm_agent::{Agent, Features, Identity, Received};
use didcomm_core::resolver::DIDResolver;
use didcomm_crypto_askar::AskarCryptoService;
use serde_json::{json, Value};
use tokio::sync::{Notify, Semaphore};
use tower_http::cors::CorsLayer;
use wyvrn_service::{address, http::receive, now, Incoming, Service};

use crate::community::{self, Community};
use crate::config::Config;
use crate::posting::{DEFAULT_LEVEL, WATCH};
use crate::questions;
use crate::voice::{self, Rooms, VoiceOutcome};

/// How many members are sent to at once.
const DELIVERY_CONCURRENCY: usize = 8;
/// How long before someone who couldn't be reached is tried again.
pub const DELIVERY_RETRY: Duration = Duration::from_secs(60);
/// How many posts one member is sent in one pass.
const DELIVERY_BATCH: usize = 100;
/// How often answers nobody finished are forgotten, and how old they have to be.
pub const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);
const ANSWERING_KEPT_FOR: i64 = 24 * 60 * 60;

pub struct AppState {
    pub agent: Agent,
    /// Which DID it speaks as: its address's, once it holds one (wyvrn_service::voice).
    pub voice: wyvrn_service::Voice,
    pub community: Community,
    /// Published at `/.well-known/did.json` when the server is a `did:web`.
    pub did_document: Option<address::Published>,
    /// Resolves the DID a `from_prior` names, to check who signed it.
    pub resolver: Box<dyn DIDResolver>,
    /// Rung when there is something new to send to members.
    pub deliver: Notify,
    /// Who just posted: the next delivery sends to them before anyone else, so
    /// their own post comes back to them (and their other devices) without
    /// waiting behind everyone's.
    pub deliver_first: Mutex<Vec<String>>,
    /// Voice channels' rooms (community-voice/1.0).
    pub rooms: Rooms,
    /// Members' live connections, by number: where to put what is sent them there.
    pub live: Mutex<HashMap<u64, (String, mpsc::Sender<Value>)>>,
    /// The live connections that are watching (community/1.0's `watch`): sent every
    /// post and change, which then don't go through their mediator.
    pub watching: Mutex<HashSet<u64>>,
    pub next_live: AtomicU64,
    /// Live pages and connections open now, by address and by member (WYV-51).
    pub live_open: crate::web::posting::LiveCounts,
    /// The AI assistant answering in its channels, where it has one
    /// (wyvrn-claude; assistant.rs).
    pub assistant: Option<Arc<crate::assistant::Assistant>>,
    /// Pages its host adds for the community's owner alone (wyvrn-community-host's
    /// moving to a server of their own), as wyvrn_service::admin pages; and those open.
    pub owner_pages: Option<Arc<dyn wyvrn_service::admin::Pages>>,
    pub owner_admin: wyvrn_service::admin::Admin,
    /// The community on the web, where the server shows it (web/).
    pub web: Option<Arc<crate::web::Web>>,
    /// Who a request is from, for every limit by address (clients.rs, WYV-98).
    pub clients: crate::clients::Clients,
    /// How an issuer's DID document and status list are fetched, to check a linked
    /// account (claims.rs): [`fetch_small_json`] unless a test serves them.
    pub fetch_json: std::sync::RwLock<Option<JsonFetch>>,
}

/// Gets a URL's JSON, as [`fetch_small_json`] does.
pub type JsonFetch = Arc<dyn Fn(String) -> futures_util::future::BoxFuture<'static, Option<Value>> + Send + Sync>;

impl AppState {
    /// The parts a new server starts with, beside what `main` builds.
    pub fn new(agent: Agent, voice: wyvrn_service::Voice, mut community: Community, did_document: Option<address::Published>, resolver: Box<dyn DIDResolver>) -> Self {
        community.own = Some(agent.did());
        community.sealed_to = Some(crate::sealing::bound_to(agent.identity()));
        community.speaks_as = voice.speaking_as().filter(|did| *did != agent.did());
        Self {
            agent,
            voice,
            community,
            did_document,
            resolver,
            deliver: Notify::new(),
            deliver_first: Mutex::new(Vec::new()),
            rooms: Rooms::default(),
            live: Mutex::new(HashMap::new()),
            watching: Mutex::new(HashSet::new()),
            next_live: AtomicU64::new(1),
            live_open: Default::default(),
            assistant: None,
            owner_pages: None,
            owner_admin: Default::default(),
            web: None,
            clients: Default::default(),
            fetch_json: Default::default(),
        }
    }

    /// Fetches `url`'s JSON for checking a linked account ([`AppState::fetch_json`]).
    async fn issuer_json(&self, url: String) -> Option<Value> {
        let fetch = self.fetch_json.read().unwrap_or_else(|p| p.into_inner()).clone();
        match fetch {
            Some(fetch) => fetch(url).await,
            None => fetch_small_json(url).await,
        }
    }

    /// With an AI assistant answering in every channel.
    pub fn with_assistant(mut self, assistant: crate::assistant::Assistant) -> Self {
        self.assistant = Some(Arc::new(assistant));
        self
    }

    fn live_sender(&self, id: u64) -> Option<mpsc::Sender<Value>> {
        self.live.lock().unwrap_or_else(|p| p.into_inner()).get(&id).map(|(_, out)| out.clone())
    }

    /// Whether `did` is watching over a live connection.
    fn is_watching(&self, did: &str) -> bool {
        let live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        let watching = self.watching.lock().unwrap_or_else(|p| p.into_inner());
        watching.iter().any(|id| live.get(id).is_some_and(|(holder, _)| holder == did))
    }

    /// Puts `message` on every watching connection of `did`'s. Whether any took it:
    /// one that can't is no longer watching, and the message goes as if it weren't.
    fn to_watching(&self, did: &str, message: &Value) -> bool {
        let ids: Vec<u64> = {
            let live = self.live.lock().unwrap_or_else(|p| p.into_inner());
            let watching = self.watching.lock().unwrap_or_else(|p| p.into_inner());
            watching.iter().filter(|id| live.get(id).is_some_and(|(holder, _)| holder == did)).copied().collect()
        };
        let mut sent = false;
        for id in ids {
            match self.live_sender(id) {
                Some(out) if out.try_send(message.clone()).is_ok() => sent = true,
                _ => {
                    self.watching.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
                }
            }
        }
        sent
    }

    /// Puts `message` on every live connection open now.
    fn to_every_live(&self, message: &Value) {
        let senders: Vec<mpsc::Sender<Value>> = self.live.lock().unwrap_or_else(|p| p.into_inner()).values().map(|(_, out)| out.clone()).collect();
        for out in senders {
            let _ = out.try_send(message.clone());
        }
    }
}

/// Sends what a voice message came to: presence to live connections, the rest to
/// each member over a live connection of theirs, or through DIDComm.
fn voice_send(state: &Arc<AppState>, outcome: &VoiceOutcome) {
    for (id, message) in &outcome.live {
        if let Some(out) = state.live_sender(*id) {
            let _ = out.try_send(message.clone());
        }
    }
    for (did, message) in &outcome.notices {
        let live = state.rooms.live_of(did);
        let mut sent = false;
        for id in live {
            if let Some(out) = state.live_sender(id) {
                sent |= out.try_send(message.clone()).is_ok();
            }
        }
        // A ring has to reach a phone that isn't listening: through its mediator too.
        if !sent || message["type"] == voice::RING {
            send_later(state, did.clone(), message.clone());
        }
    }
    if outcome.posted {
        state.deliver.notify_one();
    }
}

/// The agent for `config`: the server's own DID, which members write to.
pub fn build_agent(config: &Config, identity: Identity) -> anyhow::Result<(Agent, Option<address::Published>)> {
    build_agent_for(config, identity, Some("COMMUNITY"))
}

/// As [`build_agent`], with who it says it is taken from the environment settings
/// starting `profile_env` (`COMMUNITY_PROFILE_NAME`, …), or from `config` alone.
pub fn build_agent_for(config: &Config, identity: Identity, profile_env: Option<&str>) -> anyhow::Result<(Agent, Option<address::Published>)> {
    let (agent, document) = address::build_agent(identity, &config.server.public_url, config.server.did_method)?;
    // Who it is, for whoever looks its DID up before writing to it.
    let profile = match profile_env {
        Some(prefix) => config.profile.or(prefix, &config.community.name, &config.community.description),
        None => config.profile.or_plainly(&config.community.name, &config.community.description),
    };
    let document = document.map(|document| document.with_profile(&profile)).transpose()?;
    let features = Features::standard()
        .with_protocol(community::PIURI, &["server"])
        .with_protocol(questions::PIURI, &["processor"])
        // A member's app sends its profile to whoever takes one: how the server
        // learns what to call them.
        .with_protocol(community::USER_PROFILE, &[])
        // A post is reacted to, changed, taken back and pinned as any message is.
        .with_protocol(community::MESSAGE_REACTIONS, &["observer"])
        .with_protocol(community::MESSAGE_EDITS, &["observer"])
        .with_protocol(community::MESSAGE_PINS, &["participant"])
        // A member that speaks as its public address is followed there (community/1.0,
        // "A member may move to its public address").
        .with_protocol(wyvrn_service::named::DID_HOSTING, &[wyvrn_service::named::CORRESPONDENT]);
    // Voice channels, where the server has them; and the live connection they
    // need, listed after the address messages are posted to.
    let features = if config.voice.is_some() { features.with_protocol(voice::PIURI, &["server"]) } else { features };
    // Every community has one: posts go live to whoever is watching (community/1.0).
    let document = match document {
        Some(mut published) => {
            let did = published.did().to_string();
            if let Some(services) = published.document["service"].as_array_mut() {
                services.push(json!({ "id": format!("{did}#live"), "type": "DIDCommMessaging", "serviceEndpoint": { "uri": socket_url(&config.server.public_url), "accept": ["didcomm/v2"], "routingKeys": [] } }));
            }
            Some(published)
        }
        other => other,
    };
    Ok((agent.with_features(features), document))
}

/// `public_url` as a WebSocket address: `https` to `wss`, `http` to `ws`.
pub fn socket_url(public_url: &str) -> String {
    if let Some(rest) = public_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = public_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        public_url.to_string()
    }
}

/// A member's DID change, if `received` announces one that checks out: signed by a
/// key the DID it names as left lists, and naming the sender as who it became.
async fn rotation(state: &AppState, received: &Received, sender: &str) -> Option<(String, String)> {
    let jwt = received.message["from_prior"].as_str()?;
    match didcomm_core::rotation::verify_from_prior(&AskarCryptoService, state.resolver.as_ref(), jwt).await {
        Ok((prior, new)) if new == sender && prior != new => Some((moving_from(state, prior, &new, jwt).await, new)),
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(%sender, %error, "ignoring a from_prior that doesn't check out");
            None
        }
    }
}

/// The DID a verified move to `new` is taken from: `prior`, or, where `prior` has
/// moved already and its member has moved on since, the DID they are at now --
/// when the key that signed it is one that DID speaks with too.
///
/// An app that changed DID several times since a server last heard from it signs
/// a move from each DID it has been straight to the one it is at, oldest first
/// (community/1.0, "A member is a DID"; wyvrn-chat's rotationNotice.ts). A server
/// that followed some of those changes already holds the oldest as moved to a
/// later one, which has not moved yet as far as it knows: taken as it stands, the
/// move would be a second one from that DID, and refused (WYV-27). But whoever
/// signed it holds a key the member's current DID authenticates with, so could
/// move that DID itself: it is that DID's first move, not a fork by someone left
/// holding only the old DID's keys (a device removed along the way is not in the
/// current DID). A member at its public address moves away from it only by a
/// `from_prior` from the address itself, so this is for a `did:peer` only.
async fn moving_from(state: &AppState, prior: String, new: &str, from_prior: &str) -> String {
    let at = match state.community.store.moved_elsewhere(&prior, new) {
        Ok(Some(at)) if at.starts_with("did:peer:") => at,
        _ => return prior,
    };
    if !wyvrn_service::from_prior::signed_with_a_key_of(state.resolver.as_ref(), from_prior, &at).await {
        return prior;
    }
    tracing::debug!(%prior, %at, %new, "a move from a DID followed on already, signed by a key of where its member is now: taken from there");
    at
}

/// Sends a post as it now stands to everyone joined: over a watching connection,
/// or through the mediator to whoever chose to be sent changes. Anyone else asks
/// for what changed when they next watch.
fn send_changed(state: &Arc<AppState>, notice: &Value) -> anyhow::Result<()> {
    let channel = notice["body"]["channel"].as_str().unwrap_or_default();
    for member in state.community.store.joined()? {
        // Not to anyone who may not see the channel, nor a private thread's post to
        // anyone not in it.
        if !state.community.sees_id(&member.did, channel)? {
            continue;
        }
        if let Some(thread) = notice["body"]["thread"].as_str() {
            if let Some(found) = state.community.store.channel(channel)? {
                if !state.community.reads_thread(&member.did, &state.community.powers(&member.did)?, &found, thread)? {
                    continue;
                }
            }
        }
        if !state.to_watching(&member.did, notice) && state.community.changes_reach(&member)? {
            send_later(state, member.did, notice.clone());
        }
    }
    Ok(())
}

/// Tells everyone joined, but `except`, what the community now is to them -- after
/// whoever caused it has had their answer. Making each member's account of it reads
/// the store for every one of them, which is no reason to keep the asker waiting.
fn tell_everyone_later(state: &Arc<AppState>, except: Option<String>) {
    let state = state.clone();
    tokio::spawn(async move {
        let notices = match state.community.community_notices() {
            Ok(notices) => notices,
            Err(error) => return tracing::warn!(%error, "couldn't tell members what changed in the community"),
        };
        for (did, notice) in notices {
            if except.as_deref() != Some(did.as_str()) {
                send_later(&state, did, notice);
            }
        }
    });
}

/// [`send_changed`] for post `id` as it stands, after whoever changed it has had
/// their answer.
pub(crate) fn send_changed_later(state: &Arc<AppState>, id: String) {
    let state = state.clone();
    tokio::spawn(async move {
        let sent = state.community.store.post_by_id(&id).map_err(anyhow::Error::from).and_then(|post| match post {
            Some(post) => {
                // And to whatever takes events (outgoing.rs).
                state.community.post_changed_event(&post, now());
                send_changed(&state, &state.community.changed_notice(&post))
            }
            None => Ok(()),
        });
        if let Err(error) = sent {
            tracing::warn!(post = %id, %error, "couldn't send a changed post");
        }
    });
}

/// When each member was last asked for their profile, by DID.
static PROFILE_ASKED: std::sync::Mutex<Option<std::collections::HashMap<String, i64>>> = std::sync::Mutex::new(None);

/// Asks a member this server holds no picture or profile for to send theirs
/// (user-profile/1.0's `request-profile`), at most once a day: a member who joined
/// before the server kept pictures (or whose app sent only a name) is otherwise
/// never asked again, and is shown without a picture to everyone, on the web too.
fn ask_for_profile(state: &Arc<AppState>, did: &str) {
    let store = &state.community.store;
    let lacking = matches!(store.member(did), Ok(Some(_)))
        && matches!(store.member_picture(did), Ok(None))
        && matches!(store.member_profile(did), Ok(None));
    if !lacking {
        return;
    }
    let at = now();
    {
        let mut asked = PROFILE_ASKED.lock().unwrap_or_else(|p| p.into_inner());
        let asked = asked.get_or_insert_with(std::collections::HashMap::new);
        if asked.get(did).is_some_and(|then| at - then < 24 * 60 * 60) {
            return;
        }
        if asked.len() > 100_000 {
            asked.clear();
        }
        asked.insert(did.to_string(), at);
    }
    send_later(state, did.to_string(), json!({ "type": community::REQUEST_PROFILE, "id": uuid::Uuid::new_v4().to_string(), "body": {} }));
}

/// Sends `message` to `did` without holding up whoever caused it.
fn send_later(state: &Arc<AppState>, did: String, message: Value) {
    let state = state.clone();
    tokio::spawn(async move {
        let message = match state.community.masked_for(&did, &message) {
            Ok(masked) => masked,
            Err(error) => return tracing::warn!(%did, %error, "couldn't name people as they may be named to them"),
        };
        if let Err(error) = state.voice.send(&state.agent, &did, &message).await {
            tracing::warn!(%did, %error, "couldn't send a notice");
        }
    });
}

/// Begins a move to another server, or calls it off, and tells every joined member
/// (community/1.0's "Hosted communities, and moving"). Sent unasked, so from the
/// address the community speaks as, with the `from_prior` that brings whoever knew
/// it by its `did:web` there.
pub fn set_moving(state: &Arc<AppState>, moving: bool) -> anyhow::Result<()> {
    if state.community.set_moving(moving)? {
        tracing::info!(moving, "the community's move to another server");
        for (did, notice) in state.community.community_notices()? {
            send_later(state, did, notice);
        }
    }
    Ok(())
}

/// The server as the shared plumbing sees it: its agent, and what it answers.
pub struct CommunityService(pub Arc<AppState>);

impl Service for CommunityService {
    fn agent(&self) -> &Agent {
        &self.0.agent
    }

    /// Who is let in is decided by who a message can be shown to be from.
    fn requires_authcrypt(&self) -> bool {
        true
    }

    /// Members name each other by member id, and are told DIDs only where they
    /// moderate (member_ids.rs): what arrives is turned back into DIDs before
    /// anything here reads it, and the answer into member ids for whoever asked.
    async fn answer(&self, incoming: &Incoming<'_>) -> anyhow::Result<Option<Value>> {
        let state = &self.0;
        let mut received = incoming.received.clone();
        if let Some(sender) = &received.sender {
            received.message = state.community.untold_dids_dropped(sender, &received.message)?;
        }
        received.message = state.community.unmasked_from(&received.message)?;
        let reply = self.answer_unmasked(&Incoming { received: &received, sender_kid: incoming.sender_kid.clone() }).await?;
        match (reply, &received.sender) {
            (Some(reply), Some(sender)) => Ok(Some(state.community.masked_for(sender, &reply)?)),
            (reply, _) => Ok(reply),
        }
    }
}

impl CommunityService {
    async fn answer_unmasked(&self, incoming: &Incoming<'_>) -> anyhow::Result<Option<Value>> {
        let state = &self.0;
        let received = incoming.received;
        let Some(sender) = received.sender.clone() else {
            return Ok(None);
        };
        // A member whose devices changed is the same member under a new DID.
        if let Some((prior, new)) = rotation(state, received, &sender).await {
            if let Some(notice) = state.community.rotated(&prior, &new, now())? {
                tracing::info!(%prior, %new, "a member changed DID");
                send_later(state, new, notice);
            }
        }
        // Heard from, so there: anything held back for them goes now.
        if state.community.store.delivery_resumed(&sender)? {
            tracing::info!(did = %sender, "heard from a member who couldn't be reached: sending them what they missed");
            state.deliver.notify_one();
        }
        // A ping's answer, to one the web view sent someone signing in.
        if received.message["type"] == crate::web::sign_in::PING_RESPONSE {
            if let (Some(web), Some(thid)) = (&state.web, received.message["thid"].as_str()) {
                // Only from the DID pinged, or a key of its own document: an answer
                // from anyone else who knew the ping's id is no answer.
                if let Some(pinged) = web.sign_in.pinged(thid) {
                    let from_it = sender == pinged || wyvrn_service::named::holder_among(state.resolver.as_ref(), std::slice::from_ref(&pinged), &sender, incoming.sender_kid.as_deref()).await.is_some();
                    if from_it && web.sign_in.answered(thid) {
                        return Ok(None);
                    }
                }
            }
        }
        // What any agent answers (a ping, what it supports): it says what this is,
        // and nothing of what is in it.
        if let Some(reply) = state.agent.auto_reply(received) {
            return Ok(Some(reply));
        }

        // Someone the configuration names by their public address writes from their
        // account's DID: they are who it names, under the DID they write from.
        let store = &state.community.store;
        let config = &state.community.config;
        let kid = incoming.sender_kid.as_deref();
        // A community that lets in only someone with a public address (WYV-62): a
        // join from a DID that names none, or none that names it back, is refused
        // before anything else is done with it. A ban then follows the address.
        if received.message["type"] == crate::community::JOIN && store.member(&sender)?.is_none() && state.community.requires_address()? {
            let mut named: Vec<String> = match state.resolver.resolve(&sender).await {
                // At most five of the names a document gives are looked up (WYV-62):
                // each is a resolution, and a document can list any number.
                Ok(document) => document["alsoKnownAs"].as_array().into_iter().flatten().filter_map(Value::as_str).take(5).map(str::to_string).collect(),
                Err(_) => Vec::new(),
            };
            // A member that writes as its public address has one by the same rules: the
            // did:webvh it writes from is the address (community/1.0).
            if wyvrn_service::named::is_hosted(&sender) {
                named.insert(0, sender.clone());
            }
            // Where the community lists hosts, only a verified did:webvh at one of them
            // counts (community/1.0's address_hosts): the resolver verified its log.
            let hosts = state.community.address_hosts()?;
            let named: Vec<String> = named.into_iter().filter(|address| crate::community::Community::address_at_listed_host(address, &hosts)).collect();
            let Some(address) = wyvrn_service::named::holder_among(state.resolver.as_ref(), &named, &sender, kid).await.map(str::to_string) else {
                let refused = crate::community::refuse(&received.message, "e.p.msg.address-required", "This community lets in only someone with a verified public address: set one up in Settings, Public address, then ask again.")?;
                return Ok(refused.reply);
            };
            // The same address as someone banned: banned, under whichever DID.
            let at = now();
            let banned_address = store.bans(at)?.iter().any(|ban| store.member_profile(&ban.did).ok().flatten().and_then(|p| p.public_address).as_deref() == Some(address.as_str()));
            if banned_address {
                let refused = crate::community::refuse(&received.message, "e.p.trust.not-authorized", "This community is by invitation: you are not on its list.")?;
                return Ok(refused.reply);
            }
        }
        if !store.is_superseded(&sender)? {
            let role = store.member(&sender)?.map(|member| member.role);
            if role != Some(crate::store::Role::Admin) {
                if let Some(named) = wyvrn_service::named::holder_among(state.resolver.as_ref(), &config.admins, &sender, kid).await {
                    store.set_member(&sender, crate::store::Role::Admin, now())?;
                    tracing::info!(%sender, %named, "an admin named by their public address wrote, and is one under the DID they write from");
                } else if role.is_none() && store.banned(&sender, now())?.is_none() {
                    if let Some(named) = wyvrn_service::named::holder_among(state.resolver.as_ref(), &config.members, &sender, kid).await {
                        store.admit(&sender, crate::store::Role::Member, now())?;
                        tracing::info!(%sender, %named, "a member named by their public address wrote, and is let in under the DID they write from");
                    }
                }
            }
            // An owner named by their public address: owner under the DID they
            // write from, kept so that it follows them when they move.
            let others = state.community.owners_named_by_address_besides(&sender)?;
            if let Some(named) = wyvrn_service::named::holder_among(state.resolver.as_ref(), &others, &sender, kid).await {
                state.community.note_owner_holder(named, &sender)?;
                tracing::info!(%sender, %named, "an owner named by their public address wrote, and is one under the DID they write from");
            }
        }

        // The owner's pages from the host: theirs alone, and answered while moving
        // too (that is where a move is called off).
        let message_type = received.message["type"].as_str().unwrap_or_default();
        let is_owner = state.owner_pages.is_some() && state.community.is_owner(&sender)?;
        if let Some(pages) = &state.owner_pages {
            if message_type.starts_with(questions::PIURI) && !message_type.ends_with("/discover") && state.owner_admin.concerns(&received.message) {
                return Ok(state.owner_admin.handle(pages.as_ref(), is_owner, &sender, &received.message, now()).map(|answer| match answer {
                    Ok((kind, body)) => received.reply(&kind, body),
                    Err(refusal) => received.reply(&format!("{}/problem-report", questions::PIURI), refusal.body()),
                }));
            }
        }
        // Moving to another server: forms and files change it, and are refused as
        // everything is but reading (Community::handle). Voice changes nothing kept.
        let reads_forms = ["/discover", "/fetch-template", "/status"].iter().any(|kind| message_type.ends_with(kind));
        if ((message_type.starts_with(questions::PIURI) && !reads_forms) || crate::files::is_files(&received.message)) && state.community.moving()? {
            return Ok(Some(community::problem(&received.message, "e.p.req.moving", "This community is moving to another server. It can be read, and nothing changed, until it has.")));
        }
        // A question's answer, from someone let in. (Anyone else falls through, and
        // is told they aren't.)
        if received.message["type"].as_str().is_some_and(|message_type| message_type.starts_with(questions::PIURI)) {
            if let Some(member) = state.community.store.member(&sender)? {
                // A Discord template to import is read from Discord first (discord.rs).
                let mut message = received.message.clone();
                crate::discord::prefetch(&state.community, &member.did, &mut message).await;
                let mut handled = state.community.handle_workflow(&member, &message, now());
                // The owner is also listed the host's pages for them.
                if let (Some(pages), true, Some(Ok((_, body)))) = (&state.owner_pages, is_owner, handled.answer.as_mut()) {
                    if message_type.ends_with("/discover") {
                        let added = state.owner_admin.listed(pages.as_ref(), true, &received.message["body"]);
                        if let Some(workflows) = body["workflows"].as_array_mut() {
                            workflows.extend(added);
                            body["paging"]["total"] = json!(workflows.len());
                        }
                    }
                }
                if handled.posted {
                    state.deliver.notify_one();
                }
                // Roles or permissions changed: everyone joined is told what they may do now.
                if handled.community_changed {
                    tell_everyone_later(state, None);
                    // (Its name may have changed: a Discord template's, imported.)
                    if let Some(document) = &state.did_document {
                        document.changed(&state.community.changed());
                    }
                }
                return Ok(match handled.answer {
                    Some(Ok((message_type, body))) => Some(received.reply(&message_type, body)),
                    Some(Err(refusal)) => Some(received.reply(
                        &format!("{}/problem-report", questions::PIURI),
                        json!({ "code": refusal.code, "comment": refusal.comment, "args": refusal.args }),
                    )),
                    None => None,
                });
            }
        }

        // Files a member attaches, kept here: for whoever may attach files somewhere.
        if crate::files::is_files(&received.message) {
            if let (Some(files), Some(_)) = (&state.community.files, state.community.store.member(&sender)?) {
                let powers = state.community.powers(&sender)?;
                let may = powers.has("attach_files") || state.community.store.channels()?.iter().any(|channel| state.community.channel_permissions(&sender, &powers, channel).is_ok_and(|held| held.contains("attach_files")));
                if may {
                    return Ok(files.handle(&sender, &received.message, now()).map(|(kind, body)| received.reply(&kind, body)));
                }
            }
        }

        // Voice channels: from a member, or refused as everything is from anyone else.
        if received.message["type"].as_str().is_some_and(|message_type| message_type.starts_with(voice::PIURI)) {
            return answer_voice(state, &sender, &received.message, None);
        }
        // A public address a member's profile names is kept only once it is shown
        // to be theirs -- the key they wrote with is one of its keys -- and with it
        // the linked accounts that are about it: otherwise anyone could claim
        // anyone's address, and the accounts its whois lists.
        let mut checked;
        let message = if received.message["type"] == community::PROFILE && received.message["body"]["profile"]["publicAddress"].is_string() {
            checked = received.message.clone();
            let address = checked["body"]["profile"]["publicAddress"].as_str().unwrap_or_default().to_string();
            if wyvrn_service::named::holder_among(state.resolver.as_ref(), std::slice::from_ref(&address), &sender, kid).await.is_none() {
                if let Some(profile) = checked["body"]["profile"].as_object_mut() {
                    profile.remove("publicAddress");
                    profile.remove("linkedAccounts");
                    profile.remove("memberStatement");
                }
            } else {
                if checked["body"]["profile"].get("banner").is_none() {
                    // Theirs, and their app sent no banner (one that said "none" has
                    // `banner: null`): the one their address publishes, for the
                    // member list (address_banner.rs).
                    if let Some(banner) = crate::address_banner::banner_of_address(&address).await {
                        crate::address_banner::with_banner(&mut checked, banner);
                    }
                }
                // The address's own word that it is this member here: kept only
                // where it checks out against the address as it resolves now.
                if checked["body"]["profile"]["memberStatement"].is_object() && !statement_checks_out(state, &sender, &address, &checked["body"]["profile"]["memberStatement"]).await {
                    if let Some(profile) = checked["body"]["profile"].as_object_mut() {
                        profile.remove("memberStatement");
                    }
                }
            }
            &checked
        } else {
            &received.message
        };
        // A profile whose address is theirs, with linked accounts: one of them may be
        // a Discord account posts were imported under (claims.rs). Checked apart,
        // with what the message said, or else what was kept of their profile (an
        // address kept was shown to be theirs when it was sent).
        let linked_with = if message["type"] == community::PROFILE {
            let profile = &message["body"]["profile"];
            let said = (profile["publicAddress"].as_str().map(str::to_string), profile["linkedAccounts"].as_array().cloned());
            if said.0.is_some() || said.1.is_some() {
                let kept = state.community.store.member_profile(&sender)?;
                let address = said.0.or_else(|| kept.as_ref().and_then(|k| k.public_address.clone()));
                let linked = said.1.or_else(|| kept.and_then(|k| k.linked).and_then(|l| l.as_array().cloned()));
                address.zip(linked)
            } else {
                None
            }
        } else {
            None
        };
        // Asking for imported posts: proven again now, from what their profile
        // shows, they are theirs at once where nobody else has a hand in them;
        // else it is asked for, and decided (claims.rs, Community::handle).
        if message["type"] == community::CLAIM_IMPORTED && state.community.store.member(&sender)?.is_some() && !state.community.moving()? {
            if let Some(outcome) = claim_now(state, &sender, message).await? {
                return Ok(carry_out(state, &sender, outcome));
            }
        }
        // Someone waiting let in (by a moderator, or by joining again with an
        // invite): their profile is looked at again, as it was skipped while they
        // waited.
        let admitting = match message["type"].as_str() {
            Some(community::SET_MEMBER) => message["body"]["did"].as_str().map(str::to_string),
            Some(community::JOIN) => Some(sender.clone()),
            _ => None,
        }
        .filter(|did| matches!(state.community.store.waiting_one(did), Ok(Some(_))));
        let outcome = state.community.handle(&sender, message, now())?;
        if let Some((address, linked)) = linked_with {
            tokio::spawn(consider_profile(Arc::clone(state), sender.clone(), address, linked));
        }
        if let Some(admitted) = admitting.filter(|did| matches!(state.community.store.member(did), Ok(Some(_)))) {
            look_again_at_admitted(state, &admitted);
        }
        if message["type"] != community::PROFILE {
            ask_for_profile(state, &sender);
        }
        Ok(carry_out(state, &sender, outcome))
    }
}

/// Someone just let in from the waiting list: their profile's linked accounts
/// are checked as a member's are, from the profile kept of them; or, none kept
/// (a profile sent while waiting isn't), their app is asked for it.
fn look_again_at_admitted(state: &Arc<AppState>, did: &str) {
    let kept = state.community.store.member_profile(did).ok().flatten();
    let linked = kept.as_ref().and_then(|k| k.linked.as_ref()).and_then(Value::as_array).cloned().filter(|l| !l.is_empty());
    match (kept.as_ref().and_then(|k| k.public_address.clone()), linked) {
        (Some(address), Some(linked)) => {
            tokio::spawn(consider_profile(Arc::clone(state), did.to_string(), address, linked));
        }
        _ => send_later(state, did.to_string(), json!({ "type": community::REQUEST_PROFILE, "id": uuid::Uuid::new_v4().to_string(), "body": {} })),
    }
}

/// Whether `statement` is `address`'s, made for this community (one of its own
/// DIDs, as `domain`) and `sender`'s member id here (as `challenge`), by one of
/// the address's `authentication` keys (wyvrn_hosted_did::member).
async fn statement_checks_out(state: &Arc<AppState>, sender: &str, address: &str, statement: &Value) -> bool {
    let Ok(member) = state.community.store.member_id(sender, now()) else { return false };
    let document = match state.resolver.resolve(address).await {
        Ok(document) => document,
        Err(error) => {
            tracing::info!(%address, %error, "a member's statement isn't kept: their address couldn't be read");
            return false;
        }
    };
    let document: Value = serde_json::to_value(&document).unwrap_or_default();
    let community = statement["proof"]["domain"].as_str().unwrap_or_default();
    if !state.community.own_dids().iter().any(|own| own == community) {
        tracing::info!("a member's statement isn't kept: it is for another community");
        return false;
    }
    match wyvrn_hosted_did::member::check(statement, &document, address, community, &member) {
        Ok(_) => true,
        Err(why) => {
            tracing::info!(%why, "a member's statement isn't kept: it doesn't check out");
            false
        }
    }
}

/// The accounts `linked` proves (about `address`, a member's or someone waiting's
/// public address), kept as theirs, for bans and the gates (protection.rs). None
/// when one is held by someone banned: that ban coming back, which is suggested to
/// the moderators, and nothing imported is made theirs meanwhile (WYV-89).
async fn prove_accounts(state: &Arc<AppState>, did: &str, address: &str, linked: &[Value]) -> Option<Vec<(String, String)>> {
    let community = &state.community;
    let issuers = community.config.linked_account_issuers.clone();
    let now_text = chrono::DateTime::from_timestamp(now(), 0).map(|t| t.to_rfc3339()).unwrap_or_default();
    let verified = crate::claims::verified_linked(linked, address, &issuers, &now_text, |url| state.issuer_json(url)).await;
    let accounts: Vec<(String, String)> = verified.iter().map(|(_, account)| account.clone()).collect();
    let store = &community.store;
    let evading = (|| -> anyhow::Result<bool> {
        if store.member(did)?.is_none() && store.waiting_one(did)?.is_none() {
            return Ok(false);
        }
        store.set_member_accounts(did, &accounts, now())?;
        // What the member list says of them: a summary of each checked now
        // (linked_summary.rs), told to everyone connected where it changed.
        let pictures = store.member_linked_picture_names(did)?;
        let summaries: Vec<Value> = verified.iter().filter_map(|(index, _)| crate::linked_summary::summary(&linked[*index], now(), &pictures)).collect();
        let mut kept = store.member_linked(did)?;
        let before = kept.summaries.clone();
        kept.summaries = (!summaries.is_empty()).then(|| json!(summaries));
        // (Checked again and found the same is no change to tell anyone of.)
        let changed = without_times(before.as_ref()) != without_times(kept.summaries.as_ref());
        store.set_member_linked(did, &kept, now())?;
        if changed && store.member(did)?.is_some() {
            carry_out(state, did, community::Outcome { live: vec![community.member_changed(did)?], ..Default::default() });
        }
        let Some((banned, _service, _account)) = store.banned_holding(&accounts, now())? else { return Ok(false) };
        if banned == did {
            return Ok(false);
        }
        let outcome = community.ban_holders(&banned, now())?;
        carry_out(state, did, outcome);
        Ok(true)
    })();
    match evading {
        Ok(true) => None,
        Ok(false) => Some(accounts),
        Err(error) => {
            tracing::warn!(%error, "a member's linked accounts couldn't be kept");
            Some(accounts)
        }
    }
}

/// Summaries as they would be but for when each was checked.
fn without_times(summaries: Option<&Value>) -> Option<Vec<Value>> {
    summaries.map(|summaries| {
        summaries
            .as_array()
            .into_iter()
            .flatten()
            .map(|summary| {
                let mut summary = summary.clone();
                if let Some(fields) = summary.as_object_mut() {
                    fields.remove("checkedAt");
                }
                summary
            })
            .collect()
    })
}

/// The Discord account among `accounts`, by its id.
fn discord_of(accounts: &[(String, String)]) -> Option<String> {
    accounts.iter().find(|(service, _)| service == "https://discord.com" || service == "https://discord.gg").map(|(_, id)| id.clone())
}

/// What a claim made at once comes to: the posts sent as changed, and those who
/// decide claims told, had it been asked for.
fn claimed_outcome(community: &community::Community, changed: Vec<String>, had_asked: bool) -> anyhow::Result<community::Outcome> {
    Ok(community::Outcome { changed, notices: if had_asked { community.admins_told()? } else { Vec::new() }, ..Default::default() })
}

/// Notes the Discord account a member's profile proves (`linked`, about `address`),
/// where posts were imported under it; and makes those posts theirs at once where
/// nobody else has a hand in them (claims.rs, `Community::proven_discord`). Fetches
/// only from the trusted issuers' own addresses, with a time and size limit.
pub async fn consider_profile(state: Arc<AppState>, did: String, address: String, linked: Vec<Value>) {
    let community = &state.community;
    let Some(accounts) = prove_accounts(&state, &did, &address, &linked).await else { return };
    let Some(discord_id) = discord_of(&accounts) else { return };
    let result = (|| -> anyhow::Result<()> {
        // Someone waiting is looked at again once let in (look_again_at_admitted).
        let Some(member) = community.store.member(&did)? else { return Ok(()) };
        match community.proven_discord(&did, &discord_id, now())? {
            crate::claims::Proven::Nothing => return Ok(()),
            crate::claims::Proven::Claimed { changed, had_asked } => {
                carry_out(&state, &did, claimed_outcome(community, changed, had_asked)?);
            }
            crate::claims::Proven::Review => {}
        }
        // Told where they stand with them.
        let body = community.community(&member)?;
        carry_out(&state, &did, community::Outcome { notices: vec![(did.clone(), json!({ "type": community::COMMUNITY, "body": body }))], ..Default::default() });
        Ok(())
    })();
    if let Err(error) = result {
        tracing::warn!(%error, "a claim on imported posts couldn't be considered");
    }
}

/// `claim-imported` from a member: their profile's Discord account proven again
/// now, from the profile kept of them, and the posts theirs at once if nobody else
/// has a hand in them. None where it isn't so: then it is asked for.
async fn claim_now(state: &Arc<AppState>, did: &str, message: &Value) -> anyhow::Result<Option<community::Outcome>> {
    let community = &state.community;
    let Some(kept) = community.store.member_profile(did)? else { return Ok(None) };
    let (Some(address), Some(linked)) = (kept.public_address, kept.linked.as_ref().and_then(Value::as_array).cloned()) else { return Ok(None) };
    let Some(accounts) = prove_accounts(state, did, &address, &linked).await else { return Ok(None) };
    let Some(discord_id) = discord_of(&accounts) else { return Ok(None) };
    let crate::claims::Proven::Claimed { changed, had_asked } = community.proven_discord(did, &discord_id, now())? else { return Ok(None) };
    let mut outcome = claimed_outcome(community, changed, had_asked)?;
    let Some(member) = community.store.member(did)? else { return Ok(None) };
    outcome.reply = Some(community::reply_to(message, community::COMMUNITY, community.community(&member)?));
    Ok(Some(outcome))
}

/// A small JSON document at an HTTPS address (a DID document, a status list): at
/// most 256 KB, within ten seconds, without following redirects.
async fn fetch_small_json(url: String) -> Option<Value> {
    static HTTP: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
        reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none()).build().expect("a client with a timeout builds")
    });
    if !url.starts_with("https://") {
        return None;
    }
    let mut response = HTTP.get(&url).send().await.ok()?.error_for_status().ok()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > 256 * 1024 {
            return None;
        }
    }
    serde_json::from_slice(&bytes).ok()
}

/// Does what handling a member's message came to (`Community::handle`), however
/// it came: over DIDComm, or from the web view (web/pages.rs's posting). Gives
/// back the answer, if there is one.
pub(crate) fn carry_out(state: &Arc<AppState>, sender: &str, outcome: community::Outcome) -> Option<Value> {
    let sender = sender.to_string();
    for (did, notice) in outcome.notices {
        send_later(state, did, notice);
    }
    for message in &outcome.live {
        state.to_every_live(message);
    }
    for (did, message) in &outcome.live_to {
        state.to_watching(did, message);
    }
    // Who it says it is to someone looking its DID up changes with it.
    if outcome.profile_changed {
        if let Some(document) = &state.did_document {
            document.changed(&state.community.changed());
        }
    }
    if outcome.channels_changed {
        // Everyone else who has joined: whoever asked has it as their answer.
        tell_everyone_later(state, Some(sender.clone()));
    }
    // A post that changed is sent, as it now stands, to everyone who has joined:
    // whoever asked included, for their other devices. Not kept for anyone who
    // can't be reached now: they ask for what changed when they are back.
    for id in &outcome.changed {
        send_changed_later(state, id.clone());
    }
    if outcome.posted {
        if outcome.posted_id.is_some() {
            state.deliver_first.lock().expect("deliver_first lock").push(sender.clone());
        }
        state.deliver.notify_one();
    }
    // A post in a room: the assistant answers it, in its own time.
    if let (Some(_), Some(id)) = (&state.assistant, outcome.posted_id.clone()) {
        tokio::spawn(answer(state.clone(), id));
    }
    outcome.reply
}

/// The assistant's answer to post `id` (assistant.rs): from the room's (or the
/// thread's) latest posts, posted in its thread as the server's own, under the
/// assistant's name, and sent to everyone like any post. What stops it -- a
/// limit, or the model not answering -- is said in the room instead, so nobody
/// is left waiting.
pub async fn answer(state: Arc<AppState>, id: String) {
    if let Err(err) = answer_post(&state, &id).await {
        tracing::warn!(post = %id, "the assistant couldn't answer: {err:#}");
    }
}

async fn answer_post(state: &Arc<AppState>, id: &str) -> anyhow::Result<()> {
    let Some(assistant) = &state.assistant else { return Ok(()) };
    let store = &state.community.store;
    let Some(asked) = store.post_by_id(id)? else { return Ok(()) };
    let own = state.voice.speaking_as().unwrap_or_else(|| state.agent.did());
    if asked.author_did == own || asked.deleted {
        return Ok(());
    }
    let Some(channel) = store.channel(&asked.channel)? else { return Ok(()) };
    let at = now();
    // Each member's count for the day, under a setting named for them by a digest
    // of their DID: a setting's name is at most 64 characters on MySQL, and a DID
    // (a did:peer:4) runs to over a thousand, which MySQL refused, so on MySQL the
    // assistant never answered.
    let day = format!("assistant:day:{}:{}", at / 86_400, crate::assistant::member_key(&asked.author_did));
    let period = format!("assistant:period:{}", at / crate::assistant::PERIOD_SECS);
    // Checked and taken in one go (WYV-57): two questions at once each found room
    // for one more, and both were asked. The period's tokens are taken at their
    // most (max_tokens) now and given back but for what was used.
    static RESERVING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let most = assistant.config.max_tokens as u64;
    let refused = {
        let _reserving = RESERVING.lock().unwrap_or_else(|p| p.into_inner());
        let used: u32 = store.setting(&day)?.and_then(|v| v.parse().ok()).unwrap_or(0);
        let spent: u64 = store.setting(&period)?.and_then(|v| v.parse().ok()).unwrap_or(0);
        match assistant.refusal(used, spent) {
            Some(why) => Some(why.to_string()),
            None => {
                store.set_setting(&day, &(used + 1).to_string())?;
                store.set_setting(&period, &(spent + most).to_string())?;
                None
            }
        }
    };
    let settle = |tokens: u64| -> anyhow::Result<()> {
        let _reserving = RESERVING.lock().unwrap_or_else(|p| p.into_inner());
        let spent: u64 = store.setting(&period)?.and_then(|v| v.parse().ok()).unwrap_or(0);
        store.set_setting(&period, &(spent.saturating_sub(most) + tokens).to_string())?;
        Ok(())
    };
    let text = match refused {
        Some(why) => why,
        None => {
            // The room's, or the thread's with the post it grew from first.
            let mut posts = store.channel_posts_before(&asked.channel, asked.thread.as_deref(), Some(asked.seq + 1), assistant.config.context_posts)?;
            posts.reverse();
            if let Some(root) = asked.thread.as_deref().map(|thread| store.post_by_id(thread)).transpose()?.flatten() {
                posts.insert(0, root);
            }
            let turns = crate::assistant::turns_of(&posts, &own);
            match assistant.ask(&channel.name, &turns).await {
                Ok((text, tokens)) => {
                    settle(tokens)?;
                    text
                }
                Err(err) => {
                    settle(0)?;
                    tracing::warn!(post = %id, "the model didn't answer: {err:#}");
                    format!("{} couldn’t answer just now. Try again in a moment.", assistant.config.name)
                }
            }
        }
    };
    // As the server, under the assistant's name: a member of its own community.
    store.admit(&own, crate::store::Role::Member, at)?;
    store.set_name(&own, Some(&assistant.config.name))?;
    let Some(author) = store.member(&own)? else { return Ok(()) };
    let text: String = text.chars().take(state.community.config.max_post_length).collect();
    let post = store.add_post(&asked.channel, &uuid::Uuid::new_v4().to_string(), text.trim(), &author, None, None, now())?;
    let snippet: String = asked.content.chars().take(120).collect();
    store.set_written_with(&post.id, Some((asked.id.as_str(), asked.author_did.as_str(), Some(snippet.as_str()))), asked.thread.as_deref(), &[])?;
    if let Some(thread) = &asked.thread {
        store.touch_thread(thread)?;
        if let Some(root) = store.post_by_id(thread)? {
            send_changed(state, &state.community.changed_notice(&root))?;
        }
    }
    state.deliver.notify_one();
    Ok(())
}

/// A community-voice/1.0 message from `sender`, over the live connection `live`
/// if it came on one.
fn answer_voice(state: &Arc<AppState>, sender: &str, message: &Value, live: Option<u64>) -> anyhow::Result<Option<Value>> {
    let Some(member) = state.community.store.member(sender)? else {
        return Ok(Some(community::problem(message, "e.p.trust.not-authorized", "This community is by invitation: you are not on its list.")));
    };
    let did = state.voice.speaking_as().unwrap_or_else(|| state.agent.did());
    let outcome = state.rooms.handle(&state.community, &did, &member, message, live, now())?;
    voice_send(state, &outcome);
    Ok(outcome.reply)
}

/// community/1.0's `watch`, on the live connection `live`: send it everything
/// posted and changed, or stop. Answered with `community`, whose `latest_rev` is
/// where an app catches up from. Speaking of it settles what reaches them otherwise.
fn watch(state: &Arc<AppState>, live: u64, sender: &str, message: &Value) -> anyhow::Result<Option<Value>> {
    let Some(member) = state.community.store.member(sender)? else {
        return Ok(Some(community::problem(message, "e.p.trust.not-authorized", "This community is by invitation: you are not on its list.")));
    };
    state.community.store.settle_notify_level(sender, DEFAULT_LEVEL)?;
    let mut watching = state.watching.lock().unwrap_or_else(|p| p.into_inner());
    if message["body"]["stop"].as_bool() == Some(true) {
        watching.remove(&live);
    } else {
        watching.insert(live);
    }
    drop(watching);
    Ok(Some(community::reply_to(message, community::COMMUNITY, state.community.community(&member)?)))
}

/// How often a live connection is pinged, and how long one may say nothing at all
/// (a ping's answer included) before it is taken to have gone.
const LIVE_PING_EVERY: Duration = Duration::from_secs(20);
const LIVE_SILENT_FOR: Duration = Duration::from_secs(60);

/// Live connections open at once, in all and for one DID (WYV-51), and from one
/// client address (WYV-79).
const LIVE_IN_ALL: usize = 5_000;
const LIVE_PER_DID: usize = 8;
const LIVE_PER_ADDRESS: usize = 20;
/// How soon a new live connection says who it is (an authenticated message: the
/// app sends watch-presence as it opens), or is closed (WYV-79).
const LIVE_HELLO_WITHIN: Duration = Duration::from_secs(10);

/// A member's live connection (community-voice/1.0's Connectivity): DIDComm over a
/// WebSocket. What they send is answered on it, and presence and keys are pushed
/// on it. It is one member's: the first authenticated message says whose.
async fn live_connection(state: Arc<AppState>, socket: WebSocket, client: Option<std::net::IpAddr>) {
    let id = state.next_live.fetch_add(1, Ordering::Relaxed);
    let (mut sink, mut stream) = socket.split();
    let (out, mut outgoing) = mpsc::channel::<Value>(256);
    let mut holder: Option<String> = None;
    let writer_state = state.clone();
    let (packed_tx, mut packed_rx) = mpsc::channel::<Vec<u8>>(256);
    // What is pushed (presence, keys) is packed for the holder here.
    let holder_cell = Arc::new(Mutex::new(None::<String>));
    let pack_holder = holder_cell.clone();
    let packing = tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            let Some(to) = pack_holder.lock().unwrap_or_else(|p| p.into_inner()).clone() else { continue };
            let message = match writer_state.community.masked_for(&to, &message) {
                Ok(masked) => masked,
                Err(error) => {
                    tracing::warn!(%error, "couldn't name people as they may be named to them");
                    continue;
                }
            };
            let from = writer_state.voice.speaking_as().unwrap_or_else(|| writer_state.agent.did());
            match writer_state.agent.messaging().pack_direct(&message, &to, Some(&from)).await {
                Ok(packed) => {
                    if packed_tx.send(packed.message).await.is_err() {
                        break;
                    }
                }
                Err(error) => tracing::warn!(%error, "couldn't pack for a live connection"),
            }
        }
    });
    let (reply_tx, mut reply_rx) = mpsc::channel::<Vec<u8>>(64);
    // Closing it, saying why: too many messages from its address (WYV-97).
    let (close_tx, mut close_rx) = mpsc::channel::<(u16, &'static str)>(1);
    let mut writing = tokio::spawn(async move {
        // A ping now and then: what tells a connection that has died without
        // closing (a device asleep, its network changed) from a quiet one. Its
        // answer, or anything else, keeps the reading below going.
        let mut pings = tokio::time::interval(LIVE_PING_EVERY);
        pings.tick().await;
        loop {
            let packed = tokio::select! {
                Some((code, reason)) = close_rx.recv() => {
                    let _ = sink.send(Message::Close(Some(axum::extract::ws::CloseFrame { code, reason: reason.into() }))).await;
                    break;
                }
                Some(p) = packed_rx.recv() => p,
                Some(p) = reply_rx.recv() => p,
                _ = pings.tick() => {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                    continue;
                }
                else => break,
            };
            let frame = match String::from_utf8(packed) {
                Ok(text) => Message::Text(text.into()),
                Err(bytes) => Message::Binary(bytes.into_bytes().into()),
            };
            if sink.send(frame).await.is_err() {
                break;
            }
        }
    });
    let service = CommunityService(state.clone());
    // So many live connections in all, and for one DID (WYV-51): held while open.
    // And so many from one client address (WYV-79): a trusted proxy's own (this
    // machine's: a test, a tool beside it) aren't counted.
    let mut counted = vec![("ws:all".to_string(), LIVE_IN_ALL)];
    if let Some(client) = client {
        counted.push((format!("ws-at:{client}"), LIVE_PER_ADDRESS));
    }
    let Some(_all) = crate::web::posting::LiveHeld::take(&state.live_open, counted) else {
        packing.abort();
        writing.abort();
        return;
    };
    let mut _theirs: Option<crate::web::posting::LiveHeld> = None;
    // Nothing heard for this long, not even the answer to a ping: gone, and out of
    // any room it was in, rather than listed there until TCP gives up on it.
    // Until it has said who it is, it has LIVE_HELLO_WITHIN to (WYV-79): an idle
    // socket no one speaks on holds no place.
    let hello_by = tokio::time::Instant::now() + LIVE_HELLO_WITHIN;
    // Whom the holder's frames say they are from, as read without opening them
    // (`frame_sender`: `skid`, else `apu`): anyone else's are dropped unopened.
    let mut holder_sender: Option<String> = None;
    let mut limited = false;
    loop {
        let wait = tokio::time::Instant::now() + LIVE_SILENT_FOR;
        let until = if holder.is_none() { wait.min(hello_by) } else { wait };
        let Ok(Some(Ok(frame))) = tokio::time::timeout_at(until, stream.next()).await else { break };
        let bytes: Vec<u8> = match frame {
            Message::Text(text) => text.as_bytes().to_vec(),
            Message::Binary(bytes) => bytes.to_vec(),
            Message::Close(_) => break,
            _ => continue,
        };
        if bytes.len() > 4 * 1024 * 1024 {
            continue;
        }
        // Each frame counts against its address's messages a minute, as a POST does
        // (WYV-97). Over that, the connection is closed with "try again later",
        // WebSocket's 429: the frame is never opened, so there is nobody to answer
        // in DIDComm.
        if let Some(client) = client {
            if !message_allowed(client) {
                limited = true;
                let _ = close_tx.try_send((1013, "Too many messages from this address: wait a minute."));
                break;
            }
        }
        // One member's connection: anyone else's message on it, or one that names
        // no sender at all, is dropped before anything is resolved or opened (WYV-97,
        // WYV-120).
        if holder.is_some() && frame_sender(&bytes) != holder_sender {
            continue;
        }
        // DID documents fetched to open it are charged to the socket's address
        // (WYV-73); past its share the frame is dropped unopened.
        let Ok(mut received) = wyvrn_service::fetch_budget::charged(client, state.agent.receive(&bytes)).await else { continue };
        let Ok(unpacked) = wyvrn_service::fetch_budget::charged(client, state.agent.messaging().unpack_verified(&bytes)).await else { continue };
        let (Some(sender), Some(sender_kid)) = (received.sender.clone(), unpacked.sender_kid) else { continue };
        match &holder {
            None => {
                let Some(held) = crate::web::posting::LiveHeld::take(&state.live_open, vec![(format!("ws:{sender}"), LIVE_PER_DID)]) else { break };
                _theirs = Some(held);
                holder_sender = frame_sender(&bytes);
                holder = Some(sender.clone());
                *holder_cell.lock().unwrap_or_else(|p| p.into_inner()) = Some(sender.clone());
                state.live.lock().unwrap_or_else(|p| p.into_inner()).insert(id, (sender.clone(), out.clone()));
                state.rooms.open_live(id, &sender);
                // Seen online now, for whoever keeps a live connection, if they chose to be.
                match state.community.live_changed(&sender, true) {
                    Ok(Some(notice)) => state.to_every_live(&notice),
                    Ok(None) => {}
                    Err(error) => tracing::warn!(%error, "couldn't say who is online"),
                }
            }
            // One member's connection: anyone else's message on it is ignored.
            Some(held) if held != &sender => continue,
            Some(_) => {}
        }
        // (What `answer` turns back into DIDs itself; watching and voice, here.)
        let is_voice = received.message["type"].as_str().is_some_and(|t| t.starts_with(voice::PIURI));
        if is_voice || received.message["type"] == WATCH {
            match state.community.unmasked_from(&received.message) {
                Ok(unmasked) => received.message = unmasked,
                Err(error) => {
                    tracing::warn!(%error, "couldn't read who a message names");
                    continue;
                }
            }
        }
        let reply = if received.message["type"] == WATCH {
            watch(&state, id, &sender, &received.message).unwrap_or_else(|error| {
                tracing::warn!(%error, "couldn't answer watch");
                None
            })
        } else if is_voice {
            answer_voice(&state, &sender, &received.message, Some(id)).unwrap_or_else(|error| {
                tracing::warn!(%error, "couldn't answer a voice message");
                None
            })
        } else {
            service.answer(&Incoming { received: &received, sender_kid: Some(sender_kid) }).await.unwrap_or_else(|error| {
                tracing::warn!(%error, "couldn't answer");
                None
            })
        };
        let reply = reply.and_then(|reply| state.community.masked_for(&sender, &reply).inspect_err(|error| tracing::warn!(%error, "couldn't name people as they may be named to them")).ok());
        if let Some(reply) = reply {
            match state.agent.pack_reply(&received, &reply).await {
                Ok(packed) => {
                    if reply_tx.send(packed).await.is_err() {
                        break;
                    }
                }
                Err(error) => tracing::warn!(%error, "couldn't pack an answer"),
            }
        }
    }
    state.live.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
    state.watching.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
    if let Some(held) = &holder {
        match state.community.live_changed(held, false) {
            Ok(Some(notice)) => state.to_every_live(&notice),
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "couldn't say who is online"),
        }
    }
    let presence = state.rooms.close_live(&state.community, id);
    voice_send(&state, &VoiceOutcome { live: presence, ..VoiceOutcome::default() });
    packing.abort();
    if limited {
        // Let the close frame go out first.
        let _ = tokio::time::timeout(Duration::from_secs(2), &mut writing).await;
    }
    writing.abort();
}

/// The DID an authcrypted frame says it is from, read without resolving or opening
/// anything: its `skid`, else its `apu` (the sender kid, base64url), since DIDComm
/// v2.1 lets an authcrypt leave `skid` out and requires receivers to take the sender
/// from `apu` then (ECDH-1PU key wrapping; a COSE envelope from this library never
/// carries one). Read as the library reads it, JWE or COSE alike (WYV-120). `None`
/// for anything else.
fn frame_sender(bytes: &[u8]) -> Option<String> {
    let envelope = didcomm_core::envelope::EncryptedEnvelope::from_encoded(bytes).ok()?;
    let kid = match envelope.skid() {
        Some(skid) => skid,
        None => String::from_utf8(envelope.apu_bytes().ok()?).ok()?,
    };
    Some(kid.split('#').next().unwrap_or(&kid).to_string())
}

/// The forwarding service's question, by moq-auth's auth contract: may the session
/// holding this token connect, and publish and subscribe to what? Answered from who
/// is in the room now.
async fn voice_auth(State(state): State<Arc<AppState>>, Json(request): Json<Value>) -> Response {
    let event = request["event"].as_str().unwrap_or_default();
    if event == "end" {
        return StatusCode::NO_CONTENT.into_response();
    }
    let query = request["query"].as_str().unwrap_or_default();
    let token = query.split('&').find_map(|pair| pair.strip_prefix("jwt=")).unwrap_or_default();
    let did = state.voice.speaking_as().unwrap_or_else(|| state.agent.did());
    match state.rooms.grant(&state.community, &did, token, now()) {
        Some(grant) => Json(grant).into_response(),
        None => (StatusCode::FORBIDDEN, Json(json!({ "reason": "forbidden" }))).into_response(),
    }
}

/// How often [`polls_once`] is wanted.
pub const POLLS_EVERY: Duration = Duration::from_secs(30);
/// How often [`sweep_once`] is wanted.
pub const SWEEP_EVERY: Duration = Duration::from_secs(15);

/// Tells everyone joined of each poll that has ended since last asked, as it then
/// stands.
pub fn polls_once(state: &Arc<AppState>) {
    let ended = match state.community.store.polls_just_ended(now()) {
        Ok(ended) => ended,
        Err(error) => return tracing::warn!(%error, "couldn't look for polls that ended"),
    };
    for id in ended {
        let Ok(Some(post)) = state.community.store.post_by_id(&id) else { continue };
        if let Err(error) = send_changed(state, &state.community.changed_notice(&post)) {
            tracing::warn!(%error, "couldn't tell of a poll that ended");
        }
    }
}

/// Polls as they end, looked for every 30 seconds.
pub async fn run_polls(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(POLLS_EVERY).await;
        polls_once(&state);
    }
}

/// Takes out of their rooms those who have gone quiet.
/// Then counts the time those still in them spent there, for insights.
pub fn sweep_once(state: &Arc<AppState>) {
    let at = now();
    let presence = state.rooms.sweep(&state.community, at);
    voice_send(state, &VoiceOutcome { live: presence, ..VoiceOutcome::default() });
    let seconds = state.rooms.uncounted(at);
    if seconds > 0 {
        for (channel, dids) in state.rooms.in_rooms() {
            if let Err(e) = state.community.store.add_voice_time(&channel, &dids, at, seconds) {
                tracing::warn!("counting time in voice: {e:#}");
            }
        }
    }
}

/// Ends every voice room of a kind of channel just turned off (`kinds`: `voice`,
/// `stage`), telling whoever is in one and watches why (`ended: "kind-off"`).
/// For a host about to close the community and open it again with its new limits.
pub fn end_voice_of_kinds(state: &Arc<AppState>, kinds: &[String]) {
    let off = |channel: &crate::store::Channel| kinds.iter().any(|kind| kind == crate::community::channel_kind(channel));
    let presence = state.rooms.end_rooms(&state.community, off, "kind-off");
    voice_send(state, &VoiceOutcome { live: presence, ..VoiceOutcome::default() });
}

/// The voice rooms swept every 15 seconds.
pub async fn run_voice_sweep(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(SWEEP_EVERY).await;
        sweep_once(&state);
    }
}

/// Sends every member who has joined the posts they haven't had, in order. One
/// who can't be reached is left where they are, and tried again on the next pass.
/// Whoever just posted (`AppState::deliver_first`) is sent theirs first, before the
/// rest are started.
///
/// One who couldn't be reached through the mediator is passed over until the wait
/// `wyvrn_service::delivery` gives them is up (one whose mediator holds no mailbox
/// for them, a day at most), unless they are watching, or until they write.
pub async fn deliver_pending(state: &Arc<AppState>) -> anyhow::Result<()> {
    let store = &state.community.store;
    let latest = store.latest_serial()?;
    let first: Vec<String> = std::mem::take(&mut *state.deliver_first.lock().expect("deliver_first lock"));
    let undelivered = store.undelivered()?;
    let now = now();
    let owed = store.joined()?.into_iter().filter(|member| member.delivered < latest);
    let due = owed.filter(|member| !undelivered.get(&member.did).is_some_and(|held| held.held_back(now)) || state.is_watching(&member.did));
    let (posters, rest): (Vec<_>, Vec<_>) = due.partition(|member| first.contains(&member.did));
    for member in posters {
        let held = undelivered.contains_key(&member.did);
        deliver_to(state, member, held).await;
    }
    let limit = Arc::new(Semaphore::new(DELIVERY_CONCURRENCY));
    let mut sends = tokio::task::JoinSet::new();
    for member in rest {
        let state = state.clone();
        let limit = limit.clone();
        let held = undelivered.contains_key(&member.did);
        sends.spawn(async move {
            let _permit = limit.acquire().await;
            deliver_to(&state, member, held).await;
        });
    }
    while sends.join_next().await.is_some() {}
    Ok(())
}

/// Sends one member the posts they haven't had, in order, as far as they can be
/// reached. `held`: a delivery to them had failed (they are tried again).
async fn deliver_to(state: &Arc<AppState>, member: crate::store::Member, mut held: bool) {
    let posts = match state.community.store.posts_after(member.delivered, DELIVERY_BATCH) {
        Ok(posts) => posts,
        Err(error) => return tracing::error!(%error, "couldn't read posts to deliver"),
    };
    let seen = match state.community.seen_by(&member.did) {
        Ok(seen) => seen,
        Err(error) => return tracing::error!(%error, "couldn't tell which channels a member sees"),
    };
    for post in posts {
        // A post in a channel they may not see is passed over, for good: seeing it
        // later, they read back in it.
        let unread_thread = post.thread.is_some() && !matches!(state.community.reads_post_id(&member.did, &post), Ok(true));
        if !seen.contains(&post.channel) || unread_thread {
            if let Err(error) = state.community.store.set_delivered(&member.did, post.serial) {
                return tracing::error!(%error, "couldn't record a delivery");
            }
            continue;
        }
        // Watching: everything, there. Through the mediator, what concerns them
        // always, and the rest as they chose while nobody is watching; what isn't
        // sent they catch up on.
        let message = state.community.message(&post);
        let reaches = match state.community.reaches(&member, &post) {
            Ok(reaches) => reaches,
            Err(error) => return tracing::error!(%error, "couldn't tell whether a post concerns a member"),
        };
        // (Sent both ways, the app keeps one: a post it has is not news again.)
        let watched = state.to_watching(&member.did, &message);
        let through_mediator = match reaches {
            crate::posting::Reach::Always => true,
            crate::posting::Reach::UnlessWatching => !watched,
            crate::posting::Reach::Never => false,
        };
        if through_mediator {
            let message = match state.community.masked_for(&member.did, &message) {
                Ok(masked) => masked,
                Err(error) => return tracing::error!(%error, "couldn't name people as they may be named to them"),
            };
            match state.voice.send(&state.agent, &member.did, &message).await {
                Err(error) => match wyvrn_service::delivery::failure(&error) {
                    // This post never will be: the next may.
                    wyvrn_service::delivery::Failure::TooLarge => {
                        tracing::warn!(did = %member.did, serial = post.serial, %error, "a post is too large for a member's mediator: passed over");
                    }
                    failure => return delivery_failed(state, &member.did, post.serial, failure, &error),
                },
                Ok(()) if held => {
                    held = false;
                    if let Err(error) = state.community.store.delivery_resumed(&member.did) {
                        tracing::error!(%error, "couldn't record a delivery");
                    }
                    tracing::info!(did = %member.did, "a member who couldn't be reached can be again");
                }
                Ok(()) => {}
            }
        }
        if let Err(error) = state.community.store.set_delivered(&member.did, post.serial) {
            return tracing::error!(%error, "couldn't record a delivery");
        }
    }
}

/// Notes a failed delivery to `did`, to try them again later, and says so: once
/// for someone found gone, at each try for anyone else.
fn delivery_failed(state: &AppState, did: &str, serial: i64, failure: wyvrn_service::delivery::Failure, error: &anyhow::Error) {
    let gone = failure == wyvrn_service::delivery::Failure::Gone;
    let (next, was_gone) = match state.community.store.delivery_failed(did, gone, now()) {
        Ok(noted) => noted,
        Err(error) => return tracing::error!(%error, "couldn't record a failed delivery"),
    };
    let retry_in = next.next_at - now();
    if gone && !was_gone {
        tracing::warn!(%did, serial, %error, "nobody takes a member's mail: posts are held for them until they write or watch, or a try (at most daily) finds them");
    } else if gone {
        tracing::debug!(%did, serial, %error, tries = next.tries, retry_in, "a member is still gone");
    } else {
        tracing::warn!(%did, serial, %error, tries = next.tries, retry_in, "couldn't deliver a post; will try again");
    }
}

/// Whether a member who has joined hasn't been sent everything yet: someone who
/// couldn't be reached, to be tried again ([`DELIVERY_RETRY`]). Not someone whose
/// mediator holds no mailbox for them: they are tried when the community is open
/// anyway, or when they write, and a host needn't open it for them.
pub fn deliveries_owed(state: &AppState) -> anyhow::Result<bool> {
    let latest = state.community.store.latest_serial()?;
    let undelivered = state.community.store.undelivered()?;
    Ok(state.community.store.joined()?.iter().any(|member| member.delivered < latest && !undelivered.get(&member.did).is_some_and(|held| held.gone)))
}

/// Delivers whenever something is posted, and again a while after anyone
/// couldn't be reached.
pub async fn run_deliveries(state: Arc<AppState>) {
    loop {
        if let Err(error) = deliver_pending(&state).await {
            tracing::error!(%error, "delivering posts failed");
        }
        let _ = tokio::time::timeout(DELIVERY_RETRY, state.deliver.notified()).await;
    }
}

/// Forgets answers, and administration workflows, that were started and left.
pub fn prune_once(state: &AppState) {
    if let Err(error) = state.community.store.prune_answering(now() - ANSWERING_KEPT_FOR) {
        tracing::warn!(%error, "pruning failed");
    }
    // Administration workflows left open, as long as answers are kept.
    if let Err(error) = state.community.store.prune_flows(now() - ANSWERING_KEPT_FOR) {
        tracing::warn!(%error, "pruning failed");
    }
    // The web view's sessions, codes and tokens that have run out.
    if let Err(error) = state.community.store.prune_web(now()) {
        tracing::warn!(%error, "pruning failed");
    }
    // What deleted posts said, kept for moderators for `deleted_text_days` (0: none,
    // and what was kept before goes too).
    if let Err(error) = state.community.store.forget_deleted_texts(now() - state.community.deleted_text_kept_for()) {
        tracing::warn!(%error, "forgetting deleted posts' text failed");
    }
    // Channels deleted 30 days ago, for good.
    match state.community.store.purge_trashed(now() - crate::structure::TRASH_KEPT_FOR) {
        Ok(gone) if !gone.is_empty() => tracing::info!(channels = gone.len(), "deleted channels from the trash for good"),
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "emptying the trash failed"),
    }
}

/// How often the people banned are looked at again for accounts they linked since.
pub const BANS_LOOKED_AT_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// A ban follows the linked accounts its person shows after it, too: every few
/// hours, the public address each banned person's profile named is read again, and
/// any account it now proves (checked as a profile's are) is banned with them, and
/// whoever here holds it (protection.rs).
pub async fn run_ban_watch(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(BANS_LOOKED_AT_EVERY).await;
        look_at_bans_again(&state).await;
    }
}

pub async fn look_at_bans_again(state: &Arc<AppState>) {
    let community = &state.community;
    let issuers = community.config.linked_account_issuers.clone();
    if issuers.is_empty() {
        return;
    }
    let bans = match community.store.bans(now()) {
        Ok(bans) => bans,
        Err(error) => return tracing::warn!(%error, "couldn't read the bans to look at again"),
    };
    for ban in bans {
        let Ok(Some(address)) = community.store.member_profile(&ban.did).map(|p| p.and_then(|p| p.public_address)) else { continue };
        let Some(linked) = crate::address_banner::linked_of_address(&address).await else { continue };
        let now_text = chrono::DateTime::from_timestamp(now(), 0).map(|t| t.to_rfc3339()).unwrap_or_default();
        let accounts = crate::claims::verified_accounts(&linked, &address, &issuers, &now_text, |url| state.issuer_json(url)).await;
        match community.banned_now_hold(&ban.did, &accounts, now()) {
            Ok(Some(outcome)) => {
                carry_out(state, &ban.did, outcome);
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "a ban's new linked accounts couldn't be kept"),
        }
    }
}

/// Pruned every hour.
pub async fn run_pruning(state: Arc<AppState>) {
    loop {
        prune_once(&state);
        tokio::time::sleep(PRUNE_EVERY).await;
    }
}

/// DIDComm messages one address may send a minute (WYV-62): a flood of joins from
/// fresh DIDs, or of anything, comes from somewhere. Behind the trusted proxies,
/// the address `X-Forwarded-For` gives; otherwise the connection's. Posted or sent
/// on a live connection alike (WYV-97).
const MESSAGES_PER_ADDRESS_A_MINUTE: u32 = 300;

/// Counts one message from `address` (a bucket, `clients.rs`): whether it may be taken.
fn message_allowed(address: std::net::IpAddr) -> bool {
    static SEEN: std::sync::LazyLock<std::sync::Mutex<HashMap<std::net::IpAddr, (i64, u32)>>> = std::sync::LazyLock::new(Default::default);
    let minute = now() / 60;
    let mut seen = SEEN.lock().unwrap_or_else(|p| p.into_inner());
    if seen.len() > 50_000 {
        seen.retain(|_, (at, _)| *at == minute);
    }
    let entry = seen.entry(address).or_insert((minute, 0));
    if entry.0 != minute {
        *entry = (minute, 0);
    }
    entry.1 += 1;
    entry.1 <= MESSAGES_PER_ADDRESS_A_MINUTE
}

async fn take_message(State(state): State<Arc<AppState>>, request: axum::extract::Request) -> Response {
    // Every client but a trusted proxy's own (a test, a tool beside it), an IPv6
    // one by its /64 (WYV-74, WYV-98).
    if let Some(address) = state.clients.of(&request) {
        if !message_allowed(address) {
            return (StatusCode::TOO_MANY_REQUESTS, "Too many messages from this address: wait a minute.").into_response();
        }
    }
    let body = match axum::body::to_bytes(request.into_body(), 2 * 1024 * 1024).await {
        Ok(body) => body,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    receive(Arc::new(CommunityService(state)), body).await.into_response()
}

pub fn router(state: Arc<AppState>) -> Router {
    let didcomm = Router::new()
        .route("/", get(root).post(take_message))
        .route("/voice/auth", post(voice_auth))
        // Files members attach (files.rs): a chunk is at most what the host takes in one.
        .route("/u/{id}", axum::routing::put(crate::files::put_chunk).head(crate::files::upload_standing).layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)))
        .route("/f/{id}", get(crate::files::fetch).head(crate::files::fetch))
        // The community's own packs (expressions.rs).
        .route("/s/{name}", get(crate::expressions::manifest))
        .route("/s/{name}/pack.json", get(crate::expressions::manifest))
        .route("/s/{name}/{file}", get(crate::expressions::pack_file))
        .route("/s/drafts/{instance}/{file}", get(crate::expressions::draft_file))
        // Members' pictures, at the addresses members are given (member_ids.rs).
        .route("/m/{member}/{kind}/{hash}", get(member_picture))
        .route("/picture/{hash}", get(own_picture))
        .route("/invite/{code}", get(short_invite))
        .route("/healthz", get(|| async { "ok" }))
        // A member's calendar feed (events.rs), at the address only they were given.
        .route("/calendar/{file}", get(crate::events::feed))
        // Webhooks, as Discord's are reached (webhooks.rs).
        .route(
            "/api/webhooks/{id}/{token}",
            post(crate::webhooks::execute).get(crate::webhooks::describe).layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        // Slack's and GitHub's bodies, as Discord takes them (webhook_formats.rs).
        .route("/api/webhooks/{id}/{token}/slack", post(crate::webhooks::execute_slack))
        .route("/api/webhooks/{id}/{token}/github", post(crate::webhooks::execute_github))
        .route(
            "/api/webhooks/{id}/{token}/messages/{message}",
            get(crate::webhooks::read_message)
                .patch(crate::webhooks::edit_message)
                .delete(crate::webhooks::delete_message)
                .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .merge(address::routes(state.agent.did(), state.did_document.clone()))
        // A browser app sends its messages, and fetches files, from another origin.
        .layer(CorsLayer::permissive().expose_headers([
            axum::http::header::ETAG,
            axum::http::header::CONTENT_DISPOSITION,
            axum::http::HeaderName::from_static("upload-offset"),
            axum::http::HeaderName::from_static("upload-length"),
        ]))
        .with_state(state.clone());
    // The web view, without the CORS above; and around everything, the headers
    // its pages get (which take that CORS off the community's own page at `/`).
    let routes = match &state.web {
        Some(web) => didcomm.merge(crate::web::router(state.clone(), web)),
        None => didcomm,
    };
    routes.layer(axum::middleware::from_fn_with_state(state, crate::web::headers))
}

/// A member's picture, banner or picture here, by the address `members` gave:
/// a capability, the same for as long as the picture is, and so kept for good.
/// One of the community's short invite links (DIDComm's "Short URL Message
/// Retrieval", the link a path rather than `?_oobid=`): a browser (asking for
/// HTML) is sent to the page that shows the community and how to take the invite
/// up (`invite_landing`); anything else -- an app, an agent -- is given the
/// out-of-band invitation itself, made now, so one link stays good as the
/// community's DID or picture changes. Nothing for a code that wouldn't let anyone
/// in now (used up, run out, revoked).
async fn short_invite(State(state): State<Arc<AppState>>, axum::extract::Path(code): axum::extract::Path<String>, headers: axum::http::HeaderMap) -> Response {
    use axum::http::{header, StatusCode};
    let community = &state.community;
    let valid = code.len() <= 128 && community.store.invite(&code).ok().flatten().is_some_and(|invite| invite.valid(now()));
    if !valid {
        return (StatusCode::NOT_FOUND, [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], "This invite isn't good any more.").into_response();
    }
    let did = state.voice.speaking_as().unwrap_or_else(|| state.agent.did());
    let wants_page = headers.get(header::ACCEPT).and_then(|accept| accept.to_str().ok()).is_some_and(|accept| accept.contains("text/html"));
    if wants_page {
        let link = format!("{}/invite/{code}", community.public_url.trim_end_matches('/'));
        if let Some(landing) = invite_landing(&community.config.invite_landing, &did, &link) {
            return (StatusCode::FOUND, [(header::LOCATION, landing)]).into_response();
        }
    }
    let invitation = community.invitation(&did, &code);
    (
        [(header::CONTENT_TYPE, "application/json"), (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"), (header::CACHE_CONTROL, "no-store")],
        invitation.to_string(),
    )
        .into_response()
}

/// Where a browser opening the short invite `link` is sent: `landing` (the
/// app's page for an address, `https://chat.wyvrn.app/dl/`) at the community's
/// `did`, the link passed on. Only for a DID the page can look up (did:web,
/// did:webvh); `None` otherwise.
pub(crate) fn invite_landing(landing: &str, did: &str, link: &str) -> Option<String> {
    if !(did.starts_with("did:web:") || did.starts_with("did:webvh:")) {
        return None;
    }
    let mut url = reqwest::Url::parse(&format!("{landing}{did}")).ok()?;
    url.query_pairs_mut().append_pair("invite", link);
    Some(url.to_string())
}

/// The community's own picture, as small as Discord takes it, at the address
/// `own_picture_url` gave while it is still that picture: public, as its DID
/// document's whois already makes it.
async fn own_picture(State(state): State<Arc<AppState>>, axum::extract::Path(hash): axum::extract::Path<String>) -> Response {
    use base64::Engine;
    match state.community.changed().picture {
        Some(picture) if crate::member_ids::picture_hash(&picture.base64) == hash => match state.community.own_picture_small() {
            Some((media_type, bytes)) => picture_bytes(media_type, &base64::engine::general_purpose::STANDARD.encode(bytes)),
            None => axum::http::StatusCode::NOT_FOUND.into_response(),
        },
        _ => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

async fn member_picture(State(state): State<Arc<AppState>>, axum::extract::Path((member, kind, hash)): axum::extract::Path<(String, String, String)>) -> Response {
    let Ok(Some((media_type, data))) = state.community.picture_at(&member, &kind, &hash) else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    picture_bytes(media_type, &data)
}

/// A picture kept as base64, served for good under an address that names it.
fn picture_bytes(media_type: String, data: &str) -> Response {
    use base64::Engine;
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data.trim_end_matches('=')).or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(data.trim_end_matches('='))) else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let media_type = if matches!(media_type.as_str(), "image/png" | "image/jpeg" | "image/webp" | "image/gif") { media_type } else { "application/octet-stream".to_string() };
    (
        [
            (axum::http::header::CONTENT_TYPE, media_type),
            (axum::http::header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string()),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        bytes,
    )
        .into_response()
}

/// A live connection, if the request asks for a WebSocket; otherwise the
/// community's page, where it has one on, or what this is.
async fn root(
    State(state): State<Arc<AppState>>,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: axum::http::HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    match upgrade {
        Ok(upgrade) => {
            // The bucket it is counted in, by the trusted proxies (WYV-98).
            let client = peer.and_then(|peer| state.clients.counted(peer.0 .0.ip(), &headers));
            upgrade.max_message_size(4 * 1024 * 1024).on_upgrade(move |socket| live_connection(state, socket, client)).into_response()
        }
        Err(_) if state.web.is_some() && crate::web::page_on(&state.community.store) => crate::web::pages::home(State(state), headers).await,
        Err(_) => index(State(state)).await.into_response(),
    }
}

/// Says what this is and how to reach it. Nothing of who is in it or what is said.
async fn index(State(state): State<Arc<AppState>>) -> String {
    format!(
        "This is a Wyvrn community server.\nIts DID: {}\nIt is by invitation, and spoken to over DIDComm: {}\n",
        state.agent.did(),
        community::PIURI
    )
}

#[cfg(test)]
mod invite_landing_tests {
    #[test]
    fn a_browser_is_sent_to_the_communitys_page_with_the_link() {
        assert_eq!(
            super::invite_landing("https://chat.wyvrn.app/dl/", "did:web:c.wyvrn.app:c:frosty", "https://c.wyvrn.app/c/frosty/invite/k3J9xQ2mAbCdEf").as_deref(),
            Some("https://chat.wyvrn.app/dl/did:web:c.wyvrn.app:c:frosty?invite=https%3A%2F%2Fc.wyvrn.app%2Fc%2Ffrosty%2Finvite%2Fk3J9xQ2mAbCdEf")
        );
        assert!(super::invite_landing("https://chat.wyvrn.app/dl/", "did:peer:4zQm", "https://x/invite/a").is_none());
    }
}

#[cfg(test)]
mod frame_sender_tests {
    use base64::Engine;
    use didcomm_agent::{Agent, Identity};
    use serde_json::{json, Value};

    /// A JWE authcrypt is read by its `skid`, and with that left out (as v2.1 allows)
    /// by its `apu`; anything else names nobody (WYV-120).
    #[tokio::test]
    async fn a_frame_is_from_its_skid_else_its_apu() {
        let (alice, bob) = (Agent::new(Identity::generate().unwrap()).unwrap(), Agent::new(Identity::generate().unwrap()).unwrap());
        let message = json!({ "type": "https://didcomm.org/trust-ping/2.0/ping", "body": {} });
        let packed = alice.messaging().pack_as(&message, &bob.did(), Some(&alice.did()), didcomm_core::crypto::Encoding::Json).await.unwrap().message;
        assert_eq!(super::frame_sender(&packed).as_deref(), Some(alice.did().as_str()));

        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let mut jwe: Value = serde_json::from_slice(&packed).unwrap();
        let mut protected: Value = serde_json::from_slice(&b64.decode(jwe["protected"].as_str().unwrap()).unwrap()).unwrap();
        protected.as_object_mut().unwrap().remove("skid").expect("packed with a skid");
        jwe["protected"] = b64.encode(serde_json::to_vec(&protected).unwrap()).into();
        let apu_only = serde_json::to_vec(&jwe).unwrap();
        assert_eq!(super::frame_sender(&apu_only).as_deref(), Some(alice.did().as_str()));

        protected.as_object_mut().unwrap().remove("apu");
        jwe["protected"] = b64.encode(serde_json::to_vec(&protected).unwrap()).into();
        assert_eq!(super::frame_sender(&serde_json::to_vec(&jwe).unwrap()), None);
        let anoncrypt = alice.messaging().pack(&message, &bob.did(), None).await.unwrap().message;
        assert_eq!(super::frame_sender(&anoncrypt), None);
        assert_eq!(super::frame_sender(b"{\"junk\":1}"), None);
    }
}
