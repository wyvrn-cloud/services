//! The HTTP side: the endpoint workflow messages come to, and serving packs to
//! anyone who has their address.

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
use didcomm_agent::{Agent, Features, Identity, Received};
use didcomm_core::resolver::DIDResolver;
use didcomm_crypto_askar::AskarCryptoService;
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
use wyvrn_service::{
    address,
    http::{internal, receive},
    named, now, Incoming, Service,
};

use crate::config::Config;
use crate::processor::{Processor, PIURI};
use crate::store::{draft_scope, pack_scope};

/// What a pack is and how a sticker is sent: the protocol this service's packs follow.
pub const STICKERS_PIURI: &str = "https://wyvrn.app/stickers/1.0";

/// How often workflows nobody came back to are forgotten.
const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);

pub struct AppState {
    pub agent: Agent,
    /// Which DID it speaks as: its address's, once it holds one (wyvrn_service::voice).
    pub voice: wyvrn_service::Voice,
    pub processor: Processor,
    /// Published at `/.well-known/did.json` when the service itself is a `did:web`.
    pub did_document: Option<address::Published>,
    /// Resolves the DID a `from_prior` names, to check who signed it.
    pub resolver: Box<dyn DIDResolver>,
    /// The admin pages open now (crate::admin).
    pub admin: wyvrn_service::admin::Admin,
}

impl AppState {
    /// Whether `sender` (writing with `kid`) is one of `STICKERS_ADMINS`: by DID,
    /// or by the key of a public address's holder.
    pub async fn is_admin(&self, sender: &str, kid: Option<&str>) -> bool {
        let admins = &self.processor.config.admins;
        admins.iter().any(|did| did == sender) || named::holder_among(self.resolver.as_ref(), admins, sender, kid).await.is_some()
    }
}

/// The agent for `config`: the service's own DID, which makers write to.
pub fn build_agent(config: &Config, identity: Identity) -> anyhow::Result<(Agent, Option<address::Published>)> {
    let (agent, document) = address::build_agent(identity, &config.server.public_url, config.server.did_method)?;
    // Who it is, for whoever looks its DID up before writing to it.
    let profile = config.profile.or("STICKERS", "Stickers", "Hosts sticker and custom-emoji packs.");
    let document = document.map(|document| document.with_profile(&profile)).transpose()?;
    let features = Features::standard().with_protocol(PIURI, &["processor"]).with_protocol(STICKERS_PIURI, &["service"]);
    Ok((agent.with_features(features), document))
}

/// A maker's DID change, if `received` announces one that checks out: signed by a
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

/// The service as the shared plumbing sees it: its agent, and what it answers.
pub struct StickerService(pub Arc<AppState>);

impl Service for StickerService {
    fn agent(&self) -> &Agent {
        &self.0.agent
    }

    /// A pack is its maker's, and a maker is whoever a message can be shown to be from.
    fn requires_authcrypt(&self) -> bool {
        true
    }

