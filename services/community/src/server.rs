//! The HTTP side: the endpoint members' messages come to, and the sending of
//! what is posted to everyone who has joined.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Bytes,
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
    /// The AI assistant answering in its channels, where it has one
    /// (wyvrn-claude; assistant.rs).
    pub assistant: Option<Arc<crate::assistant::Assistant>>,
    /// Pages its host adds for the community's owner alone (wyvrn-community-host's
    /// moving to a server of their own), as wyvrn_service::admin pages; and those open.
    pub owner_pages: Option<Arc<dyn wyvrn_service::admin::Pages>>,
    pub owner_admin: wyvrn_service::admin::Admin,
    /// The community on the web, where the server shows it (web/).
    pub web: Option<Arc<crate::web::Web>>,
}

impl AppState {
    /// The parts a new server starts with, beside what `main` builds.
    pub fn new(agent: Agent, voice: wyvrn_service::Voice, mut community: Community, did_document: Option<address::Published>, resolver: Box<dyn DIDResolver>) -> Self {
        community.own = Some(agent.did());
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
            assistant: None,
            owner_pages: None,
            owner_admin: Default::default(),
            web: None,
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
        .with_protocol(community::MESSAGE_PINS, &["participant"]);
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
        Ok((prior, new)) if new == sender && prior != new => Some((prior, new)),
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(%sender, %error, "ignoring a from_prior that doesn't check out");
            None
        }
    }
}

