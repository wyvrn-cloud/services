//! The HTTP side: the endpoint `files/1.0` messages come to, where chunks are
//! put, and where stored files are fetched from.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, put},
    Router,
};
use didcomm_agent::{Agent, Features, Identity, Received};
use didcomm_core::resolver::DIDResolver;
use didcomm_crypto_askar::AskarCryptoService;
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeFile;
use wyvrn_service::{
    address,
    http::{internal, receive},
    named, now, Incoming, Service,
};

use crate::config::Config;
use crate::host::{Fetch, Host, Put, Refusal, PIURI};

/// How often what is past its time is removed.
const PRUNE_EVERY: Duration = Duration::from_secs(10 * 60);

pub struct AppState {
    pub agent: Agent,
    /// Which DID it speaks as: its address's, once it holds one (wyvrn_service::voice).
    pub voice: wyvrn_service::Voice,
    pub host: Host,
    /// Published at `/.well-known/did.json` when the service itself is a `did:web`.
    pub did_document: Option<address::Published>,
    /// Resolves the DIDs the settings name, and the one a `from_prior` names.
    pub resolver: Box<dyn DIDResolver>,
    /// One chunk at a time per upload: two at once would both see the same
    /// offset and both append.
    uploads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl AppState {
    pub fn new(agent: Agent, host: Host, did_document: Option<address::Published>) -> Self {
        Self { agent, voice: Default::default(), host, did_document, resolver: didcomm_quickstart::default_resolver(), uploads: Default::default() }
    }

    fn upload_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut uploads = self.uploads.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        uploads.retain(|_, lock| Arc::strong_count(lock) > 1);
        uploads.entry(id.to_string()).or_default().clone()
    }
}

/// The agent for `config`: the service's own DID, which uploaders write to.
pub fn build_agent(config: &Config, identity: Identity) -> anyhow::Result<(Agent, Option<address::Published>)> {
    let (agent, document) = address::build_agent(identity, &config.server.public_url, config.server.did_method)?;
    let profile = config.profile.or("FILES", "Files", "Keeps files too big for a message, for the agents it allows, and serves them to anyone with the link.");
    let document = document.map(|document| document.with_profile(&profile)).transpose()?;
    let features = Features::standard().with_protocol(PIURI, &["host"]);
    Ok((agent.with_features(features), document))
}

/// An uploader's DID change, if `received` announces one that checks out.
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

/// Whether `sender` (writing with `kid`) is one of the uploaders the settings
/// name: by DID, or by the key of a public address's holder.
async fn is_uploader(state: &AppState, sender: &str, kid: Option<&str>) -> bool {
    let named = &state.host.config.uploaders;
    named.iter().any(|did| did == sender) || named::holder_among(state.resolver.as_ref(), named, sender, kid).await.is_some()
}

/// The service as the shared plumbing sees it: its agent, and what it answers.
pub struct FilesService(pub Arc<AppState>);

impl Service for FilesService {
    fn agent(&self) -> &Agent {
        &self.0.agent
    }

    /// A file is its uploader's, and an uploader is whoever a message can be shown to be from.
    fn requires_authcrypt(&self) -> bool {
        true
    }

