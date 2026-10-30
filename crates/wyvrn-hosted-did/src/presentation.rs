//! A Verifiable Presentation made for one verifier (linked-accounts/1.0, "Over
//! DIDComm"): proved with Data Integrity, `eddsa-jcs-2022`, purpose
//! `authentication`, carrying the verifier's `challenge` and `domain` -- which
//! affinidi-data-integrity's proof type has no room for, so the cryptosuite's
//! steps are written here: the proof's configuration and the document, each
//! canonicalized (JCS) and hashed (SHA-256), the two hashes signed together.

use ed25519_dalek::{Signer, Verifier};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::keys;

const SUITE: &str = "eddsa-jcs-2022";

fn canonical(value: &Value) -> Result<Vec<u8>, String> {
    serde_json_canonicalizer::to_vec(value).map_err(|e| e.to_string())
}

/// The data a proof signs: the hash of its configuration, then the document's.
fn hash_data(document: &Value, config: &Value) -> Result<Vec<u8>, String> {
    let mut data = Sha256::digest(canonical(config)?).to_vec();
    data.extend_from_slice(&Sha256::digest(canonical(document)?));
    Ok(data)
}

/// `presentation` proved by the Ed25519 key `secret_jwk`, which
/// `verification_method` names in the holder's document under `authentication`,
/// for `challenge` and `domain`, at `created` (RFC 3339).
pub fn sign(presentation: &Value, secret_jwk: &str, verification_method: &str, challenge: &str, domain: &str, created: &str) -> Result<Value, String> {
    let mut document = presentation.clone();
    let object = document.as_object_mut().ok_or("A presentation is an object.")?;
    object.remove("proof");
    let mut config = json!({
        "type": "DataIntegrityProof",
        "cryptosuite": SUITE,
        "created": created,
        "verificationMethod": verification_method,
        "proofPurpose": "authentication",
        "challenge": challenge,
        "domain": domain,
    });
    if let Some(context) = document.get("@context") {
        config["@context"] = context.clone();
    }
    let key = ed25519_dalek::SigningKey::from_bytes(&crate::write::seed_of(secret_jwk)?);
    let signature = key.sign(&hash_data(&document, &config)?);
    config["proofValue"] = json!(format!("z{}", bs58::encode(signature.to_bytes()).into_string()));
    document["proof"] = config;
    Ok(document)
}

/// Checks `signed`'s proof: `eddsa-jcs-2022` for `authentication`, made for
/// `challenge` and `domain`, by a key `document` (the holder's) lists under
/// `authentication`. `Err` says, for a person, what didn't hold.
pub fn check(signed: &Value, document: &Value, challenge: &str, domain: &str) -> Result<(), String> {
    let proof = signed.get("proof").and_then(Value::as_object).ok_or("It is not proved.")?;
    let text = |name: &str| proof.get(name).and_then(Value::as_str);
    if text("type") != Some("DataIntegrityProof") || text("cryptosuite") != Some(SUITE) || text("proofPurpose") != Some("authentication") {
        return Err("Its proof is not one this checks.".to_string());
    }
    if text("challenge") != Some(challenge) || text("domain") != Some(domain) {
        return Err("Its proof was made for someone else, or another request.".to_string());
    }
    let named = text("verificationMethod").ok_or("Its proof names no key.")?;
    let fragment = named.rsplit('#').next().unwrap_or_default();
    let allowed = keys::keys_for(document, "authentication");
    let key = document["verificationMethod"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|method| method["id"].as_str().is_some_and(|id| id == named || id.rsplit('#').next() == Some(fragment)))
        .and_then(|method| method["publicKeyMultibase"].as_str())
        .filter(|key| allowed.iter().any(|a| a == key))
        .and_then(keys::ed25519_from_multikey)
        .ok_or("Its proof is by a key the holder doesn't sign in with.")?;
    let value = text("proofValue").and_then(|v| v.strip_prefix('z')).ok_or("Its proof has no value.")?;
    let signature: [u8; 64] = bs58::decode(value).into_vec().ok().and_then(|bytes| bytes.try_into().ok()).ok_or("Its proof's value isn't a signature.")?;
    let mut config: Map<String, Value> = proof.clone();
    config.remove("proofValue");
    let mut unsigned = signed.clone();
    if let Some(object) = unsigned.as_object_mut() {
        object.remove("proof");
    }
    let key = ed25519_dalek::VerifyingKey::from_bytes(&key).map_err(|e| e.to_string())?;
    key.verify(&hash_data(&unsigned, &Value::Object(config))?, &ed25519_dalek::Signature::from_bytes(&signature)).map_err(|_| "Its proof doesn't check out.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::public_multikey;

    const JWK: &str = r#"{"kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo","d":"nWGxne_9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A"}"#;
    const HOLDER: &str = "did:webvh:Qm:id.example:alice";

    fn holder_document() -> Value {
        json!({ "id": HOLDER, "verificationMethod": [{ "id": format!("{HOLDER}#key-1"), "type": "Multikey", "controller": HOLDER, "publicKeyMultibase": public_multikey(JWK).unwrap() }], "authentication": [format!("{HOLDER}#key-1")] })
    }

    fn presentation() -> Value {
        json!({ "@context": ["https://www.w3.org/ns/credentials/v2"], "type": ["VerifiablePresentation"], "holder": HOLDER, "verifiableCredential": [{ "id": "urn:uuid:1" }] })
    }

    #[test]
    fn a_presentation_proved_for_a_verifier_checks_only_for_it() {
        let signed = sign(&presentation(), JWK, &format!("{HOLDER}#key-1"), "c-1", "did:web:v.example", "2026-10-30T00:00:00Z").unwrap();
        check(&signed, &holder_document(), "c-1", "did:web:v.example").unwrap();
        assert!(check(&signed, &holder_document(), "c-2", "did:web:v.example").is_err());
        assert!(check(&signed, &holder_document(), "c-1", "did:web:other.example").is_err());
        let mut changed = signed.clone();
        changed["holder"] = json!("did:example:mallory");
        assert!(check(&changed, &holder_document(), "c-1", "did:web:v.example").is_err());
        let mut keyless = holder_document();
        keyless["authentication"] = json!([]);
        assert!(check(&signed, &keyless, "c-1", "did:web:v.example").is_err());
    }
}
