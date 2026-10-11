//! Keys as a DID document has them, and a signature checked against them.

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::Value;

/// The multibase public keys a document lists under `relationship`
/// (`keyAgreement`, `authentication`): each entry there is a verification method,
/// or the id of one in `verificationMethod`.
pub fn keys_for(document: &Value, relationship: &str) -> Vec<String> {
    let methods = document["verificationMethod"].as_array().cloned().unwrap_or_default();
    let did = document["id"].as_str().unwrap_or_default();
    let multibase = |method: &Value| method["publicKeyMultibase"].as_str().map(str::to_string);
    let mut keys = Vec::new();
    for entry in document[relationship].as_array().into_iter().flatten() {
        let key = match entry {
            Value::String(id) => methods.iter().find(|m| same_id(m["id"].as_str().unwrap_or_default(), id, did)).and_then(multibase),
            method => multibase(method),
        };
        if let Some(key) = key.filter(|key| !keys.contains(key)) {
            keys.push(key);
        }
    }
    keys
}

/// Every key the document has, under any relationship or none.
pub fn all_keys(document: &Value) -> Vec<String> {
    let mut keys: Vec<String> = document["verificationMethod"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["publicKeyMultibase"].as_str().map(str::to_string))
        .collect();
    for relationship in ["authentication", "assertionMethod", "keyAgreement", "capabilityInvocation", "capabilityDelegation"] {
        keys.extend(keys_for(document, relationship));
    }
    keys.sort();
    keys.dedup();
    keys
}

/// `#key-1` and `did:x#key-1` are the same method of `did:x`.
fn same_id(a: &str, b: &str, did: &str) -> bool {
    let full = |id: &str| if id.starts_with('#') { format!("{did}{id}") } else { id.to_string() };
    full(a) == full(b)
}

/// The 32 bytes of an Ed25519 public key written as a multikey (`z6Mk…`).
pub fn ed25519_from_multikey(multikey: &str) -> Option<[u8; 32]> {
    let bytes = bs58::decode(multikey.strip_prefix('z')?).into_vec().ok()?;
    // The multicodec for an Ed25519 public key, then the key.
    let key = bytes.strip_prefix(&[0xed, 0x01])?;
    key.try_into().ok()
}

/// Whether `jws` (`{protected, signature}`) is a detached EdDSA signature over
/// `payload` by one of `keys` (multikeys): the JWS signing input is
/// `protected + "." + base64url(payload)`.
pub fn signed_by_one_of(payload: &[u8], jws: &Value, keys: &[String]) -> bool {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let (Some(protected), Some(signature)) = (jws["protected"].as_str(), jws["signature"].as_str()) else {
        return false;
    };
    let header: Value = match b64.decode(protected).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
        Some(header) => header,
        None => return false,
    };
    if header["alg"] != "EdDSA" {
        return false;
    }
    let Some(signature) = b64.decode(signature).ok().and_then(|bytes| Signature::from_slice(&bytes).ok()) else {
        return false;
    };
    let input = format!("{protected}.{}", b64.encode(payload));
    keys.iter()
        .filter_map(|key| ed25519_from_multikey(key))
        .filter_map(|bytes| VerifyingKey::from_bytes(&bytes).ok())
        .any(|key| key.verify(input.as_bytes(), &signature).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_documents_keys_are_found_by_reference_or_embedded() {
        let document = json!({
            "id": "did:x",
            "verificationMethod": [
                {"id": "did:x#key-1", "publicKeyMultibase": "z6MkA"},
                {"id": "#key-2", "publicKeyMultibase": "z6LSB"},
            ],
            "authentication": ["#key-1"],
            "keyAgreement": ["did:x#key-2", {"id": "did:x#key-3", "publicKeyMultibase": "z6LSC"}],
        });
        assert_eq!(keys_for(&document, "authentication"), ["z6MkA"]);
        assert_eq!(keys_for(&document, "keyAgreement"), ["z6LSB", "z6LSC"]);
        assert_eq!(all_keys(&document), ["z6LSB", "z6LSC", "z6MkA"]);
    }
}
