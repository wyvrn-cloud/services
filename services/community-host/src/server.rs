//! The HTTP side: the host's own DID (its admins' pages), each community under
//! `/c/<name>/`, and the voice relay's one question, answered by the community a
//! token is of.

use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use didcomm_agent::{Agent, Features, Identity};
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
use wyvrn_service::{address, http::receive, now, Incoming, Service};

use crate::config::Config;
use crate::tenants::Host;

pub struct AppState {
    pub agent: Agent,
    pub host: Arc<Host>,
    /// Published at `/.well-known/did.json`: the host is the `did:web` at its URL.
    pub did_document: Option<address::Published>,
    pub resolver: Box<dyn didcomm_core::resolver::DIDResolver>,
    /// The admin pages open now (crate::admin).
    pub admin: wyvrn_service::admin::Admin,
}

/// The agent for `config`: the host's own DID, which its admins write to.
pub fn build_agent(config: &Config, identity: Identity) -> anyhow::Result<(Agent, Option<address::Published>)> {
    let (agent, document) = address::build_agent(identity, &config.server.public_url, config.server.did_method)?;
    let profile = wyvrn_service::address::ProfileConfig::default().or("COMMUNITY_HOST", "Communities", "Keeps communities, each its own server.");
    let document = document.map(|document| document.with_profile(&profile)).transpose()?;
    Ok((agent.with_features(Features::standard().with_protocol(wyvrn_service::admin::PIURI, &["processor"])), document))
}

impl AppState {
    /// Whether `sender` (with the key `kid`) is one of the host's admins: named by
    /// their DID, or by a public address they hold.
    pub async fn is_admin(&self, sender: &str, kid: Option<&str>) -> bool {
        let admins = &self.host.config.host.admins;
        admins.iter().any(|admin| admin == sender) || wyvrn_service::named::holder_among(self.resolver.as_ref(), admins, sender, kid).await.is_some()
    }
}

/// The host as the shared plumbing sees it: its agent, and what it answers.
pub struct HostService(pub Arc<AppState>);

impl Service for HostService {
    fn agent(&self) -> &Agent {
        &self.0.agent
    }

    fn requires_authcrypt(&self) -> bool {
        true
    }

    async fn answer(&self, incoming: &Incoming<'_>) -> anyhow::Result<Option<Value>> {
        let received = incoming.received;
        if let Some(reply) = self.0.agent.auto_reply(received) {
            return Ok(Some(reply));
        }
        let Some(sender) = received.sender.as_deref() else { return Ok(None) };
        if !received.message_type().starts_with(wyvrn_service::admin::PIURI) {
            return Ok(None);
        }
        let is_admin = self.0.is_admin(sender, incoming.sender_kid.as_deref()).await;
        Ok(self.0.admin.handle(self.0.as_ref(), is_admin, sender, &received.message, now()).map(|answer| match answer {
            Ok((kind, body)) => received.reply(&kind, body),
            Err(refusal) => received.reply(&format!("{}/problem-report", wyvrn_service::admin::PIURI), refusal.body()),
        }))
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    // The host's own: its DID and its admins' messages. Each community answers
    // for itself under /c/<name>/, CORS and all.
    let own = Router::new()
        .route("/", get(index).post(|State(state): State<Arc<AppState>>, body: Bytes| receive(Arc::new(HostService(state)), body)))
        .route("/healthz", get(|| async { "ok" }))
        .merge(address::routes(state.agent.did(), state.did_document.clone()))
        .layer(CorsLayer::permissive());
    Router::new()
        .merge(own)
        .route("/voice/auth", post(voice_auth))
        // An owner's export, once (export.rs).
        .route("/export/{token}", get(crate::export::download))
        .route("/c/{name}", any(community))
        .route("/c/{name}/", any(community))
        .route("/c/{name}/{*rest}", any(community))
        .with_state(state)
}

async fn index(State(state): State<Arc<AppState>>) -> String {
    format!("This keeps Wyvrn communities, each at /c/<name>/ and its own DID.\nThe host's DID: {}\n", state.agent.did())
}

/// A request for one community: handed to it with `/c/<name>` taken off its path.
async fn community(State(state): State<Arc<AppState>>, mut request: Request) -> Response {
    let path = request.uri().path().to_string();
    let rest = path.strip_prefix("/c/").unwrap_or_default();
    let (name, inner) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    // A browser at `/c/<name>` would take the page's links from `/c/`: to the
    // address with its `/`, which is the community's own.
    if !rest.contains('/') && request.method() == axum::http::Method::GET && !request.headers().contains_key(axum::http::header::UPGRADE) {
        let to = match request.uri().query() {
            Some(query) => format!("{name}/?{query}"),
            None => format!("{name}/"),
        };
        return (StatusCode::PERMANENT_REDIRECT, [(axum::http::header::LOCATION, to)]).into_response();
    }
    let target = match request.uri().query() {
        Some(query) => format!("{inner}?{query}"),
        None => inner.to_string(),
    };
    let Ok(uri) = target.parse::<Uri>() else { return StatusCode::BAD_REQUEST.into_response() };
    *request.uri_mut() = uri;
    state.host.forward(name, request).await
}

/// moq-relay asks one address whether a token may connect. A token starts with
/// the name of the community it is of (`<name>.`), whose own answer it is given.
/// A community that isn't open has nobody in its rooms, and so no token to grant.
async fn voice_auth(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or_default();
    if request["event"] == "end" {
        return StatusCode::NO_CONTENT.into_response();
    }
    let query = request["query"].as_str().unwrap_or_default();
    let token = query.split('&').find_map(|pair| pair.strip_prefix("jwt=")).unwrap_or_default();
    let forbidden = || (StatusCode::FORBIDDEN, Json(json!({ "reason": "forbidden" }))).into_response();
    let Some((name, _)) = token.split_once('.') else { return forbidden() };
    let Some(tenant) = state.host.loaded(name) else { return forbidden() };
    let asked = Request::post("/voice/auth").header(header::CONTENT_TYPE, "application/json").body(Body::from(body));
    match asked {
        Ok(asked) => tenant.serve(asked).await,
        Err(_) => forbidden(),
    }
}
