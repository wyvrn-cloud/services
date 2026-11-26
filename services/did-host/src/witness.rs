//! The host as its logs' witness (did-hosting/1.0, "The host as a witness"): its
//! witness key, and which entries it holds back.
//!
//! A holder's update keys are its trusted devices' keys, and did:webvh takes an
//! entry signed by any one of them. So an entry that takes something away is
//! held back for a day, its holder's devices are told, and a recovery key ends
//! the wait; everything else is witnessed, and published, at once.

use serde_json::{json, Value};
use wyvrn_hosted_did::{keys, recovery, write};

use crate::store::Store;

/// How long an entry that takes something away is held back.
pub const HOLD_SECONDS: i64 = 24 * 60 * 60;
/// How long before it is published its holder is told again.
pub const SOON_SECONDS: i64 = 60 * 60;

/// The host's witness: a `did:key` of an Ed25519 key of its own.
pub struct Witness {
    pub did: String,
    jwk: String,
}

/// Where the witness key is kept in the host's store.
const KEPT_AS: &str = "witness-key";

impl Witness {
    /// A witness of the key `jwk` (an Ed25519 JWK with its private part).
    pub fn of(jwk: &str) -> anyhow::Result<Self> {
        Ok(Self { did: write::witness_did(jwk).map_err(anyhow::Error::msg)?, jwk: jwk.to_string() })
    }

    /// A new key.
    pub fn generate() -> anyhow::Result<Self> {
        let identity = didcomm_agent::Identity::generate()?;
        let keys: Value = serde_json::from_str(&identity.to_json()?)?;
        Self::of(&keys["verification_key"].to_string())
    }

    /// The host's witness, kept in its store as its other keys are kept: sealed with
    /// the service's seal key where it has one (wyvrn_service::identity), bound to
    /// `bound_to` (the host's own signing key, as a `did:key`) so that a row copied
    /// into another host's store doesn't open there. Made the first time.
    pub fn load_or_make(store: &Store, bound_to: &str) -> anyhow::Result<Self> {
        let at = format!("{bound_to}#{KEPT_AS}");
        if let Some(kept) = store.secret(KEPT_AS)? {
            let (jwk, again) = wyvrn_service::identity::open_text(&kept, &at)?;
            if again {
                store.set_secret(KEPT_AS, &wyvrn_service::identity::seal_text(&jwk, &at)?)?;
            }
            return Self::of(&jwk);
        }
        let made = Self::generate()?;
        store.set_secret(KEPT_AS, &wyvrn_service::identity::seal_text(&made.jwk, &at)?)?;
        tracing::info!(witness = %made.did, "the host's witness key made");
        Ok(made)
    }

    /// The `witness` parameter a log hosted here carries.
    pub fn parameter(&self) -> Value {
        json!({ "threshold": 1, "witnesses": [{ "id": self.did }] })
    }

    /// `name`'s `did-witness.json` once the entry `version_id` is witnessed: the
    /// proof for the last entry witnessed before, kept so that a reader who read the
    /// log just before this one was published still finds a proof for it, and one
    /// for this.
    pub async fn proofs_with(&self, before: Option<&str>, version_id: &str) -> anyhow::Result<String> {
        let mut proofs: Vec<(String, Value)> = before
            .and_then(|file| serde_json::from_str::<Vec<Value>>(file).ok())
            .and_then(|file| file.last().cloned())
            .and_then(|last| Some((last["versionId"].as_str()?.to_string(), last["proof"].as_array()?.first()?.clone())))
            .filter(|(witnessed, _)| witnessed != version_id)
            .into_iter()
            .collect();
        proofs.push((version_id.to_string(), write::witness_proof(version_id, &self.jwk).await.map_err(anyhow::Error::msg)?));
        Ok(write::witness_file(&proofs))
    }
}

/// The `did:key` a signing key is, for what the host's secrets are bound to.
pub fn bound_to(identity: &didcomm_agent::Identity) -> String {
    let document = identity.did_document("did:key:x", "");
    format!("did:key:{}", document["verificationMethod"][0]["publicKeyMultibase"].as_str().unwrap_or_default())
}

/// What a log's parameters come to after an entry: each entry says only what
/// changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InForce {
    pub update_keys: Vec<String>,
    /// `Null` when nothing is witnessed.
    pub witness: Value,
    pub deactivated: bool,
}