/// Sends a post as it now stands to everyone joined: over a watching connection,
/// or through the mediator to whoever chose to be sent changes. Anyone else asks
/// for what changed when they next watch.
fn send_changed(state: &Arc<AppState>, notice: &Value) -> anyhow::Result<()> {
    for member in state.community.store.joined()? {
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
fn send_changed_later(state: &Arc<AppState>, id: String) {
    let state = state.clone();
    tokio::spawn(async move {
        let sent = state.community.store.post_by_id(&id).map_err(anyhow::Error::from).and_then(|post| match post {
            Some(post) => send_changed(&state, &state.community.changed_notice(&post)),
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
        // A ping's answer, to one the web view sent someone signing in.
        if received.message["type"] == crate::web::sign_in::PING_RESPONSE {
            if let (Some(web), Some(thid)) = (&state.web, received.message["thid"].as_str()) {
                if web.sign_in.answered(thid) {
                    return Ok(None);
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
        if !store.is_superseded(&sender)? {
            let role = store.member(&sender)?.map(|member| member.role);
            if role != Some(crate::store::Role::Admin) {
                if let Some(named) = wyvrn_service::named::holder_among(state.resolver.as_ref(), &config.admins, &sender, kid).await {
                    store.set_member(&sender, crate::store::Role::Admin, now())?;
                    tracing::info!(%sender, %named, "an admin named by their public address wrote, and is one under the DID they write from");
                } else if role.is_none() {
                    if let Some(named) = wyvrn_service::named::holder_among(state.resolver.as_ref(), &config.members, &sender, kid).await {
                        store.admit(&sender, crate::store::Role::Member, now())?;
                        tracing::info!(%sender, %named, "a member named by their public address wrote, and is let in under the DID they write from");
                    }
                }
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
                let mut handled = state.community.handle_workflow(&member, &received.message, now());
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
                }
            }
            &checked
        } else {
            &received.message
        };
        let outcome = state.community.handle(&sender, message, now())?;
        if message["type"] != community::PROFILE {
            ask_for_profile(state, &sender);
        }
        for (did, notice) in outcome.notices {
            send_later(state, did, notice);
        }
        for message in &outcome.live {
            state.to_every_live(message);
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
        Ok(outcome.reply)
    }
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
    let day = format!("assistant:day:{}:{}", at / 86_400, asked.author_did);
    let period = format!("assistant:period:{}", at / crate::assistant::PERIOD_SECS);
    let used: u32 = store.setting(&day)?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let spent: u64 = store.setting(&period)?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let text = match assistant.refusal(used, spent) {
        Some(why) => why.to_string(),
        None => {
            // The room's, or the thread's with the post it grew from first.
            let mut posts = store.channel_posts_before(&asked.channel, asked.thread.as_deref(), Some(asked.seq + 1), assistant.config.context_posts)?;
            posts.reverse();
            if let Some(root) = asked.thread.as_deref().map(|thread| store.post_by_id(thread)).transpose()?.flatten() {
                posts.insert(0, root);
            }
            let turns = crate::assistant::turns_of(&posts, &own);
            store.set_setting(&day, &(used + 1).to_string())?;
            match assistant.ask(&channel.name, &turns).await {
                Ok((text, tokens)) => {
                    store.set_setting(&period, &(spent + tokens).to_string())?;
                    text
                }
                Err(err) => {
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

/// A member's live connection (community-voice/1.0's Connectivity): DIDComm over a
/// WebSocket. What they send is answered on it, and presence and keys are pushed
/// on it. It is one member's: the first authenticated message says whose.
async fn live_connection(state: Arc<AppState>, socket: WebSocket) {
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
    let writing = tokio::spawn(async move {
        loop {
            let packed = tokio::select! {
                Some(p) = packed_rx.recv() => p,
                Some(p) = reply_rx.recv() => p,
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
    while let Some(Ok(frame)) = stream.next().await {
        let bytes: Vec<u8> = match frame {
            Message::Text(text) => text.as_bytes().to_vec(),
            Message::Binary(bytes) => bytes.to_vec(),
            Message::Close(_) => break,
            _ => continue,
        };
        if bytes.len() > 4 * 1024 * 1024 {
            continue;
        }
        let Ok(mut received) = state.agent.receive(&bytes).await else { continue };
        let Ok(unpacked) = state.agent.messaging().unpack_verified(&bytes).await else { continue };
        let (Some(sender), Some(sender_kid)) = (received.sender.clone(), unpacked.sender_kid) else { continue };
        match &holder {
            None => {
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
    writing.abort();
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
pub fn sweep_once(state: &Arc<AppState>) {
    let presence = state.rooms.sweep(&state.community, now());
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
pub async fn deliver_pending(state: &Arc<AppState>) -> anyhow::Result<()> {
    let store = &state.community.store;
    let latest = store.latest_serial()?;
    let first: Vec<String> = std::mem::take(&mut *state.deliver_first.lock().expect("deliver_first lock"));
    let (posters, rest): (Vec<_>, Vec<_>) = store.joined()?.into_iter().filter(|member| member.delivered < latest).partition(|member| first.contains(&member.did));
    for member in posters {
        deliver_to(state, member).await;
    }
    let limit = Arc::new(Semaphore::new(DELIVERY_CONCURRENCY));
    let mut sends = tokio::task::JoinSet::new();
    for member in rest {
        let state = state.clone();
        let limit = limit.clone();
        sends.spawn(async move {
            let _permit = limit.acquire().await;
            deliver_to(&state, member).await;
        });
    }
    while sends.join_next().await.is_some() {}
    Ok(())
}

/// Sends one member the posts they haven't had, in order, as far as they can be
/// reached.
async fn deliver_to(state: &Arc<AppState>, member: crate::store::Member) {
    let posts = match state.community.store.posts_after(member.delivered, DELIVERY_BATCH) {
        Ok(posts) => posts,
        Err(error) => return tracing::error!(%error, "couldn't read posts to deliver"),
    };
    for post in posts {
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
            if let Err(error) = state.voice.send(&state.agent, &member.did, &message).await {
                tracing::warn!(did = %member.did, serial = post.serial, %error, "couldn't deliver a post; will try again");
                return;
            }
        }
        if let Err(error) = state.community.store.set_delivered(&member.did, post.serial) {
            return tracing::error!(%error, "couldn't record a delivery");
        }
    }
}

/// Whether a member who has joined hasn't been sent everything yet: someone who
/// couldn't be reached, to be tried again ([`DELIVERY_RETRY`]).
pub fn deliveries_owed(state: &AppState) -> anyhow::Result<bool> {
    let latest = state.community.store.latest_serial()?;
    Ok(state.community.store.joined()?.iter().any(|member| member.delivered < latest))
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
    // Channels deleted 30 days ago, for good.
    match state.community.store.purge_trashed(now() - crate::structure::TRASH_KEPT_FOR) {
        Ok(gone) if !gone.is_empty() => tracing::info!(channels = gone.len(), "deleted channels from the trash for good"),
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "emptying the trash failed"),
    }
}

/// Pruned every hour.
pub async fn run_pruning(state: Arc<AppState>) {
    loop {
        prune_once(&state);
        tokio::time::sleep(PRUNE_EVERY).await;
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let didcomm = Router::new()
        .route("/", get(root).post(|State(state): State<Arc<AppState>>, body: Bytes| receive(Arc::new(CommunityService(state)), body)))
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
        .route("/healthz", get(|| async { "ok" }))
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
async fn member_picture(State(state): State<Arc<AppState>>, axum::extract::Path((member, kind, hash)): axum::extract::Path<(String, String, String)>) -> Response {
    use base64::Engine;
    let Ok(Some((media_type, data))) = state.community.picture_at(&member, &kind, &hash) else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
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
async fn root(State(state): State<Arc<AppState>>, headers: axum::http::HeaderMap, upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>) -> Response {
    match upgrade {
        Ok(upgrade) => upgrade.max_message_size(4 * 1024 * 1024).on_upgrade(move |socket| live_connection(state, socket)).into_response(),
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
