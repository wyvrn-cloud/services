//! The HTTP side: the endpoint members' messages come to, and the sending of
//! what is posted to everyone who has joined.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Bytes,
    extract::State,
    routing::get,
    Router,
};
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
    pub community: Community,
    /// Published at `/.well-known/did.json` when the server is a `did:web`.
    pub did_document: Option<address::Published>,
    /// Resolves the DID a `from_prior` names, to check who signed it.
    pub resolver: Box<dyn DIDResolver>,
    /// Rung when there is something new to send to members.
    pub deliver: Notify,
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
        .with_protocol(community::USER_PROFILE, &[]);
    Ok((agent.with_features(features), document))
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
        if let Err(error) = state.agent.send(&did, &message).await {
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

        let outcome = state.community.handle(&sender, &received.message, now())?;
        for (did, notice) in outcome.notices {
            send_later(state, did, notice);
        }
        if outcome.channels_changed {
            // Everyone else who has joined: whoever asked has it as their answer.
            for (did, notice) in state.community.community_notices()? {
                if did != sender {
                    send_later(state, did, notice);
                }
            }
        }
        if outcome.posted {
            state.deliver.notify_one();
        }
        Ok(outcome.reply)
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
                if let Err(error) = state.agent.send(&member.did, &state.community.message(&post)).await {
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
        .route("/", get(index).post(|State(state): State<Arc<AppState>>, body: Bytes| receive(Arc::new(CommunityService(state)), body)))
        .route("/healthz", get(|| async { "ok" }))
        .merge(address::routes(state.agent.did(), state.did_document.clone()))
        // A browser app sends its messages from another origin.
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Says what this is and how to reach it. Nothing of who is in it or what is said.
async fn index(State(state): State<Arc<AppState>>) -> String {
    format!(
        "This is a Wyvrn community server.\nIts DID: {}\nIt is by invitation, and spoken to over DIDComm: {}\n",
        state.agent.did(),
        community::PIURI
    )
}
