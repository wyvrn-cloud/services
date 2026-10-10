//! The HTTP side: DIDComm messages on `POST /`, the web pages on `GET`, and sending
//! posts out to subscribers.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use base64::Engine;
use didcomm_agent::{Agent, Features, Identity, Received};
use didcomm_core::resolver::DIDResolver;
use didcomm_crypto_askar::AskarCryptoService;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{Notify, Semaphore};
use tower_http::cors::CorsLayer;

use crate::channel::{self, Channel, ChannelInfo};
use crate::config::{Config, DidMethod};
use crate::web;

/// How many subscribers are sent to at once.
const DELIVERY_CONCURRENCY: usize = 8;
/// How often what couldn't be delivered is tried again.
const DELIVERY_RETRY: Duration = Duration::from_secs(60);
/// How many posts a subscriber who is behind is sent in one pass.
const DELIVERY_BATCH: usize = 50;
/// How many posts the feed carries.
const FEED_SIZE: usize = 30;

pub struct AppState {
    pub agent: Agent,
    pub channel: Channel,
    /// Published at `/.well-known/did.json` and `/did.json` when the channel is a
    /// `did:web`.
    pub did_document: Option<Value>,
    /// Resolves the DID a `from_prior` names, to check who signed it.
    pub resolver: Box<dyn DIDResolver>,
    /// Rung when there is something new to send to subscribers.
    pub deliver: Notify,
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The agent for `config`: a `did:web` derived from `server.public_url`, or a
/// `did:peer:4` derived from the identity's keys and that URL.
pub fn build_agent(config: &Config, identity: Identity) -> anyhow::Result<(Agent, Option<Value>)> {
    let endpoint = &config.server.public_url;
    let (agent, document) = match config.server.did_method {
        DidMethod::Peer => (Agent::with_endpoint(identity, endpoint)?, None),
        DidMethod::Web => {
            let did = did_web_for(endpoint)?;
            let document = identity.did_document(&did, endpoint);
            (Agent::with_did(identity, &did), Some(document))
        }
    };
    let features = Features::standard()
        .with_protocol(channel::PIURI, &["channel"])
        .with_protocol(channel::USER_PROFILE, &[]);
    Ok((agent.with_features(features), document))
}

/// What the channel says of itself, from `config`.
pub fn channel_info(config: &Config) -> anyhow::Result<ChannelInfo> {
    let picture = match &config.channel.picture {
        None => None,
        Some(path) => {
            let media_type = match path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref() {
                Some("png") => "image/png",
                Some("jpg" | "jpeg") => "image/jpeg",
                Some("webp") => "image/webp",
                Some("gif") => "image/gif",
                _ => anyhow::bail!("channel.picture {} is not a PNG, JPEG, WebP or GIF", path.display()),
            };
            let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
            Some((media_type.to_string(), base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)))
        }
    };
    Ok(ChannelInfo {
        name: config.channel.name.clone(),
        description: config.channel.description.clone(),
        url: config.server.public_url.clone(),
        picture,
        show_authors: config.channel.show_authors,
        max_post_length: config.channel.max_post_length,
    })
}

/// The `did:web` for a URL: host (`:port` percent-encoded), then any path segments
/// joined by `:` -- `https://example.com:8443/news/` → `did:web:example.com%3A8443:news`.
pub fn did_web_for(url: &str) -> anyhow::Result<String> {
    let url = reqwest::Url::parse(url)?;
    let host = url.host_str().ok_or_else(|| anyhow::anyhow!("{url} has no host"))?;
    let mut did = format!("did:web:{host}");
    if let Some(port) = url.port() {
        did.push_str(&format!("%3A{port}"));
    }
    for segment in url.path_segments().into_iter().flatten().filter(|s| !s.is_empty()) {
        did.push(':');
        did.push_str(segment);
    }
    Ok(did)
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index).post(receive))
        .route("/posts/{seq}", get(post_page))
        .route("/feed.xml", get(feed))
        .route("/api/channel", get(api_channel))
        .route("/api/posts", get(api_posts))
        .route("/did", get(did))
        .route("/.well-known/did.json", get(did_document))
        .route("/did.json", get(did_document))
        .route("/healthz", get(|| async { "ok" }))
        // A browser app sends its messages, and resolves a did:web, from another origin.
        .layer(CorsLayer::permissive())
        .with_state(state)
}

