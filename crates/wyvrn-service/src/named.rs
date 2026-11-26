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

/// did-hosting/1.0, which a service that follows a correspondent to its public
/// address discloses in the role [`CORRESPONDENT`] (did-hosting/1.0, "Roles"): it
/// resolves a `did:webvh` sender, follows a `from_prior` onto one, and reads its log
/// again for a key its copy doesn't list (as the DIDComm library's resolver does).
pub const DID_HOSTING: &str = "https://wyvrn.app/did-hosting/1.0";
pub const CORRESPONDENT: &str = "correspondent";

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

static KEYS: Mutex<Option<Remembered>> = Mutex::new(None);

fn remember(did: &str, keys: &[String]) {
    KEYS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get_or_insert_with(HashMap::new).insert(did.to_string(), (Instant::now(), keys.to_vec()));
}

async fn keys_of(resolver: &dyn DIDResolver, did: &str) -> Vec<String> {
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
            remember(did, &keys);
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
///
/// A sender that is itself a hosted DID `named` names -- an account speaking as
/// its public address (multi-device/1.0, "Speaking as a public address") -- is
/// that DID's holder when the key it wrote with is one its own document lists:
/// nothing else is looked up for it.
pub async fn holder_among<'a>(resolver: &dyn DIDResolver, named: &'a [String], sender: &str, kid: Option<&str>) -> Option<&'a str> {
    let hosted: Vec<&String> = named.iter().filter(|did| is_hosted(did)).collect();
    if hosted.is_empty() {
        return None;
    }
    let sender_document = resolver.resolve(sender).await.ok()?;
    let key = key_written_with(&sender_document, kid?)?;
    if let Some(itself) = hosted.iter().find(|did| did.as_str() == sender) {
        return Some(itself.as_str());
    }
    let hosted: Vec<&String> = hosted.into_iter().filter(|did| did.as_str() != sender).collect();
    for did in &hosted {
        if keys_of(resolver, did).await.contains(&key) {
            return Some(did.as_str());
        }
    }
    // None lists it as held: an address whose holder has since added a device
    // lists the new key only in a newer document. Asked again past what the
    // resolver keeps (it says when it has nothing fresher, or fetched too
    // recently to fetch again).
    for did in hosted {
        match resolver.resolve_fresh(did).await {
            Ok(Some(document)) => {
                let keys = keys_for(&document, "keyAgreement");
                remember(did, &keys);
                if keys.contains(&key) {
                    return Some(did.as_str());
                }
            }
            Ok(None) => {}
            Err(error) => tracing::debug!(%did, %error, "couldn't read a named DID afresh"),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use didcomm_core::resolver::ResolutionError;
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

    /// A resolver that holds an old document for the address, and has a newer
    /// one when asked afresh.
    struct Held {
        sender: Value,
        held: Value,
        fresh: Option<Value>,
    }

    #[async_trait::async_trait]
    impl DIDResolver for Held {
        async fn resolve(&self, did: &str) -> Result<Value, ResolutionError> {
            Ok(if did.starts_with("did:peer:") { self.sender.clone() } else { self.held.clone() })
        }
        async fn is_resolvable(&self, _did: &str) -> bool {
            true
        }
        async fn resolve_fresh(&self, _did: &str) -> Result<Option<Value>, ResolutionError> {
            Ok(self.fresh.clone())
        }
    }

    #[tokio::test]
    async fn a_sender_that_is_the_named_address_is_its_holder_by_a_key_it_lists() {
        let address = "did:webvh:QmScid:id.example:speaks";
        let own = document(address, &[("z6LSphone", "z6LSphone")], &[("z6Mksigns", "z6Mksigns")]);
        // The resolver gives the address's own document for the address, and nothing
        // newer: nothing more is asked of it.
        let resolver = Held { sender: Value::Null, held: own.clone(), fresh: None };
        let named = vec!["did:web:id.example:speaks".to_string(), address.to_string()];
        assert_eq!(holder_among(&resolver, &named, address, Some(&format!("{address}#z6LSphone"))).await, Some(address));
        // Not by a key it doesn't list for agreeing keys, nor by nobody.
        assert_eq!(holder_among(&resolver, &named, address, Some(&format!("{address}#z6Mksigns"))).await, None);
        assert_eq!(holder_among(&resolver, &named, address, None).await, None);
    }

    #[tokio::test]
    async fn a_holder_with_a_new_device_is_found_in_the_addresss_newer_document() {
        let address = "did:webvh:QmScid:id.example:newdevice";
        let sender = document("did:peer:4zNew", &[("key-2", "z6LSnewphone")], &[]);
        let held = document(address, &[("key-1", "z6LSoldphone")], &[]);
        let named = vec![address.to_string()];
        // Only the held document, and nothing fresher: not recognised.
        let stale = Held { sender: sender.clone(), held: held.clone(), fresh: None };
        assert_eq!(holder_among(&stale, &named, "did:peer:4zNew", Some("did:peer:4zNew#key-2")).await, None);
        // The address read afresh lists the new device's key.
        let fresh = Held { sender, held, fresh: Some(document(address, &[("key-1", "z6LSoldphone"), ("key-2", "z6LSnewphone")], &[])) };
        assert_eq!(holder_among(&fresh, &named, "did:peer:4zNew", Some("did:peer:4zNew#key-2")).await, Some(address));
    }
}