impl InForce {
    fn then(&self, entry: &Value) -> Self {
        let parameters = &entry["parameters"];
        let mut next = self.clone();
        if let Some(keys) = parameters["updateKeys"].as_array() {
            next.update_keys = keys.iter().filter_map(Value::as_str).map(str::to_string).collect();
        }
        if let Some(witness) = parameters.get("witness") {
            let named = witness["witnesses"].as_array().is_some_and(|witnesses| !witnesses.is_empty());
            next.witness = if named { json!({ "threshold": witness["threshold"], "witnesses": witness["witnesses"] }) } else { Value::Null };
        }
        if parameters["deactivated"] == true {
            next.deactivated = true;
        }
        next
    }
}

fn entries(log: &str) -> Vec<Value> {
    log.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
}

/// The parameters in force after the whole of `log`.
pub fn in_force(log: &str) -> InForce {
    entries(log).iter().fold(InForce::default(), |held, entry| held.then(entry))
}

fn removes(before: &[String], after: &[String]) -> bool {
    before.iter().any(|key| !after.contains(key))
}

fn messaging(document: &Value) -> Vec<Value> {
    document["service"].as_array().into_iter().flatten().filter(|service| service["type"] == "DIDCommMessaging").cloned().collect()
}

/// The recovery keys a document lists, by the keys themselves: what they are named
/// in the document is not what makes them the account's (an app of 0.0.28 names an
/// address's keys after themselves, where one before numbered them, and the same
/// recovery key under its new name was held back as changed).
fn recovery_keys(document: &Value) -> Vec<String> {
    let mut listed: Vec<String> = recovery::recovery_keys_listed_in(document).into_iter().map(|(_, key)| key).collect();
    listed.sort();
    listed.dedup();
    listed
}

/// What one entry takes away from the one before it, if anything: what makes it
/// held back. `None` for an entry that only adds.
fn taken_away(before: (&InForce, &Value), after: (&InForce, &Value)) -> Option<&'static str> {
    let ((was, old), (now, new)) = (before, after);
    if removes(&was.update_keys, &now.update_keys) {
        return Some("it removes an update key");
    }
    for relationship in ["authentication", "keyAgreement"] {
        if removes(&keys::keys_for(old, relationship), &keys::keys_for(new, relationship)) {
            return Some("it removes a device's key");
        }
    }
    if recovery_keys(old) != recovery_keys(new) {
        return Some("it changes a recovery key");
    }
    // Turning witnessing on is witnessed at once, as did:webvh requires.
    if !was.witness.is_null() && was.witness != now.witness {
        return Some("it changes the witness");
    }
    if messaging(old) != messaging(new) {
        return Some("it changes where mail goes");
    }
    if now.deactivated && !was.deactivated {
        return Some("it ends the DID");
    }
    None
}

