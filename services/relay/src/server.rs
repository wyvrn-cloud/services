//! The relay's endpoint: a WebSocket each client holds open (`GET /` upgraded),
//! and `POST /` for whoever asks something over plain HTTP. Every frame is one
//! packed message, authcrypted by the client; answers go back on the socket.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{
        ws::{rejection::WebSocketUpgradeRejection, Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use didcomm_agent::{features::DISCOVER_FEATURES_QUERIES, Agent, Features, Identity, Received};
use futures_util::{stream::SplitSink, SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use wyvrn_service::{address, http::receive, now, Incoming, Service};

use crate::config::Config;
use crate::relay::{attached, Refusal, Relay, CLOSE, FRAME, OPEN, OPENED, PIURI, PROBLEM_REPORT, RELEASE, RELEASED, REQUEST, SEND, SENT, GRANT};

/// How many connections one client socket may have open through the relay.
const MAX_TUNNELS: usize = 4;

pub struct AppState {
    pub agent: Agent,
    pub relay: Relay,
    pub did_document: Option<address::Published>,
}

/// The agent for `config`. A relay that is a `did:web` lists its WebSocket
/// address in its document, before the HTTP one.
pub fn build_agent(config: &Config, identity: Identity) -> anyhow::Result<(Agent, Option<address::Published>)> {
    let (agent, document) = address::build_agent(identity, &config.server.public_url, config.server.did_method)?;
    let profile = config.profile.or("RELAY", "Relay", "Sends what its clients send, so that where it goes sees the relay and not them.");
    let document = document
        .map(|mut published| {
            let did = published.did().to_string();
            if let Some(services) = published.document["service"].as_array_mut() {
                services.insert(0, json!({ "id": format!("{did}#ws"), "type": "DIDCommMessaging", "serviceEndpoint": { "uri": socket_url(&config.server.public_url), "accept": ["didcomm/v2"], "routingKeys": [] } }));
            }
            published.with_profile(&profile)
        })
        .transpose()?;
    let features = Features::standard().with_protocol(PIURI, &["relay"]);
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

pub struct RelayService(pub Arc<AppState>);

fn refused(received: &Received, refusal: &Refusal) -> Value {
    tracing::debug!(code = refusal.code, comment = %refusal.comment, "refused");
    let mut report = received.reply(PROBLEM_REPORT, refusal.body());
    report["pthid"] = received.message["id"].clone();
    report
}

impl Service for RelayService {
    fn agent(&self) -> &Agent {
        &self.0.agent
    }

    /// A grant is a DID's, and only a message its sender authenticated says whose.
    fn requires_authcrypt(&self) -> bool {
        true
    }

    async fn answer(&self, incoming: &Incoming<'_>) -> anyhow::Result<Option<Value>> {
        let received = incoming.received;
        let Some(sender) = received.sender.as_deref() else { return Ok(None) };
        let state = &self.0;
        if received.message_type() == DISCOVER_FEATURES_QUERIES {
            let Some(mut reply) = state.agent.auto_reply(received) else { return Ok(None) };
            if let Some(disclosures) = reply["body"]["disclosures"].as_array_mut() {
                disclosures.extend(state.relay.disclosures(&received.message["body"]));
            }
            return Ok(Some(reply));
        }
        if let Some(reply) = state.agent.auto_reply(received) {
            return Ok(Some(reply));
        }
        let answer = match received.message_type() {
            REQUEST => state.relay.request(sender, now()).map(|body| received.reply(GRANT, body)),
            RELEASE => state.relay.release(sender).map(|body| received.reply(RELEASED, body)),
            SEND => state.relay.send(sender, &received.message).await.map(|posted| {
                let mut sent = received.reply(SENT, posted.body);
                if let Some(answer) = posted.answer {
                    sent["attachments"] = json!([{ "id": "answer", "data": { "base64": STANDARD.encode(answer) } }]);
                }
                sent
            }),
            OPEN | FRAME | CLOSE => Err(Refusal::not_carried("a WebSocket over plain HTTP: connect one")),
            _ => return Ok(None),
        };
        Ok(Some(answer.unwrap_or_else(|refusal| refused(received, &refusal))))
    }
}

/// A connection a client opened through the relay.
struct Tunnel {
    upstream: Arc<Mutex<SplitSink<reqwest_websocket::WebSocket, reqwest_websocket::Message>>>,
    reading: tokio::task::JoinHandle<()>,
}

/// The DID a message was addressed to (its key id without the fragment): what
/// the relay answers as.
fn addressed(received: &Received) -> String {
    received.recipient_kid.split('#').next().unwrap_or_default().to_string()
}

/// One client's socket, until it closes. Every connection opened through it
/// closes with it.
async fn connection(state: Arc<AppState>, socket: WebSocket) {
    tracing::debug!("a client connected");
    let (mut sink, mut stream) = socket.split();
    let (out, mut outgoing) = mpsc::channel::<Vec<u8>>(256);
    let writing = tokio::spawn(async move {
        while let Some(packed) = outgoing.recv().await {
            let frame = match String::from_utf8(packed) {
                Ok(text) => Message::Text(text.into()),
                Err(bytes) => Message::Binary(bytes.into_bytes().into()),
            };
            if sink.send(frame).await.is_err() {
                break;
            }
        }
    });
    let service = RelayService(state.clone());
    let mut tunnels: HashMap<String, Tunnel> = HashMap::new();
    // A frame may carry a message of max_bytes in base64, and the envelope around it.
    let largest = state.relay.config.max_bytes / 3 * 4 + 64 * 1024;
    while let Some(Ok(frame)) = stream.next().await {
        let bytes: Vec<u8> = match frame {
            Message::Text(text) => text.as_bytes().to_vec(),
            Message::Binary(bytes) => bytes.to_vec(),
            Message::Close(_) => break,
            _ => continue,
        };
        if bytes.len() > largest {
            continue;
        }
        let received = match state.agent.receive(&bytes).await {
            Ok(received) => received,
            Err(error) => {
                tracing::debug!(%error, "a frame that didn't open");
                continue;
            }
        };
        let Ok(unpacked) = state.agent.messaging().unpack_verified(&bytes).await else { continue };
        let (Some(sender), Some(sender_kid)) = (received.sender.clone(), unpacked.sender_kid) else {
            tracing::debug!("a frame nobody authenticated");
            continue;
        };
        tracing::debug!(%sender, message_type = received.message_type(), "heard");
        let reply = match received.message_type() {
            OPEN => Some(open(&state, &received, &sender, &mut tunnels, out.clone()).await.unwrap_or_else(|refusal| refused(&received, &refusal))),
            FRAME => frame_out(&state, &received, &sender, &mut tunnels).await,
            CLOSE => {
                let thid = received.message["thid"].as_str().unwrap_or_default().to_string();
                if let Some(tunnel) = tunnels.remove(&thid) {
                    tunnel.reading.abort();
                    let _ = tunnel.upstream.lock().await.close().await;
                }
                None
            }
            _ => service.answer(&Incoming { received: &received, sender_kid: Some(sender_kid) }).await.unwrap_or_else(|error| {
                tracing::warn!(%error, "couldn't answer");
                None
            }),
        };
        if let Some(reply) = reply {
            tracing::debug!(message_type = reply["type"].as_str().unwrap_or_default(), "answering");
            match state.agent.pack_reply(&received, &reply).await {
                Ok(packed) => {
                    if out.send(packed).await.is_err() {
                        break;
                    }
                }
                Err(error) => tracing::warn!(%error, "couldn't pack an answer"),
            }
        }
        tunnels.retain(|_, tunnel| !tunnel.reading.is_finished());
    }
    for (_, tunnel) in tunnels.drain() {
        tunnel.reading.abort();
        let _ = tunnel.upstream.lock().await.close().await;
    }
    drop(out);
    let _ = writing.await;
}

/// Opens what `open` asks for, and carries what comes back on it to the client
/// as `frame`s, ending with a `close`.
async fn open(state: &Arc<AppState>, received: &Received, sender: &str, tunnels: &mut HashMap<String, Tunnel>, out: mpsc::Sender<Vec<u8>>) -> Result<Value, Refusal> {
    state.relay.granted(sender)?;
    tunnels.retain(|_, tunnel| !tunnel.reading.is_finished());
    if tunnels.len() >= MAX_TUNNELS {
        return Err(Refusal::unreachable(format!("at most {MAX_TUNNELS} connections at once")));
    }
    let id = received.message["id"].as_str().ok_or_else(|| Refusal::malformed("open needs an id"))?.to_string();
    let socket = state.relay.open(sender, &received.message).await?;
    let (upstream, mut downstream) = socket.split();
    let (to, from) = (sender.to_string(), addressed(received));
    let (reader_state, thid, largest) = (state.clone(), id.clone(), state.relay.config.max_bytes);
    let reading = tokio::spawn(async move {
        let tell = |message: Value| {
            let (state, to, from, out) = (reader_state.clone(), to.clone(), from.clone(), out.clone());
            async move {
                match state.agent.messaging().pack_direct(&message, &to, Some(&from)).await {
                    Ok(packed) => out.send(packed.message).await.is_ok(),
                    Err(_) => false,
                }
            }
        };
        while let Some(Ok(message)) = downstream.next().await {
            let (text, bytes) = match message {
                reqwest_websocket::Message::Text(text) => (true, text.into_bytes()),
                reqwest_websocket::Message::Binary(bytes) => (false, bytes.to_vec()),
                reqwest_websocket::Message::Close { .. } => break,
                _ => continue,
            };
            if bytes.len() > largest {
                continue;
            }
            let frame = json!({
                "id": uuid::Uuid::new_v4().to_string(), "type": FRAME, "thid": thid, "body": { "text": text },
                "attachments": [{ "id": "frame", "data": { "base64": STANDARD.encode(bytes) } }],
            });
            if !tell(frame).await {
                return;
            }
        }
        tell(json!({ "id": uuid::Uuid::new_v4().to_string(), "type": CLOSE, "thid": thid, "body": {} })).await;
    });
    tunnels.insert(id, Tunnel { upstream: Arc::new(Mutex::new(upstream)), reading });
    Ok(received.reply(OPENED, json!({})))
}

/// A frame the client sends on a connection it opened. Answered only with a
/// `close`, when that connection has gone.
async fn frame_out(state: &AppState, received: &Received, sender: &str, tunnels: &mut HashMap<String, Tunnel>) -> Option<Value> {
    let thid = received.message["thid"].as_str().unwrap_or_default().to_string();
    let gone = || Some(received.reply(CLOSE, json!({})));
    let Some(tunnel) = tunnels.get(&thid) else { return gone() };
    if state.relay.granted(sender).is_err() {
        return gone();
    }
    let bytes = attached(&received.message)?;
    if bytes.len() > state.relay.config.max_bytes {
        return Some(refused(received, &Refusal::too_large(state.relay.config.max_bytes)));
    }
    let message = if received.message["body"]["text"] == true {
        reqwest_websocket::Message::Text(String::from_utf8(bytes).ok()?)
    } else {
        reqwest_websocket::Message::Binary(bytes.into())
    };
    if tunnel.upstream.lock().await.send(message).await.is_err() {
        if let Some(tunnel) = tunnels.remove(&thid) {
            tunnel.reading.abort();
        }
        return gone();
    }
    None
}

async fn root(State(state): State<Arc<AppState>>, upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>) -> Response {
    match upgrade {
        Ok(upgrade) => {
            let limit = state.relay.config.max_bytes / 3 * 4 + 64 * 1024;
            upgrade.max_message_size(limit).max_frame_size(limit).on_upgrade(move |socket| connection(state, socket)).into_response()
        }
        Err(_) => format!(
            "This is a Wyvrn outbound relay ({PIURI}).\nIts DID: {}\nIt carries: {}.\nConnect a WebSocket here.\n",
            state.agent.did(),
            state.relay.config.carries.as_str()
        )
        .into_response(),
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root).post(|State(state): State<Arc<AppState>>, body: Bytes| receive(Arc::new(RelayService(state)), body)))
        .route("/healthz", get(|| async { "ok" }))
        .merge(address::routes(state.agent.did(), state.did_document.clone()))
        .with_state(state)
}
