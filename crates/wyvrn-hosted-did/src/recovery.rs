//! A recovery key's or passkey's co-signature of a hosted DID's log entry
//! (did-hosting/1.0, "The host as a witness"): what lets a host that holds back an
//! entry taking something away witness it at once.
//!
//! `recovery_proof` is `{ key, purpose, signature, authenticator_data?,
//! client_data_json? }`, as multi-device/1.0 has it (the byte strings base64url),
//! `purpose` [`ADDRESS_ENTRY`], and what is signed the entry's `versionId`: for a
//! recovery key (`z6Mk…`) its UTF-8 bytes, signed with Ed25519; for a passkey
//! (`zDn…`) a WebAuthn assertion whose `challenge` is those bytes (base64url in
//! the client data, as WebAuthn writes a challenge).
//!
//! `key` names a recovery key the way multi-device/1.0's "Recovery keys in the
//! document" lists one: a `verificationMethod` entry that no verification
//! relationship names, by its fragment.

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey, Verifier as _};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::keys::ed25519_from_multikey;

/// `recovery_proof.purpose` for a hosted DID's log entry.
pub const ADDRESS_ENTRY: &str = "wyvrn-address-entry/1";

const RELATIONSHIPS: [&str; 5] = ["authentication", "assertionMethod", "keyAgreement", "capabilityInvocation", "capabilityDelegation"];

fn fragment(id: &str) -> &str {
    id.rsplit_once('#').map_or("", |(_, fragment)| fragment)
}