/// Why `added` (entries following the witnessed `log`, one per line) is held back,
/// if it is: compared with the last witnessed entry, any of them takes something
/// away. A first publication takes nothing away.
pub fn held_back_because(log: &str, added: &str) -> Option<&'static str> {
    let mut held = in_force(log);
    let mut document = entries(log).last()?.get("state").cloned().unwrap_or_default();
    for entry in entries(added) {
        let next = held.then(&entry);
        let state = entry.get("state").cloned().unwrap_or_default();
        if let Some(why) = taken_away((&held, &document), (&next, &state)) {
            return Some(why);
        }
        (held, document) = (next, state);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(update_keys: Option<&[&str]>, witness: Option<Value>, document: Value) -> String {
        let mut parameters = json!({});
        if let Some(keys) = update_keys {
            parameters["updateKeys"] = json!(keys);
        }
        if let Some(witness) = witness {
            parameters["witness"] = witness;
        }
        json!({ "versionId": "1-Qm", "parameters": parameters, "state": document }).to_string()
    }

    fn document(agreement: &[&str], recovery: &[&str], uri: &str) -> Value {
        let did = "did:webvh:Qm:id.example:alice";
        let mut methods: Vec<Value> = agreement.iter().map(|key| json!({ "id": format!("{did}#{key}"), "publicKeyMultibase": key })).collect();
        methods.push(json!({ "id": format!("{did}#z6Mkauth"), "publicKeyMultibase": "z6Mkauth" }));
        methods.extend(recovery.iter().map(|key| json!({ "id": format!("{did}#recovery-{key}"), "publicKeyMultibase": key })));
        json!({
            "id": did,
            "verificationMethod": methods,
            "authentication": [format!("{did}#z6Mkauth")],
            "keyAgreement": agreement.iter().map(|key| format!("{did}#{key}")).collect::<Vec<_>>(),
            "service": [{ "id": format!("{did}#didcomm"), "type": "DIDCommMessaging", "serviceEndpoint": { "uri": uri } }],
        })
    }

    #[test]
    fn what_takes_something_away_is_held_back_and_what_only_adds_is_not() {
        let witness = json!({ "threshold": 1, "witnesses": [{ "id": "did:key:z6Mkhost" }] });
        let log = entry(Some(&["z6Mka", "z6Mkb"]), Some(witness.clone()), document(&["z6LSa", "z6LSb"], &["z6Mkr"], "did:peer:2.mediator"));
        let same = |update_keys: Option<&[&str]>, document: Value| entry(update_keys, None, document);

        // A device added, a key trusted, nothing changed: at once.
        assert_eq!(held_back_because(&log, &same(Some(&["z6Mka", "z6Mkb", "z6Mkc"]), document(&["z6LSa", "z6LSb", "z6LSc"], &["z6Mkr"], "did:peer:2.mediator"))), None);
        assert_eq!(held_back_because(&log, &same(None, document(&["z6LSa", "z6LSb"], &["z6Mkr"], "did:peer:2.mediator"))), None);
        // A first publication.
        assert_eq!(held_back_because("", &log), None);

        // A device's key removed, an update key removed, a recovery key changed, mail
        // moved, the witness taken out, the DID ended: held.
        assert!(held_back_because(&log, &same(None, document(&["z6LSa"], &["z6Mkr"], "did:peer:2.mediator"))).is_some());
        assert!(held_back_because(&log, &same(Some(&["z6Mka"]), document(&["z6LSa", "z6LSb"], &["z6Mkr"], "did:peer:2.mediator"))).is_some());
        assert!(held_back_because(&log, &same(None, document(&["z6LSa", "z6LSb"], &["z6Mkr", "z6Mks"], "did:peer:2.mediator"))).is_some());
        assert!(held_back_because(&log, &same(None, document(&["z6LSa", "z6LSb"], &[], "did:peer:2.mediator"))).is_some());
        assert!(held_back_because(&log, &same(None, document(&["z6LSa", "z6LSb"], &["z6Mkr"], "did:peer:2.elsewhere"))).is_some());
        assert!(held_back_because(&log, &entry(None, Some(json!({})), document(&["z6LSa", "z6LSb"], &["z6Mkr"], "did:peer:2.mediator"))).is_some());
        let ended = json!({ "versionId": "2-Qm", "parameters": { "deactivated": true }, "state": document(&["z6LSa", "z6LSb"], &["z6Mkr"], "did:peer:2.mediator") }).to_string();
        assert!(held_back_because(&log, &ended).is_some());

        // Taken away in one entry and given back in the next is still taken away.
        let both = format!("{}\n{}", same(None, document(&["z6LSa"], &["z6Mkr"], "did:peer:2.mediator")), same(None, document(&["z6LSa", "z6LSb"], &["z6Mkr"], "did:peer:2.mediator")));
        assert!(held_back_because(&log, &both).is_some());

        // The same keys named again (a device's, and a recovery key): at once.
        let renamed = |document: Value| {
            let text = document.to_string().replace("#recovery-z6Mkr", "#key-7").replace("#z6LSb", "#key-3");
            serde_json::from_str::<Value>(&text).unwrap()
        };
        assert_eq!(held_back_because(&log, &same(None, renamed(document(&["z6LSa", "z6LSb"], &["z6Mkr"], "did:peer:2.mediator")))), None);

        // Witnessing turned on for a log from before: at once.
        let before = entry(Some(&["z6Mka"]), None, document(&["z6LSa"], &[], "did:peer:2.mediator"));
        assert_eq!(held_back_because(&before, &entry(None, Some(witness.clone()), document(&["z6LSa"], &[], "did:peer:2.mediator"))), None);
        assert_eq!(in_force(&format!("{before}\n{}", entry(None, Some(witness.clone()), document(&[], &[], "x")))).witness, witness);
    }
}