fn internal(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "request failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "something went wrong").into_response()
}

async fn did(State(state): State<Arc<AppState>>) -> String {
    state.agent.did()
}

async fn did_document(State(state): State<Arc<AppState>>) -> Response {
    match &state.did_document {
        Some(document) => Json(document.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "this channel's DID is not a did:web").into_response(),
    }
}

#[derive(Deserialize)]
struct Paging {
    before: Option<i64>,
    limit: Option<usize>,
}

async fn index(State(state): State<Arc<AppState>>, Query(paging): Query<Paging>) -> Response {
    let store = &state.channel.store;
    // One more than a page, to know whether there is anything older.
    let mut posts = match store.posts_before(paging.before, web::PAGE_SIZE + 1) {
        Ok(posts) => posts,
        Err(e) => return internal(e),
    };
    let older = (posts.len() > web::PAGE_SIZE).then(|| {
        posts.truncate(web::PAGE_SIZE);
        posts.last().map_or(0, |post| post.seq)
    });
    Html(web::index_page(&state.channel, &state.agent.did(), &posts, older, paging.before.is_some())).into_response()
}

async fn post_page(State(state): State<Arc<AppState>>, Path(seq): Path<i64>) -> Response {
    match state.channel.store.post(seq) {
        Ok(Some(post)) => Html(web::post_page(&state.channel, &state.agent.did(), &post)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "there is no such post").into_response(),
        Err(e) => internal(e),
    }
}

async fn feed(State(state): State<Arc<AppState>>) -> Response {
    match state.channel.store.posts_before(None, FEED_SIZE) {
        Ok(posts) => (
            [(header::CONTENT_TYPE, "application/atom+xml; charset=utf-8")],
            web::feed(&state.channel, &state.agent.did(), &posts),
        )
            .into_response(),
        Err(e) => internal(e),
    }
}

async fn api_channel(State(state): State<Arc<AppState>>) -> Response {
    let store = &state.channel.store;
    match (store.latest_seq(), store.subscriber_count()) {
        (Ok(latest_seq), Ok(subscribers)) => Json(json!({
            "did": state.agent.did(),
            "name": state.channel.info.name,
            "description": state.channel.info.description,
            "url": state.channel.info.url,
            "latest_seq": latest_seq,
            "subscribers": subscribers,
        }))
        .into_response(),
        (Err(e), _) | (_, Err(e)) => internal(e),
    }
}

