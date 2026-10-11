//! The HTTP side: the endpoint `did-hosting` messages come to, and serving what
//! is hosted to anyone who resolves it.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use didcomm_agent::{Agent, Features, Identity};
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
use wyvrn_service::{
    address,
    http::{internal, receive},
    now, Incoming, Service,
};

use crate::config::Config;
use crate::host::{Host, PIURI};
use crate::names::Name;
use crate::store::Published;

/// How often the host looks for holders it hasn't heard from.
const STALENESS_CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const DAY: i64 = 24 * 60 * 60;

pub struct AppState {
    pub agent: Agent,
    pub host: Host,
    /// Published at `/.well-known/did.json` when the host itself is a `did:web`.
    pub did_document: Option<Value>,
}

/// The agent for `config`: the host's own DID, which holders write to.
pub fn build_agent(config: &Config, identity: Identity) -> anyhow::Result<(Agent, Option<Value>)> {
    let (agent, document) = address::build_agent(identity, &config.server.public_url, config.server.did_method)?;
    Ok((agent.with_features(Features::standard().with_protocol(PIURI, &["host"])), document))
}

/// The host as the shared plumbing sees it: its agent, and what it answers.
pub struct HostService(pub Arc<AppState>);

impl Service for HostService {
    fn agent(&self) -> &Agent {
        &self.0.agent
    }

    /// Everything here is decided by who is writing, down to the key.
    fn requires_authcrypt(&self) -> bool {
        true
    }

    async fn answer(&self, incoming: &Incoming<'_>) -> anyhow::Result<Option<Value>> {
        let received = incoming.received;
        // Nothing is done for someone nobody vouches for. (The endpoint has turned
        // such a message away already; this is for any other way in.)
        let Some(kid) = incoming.sender_kid.as_deref() else {
            return Ok(None);
        };
        // A ping's answer, and anything else, is a holder saying it is still there:
        // noted before the automatic replies, which are otherwise all a ping gets.
        self.0.host.note_sender(received, kid, now()).await?;
        if let Some(reply) = self.0.agent.auto_reply(received) {
            return Ok(Some(reply));
        }
        self.0.host.handle(received, kid, now()).await
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index).post(|State(state): State<Arc<AppState>>, body: Bytes| receive(Arc::new(HostService(state)), body)))
        .route("/healthz", get(|| async { "ok" }))
        .merge(address::routes(state.agent.did(), state.did_document.clone()))
        // Everything else is a hosted DID's file: <name>/did.jsonl and its neighbours.
        .route("/{*path}", get(hosted))
        // A browser app resolves DIDs, and sends its messages, from another origin.
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn index(State(state): State<Arc<AppState>>) -> String {
    format!(
        "This is a did-hosting/1.0 host for {}.\nIts DID: {}\nA hosted DID's log is at /<name>/did.jsonl.\n",
        state.host.config.domain,
        state.agent.did()
    )
}

/// One of a hosted DID's files. A name nothing is published under has none, and
/// neither says whether it is free, held or reserved.
async fn hosted(State(state): State<Arc<AppState>>, Path(path): Path<String>, request_headers: HeaderMap) -> Response {
    let Some((name, file)) = path.rsplit_once('/') else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(name) = Name::parse(&name.replace('/', ":"), state.host.config.max_labels) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let record = match state.host.store.get(&name.as_did_path()) {
        Ok(Some(record)) if record.published != Published::No => record,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal(error),
    };
    let (body, content_type): (Vec<u8>, &str) = match file {
        "did.jsonl" => match record.log {
            // The log's last line ends with a newline, as a file of lines does.
            Some(log) => (format!("{log}\n").into_bytes(), "text/jsonl; charset=utf-8"),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
        "did.json" => match record.web_document {
            Some(document) => (document.into_bytes(), "application/did+json"),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
        "whois.vp" => match record.whois {
            Some(whois) => (whois, "application/vp"),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    // A resolver asks again often and the answer seldom changes: the version is
    // the tag, so an unchanged file costs it nothing but the question.
    let tag = format!("\"{}-{}\"", record.version_id.unwrap_or_default(), body.len());
    if request_headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(tag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if let Ok(tag) = HeaderValue::from_str(&tag) {
        headers.insert(header::ETAG, tag);
    }
    response
}

/// Asks holders the host hasn't heard from in a long while whether they are still
/// there, and marks the names of those that don't answer `stale` -- which changes
/// nothing for anyone resolving them, and lets an admin free them.
pub async fn run_staleness_checks(state: Arc<AppState>) {
    loop {
        if let Err(error) = check_staleness(&state, now()).await {
            tracing::warn!(%error, "checking for stale names failed");
        }
        tokio::time::sleep(STALENESS_CHECK_EVERY).await;
    }
}

pub async fn check_staleness(state: &Arc<AppState>, now: i64) -> anyhow::Result<()> {
    let config = &state.host.config;
    for record in state.host.store.all()? {
        if record.published != Published::Yes {
            continue;
        }
        match record.asked_at {
            Some(asked) if now - asked > config.answer_within_days * DAY => {
                tracing::info!(name = %record.name, "no answer from its holder: stale");
                state.host.store.mark_stale(&record.name)?;
            }
            Some(_) => {}
            None if now - record.heard_at > config.ask_after_days * DAY => {
                // Asked at the did:peer:4 its document names, where it names one: that
                // carries its own document, so nothing has to be fetched to reach it
                // -- least of all this host's own files, from itself.
                let document = record.log.as_deref().and_then(|log| log.lines().last()).and_then(|line| serde_json::from_str::<Value>(line).ok());
                let peer = document.as_ref().and_then(|entry| {
                    wyvrn_hosted_did::verify::also_known_as(&entry["state"]).into_iter().find(|did| did.starts_with("did:peer:")).map(str::to_string)
                });
                let Some(did) = peer.as_deref().or(record.did.as_deref()) else { continue };
                // Any authenticated message back counts as an answer (Host::handle).
                let ping = json!({
                    "type": "https://didcomm.org/trust-ping/2.0/ping",
                    "body": { "response_requested": true },
                });
                if let Err(error) = state.agent.send(did, &ping).await {
                    tracing::debug!(name = %record.name, %error, "couldn't reach a holder to ask");
                }
                state.host.store.asked(&record.name, now)?;
            }
            None => {}
        }
    }
    Ok(())
}
