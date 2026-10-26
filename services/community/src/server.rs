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
use std::collections::HashMap;
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
use crate::questions;
use crate::voice::{self, Rooms, VoiceOutcome};

/// How many members are sent to at once.
const DELIVERY_CONCURRENCY: usize = 8;
/// How long before someone who couldn't be reached is tried again.
const DELIVERY_RETRY: Duration = Duration::from_secs(60);
/// How many posts one member is sent in one pass.
const DELIVERY_BATCH: usize = 100;
/// How often answers nobody finished are forgotten, and how old they have to be.
const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);
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
    /// Voice channels' rooms (community-voice/1.0).
    pub rooms: Rooms,
    /// Members' live connections, by number: where to put what is sent them there.
    pub live: Mutex<HashMap<u64, (String, mpsc::Sender<Value>)>>,
    pub next_live: AtomicU64,
    /// The AI assistant answering in its channels, where it has one
    /// (wyvrn-claude; assistant.rs).
    pub assistant: Option<Arc<crate::assistant::Assistant>>,
}

impl AppState {
    /// The parts a new server starts with, beside what `main` builds.
    pub fn new(agent: Agent, voice: wyvrn_service::Voice, community: Community, did_document: Option<address::Published>, resolver: Box<dyn DIDResolver>) -> Self {
        Self {
            agent,
            voice,
            community,
            did_document,
            resolver,
            deliver: Notify::new(),
            rooms: Rooms::default(),
            live: Mutex::new(HashMap::new()),
            next_live: AtomicU64::new(1),
            assistant: None,
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
    let (agent, document) = address::build_agent(identity, &config.server.public_url, config.server.did_method)?;
    // Who it is, for whoever looks its DID up before writing to it.
    let profile = config.profile.or("COMMUNITY", &config.community.name, &config.community.description);
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
    let document = match document {
        Some(mut published) if config.voice.is_some() => {
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

/// Sends `message` to `did` without holding up whoever caused it.
fn send_later(state: &Arc<AppState>, did: String, message: Value) {
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = state.voice.send(&state.agent, &did, &message).await {
            tracing::warn!(%did, %error, "couldn't send a notice");
        }
    });
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

    async fn answer(&self, incoming: &Incoming<'_>) -> anyhow::Result<Option<Value>> {
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

        // A question's answer, from someone let in. (Anyone else falls through, and
        // is told they aren't.)
        if received.message["type"].as_str().is_some_and(|message_type| message_type.starts_with(questions::PIURI)) {
            if let Some(member) = state.community.store.member(&sender)? {
                let handled = state.community.handle_workflow(&member, &received.message, now());
                if handled.posted {
                    state.deliver.notify_one();
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

        // Voice channels: from a member, or refused as everything is from anyone else.
        if received.message["type"].as_str().is_some_and(|message_type| message_type.starts_with(voice::PIURI)) {
            return answer_voice(state, &sender, &received.message, None);
        }
        let outcome = state.community.handle(&sender, &received.message, now())?;
        for (did, notice) in outcome.notices {
            send_later(state, did, notice);
        }
        // Who it says it is to someone looking its DID up changes with it.
        if outcome.profile_changed {
            if let Some(document) = &state.did_document {
                document.changed(&state.community.changed());
            }
        }
        if outcome.channels_changed {
            // Everyone else who has joined: whoever asked has it as their answer.
            for (did, notice) in state.community.community_notices()? {
                if did != sender {
                    send_later(state, did, notice);
                }
            }
        }
        // A post that changed is sent, as it now stands, to everyone who has joined:
        // whoever asked included, for their other devices. Not kept for anyone who
        // can't be reached now: they ask for what changed when they are back.
        for id in &outcome.changed {
            if let Some(post) = state.community.store.post_by_id(id)? {
                let notice = state.community.changed_notice(&post);
                for member in state.community.store.joined()? {
                    send_later(state, member.did, notice.clone());
                }
            }
        }
        if outcome.posted {
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
            let notice = state.community.changed_notice(&root);
            for member in store.joined()? {
                send_later(state, member.did, notice.clone());
            }
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
        let Ok(received) = state.agent.receive(&bytes).await else { continue };
        let Ok(unpacked) = state.agent.messaging().unpack_verified(&bytes).await else { continue };
        let (Some(sender), Some(sender_kid)) = (received.sender.clone(), unpacked.sender_kid) else { continue };
        match &holder {
            None => {
                holder = Some(sender.clone());
                *holder_cell.lock().unwrap_or_else(|p| p.into_inner()) = Some(sender.clone());
                state.live.lock().unwrap_or_else(|p| p.into_inner()).insert(id, (sender.clone(), out.clone()));
                state.rooms.open_live(id, &sender);
            }
            // One member's connection: anyone else's message on it is ignored.
            Some(held) if held != &sender => continue,
            Some(_) => {}
        }
        let is_voice = received.message["type"].as_str().is_some_and(|t| t.starts_with(voice::PIURI));
        let reply = if is_voice {
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

/// Takes out of their rooms those who have gone quiet, every 15 seconds.
pub async fn run_voice_sweep(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(Duration::from_secs(15)).await;
        let presence = state.rooms.sweep(&state.community, now());
        voice_send(&state, &VoiceOutcome { live: presence, ..VoiceOutcome::default() });
    }
}

/// Sends every member who has joined the posts they haven't had, in order. One
/// who can't be reached is left where they are, and tried again on the next pass.
pub async fn deliver_pending(state: &Arc<AppState>) -> anyhow::Result<()> {
    let store = &state.community.store;
    let latest = store.latest_serial()?;
    let limit = Arc::new(Semaphore::new(DELIVERY_CONCURRENCY));
    let mut sends = tokio::task::JoinSet::new();
    for member in store.joined()?.into_iter().filter(|member| member.delivered < latest) {
        let state = state.clone();
        let limit = limit.clone();
        sends.spawn(async move {
            let _permit = limit.acquire().await;
            let posts = match state.community.store.posts_after(member.delivered, DELIVERY_BATCH) {
                Ok(posts) => posts,
                Err(error) => return tracing::error!(%error, "couldn't read posts to deliver"),
            };
            for post in posts {
                if let Err(error) = state.voice.send(&state.agent, &member.did, &state.community.message(&post)).await {
                    tracing::warn!(did = %member.did, serial = post.serial, %error, "couldn't deliver a post; will try again");
                    return;
                }
                if let Err(error) = state.community.store.set_delivered(&member.did, post.serial) {
                    return tracing::error!(%error, "couldn't record a delivery");
                }
            }
        });
    }
    while sends.join_next().await.is_some() {}
    Ok(())
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

/// Forgets answers that were started and left.
pub async fn run_pruning(state: Arc<AppState>) {
    loop {
        if let Err(error) = state.community.store.prune_answering(now() - ANSWERING_KEPT_FOR) {
            tracing::warn!(%error, "pruning failed");
        }
        tokio::time::sleep(PRUNE_EVERY).await;
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root).post(|State(state): State<Arc<AppState>>, body: Bytes| receive(Arc::new(CommunityService(state)), body)))
        .route("/voice/auth", post(voice_auth))
        .route("/healthz", get(|| async { "ok" }))
        .merge(address::routes(state.agent.did(), state.did_document.clone()))
        // A browser app sends its messages from another origin.
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// A live connection, if the request asks for a WebSocket; otherwise what this is.
async fn root(State(state): State<Arc<AppState>>, upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>) -> Response {
    match upgrade {
        Ok(upgrade) => upgrade.max_message_size(4 * 1024 * 1024).on_upgrade(move |socket| live_connection(state, socket)).into_response(),
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
