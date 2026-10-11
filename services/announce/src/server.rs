//! The HTTP side: DIDComm messages on `POST /`, the web pages on `GET`, and sending
//! posts out to subscribers.

use std::sync::Arc;
use std::time::Duration;

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
use crate::config::Config;
use wyvrn_service::{
    address,
    http::{internal, receive},
    Service,
};
pub use wyvrn_service::{address::did_web_for, now};
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
    /// Posts whose reactions changed and that subscribers haven't been told of yet.
    pub reacted: std::sync::Mutex<std::collections::BTreeSet<i64>>,
    /// Rung when `reacted` gains one.
    pub react: Notify,
}

/// The agent for `config`: a `did:web` derived from `server.public_url`, or a
/// `did:peer:4` derived from the identity's keys and that URL.
pub fn build_agent(config: &Config, identity: Identity) -> anyhow::Result<(Agent, Option<Value>)> {
    let (agent, document) = address::build_agent(identity, &config.server.public_url, config.server.did_method)?;
    let features = Features::standard()
        .with_protocol(channel::PIURI, &["channel"])
        .with_protocol(channel::USER_PROFILE, &[])
        // Subscribers react to posts as they would to any message.
        .with_protocol(channel::MESSAGE_REACTIONS_PIURI, &["observer"]);
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

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index).post(|State(state): State<Arc<AppState>>, body: Bytes| receive(Arc::new(ChannelService(state)), body)))
        .route("/posts/{seq}", get(post_page))
        .route("/feed.xml", get(feed))
        .route("/api/channel", get(api_channel))
        .route("/api/posts", get(api_posts))
        .merge(address::routes(state.agent.did(), state.did_document.clone()))
        .route("/healthz", get(|| async { "ok" }))
        // A browser app sends its messages, and resolves a did:web, from another origin.
        .layer(CorsLayer::permissive())
        .with_state(state)
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
                if let Ok(counts) = state.channel.reaction_counts(post.seq) {
                    entry["reactions"] = counts;
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

/// The channel as the shared plumbing sees it (`wyvrn_service::Service`): its
/// agent, and what it answers. Around the `Arc`, because answering can leave
/// something to send later, which outlives the request.
pub struct ChannelService(pub Arc<AppState>);

impl Service for ChannelService {
    fn agent(&self) -> &Agent {
        &self.0.agent
    }

    async fn answer(&self, received: &Received) -> anyhow::Result<Option<Value>> {
        answer(&self.0, received).await
    }
}

/// Acts on one message, however it got here, and returns what to answer it with.
async fn answer(state: &Arc<AppState>, received: &Received) -> anyhow::Result<Option<Value>> {
    // Nothing is done for someone nobody vouches for: there is no one to answer.
    let Some(sender) = received.sender.clone() else {
        return Ok(None);
    };

    if let Some((prior, new)) = rotation(state, received, &sender).await {
        if let Some(notice) = state.channel.rotated(&prior, &new, now())? {
            tracing::info!(%prior, %new, "a member changed DID");
            send_later(state, new, notice);
        }
    }

    if let Some(reply) = state.agent.auto_reply(received) {
        return Ok(Some(reply));
    }
    let outcome = state.channel.handle(&sender, &received.message, now())?;
    for (did, notice) in outcome.notices {
        send_later(state, did, notice);
    }
    if outcome.posted {
        state.deliver.notify_one();
    }
    if let Some(seq) = outcome.reacted {
        state.reacted.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(seq);
        state.react.notify_one();
    }
    Ok(outcome.reply)
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

/// How long reactions to a post are gathered before subscribers are told. Telling
/// everyone is a message each, so a burst of reactions is told once, not each time.
const REACTIONS_SETTLE: std::time::Duration = std::time::Duration::from_secs(1);

/// Tells every subscriber where the reactions to `seqs` now stand: how many of
/// each, and their own. Best-effort; whoever misses it hears with the next change,
/// or reads it from the log.
pub async fn tell_reactions(state: &Arc<AppState>, seqs: Vec<i64>) -> anyhow::Result<()> {
    let limit = Arc::new(Semaphore::new(DELIVERY_CONCURRENCY));
    let mut sends = tokio::task::JoinSet::new();
    for member in state.channel.store.subscribers()? {
        let state = state.clone();
        let limit = limit.clone();
        let seqs = seqs.clone();
        sends.spawn(async move {
            let _permit = limit.acquire().await;
            for seq in seqs {
                let notice = match state.channel.reactions_notice(seq, &member.did, now()) {
                    Ok(Some(notice)) => notice,
                    Ok(None) => continue,
                    Err(error) => return tracing::error!(%error, "couldn't read a post's reactions"),
                };
                if let Err(error) = state.agent.send(&member.did, &notice).await {
                    return tracing::debug!(did = %member.did, seq, %error, "couldn't tell a subscriber of reactions");
                }
            }
        });
    }
    while sends.join_next().await.is_some() {}
    Ok(())
}

/// Tells subscribers of changed reactions, a little after they change. Runs for as
/// long as the server does.
pub async fn run_reaction_notices(state: Arc<AppState>) {
    loop {
        state.react.notified().await;
        tokio::time::sleep(REACTIONS_SETTLE).await;
        let seqs: Vec<i64> =
            std::mem::take(&mut *state.reacted.lock().unwrap_or_else(|poisoned| poisoned.into_inner())).into_iter().collect();
        if let Err(error) = tell_reactions(&state, seqs).await {
            tracing::error!(%error, "telling subscribers of reactions failed");
        }
    }
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