    async fn answer(&self, incoming: &Incoming<'_>) -> anyhow::Result<Option<Value>> {
        let received = incoming.received;
        let Some(sender) = received.sender.as_deref() else {
            return Ok(None);
        };
        // A maker whose devices changed is the same maker under a new DID: their
        // packs, and what they were in the middle of, go with them.
        if let Some((prior, new)) = rotation(&self.0, received, sender).await {
            if self.0.processor.store.rotate(&prior, &new)? {
                tracing::info!(%prior, %new, "a maker changed DID");
            }
        }
        if let Some(reply) = self.0.agent.auto_reply(received) {
            return Ok(Some(reply));
        }
        // Admin pages are workflows too: discover lists them beside the service's
        // own (to an admin), and a message about one goes to them.
        if received.message_type().starts_with(PIURI) {
            let admin = &self.0.admin;
            let is_admin = self.0.is_admin(sender, incoming.sender_kid.as_deref()).await;
            if received.message_type().ends_with("/discover") {
                if let Some(Ok((kind, mut body))) = self.0.processor.handle(sender, &received.message, now()) {
                    let listed = admin.listed(self.0.as_ref(), is_admin, &received.message["body"]);
                    if let Some(workflows) = body["workflows"].as_array_mut() {
                        workflows.extend(listed);
                        body["paging"]["total"] = json!(workflows.len());
                    }
                    return Ok(Some(received.reply(&kind, body)));
                }
            } else if admin.concerns(&received.message) {
                return Ok(admin.handle(self.0.as_ref(), is_admin, sender, &received.message, now()).map(|answer| match answer {
                    Ok((kind, body)) => received.reply(&kind, body),
                    Err(refusal) => received.reply(&format!("{PIURI}/problem-report"), refusal.body()),
                }));
            }
        }
        Ok(match self.0.processor.handle(sender, &received.message, now()) {
            Some(Ok((message_type, body))) => Some(received.reply(&message_type, body)),
            Some(Err(refusal)) => {
                tracing::debug!(code = refusal.code, comment = %refusal.comment, "refused");
                Some(received.reply(&format!("{PIURI}/problem-report"), json!({ "code": refusal.code, "comment": refusal.comment, "args": refusal.args })))
            }
            None => None,
        })
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index).post(|State(state): State<Arc<AppState>>, body: Bytes| receive(Arc::new(StickerService(state)), body)))
        .route("/healthz", get(|| async { "ok" }))
        .merge(address::routes(state.agent.did(), state.did_document.clone()))
        .route("/drafts/{instance}/{file}", get(draft_file))
        .route("/{name}", get(manifest))
        .route("/{name}/pack.json", get(manifest))
        .route("/{name}/{file}", get(pack_file))
        // A browser app fetches packs, and sends its messages, from another origin.
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn index(State(state): State<Arc<AppState>>) -> String {
    format!(
        "This is a Wyvrn sticker service.\nIts DID: {}\nA pack is at /<name>/pack.json, and its pictures beside it.\nPacks are made by talking to it: {PIURI}\n",
        state.agent.did()
    )
}

fn picture(bytes: Vec<u8>, media_type: &str, cache: &'static str) -> Response {
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    if let Ok(media_type) = HeaderValue::from_str(media_type) {
        headers.insert(header::CONTENT_TYPE, media_type);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    // A picture is shown as a picture, whatever a browser might make of its bytes.
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

/// A pack, as anyone may fetch it. One that was taken down is gone, and says so.
async fn manifest(State(state): State<Arc<AppState>>, Path(name): Path<String>, request_headers: HeaderMap) -> Response {
    let pack = match state.processor.store.pack(&name) {
        Ok(Some(pack)) => pack,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal(error),
    };
    if pack.removed {
        return StatusCode::GONE.into_response();
    }
    let body = match state.processor.manifest(&pack) {
        Ok(manifest) => manifest.to_string(),
        Err(error) => return internal(error),
    };
    // A pack is asked for again often and seldom changes: its version is the tag.
    let tag = format!("\"{}-{}\"", pack.version, body.len());
    if request_headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(tag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if let Ok(tag) = HeaderValue::from_str(&tag) {
        headers.insert(header::ETAG, tag);
    }
    response
}

/// One picture of a pack. A file is named for what is in it, so it never changes
/// and can be kept for good.
async fn pack_file(State(state): State<Arc<AppState>>, Path((name, file)): Path<(String, String)>) -> Response {
    match state.processor.store.pack(&name) {
        Ok(Some(pack)) if pack.removed => return StatusCode::GONE.into_response(),
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal(error),
    }
    match state.processor.store.file(&pack_scope(&name), &file) {
        Ok(Some((bytes, media_type))) => picture(bytes, &media_type, "public, max-age=31536000, immutable"),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal(error),
    }
}

/// A picture of a pack that is being made, for its maker's app to show. Whoever
/// has the instance's id has its address; nothing else leads to it.
async fn draft_file(State(state): State<Arc<AppState>>, Path((instance, file)): Path<(String, String)>) -> Response {
    match state.processor.store.file(&draft_scope(&instance), &file) {
        Ok(Some((bytes, media_type))) => picture(bytes, &media_type, "private, no-store"),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal(error),
    }
}

/// Forgets workflows that were started and left, with what they had gathered.
pub async fn run_pruning(state: Arc<AppState>) {
    loop {
        let before = now() - state.processor.config.draft_hours * 60 * 60;
        match state.processor.store.prune(before) {
            Ok(0) => {}
            Ok(gone) => tracing::info!(gone, "forgot workflows nobody came back to"),
            Err(error) => tracing::warn!(%error, "pruning failed"),
        }
        tokio::time::sleep(PRUNE_EVERY).await;
    }
}
