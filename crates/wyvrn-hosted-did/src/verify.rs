//! Deciding that a DID is what it claims, from what was fetched.

use didwebvh_rs::DIDWebVHState;
use serde_json::Value;

use crate::keys;
use crate::urls::Location;

/// What a `did:webvh` log comes to, once it has verified.
#[derive(Debug, Clone)]
pub struct Verified {
    pub document: Value,
    pub version_id: String,
    pub deactivated: bool,
}

/// Verifies `log` as the history of `did`: its SCID against the first entry, each
/// entry against the one before, every proof against the keys allowed to update at
/// that point. The error says what is wrong.
pub async fn log(did: &str, log: &str) -> Result<Verified, String> {
    let mut state = DIDWebVHState::default();
    let (_, metadata) = state.resolve_log(did, log, None).await.map_err(|e| format!("the log doesn't verify: {e}"))?;
    let last = state.log_entries().last().ok_or("the log is empty")?;
    let document = last.get_did_document().map_err(|e| format!("the log gives no document: {e}"))?;
    if document["id"].as_str() != Some(did) {
        return Err(format!("the log is not {did}'s"));
    }
    Ok(Verified { document, version_id: metadata.version_id, deactivated: metadata.deactivated })
}

/// The DIDs a document says are also its subject.
pub fn also_known_as(document: &Value) -> Vec<&str> {
    document["alsoKnownAs"].as_array().into_iter().flatten().filter_map(Value::as_str).collect()
}

/// The `did:webvh` a `did:web` document says it is another name of. One at its
/// own host and path, if it names one: that is the `did:webvh` it is the shadow
/// of. Failing that, one anywhere: a `did:web` that stands at its own domain (a
/// service) and holds an address at a DID host says so this way. Either is only
/// the `did:web`'s word until the `did:webvh` [names it back](names_back).
pub fn webvh_behind<'a>(did_web: &str, web_document: &'a Value) -> Option<&'a str> {
    let here = Location::of(did_web)?;
    let named: Vec<(&str, Location)> = also_known_as(web_document)
        .into_iter()
        .filter(|did| did.starts_with("did:webvh:"))
        .filter_map(|did| Location::of(did).map(|there| (did, there)))
        .collect();
    named.iter().find(|(_, there)| there.same_place(&here)).or(named.first()).map(|(did, _)| *did)
}

/// Whether a `did:webvh` document names `did_web` back: without that, anyone's
/// `did:web` could claim to be the short name of someone else's `did:webvh`.
pub fn names_back(webvh_document: &Value, did_web: &str) -> bool {
    also_known_as(webvh_document).contains(&did_web)
}

/// Whether a `did:peer:4` (by its own document, which it carries) is the party a
/// hosted DID belongs to: it names the hosted DID's `did:web`, and shares a
/// key-agreement key with the hosted document. The first is its word; the second
/// is what makes that word the holder's, since only the holder's log could have
/// put that key in the hosted document.
pub fn peer_is_holder(peer_document: &Value, did_web: &str, hosted_document: &Value) -> bool {
    if !also_known_as(peer_document).contains(&did_web) {
        return false;
    }
    let hosted = keys::keys_for(hosted_document, "keyAgreement");
    keys::keys_for(peer_document, "keyAgreement").iter().any(|key| hosted.contains(key))
}

/// The document a long-form `did:peer:4` carries in itself, if it is one and its
/// hash is that document's: `did:peer:4<hash>:<document>`, where the document is
/// multibase (base58btc) of the JSON multicodec and the JSON, and the hash is the
/// multibase of its SHA-256 multihash. Ids in it are relative (`#key-2`).
pub fn peer4_document(did: &str) -> Option<Value> {
    use sha2::{Digest, Sha256};
    let (hash, encoded) = did.strip_prefix("did:peer:4")?.split_once(':')?;
    let mut multihash = vec![0x12, 0x20];
    multihash.extend_from_slice(&Sha256::digest(encoded.as_bytes()));
    if hash.strip_prefix('z')? != bs58::encode(multihash).into_string() {
        return None;
    }
    let bytes = bs58::decode(encoded.strip_prefix('z')?).into_vec().ok()?;
    // 0x0200, the multicodec for JSON, as a varint.
    serde_json::from_slice(bytes.strip_prefix(&[0x80, 0x04])?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_did_web_points_at_the_did_webvh_in_its_own_place_before_one_elsewhere() {
        let document = json!({ "alsoKnownAs": ["did:peer:4zQm", "did:webvh:QmOther:elsewhere.example:alice", "did:webvh:QmScid:id.example:alice"] });
        assert_eq!(webvh_behind("did:web:id.example:alice", &document), Some("did:webvh:QmScid:id.example:alice"));
        // Named from somewhere else, it is still the one named: a service at its own
        // domain, holding an address here. (Whether that is so is the did:webvh's to say.)
        assert_eq!(webvh_behind("did:web:news.example", &document), Some("did:webvh:QmOther:elsewhere.example:alice"));
        assert_eq!(webvh_behind("did:web:id.example:alice", &json!({})), None);
    }

    #[test]
    fn a_peer_did_is_the_holder_when_it_says_so_and_shares_a_key() {
        let hosted = json!({ "id": "did:webvh:Qm:id.example:alice", "verificationMethod": [{ "id": "#k", "publicKeyMultibase": "z6LSalice" }], "keyAgreement": ["#k"] });
        let peer = |aka: &[&str], key: &str| json!({ "id": "did:peer:4x", "alsoKnownAs": aka, "verificationMethod": [{ "id": "#k", "publicKeyMultibase": key }], "keyAgreement": ["#k"] });
        let web = "did:web:id.example:alice";
        assert!(peer_is_holder(&peer(&[web], "z6LSalice"), web, &hosted));
        // Saying so with someone else's key, or having the key and not saying so.
        assert!(!peer_is_holder(&peer(&[web], "z6LSmallory"), web, &hosted));
        assert!(!peer_is_holder(&peer(&[], "z6LSalice"), web, &hosted));
    }
}
