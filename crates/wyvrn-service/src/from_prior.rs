//! Who signed a `from_prior`, for a server that follows a member's DID.
//!
//! A `from_prior` (DIDComm v2.1, DID Rotation) is checked by the DIDComm library
//! (`didcomm_core::rotation::verify_from_prior`): signed by a key the prior DID
//! authenticates with, naming its sender as the new DID. It says which DIDs, not
//! which key; a server that has to weigh a move against what it already holds
//! (one DID moves once: security review WYV-27) asks here whether that key is one
//! another DID authenticates with too.

use base64::Engine;
use didcomm_core::resolver::DIDResolver;
use serde_json::Value;

/// The `kid` and `iss` of a `from_prior` JWT (not checked here: verify it first).
fn kid_and_issuer(jwt: &str) -> Option<(String, String)> {
    let mut parts = jwt.split('.');
    let decode = |part: &str| -> Option<Value> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(part.trim_end_matches('=')).ok()?;
        serde_json::from_slice(&bytes).ok()
    };
    let header = decode(parts.next()?)?;
    let payload = decode(parts.next()?)?;
    Some((header["kid"].as_str()?.to_string(), payload["iss"].as_str()?.to_string()))
}

/// A verification method's key, however it is written: its multibase, or its JWK's
/// coordinates.
fn key_of(method: &Value) -> Option<String> {
    if let Some(multibase) = method["publicKeyMultibase"].as_str() {
        return Some(multibase.to_string());
    }
    let jwk = &method["publicKeyJwk"];
    let x = jwk["x"].as_str()?;
    Some(format!("{}:{}:{x}:{}", jwk["kty"].as_str().unwrap_or_default(), jwk["crv"].as_str().unwrap_or_default(), jwk["y"].as_str().unwrap_or_default()))
}

fn fragment(id: &str) -> &str {
    id.rsplit_once('#').map_or(id, |(_, fragment)| fragment)
}

/// The keys `document` lists under `authentication`, by value.
fn authentication_keys(document: &Value) -> Vec<String> {
    let methods = document["verificationMethod"].as_array().cloned().unwrap_or_default();
    document["authentication"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| match entry {
            Value::String(id) => methods.iter().find(|m| m["id"].as_str().is_some_and(|m| fragment(m) == fragment(id))).and_then(key_of),
            method => key_of(method),
        })
        .collect()
}

/// Whether the key that signed `from_prior` (already verified) is one `did`'s
/// document lists under `authentication` too: whoever signed it could move `did`
/// itself. `false` for anything that can't be read.
pub async fn signed_with_a_key_of(resolver: &dyn DIDResolver, from_prior: &str, did: &str) -> bool {
    let Some((kid, issuer)) = kid_and_issuer(from_prior) else { return false };
    let (Ok(prior), Ok(other)) = (resolver.resolve(&issuer).await, resolver.resolve(did).await) else {
        return false;
    };
    let methods = prior["verificationMethod"].as_array().cloned().unwrap_or_default();
    let Some(key) = methods.iter().find(|m| m["id"].as_str().is_some_and(|id| fragment(id) == fragment(&kid))).and_then(key_of) else {
        return false;
    };
    // Only a key the prior DID speaks with: the library checked as much, and a
    // key it lists for nothing else (a recovery key, an untrusted device's) is no
    // device of the account's.
    authentication_keys(&prior).contains(&key) && authentication_keys(&other).contains(&key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Documents(Vec<Value>);

    #[async_trait::async_trait]
    impl DIDResolver for Documents {
        async fn resolve(&self, did: &str) -> Result<Value, didcomm_core::resolver::ResolutionError> {
            self.0.iter().find(|d| d["id"] == did).cloned().ok_or_else(|| didcomm_core::resolver::ResolutionError::NotFound(did.to_string()))
        }
        async fn is_resolvable(&self, _did: &str) -> bool {
            true
        }
    }

    fn document(did: &str, keys: &[&str]) -> Value {
        json!({
            "id": did,
            "verificationMethod": keys.iter().enumerate().map(|(n, key)| json!({ "id": format!("#key-{}", n + 1), "type": "Multikey", "publicKeyMultibase": key })).collect::<Vec<_>>(),
            "authentication": keys.iter().enumerate().map(|(n, _)| format!("#key-{}", n + 1)).collect::<Vec<_>>(),
        })
    }

    fn jwt(kid: &str, iss: &str, sub: &str) -> String {
        let part = |value: Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string());
        format!("{}.{}.c2ln", part(json!({ "alg": "EdDSA", "kid": kid })), part(json!({ "iss": iss, "sub": sub, "iat": 1 })))
    }

    #[tokio::test]
    async fn a_device_of_the_current_did_signed_it_and_a_removed_one_did_not() {
        let resolver = Documents(vec![
            document("did:example:a", &["zPhone", "zRemoved"]),
            document("did:example:b", &["zPhone", "zLaptop"]),
            document("did:example:c", &["zPhone", "zLaptop"]),
        ]);
        assert!(signed_with_a_key_of(&resolver, &jwt("did:example:a#key-1", "did:example:a", "did:example:c"), "did:example:b").await);
        assert!(!signed_with_a_key_of(&resolver, &jwt("did:example:a#key-2", "did:example:a", "did:example:x"), "did:example:b").await);
        // Nothing to read: no.
        assert!(!signed_with_a_key_of(&resolver, "not.a.jwt", "did:example:b").await);
        assert!(!signed_with_a_key_of(&resolver, &jwt("did:example:a#key-1", "did:example:a", "did:example:c"), "did:example:unknown").await);
    }
}
