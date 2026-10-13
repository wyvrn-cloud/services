//! Someone a service's settings name by a public address, recognised when they
//! write.
//!
//! A service knows people by the DID their messages come from. A person's app
//! writes from its account's DID, and shows them a public address
//! (`did:webvh:…:id.example:alice`): another DID, of the same holder. An address
//! put in a setting (`ANNOUNCE_ADMINS`) therefore never matches a sender by name.
//! It matches by key: the address's document lists the key-agreement keys of its
//! holder's devices, and a message is authenticated with one of the sender's.
//! Whoever wrote with a key the address lists is who the address names.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use didcomm_core::resolver::DIDResolver;
use serde_json::Value;
use wyvrn_hosted_did::keys::keys_for;

/// Whether a DID is one fetched from a host, whose document can change: a
/// `did:web` or `did:webvh`. Only those are looked into; any other DID in a
/// setting is who it says, and matched by name.
pub fn is_hosted(did: &str) -> bool {
    did.starts_with("did:web:") || did.starts_with("did:webvh:")
}

/// The key a message was authenticated with, if `kid` names a key-agreement key of
/// the sender's own document. That one key: what else the document lists says
/// nothing of who wrote.
pub fn key_written_with(sender_document: &Value, kid: &str) -> Option<String> {
    let listed = keys_for(sender_document, "keyAgreement");
    let fragment = kid.rsplit('#').next().unwrap_or_default();
    sender_document["verificationMethod"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|method| method["id"].as_str().is_some_and(|id| id == kid || id.rsplit('#').next() == Some(fragment)))
        .and_then(|method| method["publicKeyMultibase"].as_str())
        .filter(|key| listed.iter().any(|k| k == key))
        .map(str::to_string)
}

/// How long a named DID's keys are remembered: a message from anyone not yet
/// recognised asks after them.
const REMEMBERED: Duration = Duration::from_secs(300);

/// A DID's keys, and when they were read.
type Remembered = HashMap<String, (Instant, Vec<String>)>;

async fn keys_of(resolver: &dyn DIDResolver, did: &str) -> Vec<String> {
    static KEYS: Mutex<Option<Remembered>> = Mutex::new(None);
    let remembered = |did: &str| {
        let keys = KEYS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        keys.as_ref().and_then(|keys| keys.get(did)).filter(|(at, _)| at.elapsed() < REMEMBERED).map(|(_, keys)| keys.clone())
    };
    if let Some(keys) = remembered(did) {
        return keys;
    }
    match resolver.resolve(did).await {
        Ok(document) => {
            let keys = keys_for(&document, "keyAgreement");
            KEYS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get_or_insert_with(HashMap::new).insert(did.to_string(), (Instant::now(), keys.clone()));
            keys
        }
        Err(error) => {
            tracing::warn!(%did, %error, "couldn't read a DID the settings name, to see whether this is its holder writing");
            Vec::new()
        }
    }
}

/// The DID among `named` whose holder wrote this message: a hosted one that lists
/// the key `sender` authenticated with (`kid`). `None` if there is none, or the
/// message was authenticated by nobody.
pub async fn holder_among<'a>(resolver: &dyn DIDResolver, named: &'a [String], sender: &str, kid: Option<&str>) -> Option<&'a str> {
    let hosted: Vec<&String> = named.iter().filter(|did| is_hosted(did) && did.as_str() != sender).collect();
    if hosted.is_empty() {
        return None;
    }
    let key = key_written_with(&resolver.resolve(sender).await.ok()?, kid?)?;
    for did in hosted {
        if keys_of(resolver, did).await.contains(&key) {
            return Some(did.as_str());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document(did: &str, agreement: &[(&str, &str)], other: &[(&str, &str)]) -> Value {
        let method = |(fragment, key): &(&str, &str)| json!({ "id": format!("{did}#{fragment}"), "type": "Multikey", "publicKeyMultibase": key });
        json!({
            "id": did,
            "verificationMethod": agreement.iter().chain(other).map(method).collect::<Vec<_>>(),
            "keyAgreement": agreement.iter().map(|(fragment, _)| format!("{did}#{fragment}")).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn the_key_written_with_is_the_senders_own_agreement_key_and_no_other() {
        let sender = document("did:peer:4zSender", &[("key-2", "z6LSphone")], &[("key-1", "z6Mksigns")]);
        assert_eq!(key_written_with(&sender, "did:peer:4zSender#key-2").as_deref(), Some("z6LSphone"));
        // A short id, as some senders write it.
        assert_eq!(key_written_with(&sender, "#key-2").as_deref(), Some("z6LSphone"));
        // A key the document has, but not for this; and one it doesn't have.
        assert_eq!(key_written_with(&sender, "did:peer:4zSender#key-1"), None);
        assert_eq!(key_written_with(&sender, "did:peer:4zSender#key-9"), None);
    }

    #[test]
    fn only_a_did_fetched_from_a_host_is_looked_into() {
        assert!(is_hosted("did:webvh:QmScid:id.example:alice") && is_hosted("did:web:id.example:alice"));
        assert!(!is_hosted("did:peer:4zQm") && !is_hosted("did:key:z6Mk"));
    }
}
