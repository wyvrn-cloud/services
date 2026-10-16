//! Which DID a service speaks as, once it holds a public address.
//!
//! A service is the `did:web` of its own domain. Holding an address at a DID host
//! (hosted_address.rs) gives it a `did:webvh` there with the very same keys, whose
//! history the host can't rewrite: that is the DID it is known by from then on.
//! So, from the moment it holds one:
//!
//! - **What is sent to the address is read.** The service's key-agreement key is
//!   filed under the address's key ids too (the `did:webvh`'s and the address's own
//!   `did:web`'s `#key-2`): a message is opened by the key id it names, and the
//!   same key under another id is not found.
//! - **What it sends unasked comes from the `did:webvh`** ([`Voice::send`]), with a
//!   `from_prior` signed by its `did:web`: whoever knew it by that follows it to the
//!   address, as people's apps follow a contact's change of DID.
//! - **An answer comes from the DID it answers for** (the agent's own `respond`):
//!   someone who wrote to the `did:web` is answered from it, and has the move from
//!   the next thing the service sends unasked.
//!
//! What the service asks of its DID host it asks as its `did:web`, the DID the
//! host set its name aside for (`Agent::request_as` with `base_did`).

use std::sync::RwLock;

use anyhow::{anyhow, Context};
use didcomm_agent::Agent;
use didcomm_crypto_askar::{AskarSecretKey, AskarSigningKey};
use serde_json::Value;

/// The address a service speaks as, and the `from_prior` that says it moved there.
#[derive(Debug, Clone)]
struct Speaking {
    did: String,
    from_prior: String,
}

/// Which DID a service speaks as: its `did:web` until it holds an address, the
/// address's `did:webvh` after. One per service, shared by everything that sends.
#[derive(Debug, Default)]
pub struct Voice {
    speaking: RwLock<Option<Speaking>>,
}

impl Voice {
    /// The DID the service speaks as now, if it is an address's.
    pub fn speaking_as(&self) -> Option<String> {
        self.speaking.read().expect("voice lock poisoned").as_ref().map(|speaking| speaking.did.clone())
    }

    /// The service holds the address `did` (a `did:webvh`), whose own `did:web` is
    /// `address`: what is sent to either is read from now on, and what the service
    /// sends unasked comes from `did`. `now` is Unix seconds, when it moved.
    pub async fn take_up(&self, agent: &Agent, did: &str, address: &str, now: i64) -> anyhow::Result<()> {
        if self.speaking_as().as_deref() == Some(did) {
            return Ok(());
        }
        let messaging = agent.messaging();
        let identity = agent.identity();
        for name in [did, address] {
            messaging.secrets.add_secret(AskarSecretKey::new(format!("{name}#key-2"), identity.key_agreement_key().clone()));
        }
        let prior = agent.base_did();
        let signing_key = AskarSigningKey::new(format!("{prior}#key-1"), identity.verification_key().clone());
        let from_prior = didcomm_core::rotation::build_from_prior(&messaging.crypto, prior, did, &signing_key, now)
            .await
            .map_err(|why| anyhow!("signing the move to {did}: {why}"))?;
        *self.speaking.write().expect("voice lock poisoned") = Some(Speaking { did: did.to_string(), from_prior });
        tracing::info!(%did, from = %prior, "this service now speaks as its public address");
        Ok(())
    }

    /// Sends `message` to `to`, unasked: from the address the service holds, with
    /// the `from_prior` that moves whoever knew it by its `did:web` there, or as the
    /// agent would (its `did:web`, or its mediated DID) before it holds one. A
    /// mediated service goes on speaking as its mediated DID.
    pub async fn send(&self, agent: &Agent, to: &str, message: &Value) -> anyhow::Result<()> {
        let speaking = self.speaking.read().expect("voice lock poisoned").clone();
        match speaking {
            Some(speaking) if agent.mediation().is_none() => {
                let mut message = message.clone();
                let headers = message.as_object_mut().context("a message is a JSON object")?;
                headers.entry("from_prior").or_insert_with(|| Value::String(speaking.from_prior.clone()));
                agent.send_as(&speaking.did, to, &message).await?;
            }
            _ => {
                agent.send(to, message).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use didcomm_agent::Identity;
    use didcomm_core::crypto::SecretsManager;

    const SERVICE: &str = "did:web:news.example";
    const ADDRESS: &str = "did:web:id.example:official:news";
    const HELD: &str = "did:webvh:QmScid:id.example:official:news";

    #[tokio::test]
    async fn holding_an_address_it_reads_what_is_sent_there_and_speaks_as_it() {
        let agent = Agent::with_did(Identity::generate().unwrap(), SERVICE);
        let voice = Voice::default();
        assert_eq!(voice.speaking_as(), None);
        assert!(agent.messaging().secrets.get_secret_by_kid(&format!("{HELD}#key-2")).await.is_none());

        voice.take_up(&agent, HELD, ADDRESS, 1_800_000_000).await.unwrap();
        assert_eq!(voice.speaking_as().as_deref(), Some(HELD));
        // Its key, under the address's names as well as its own.
        for kid in [format!("{HELD}#key-2"), format!("{ADDRESS}#key-2"), format!("{SERVICE}#key-2")] {
            assert!(agent.messaging().secrets.get_secret_by_kid(&kid).await.is_some(), "{kid}");
        }
        // The move, signed as the did:web it moves from.
        let from_prior = voice.speaking.read().unwrap().clone().unwrap().from_prior;
        let [header, payload, _signature] = from_prior.split('.').collect::<Vec<_>>()[..] else { panic!("{from_prior}") };
        // (Each part unpadded base64url.)
        let decode = |part: &str| -> Value { serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap() };
        assert_eq!(decode(header)["kid"], format!("{SERVICE}#key-1"));
        assert_eq!((decode(payload)["iss"].as_str(), decode(payload)["sub"].as_str(), decode(payload)["iat"].as_i64()), (Some(SERVICE), Some(HELD), Some(1_800_000_000)));
    }
}