/// The recovery keys and passkeys a document lists: `(fragment, publicKeyMultibase)`
/// of each `verificationMethod` entry that no verification relationship names. The
/// first method of a fragment counts. A device's credential key (`#cred-…`, as
/// did:peer:4 names them: website-sign-in/1.0) is never one: an address's document
/// from before 0.0.28 listed those under no relationship, having no
/// `assertionMethod`, and so they were taken for recovery keys -- able to co-sign a
/// held-back entry, and seen "removed" when the next entry listed them where they
/// belong (found 2026-11-26).
pub fn recovery_keys_listed_in(document: &Value) -> Vec<(String, String)> {
    let mut named = std::collections::BTreeSet::new();
    for relationship in RELATIONSHIPS {
        for entry in document[relationship].as_array().into_iter().flatten() {
            if let Some(id) = entry.as_str().or_else(|| entry["id"].as_str()) {
                named.insert(fragment(id).to_string());
            }
        }
    }
    let mut listed: Vec<(String, String)> = Vec::new();
    for method in document["verificationMethod"].as_array().into_iter().flatten() {
        let id = fragment(method["id"].as_str().unwrap_or_default());
        let Some(public_key) = method["publicKeyMultibase"].as_str() else { continue };
        if id.is_empty() || id.starts_with("cred-") || named.contains(id) || listed.iter().any(|(held, _)| held == id) {
            continue;
        }
        listed.push((id.to_string(), public_key.to_string()));
    }
    listed
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

fn decode(text: &str) -> Option<Vec<u8>> {
    b64().decode(text.trim_end_matches('=')).ok()
}

/// A recovery key's co-signature of the entry `version_id`: the `recovery_proof`
/// a `publish` or `cosign` carries. `key` is the recovery key's id (the fragment
/// its method has in the document, `recovery-1a2b3c4d`), and `recovery_secret_jwk`
/// its private half as an Ed25519 JWK.
pub fn sign_address_entry(version_id: &str, recovery_secret_jwk: &str, key: &str) -> Result<Value, String> {
    let signing = SigningKey::from_bytes(&crate::write::seed_of(recovery_secret_jwk)?);
    let signature = signing.sign(version_id.as_bytes());
    Ok(json!({ "key": key, "purpose": ADDRESS_ENTRY, "signature": b64().encode(signature.to_bytes()) }))
}

/// Checks `proof` as a co-signature of the entry `version_id` by a recovery key or
/// passkey `document` lists. Returns the key's id. The error says what is wrong.
pub fn check_address_entry(proof: &Value, document: &Value, version_id: &str) -> Result<String, String> {
    if proof["purpose"] != ADDRESS_ENTRY {
        return Err(format!("the recovery proof's purpose is not {ADDRESS_ENTRY}"));
    }
    let key = proof["key"].as_str().ok_or("the recovery proof names no key")?;
    let wanted = key.rsplit_once('#').map_or(key, |(_, fragment)| fragment);
    let listed = recovery_keys_listed_in(document);
    let Some((id, public_key)) = listed.iter().find(|(id, _)| id == wanted) else {
        return Err(format!("the document doesn't list a recovery key {wanted}"));
    };
    let signature = proof["signature"].as_str().and_then(decode).ok_or("the recovery proof's signature is not base64url")?;
    let verified = if public_key.starts_with("z6Mk") {
        verifies_recovery_key(public_key, version_id, &signature)
    } else if public_key.starts_with("zDn") {
        verifies_passkey(public_key, version_id, proof, &signature)
    } else {
        false
    };
    if verified {
        Ok(id.clone())
    } else {
        Err("the recovery proof doesn't verify".to_string())
    }
}

fn verifies_recovery_key(public_key: &str, version_id: &str, signature: &[u8]) -> bool {
    let (Some(key), Ok(signature)) = (ed25519_from_multikey(public_key), ed25519_dalek::Signature::from_slice(signature)) else { return false };
    ed25519_dalek::VerifyingKey::from_bytes(&key).is_ok_and(|key| key.verify(version_id.as_bytes(), &signature).is_ok())
}

/// The SEC1 point of a P-256 public key written as a multikey (`zDn…`).
fn p256_from_multikey(multikey: &str) -> Option<Vec<u8>> {
    let bytes = bs58::decode(multikey.strip_prefix('z')?).into_vec().ok()?;
    // The multicodec for a P-256 public key (0x1200, as a varint), then the point.
    bytes.strip_prefix(&[0x80, 0x24]).map(<[u8]>::to_vec)
}

/// WebAuthn's verification of an assertion, as multi-device/1.0 has a recovery
/// proof checked: `type` `webauthn.get`, the challenge, user present and verified,
/// and the ES256 signature over the authenticator data and the client data's hash.
/// Not the origin, which differs from one of the account's apps to another; and
/// not the RP ID, which the document doesn't carry.
fn verifies_passkey(public_key: &str, version_id: &str, proof: &Value, signature: &[u8]) -> bool {
    use p256::ecdsa::signature::Verifier;
    let (Some(authenticator_data), Some(client_data_json)) =
        (proof["authenticator_data"].as_str().and_then(decode), proof["client_data_json"].as_str().and_then(decode))
    else {
        return false;
    };
    if authenticator_data.len() < 37 {
        return false;
    }
    let Ok(client_data) = serde_json::from_slice::<Value>(&client_data_json) else { return false };
    if client_data["type"] != "webauthn.get" || client_data["challenge"] != b64().encode(version_id.as_bytes()).as_str() {
        return false;
    }
    let flags = authenticator_data[32];
    if flags & 0x01 == 0 || flags & 0x04 == 0 {
        return false;
    }
    let (Some(point), Ok(signature)) = (p256_from_multikey(public_key), p256::ecdsa::Signature::from_der(signature)) else { return false };
    let Ok(key) = p256::ecdsa::VerifyingKey::from_sec1_bytes(&point) else { return false };
    let signature = signature.normalize_s().unwrap_or(signature);
    let mut signed = authenticator_data;
    signed.extend_from_slice(&Sha256::digest(&client_data_json));
    key.verify(&signed, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECOVERY: &str = r#"{"kty":"OKP","crv":"Ed25519","d":"nWGxne_9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"}"#;

    fn document(recovery: &[(&str, &str)]) -> Value {
        let did = "did:webvh:Qm:id.example:alice";
        let mut methods = vec![json!({ "id": format!("{did}#key-1"), "type": "Multikey", "publicKeyMultibase": "z6MkDevice" })];
        methods.extend(recovery.iter().map(|(id, key)| json!({ "id": format!("{did}#{id}"), "type": "Multikey", "controller": did, "publicKeyMultibase": key })));
        json!({ "id": did, "verificationMethod": methods, "authentication": [format!("{did}#key-1")] })
    }

    #[test]
    fn a_recovery_keys_co_signature_is_checked_against_the_documents_recovery_keys() {
        let public = crate::write::public_multikey(RECOVERY).unwrap();
        let listed = document(&[("recovery-1a2b", &public)]);
        let proof = sign_address_entry("3-QmEntry", RECOVERY, "recovery-1a2b").unwrap();
        assert_eq!(check_address_entry(&proof, &listed, "3-QmEntry").unwrap(), "recovery-1a2b");
        // Named by its whole id, as well.
        let mut whole = proof.clone();
        whole["key"] = json!("did:webvh:Qm:id.example:alice#recovery-1a2b");
        assert!(check_address_entry(&whole, &listed, "3-QmEntry").is_ok());
        // Another entry; a document that doesn't list the key; a key a relationship names.
        assert!(check_address_entry(&proof, &listed, "3-QmOther").is_err());
        assert!(check_address_entry(&proof, &document(&[]), "3-QmEntry").is_err());
        let mut named = listed.clone();
        named["authentication"] = json!(["did:webvh:Qm:id.example:alice#key-1", "#recovery-1a2b"]);
        assert!(check_address_entry(&proof, &named, "3-QmEntry").is_err());
        // Another purpose.
        let mut other = proof.clone();
        other["purpose"] = json!("wyvrn-recovery-keys/1");
        assert!(check_address_entry(&other, &listed, "3-QmEntry").is_err());
    }

    #[test]
    fn a_passkeys_assertion_over_the_version_id_is_taken() {
        use p256::ecdsa::{signature::Signer as _, SigningKey as P256Key};
        let key = P256Key::from_slice(&[7u8; 32]).unwrap();
        let point = key.verifying_key().to_encoded_point(true);
        let mut multikey = vec![0x80, 0x24];
        multikey.extend_from_slice(point.as_bytes());
        let public = format!("z{}", bs58::encode(multikey).into_string());
        let listed = document(&[("passkey-1", &public)]);
        let assertion = |challenge: &str, flags: u8| {
            let client_data = json!({ "type": "webauthn.get", "challenge": b64().encode(challenge.as_bytes()), "origin": "https://app.example" }).to_string();
            let mut authenticator_data = Sha256::digest(b"wyvrn.app").to_vec();
            authenticator_data.push(flags);
            authenticator_data.extend_from_slice(&[0, 0, 0, 1]);
            let mut signed = authenticator_data.clone();
            signed.extend_from_slice(&Sha256::digest(client_data.as_bytes()));
            let signature: p256::ecdsa::Signature = key.sign(&signed);
            json!({
                "key": "passkey-1", "purpose": ADDRESS_ENTRY,
                "signature": b64().encode(signature.to_der().as_bytes()),
                "authenticator_data": b64().encode(&authenticator_data),
                "client_data_json": b64().encode(client_data.as_bytes()),
            })
        };
        assert_eq!(check_address_entry(&assertion("2-QmEntry", 0x05), &listed, "2-QmEntry").unwrap(), "passkey-1");
        // Over another entry; without the user verified.
        assert!(check_address_entry(&assertion("2-QmOther", 0x05), &listed, "2-QmEntry").is_err());
        assert!(check_address_entry(&assertion("2-QmEntry", 0x01), &listed, "2-QmEntry").is_err());
    }
}