    async fn answer(&self, incoming: &Incoming<'_>) -> anyhow::Result<Option<Value>> {
        let received = incoming.received;
        let Some(sender) = received.sender.as_deref() else {
            return Ok(None);
        };
        if let Some((prior, new)) = rotation(&self.0, received, sender).await {
            if self.0.host.store.rotate(&prior, &new)? {
                tracing::info!(%prior, %new, "an uploader changed DID");
            }
        }
        if let Some(reply) = self.0.agent.auto_reply(received) {
            return Ok(Some(reply));
        }
        if !received.message_type().starts_with(PIURI) {
            return Ok(None);
        }
        let answer = if is_uploader(&self.0, sender, incoming.sender_kid.as_deref()).await {
            self.0.host.handle(sender, &received.message, now())
        } else {
            Some(Err(Refusal::not_allowed()))
        };
        Ok(match answer {
            Some(Ok((kind, body))) => Some(received.reply(&format!("{PIURI}/{kind}"), body)),
            Some(Err(refusal)) => {
                tracing::debug!(code = refusal.code, comment = %refusal.comment, "refused");
                Some(received.reply(&format!("{PIURI}/problem-report"), json!({ "code": refusal.code, "comment": refusal.comment, "args": refusal.args })))
            }
            None => None,
        })
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let chunk_bytes = state.host.config.chunk_bytes as usize;
    Router::new()
        .route("/", get(index).post(|State(state): State<Arc<AppState>>, body: Bytes| receive(Arc::new(FilesService(state)), body)))
        .route("/healthz", get(|| async { "ok" }))
        .merge(address::routes(state.agent.did(), state.did_document.clone()))
        // A chunk is at most chunk_bytes; anything longer is refused before it is read.
        .route("/u/{id}", put(put_chunk).head(upload_standing).layer(DefaultBodyLimit::max(chunk_bytes)))
        .route("/f/{id}", get(fetch).head(fetch))
        // A browser app fetches files, and sends its messages, from another origin.
        .layer(CorsLayer::permissive().expose_headers([header::ETAG, header::CONTENT_DISPOSITION, HeaderName::from_static("upload-offset"), HeaderName::from_static("upload-length")]))
        .with_state(state)
}

async fn index(State(state): State<Arc<AppState>>) -> String {
    format!(
        "This is a Wyvrn files service.\nIts DID: {}\nFiles are put here by talking to it ({PIURI}), and fetched from /f/<id>.\n",
        state.agent.did()
    )
}

fn with_offset(status: StatusCode, offset: u64) -> Response {
    let mut response = status.into_response();
    if let Ok(value) = HeaderValue::from_str(&offset.to_string()) {
        response.headers_mut().insert("upload-offset", value);
    }
    response
}

fn fetch_response(error: Fetch) -> Response {
    match error {
        Fetch::NotFound => StatusCode::NOT_FOUND.into_response(),
        Fetch::Gone => StatusCode::GONE.into_response(),
        Fetch::Failed(error) => internal(error),
    }
}

/// `HEAD /u/<id>`: where an upload stands.
async fn upload_standing(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match state.host.upload_in_progress(&id, now()) {
        Ok(file) => {
            let mut response = with_offset(StatusCode::OK, state.host.store.received(&id));
            if let Ok(value) = HeaderValue::from_str(&file.byte_count.to_string()) {
                response.headers_mut().insert("upload-length", value);
            }
            response
        }
        Err(error) => fetch_response(error),
    }
}

/// `PUT /u/<id>` with `Upload-Offset`: one chunk, appended if it is the next.
async fn put_chunk(State(state): State<Arc<AppState>>, Path(id): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(offset) = headers.get("upload-offset").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok()) else {
        return (StatusCode::BAD_REQUEST, "Upload-Offset is missing or not a number").into_response();
    };
    let lock = state.upload_lock(&id);
    let _held = lock.lock().await;
    // The append is a write of up to chunk_bytes: off the async threads.
    let outcome = tokio::task::spawn_blocking(move || state.host.put_chunk(&id, offset, &body, now())).await;
    match outcome {
        Ok(Ok(held)) => with_offset(StatusCode::NO_CONTENT, held),
        Ok(Err(Put::WrongOffset(held))) => with_offset(StatusCode::CONFLICT, held),
        Ok(Err(Put::TooLong)) => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Ok(Err(Put::Fetch(error))) => fetch_response(error),
        Err(error) => internal(error),
    }
}

/// `GET`/`HEAD /f/<id>`: a stored file, whole or by `Range`.
async fn fetch(State(state): State<Arc<AppState>>, Path(id): Path<String>, request: Request) -> Response {
    let file = match state.host.stored(&id, now()) {
        Ok(file) => file,
        Err(error) => return fetch_response(error),
    };
    // What a fetcher holds, if it holds the file: no need to send it twice.
    let tag = format!("\"{}\"", file.hash);
    if request.headers().get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(tag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut response = match ServeFile::new(state.host.store.path(&id)).try_call(request).await {
        Ok(response) => response.into_response(),
        Err(error) => return internal(error),
    };
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&file.media_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", file.name.replace(['"', '\\'], "_"))) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    if let Ok(value) = HeaderValue::from_str(&tag) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=3600"));
    // Bytes are bytes, whatever a browser might make of them.
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

/// Removes what is past its time, now and then.
pub async fn run_pruning(state: Arc<AppState>) {
    loop {
        match state.host.store.prune(now()) {
            Ok(0) => {}
            Ok(gone) => tracing::info!(gone, "removed files past their time"),
            Err(error) => tracing::warn!(%error, "pruning failed"),
        }
        tokio::time::sleep(PRUNE_EVERY).await;
    }
}
