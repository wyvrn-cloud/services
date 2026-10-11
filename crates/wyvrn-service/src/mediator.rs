//! Being reached through a mediator: for a service with no address of its own that
//! the people it serves can get to (behind a home router, or serving a phone app
//! that refuses plain HTTP).

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use didcomm_agent::Agent;
use serde::Deserialize;

use crate::Service;

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

/// Collects what the mediator holds for the service, for as long as it runs. Each
/// message is acted on as one posted to the service directly would be; its answer
/// goes to the sender's own address, there being no connection to answer on.
pub async fn run_pickups<S: Service>(service: Arc<S>, every: Duration) {
    let agent = service.agent();
    loop {
        match agent.pickup(PICKUP_BATCH).await {
            Ok(pickup) => {
                for (id, why) in &pickup.failed {
                    tracing::warn!(%id, %why, "couldn't read a message the mediator held");
                }
                for received in &pickup.messages {
                    tracing::debug!(sender = ?received.sender, message_type = received.message_type(), "picked up");
                    match service.answer(received).await {
                        Ok(Some(reply)) => {
                            if let Err(e) = agent.respond(received, &reply).await {
                                tracing::warn!(error = %e, "couldn't deliver reply");
                            }
                        }
                        Ok(None) => {}
                        Err(error) => tracing::error!(%error, "handling a picked-up message failed"),
                    }
                }
                // More may be waiting than one pickup takes.
                if pickup.messages.len() >= PICKUP_BATCH {
                    continue;
                }
            }
            Err(e) => tracing::warn!(error = %e, "couldn't collect from the mediator"),
        }
        tokio::time::sleep(every).await;
    }
}
