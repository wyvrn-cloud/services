//! Being reached through a mediator: for a service with no address of its own that
//! the people it serves can get to (behind a home router, or serving a phone app
//! that refuses plain HTTP).

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use base64::Engine;
use didcomm_agent::Agent;
use serde_json::{json, Value};
use serde::Deserialize;

use crate::{Incoming, Service};

/// How many messages one pickup asks for.
const PICKUP_BATCH: usize = 20;

/// A service's `[mediator]` setting.
#[derive(Debug, Clone, Deserialize)]
pub struct MediatorConfig {
    /// The mediator's DID, e.g. `did:web:mediator.wyvrn.app`.
    pub did: String,
    /// How often the service collects what the mediator holds for it.
    #[serde(default = "default_poll_seconds")]
    pub poll_seconds: u64,
}

fn default_poll_seconds() -> u64 {
    2
}

impl MediatorConfig {
    pub fn poll_every(&self) -> Duration {
        Duration::from_secs(self.poll_seconds.max(1))
    }
}

/// Asks the mediator to carry the service's messages. From then on the service's
/// DID names the mediator as the way in, so do this before saying what the DID is.
pub async fn register(agent: &Agent, mediator: &MediatorConfig) -> anyhow::Result<()> {
    let mediation = agent
        .mediate(&mediator.did)
        .await
        .with_context(|| format!("asking {} to mediate", mediator.did))?;
    tracing::info!(mediator = %mediation.mediator_did, "reached through a mediator");
    Ok(())
}

const DELIVERY_REQUEST: &str = "https://didcomm.org/messagepickup/3.0/delivery-request";
const DELIVERY: &str = "https://didcomm.org/messagepickup/3.0/delivery";
const STATUS: &str = "https://didcomm.org/messagepickup/3.0/status";
const MESSAGES_RECEIVED: &str = "https://didcomm.org/messagepickup/3.0/messages-received";

/// Collects what the mediator holds for the service, for as long as it runs. Each
/// message is acted on as one posted to the service directly would be; its answer
/// goes to the sender's own address, there being no connection to answer on.
///
/// The deliveries are asked for and opened here, rather than by the agent's own
/// `pickup`: that hands back each message already opened, without the key its
/// sender authenticated it with. What a mediator holds is the sender's sealed
/// message, exactly as sent -- the mediator only carried it -- so opened here it
/// says as much about its sender as one that came straight in.
pub async fn run_pickups<S: Service>(service: Arc<S>, every: Duration) {
    loop {
        match collect(service.agent()).await {
            Ok(deliveries) => {
                let full = deliveries.len() >= PICKUP_BATCH;
                let mut ids = Vec::new();
                for (id, packed) in deliveries {
                    match packed {
                        Some(packed) => deliver(&service, &packed).await,
                        None => tracing::warn!(%id, "a delivery from the mediator carried no message"),
                    }
                    // Acknowledged whatever became of it: trying again can't make a
                    // message readable, and left queued it would come back every time.
                    if !id.is_empty() {
                        ids.push(id);
                    }
                }
                if let Err(e) = acknowledge(service.agent(), &ids).await {
                    tracing::warn!(error = %e, "couldn't tell the mediator what was collected");
                }
                // More may be waiting than one pickup takes.
                if full {
                    continue;
                }
            }
            Err(e) => tracing::warn!(error = %e, "couldn't collect from the mediator"),
        }
        tokio::time::sleep(every).await;
    }
}

/// What the mediator holds, up to a batch: each delivery's id, and the sealed
/// message in it.
async fn collect(agent: &Agent) -> anyhow::Result<Vec<(String, Option<Vec<u8>>)>> {
    let mediation = agent.mediation().context("not registered with a mediator")?;
    let request = json!({ "type": DELIVERY_REQUEST, "body": { "limit": PICKUP_BATCH } });
    let reply = agent.request_as(agent.base_did(), &mediation.mediator_did, &request).await?;
    match reply.message_type() {
        // The answer to a delivery-request when nothing is waiting.
        STATUS => Ok(Vec::new()),
        DELIVERY => Ok(reply.message["attachments"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|attachment| (attachment["id"].as_str().unwrap_or_default().to_string(), attachment_payload(attachment)))
            .collect()),
        other => anyhow::bail!("the mediator answered a delivery-request with {other}"),
    }
}

async fn acknowledge(agent: &Agent, ids: &[String]) -> anyhow::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mediation = agent.mediation().context("not registered with a mediator")?;
    // Asks for the reply on this connection: the mediator answers with a status,
    // which must never be queued for pickup instead.
    let ack = json!({ "type": MESSAGES_RECEIVED, "body": { "message_id_list": ids }, "return_route": "all" });
    agent.send_as(agent.base_did(), &mediation.mediator_did, &ack).await?;
    Ok(())
}

/// The sealed message in a delivery attachment: `data.json` (a JSON envelope) or
/// `data.base64` (base64url or the ordinary alphabet, padded or not).
fn attachment_payload(attachment: &Value) -> Option<Vec<u8>> {
    let data = &attachment["data"];
    if let Some(envelope) = data.get("json").filter(|json| !json.is_null()) {
        return serde_json::to_vec(envelope).ok();
    }
    let text = data["base64"].as_str()?.trim_end_matches('=');
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(text))
        .ok()
}

/// Acts on one sealed message that was carried here rather than posted: opens it,
/// has the service answer it, and sends the answer to the sender's own address.
/// On its own so that it can be handed a message without a mediator to fetch it
/// from.
pub async fn deliver<S: Service>(service: &Arc<S>, packed: &[u8]) {
    let agent = service.agent();
    let mut received = match agent.receive(packed).await {
        Ok(received) => received,
        Err(why) => {
            tracing::warn!(%why, "couldn't read a message the mediator held");
            return;
        }
    };
    // Whatever it asked about replying on the connection was said to a connection
    // long gone: the answer goes to the sender's endpoint.
    if let Some(headers) = received.message.as_object_mut() {
        headers.remove("return_route");
    }
    let sender_kid = match agent.messaging().unpack_verified(packed).await {
        Ok(unpacked) => unpacked.sender_kid,
        Err(_) => None,
    };
    tracing::debug!(sender = ?received.sender, message_type = received.message_type(), "picked up");
    if service.requires_authcrypt() && sender_kid.is_none() {
        tracing::debug!(message_type = received.message_type(), "dropped a message nobody authenticated");
        return;
    }
    match service.answer(&Incoming { received: &received, sender_kid }).await {
        Ok(Some(reply)) => {
            if let Err(e) = agent.respond(&received, &reply).await {
                tracing::warn!(error = %e, "couldn't deliver reply");
            }
        }
        Ok(None) => {}
        Err(error) => tracing::error!(%error, "handling a picked-up message failed"),
    }
}