async fn api_posts(State(state): State<Arc<AppState>>, Query(paging): Query<Paging>) -> Response {
    let limit = paging.limit.unwrap_or(web::PAGE_SIZE).clamp(1, channel::MAX_HISTORY_LIMIT);
    match state.channel.store.posts_before(paging.before, limit) {
        Ok(posts) => Json(json!({
            "posts": posts.iter().map(|post| {
                let mut entry = json!({
                    "seq": post.seq,
                    "id": post.id,
                    "content": post.content,
                    "posted_at": post.posted_at,
                });
                if let Some(author) = state.channel.author_of(post) {
                    entry["author"] = json!(author);
                }
                entry
            }).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => internal(e),
    }
}

/// A member's DID change, if `received` announces one that checks out: signed by a
/// key the DID it names as left lists, and naming the sender as who it became.
async fn rotation(state: &AppState, received: &Received, sender: &str) -> Option<(String, String)> {
    let jwt = received.message["from_prior"].as_str()?;
    match didcomm_core::rotation::verify_from_prior(&AskarCryptoService, state.resolver.as_ref(), jwt).await {
        Ok((prior, new)) if new == sender && prior != new => Some((prior, new)),
        Ok((_, new)) => {
            tracing::warn!(%sender, %new, "ignoring a from_prior that is for a DID other than its sender's");
            None
        }
        Err(error) => {
            tracing::warn!(%sender, %error, "ignoring a from_prior that doesn't check out");
            None
        }
    }
}

async fn receive(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    let received = match state.agent.receive(&body).await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("unpack failed: {e}")).into_response(),
    };
    tracing::debug!(sender = ?received.sender, message_type = received.message_type(), "received");
    // Nothing is done for someone nobody vouches for: there is no one to answer.
    let Some(sender) = received.sender.clone() else {
        return StatusCode::ACCEPTED.into_response();
    };

    if let Some((prior, new)) = rotation(&state, &received, &sender).await {
        match state.channel.rotated(&prior, &new, now()) {
            Ok(Some(notice)) => {
                tracing::info!(%prior, %new, "a member changed DID");
                send_later(&state, new, notice);
            }
            Ok(None) => {}
            Err(e) => return internal(e),
        }
    }

    let reply = match state.agent.auto_reply(&received) {
        Some(reply) => Some(reply),
        None => match state.channel.handle(&sender, &received.message, now()) {
            Ok(outcome) => {
                for (did, notice) in outcome.notices {
                    send_later(&state, did, notice);
                }
                if outcome.posted {
                    state.deliver.notify_one();
                }
                outcome.reply
            }
            Err(e) => return internal(e),
        },
    };
    let Some(reply) = reply else {
        return StatusCode::ACCEPTED.into_response();
    };
    match state.agent.respond(&received, &reply).await {
        Ok(Some(packed)) => ([(header::CONTENT_TYPE, "application/didcomm-encrypted+json")], packed).into_response(),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "couldn't deliver reply");
            (StatusCode::BAD_GATEWAY, format!("couldn't deliver reply: {e}")).into_response()
        }
    }
}

/// Sends `message` to `did` without holding up whoever caused it.
fn send_later(state: &Arc<AppState>, did: String, message: Value) {
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = state.agent.send(&did, &message).await {
            tracing::warn!(%did, %error, "couldn't send a notice");
        }
    });
}

/// Sends every subscriber the posts they haven't had, in order. A subscriber who
/// can't be reached is left where they are, and tried again on the next pass.
pub async fn deliver_pending(state: &Arc<AppState>) -> anyhow::Result<()> {
    let store = &state.channel.store;
    let latest = store.latest_seq()?;
    let limit = Arc::new(Semaphore::new(DELIVERY_CONCURRENCY));
    let mut sends = tokio::task::JoinSet::new();
    for member in store.subscribers()?.into_iter().filter(|m| m.delivered_seq < latest) {
        let state = state.clone();
        let limit = limit.clone();
        sends.spawn(async move {
            let _permit = limit.acquire().await;
            let posts = match state.channel.store.posts_after(member.delivered_seq, DELIVERY_BATCH) {
                Ok(posts) => posts,
                Err(error) => return tracing::error!(%error, "couldn't read posts to deliver"),
            };
            for post in posts {
                if let Err(error) = state.agent.send(&member.did, &state.channel.announcement(&post)).await {
                    tracing::warn!(did = %member.did, seq = post.seq, %error, "couldn't deliver a post; will try again");
                    return;
                }
                if let Err(error) = state.channel.store.set_delivered(&member.did, post.seq) {
                    return tracing::error!(%error, "couldn't record a delivery");
                }
            }
        });
    }
    while sends.join_next().await.is_some() {}
    Ok(())
}

/// Delivers whenever there is something new, and again every so often for whoever
/// couldn't be reached. Runs for as long as the server does.
pub async fn run_deliveries(state: Arc<AppState>) {
    loop {
        if let Err(error) = deliver_pending(&state).await {
            tracing::error!(%error, "delivery pass failed");
        }
        tokio::select! {
            _ = state.deliver.notified() => {}
            _ = tokio::time::sleep(DELIVERY_RETRY) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn did_web_derivation() {
        assert_eq!(did_web_for("https://updates.example/").unwrap(), "did:web:updates.example");
        assert_eq!(did_web_for("https://updates.example:8443/a/b/").unwrap(), "did:web:updates.example%3A8443:a:b");
    }
}
