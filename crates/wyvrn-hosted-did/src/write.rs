//! The holder's side: writing a `did:webvh` log, and signing what is uploaded.
//!
//! Keys come in as the JWKs an app keeps them as (`{"kty":"OKP","crv":"Ed25519",
//! "x":…,"d":…}`), and nothing here keeps one.

use std::sync::Arc;

use affinidi_data_integrity::{DataIntegrityProof, SignOptions};
use affinidi_secrets_resolver::secrets::Secret;
use base64::Engine;
use didwebvh_rs::witness::{Witness, Witnesses};
use didwebvh_rs::{log_entry::LogEntryMethods, parameters::Parameters, DIDWebVHError, DIDWebVHState, Multibase};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};

use crate::whois::{PROFILE_CONTEXT, PROFILE_CREDENTIAL};

/// What a log now is, after an entry was written.
#[derive(Debug, Clone)]
pub struct Written {
    /// `did:webvh:<SCID>:<host>:<name>`.
    pub did: String,
    /// The entry just written, as one line of `did.jsonl`: what `publish` carries.
    pub entry: String,
    /// The whole log, that entry included.
    pub log: String,
    pub version_id: String,
}

pub(crate) fn seed_of(secret_jwk: &str) -> Result<[u8; 32], String> {
    let jwk: Value = serde_json::from_str(secret_jwk).map_err(|_| "the key is not a JWK".to_string())?;
    if jwk["kty"] != "OKP" || jwk["crv"] != "Ed25519" {
        return Err("the key is not an Ed25519 key".to_string());
    }
    let d = jwk["d"].as_str().ok_or("the key has no private part")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(d.trim_end_matches('=')).map_err(|_| "the key's private part is not base64url".to_string())?;
    bytes.try_into().map_err(|_| "the key's private part is not 32 bytes".to_string())
}

/// The key as the log library signs with it, under the id it expects of an update
/// key (`did:key:<multikey>#<multikey>`) unless `kid` says otherwise.
pub fn signing_secret(secret_jwk: &str, kid: Option<&str>) -> Result<Secret, String> {
    let seed = seed_of(secret_jwk)?;
    let mut secret = Secret::generate_ed25519(None, Some(&seed));
    let public = secret.get_public_keymultibase().map_err(|e| e.to_string())?;
    secret.id = match kid {
        Some(kid) => kid.to_string(),
        None => format!("did:key:{public}#{public}"),
    };
    Ok(secret)
}

/// The public half of an Ed25519 key, as a multikey (`z6Mk…`): what goes in a
/// log's `updateKeys`, and in a document.
pub fn public_multikey(secret_jwk: &str) -> Result<String, String> {
    signing_secret(secret_jwk, None)?.get_public_keymultibase().map_err(|e| e.to_string())
}

fn lines(state: &DIDWebVHState) -> Result<Vec<String>, String> {
    state.log_entries().iter().map(|entry| serde_json::to_string(&entry.log_entry).map_err(|e| e.to_string())).collect()
}

fn written(state: &DIDWebVHState) -> Result<Written, String> {
    let all = lines(state)?;
    let last = state.log_entries().last().ok_or("the log is empty")?;
    let did = last.get_did_document().map_err(|e| e.to_string())?["id"].as_str().unwrap_or_default().to_string();
    Ok(Written { did, entry: all.last().cloned().unwrap_or_default(), log: all.join("\n"), version_id: last.get_version_id().to_string() })
}

/// Writes a DID's first entry. `document` is the DID document with `{SCID}` where
/// the log's hash will go (`did:webvh:{SCID}:id.example:alice`). `update_keys` are
/// the multikeys that may sign the next entry -- one for each of the holder's
/// trusted devices -- and `update_secret_jwk`, which signs this one, has to be the
/// secret of one of them. The DID is `portable`: it can later move to another host.
pub async fn create(document: &Value, update_secret_jwk: &str, update_keys: &[String]) -> Result<Written, String> {
    create_with(document, update_secret_jwk, update_keys, &Options::default()).await
}

/// What else an entry may say of the log, beside its document and who may update
/// it next.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct Options {
    /// The `did:key` of the host's witness, when the host witnesses the logs it
    /// hosts (did-hosting/1.0, "The host as a witness", its `reservation`'s
    /// `witness`): the log names it as its only witness, threshold 1. Absent on
    /// an append, the log's witnessing stays as it is.
    pub witness: Option<String>,
    /// `ttl`, in seconds: how long a resolver may use what it read. An address that
    /// is spoken as SHOULD set 600 (did-hosting/1.0, "Speaking as a hosted DID").
    pub ttl: Option<u32>,
}

/// `{ "threshold": 1, "witnesses": [{ "id": witness }] }`.
pub fn witness_parameter(witness: &str) -> Witnesses {
    Witnesses::Value { threshold: 1, witnesses: vec![Witness::new(witness)] }
}

/// [`create`], saying what `options` say from the first entry.
pub async fn create_with(document: &Value, update_secret_jwk: &str, update_keys: &[String], options: &Options) -> Result<Written, String> {
    let signer = signing_secret(update_secret_jwk, None)?;
    let parameters = Parameters {
        update_keys: Some(Arc::new(update_keys.iter().cloned().map(Multibase::new).collect())),
        portable: Some(true),
        witness: options.witness.as_deref().map(|witness| Arc::new(witness_parameter(witness))),
        ttl: options.ttl,
        ..Default::default()
    };
    let mut state = DIDWebVHState::default();
    state.create_log_entry(None, document, &parameters, &signer).await.map_err(|e| e.to_string())?;
    // The document as the log has it, with the hash in place.
    let scid = state.log_entries()[0].log_entry.get_scid().ok_or("the first entry has no SCID")?.to_string();
    let mut made = written(&state)?;
    if made.did.contains("{SCID}") {
        made.did = made.did.replace("{SCID}", &scid);
    }
    Ok(made)
}

/// The log, loaded and verified, ready to be continued.
///
/// Verified but for its witness proofs: it is the holder's own, which the holder
/// wrote, and continuing it needs nothing a witness says. (Those are for whoever
/// reads it from its host, who has the host's word for it and nothing else.)
async fn load(did: &str, log: &str) -> Result<DIDWebVHState, String> {
    let mut state = DIDWebVHState::default();
    let verified = state.resolve_log(did, log, None).await.map(|_| ());
    // Every entry verified before the witnesses' proofs were looked at.
    let entries = log.lines().filter(|line| !line.trim().is_empty()).count();
    match verified {
        Ok(()) => Ok(state),
        Err(DIDWebVHError::WitnessProofError(_)) if state.log_entries().len() == entries => Ok(state),
        Err(e) => Err(format!("the log doesn't verify: {e}")),
    }
}

/// Appends an entry giving the DID a new document, signed by one of the keys the
/// log currently lets update it. `update_keys`, if given, replaces that set from
/// the next entry on: how a device is added to, or taken out of, those that can.
pub async fn append(did: &str, log: &str, document: &Value, update_secret_jwk: &str, update_keys: Option<&[String]>) -> Result<Written, String> {
    append_with(did, log, document, update_secret_jwk, update_keys, &Options::default()).await
}

/// [`append`], with what `options` say from this entry on: a witness (turning
/// witnessing on for a log published before its host witnessed), a `ttl`.
pub async fn append_with(did: &str, log: &str, document: &Value, update_secret_jwk: &str, update_keys: Option<&[String]>, options: &Options) -> Result<Written, String> {
    let signer = signing_secret(update_secret_jwk, None)?;
    let mut state = load(did, log).await?;
    let mut parameters = state.log_entries().last().ok_or("the log is empty")?.validated_parameters.clone();
    if let Some(keys) = update_keys {
        parameters.update_keys = Some(Arc::new(keys.iter().cloned().map(Multibase::new).collect()));
    }
    if let Some(witness) = &options.witness {
        parameters.witness = Some(Arc::new(witness_parameter(witness)));
    }
    if let Some(ttl) = options.ttl {
        parameters.ttl = Some(ttl);
    }
    state.create_log_entry(None, document, &parameters, &signer).await.map_err(|e| e.to_string())?;
    written(&state)
}

/// The `did:key` a witness signs as, for the key `witness_secret_jwk` (Ed25519):
/// what a log's `witness` parameter names.
pub fn witness_did(witness_secret_jwk: &str) -> Result<String, String> {
    Ok(format!("did:key:{}", public_multikey(witness_secret_jwk)?))
}

/// A witness's proof that it has seen the entry `version_id` (did:webvh 1.0, "The
/// Witness Proofs File"): `eddsa-jcs-2022` over `{ "versionId": version_id }`,
/// `proofPurpose` `assertionMethod`, by `did:key:<key>#<key>`. A proof for an
/// entry stands for every entry before it too.
pub async fn witness_proof(version_id: &str, witness_secret_jwk: &str) -> Result<Value, String> {
    let signer = signing_secret(witness_secret_jwk, None)?;
    let options = SignOptions::new().with_proof_purpose("assertionMethod");
    let proof = DataIntegrityProof::sign(&json!({ "versionId": version_id }), &signer, options).await.map_err(|e| e.to_string())?;
    serde_json::to_value(proof).map_err(|e| e.to_string())
}

/// `did-witness.json` holding `proofs` (`(versionId, proof)`, oldest first), one
/// witness's each: `[{ "versionId", "proof": [proof] }]`.
pub fn witness_file(proofs: &[(String, Value)]) -> String {
    Value::Array(proofs.iter().map(|(version_id, proof)| json!({ "versionId": version_id, "proof": [proof] })).collect()).to_string()
}

/// Appends the entry that ends the DID.
pub async fn deactivate(did: &str, log: &str, update_secret_jwk: &str) -> Result<Written, String> {
    let signer = signing_secret(update_secret_jwk, None)?;
    let mut state = load(did, log).await?;
    state.deactivate(&signer).await.map_err(|e| e.to_string())?;
    written(&state)
}

/// The `jws` of a signed attachment: a detached EdDSA signature over the
/// attachment's bytes, by an authentication key of the DID it is uploaded for.
pub fn sign_attachment(bytes: &[u8], authentication_secret_jwk: &str) -> Result<Value, String> {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let protected = b64.encode(json!({ "alg": "EdDSA" }).to_string());
    let signature = SigningKey::from_bytes(&seed_of(authentication_secret_jwk)?).sign(format!("{protected}.{}", b64.encode(bytes)).as_bytes());
    Ok(json!({ "protected": protected, "signature": b64.encode(signature.to_bytes()) }))
}

/// `payload` signed with the authentication key under `header` (which must say
/// `"alg": "EdDSA"`), as a compact JWS: `header.payload.signature`. For a JWT
/// whose header says more than [`sign_attachment`]'s does: an SD-JWT's `typ`
/// and `kid`, say.
pub fn sign_compact(header: &Value, payload: &[u8], authentication_secret_jwk: &str) -> Result<String, String> {
    if header["alg"] != "EdDSA" {
        return Err("the header must say alg EdDSA".to_string());
    }
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let input = format!("{}.{}", b64.encode(header.to_string()), b64.encode(payload));
    let signature = SigningKey::from_bytes(&seed_of(authentication_secret_jwk)?).sign(input.as_bytes());
    Ok(format!("{input}.{}", b64.encode(signature.to_bytes())))
}

/// What goes into a profile: all of it optional.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProfileFields {
    /// `Person` or `Organization`.
    pub kind: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Each an `ImageObject` as the profile credential has it (`contentUrl`,
    /// `encodingFormat`, `width`, `height`, `sha256`).
    pub image: Option<Value>,
    pub logo: Option<Value>,
    pub banner: Option<Value>,
    pub background: Option<Value>,
    /// Credentials others issued about this DID, carried as they are beside the
    /// profile: linked accounts (linked-accounts/1.0). Any not about it are left out.
    #[serde(default)]
    pub credentials: Vec<Value>,
}

fn image_object(image: &Value) -> Value {
    let mut object = image.clone();
    object["type"] = json!("ImageObject");
    object
}

/// A whois for `did` saying who it is: a presentation holding one profile
/// credential it issues about itself. Both are signed with
/// `authentication_secret_jwk`, which `verification_method` (`<did>#key-1`) names
/// in its document for authentication (the presentation) and for assertions (the
/// credential).
pub async fn whois(did: &str, profile: &ProfileFields, authentication_secret_jwk: &str, verification_method: &str, valid_from: &str) -> Result<Value, String> {
    let contexts = json!(["https://www.w3.org/ns/credentials/v2", PROFILE_CONTEXT]);
    let mut subject = json!({ "id": did });
    if let Some(kind) = profile.kind.as_deref().filter(|kind| matches!(*kind, "Person" | "Organization")) {
        subject["type"] = json!(kind);
    }
    if let Some(name) = &profile.name {
        subject["name"] = json!(name);
    }
    if let Some(description) = &profile.description {
        subject["description"] = json!(description);
    }
    if let Some(image) = &profile.image {
        subject["image"] = image_object(image);
    }
    let mut brand = json!({ "type": "Brand" });
    if let Some(logo) = &profile.logo {
        brand["logo"] = image_object(logo);
    }
    if let Some(banner) = &profile.banner {
        brand["image"] = image_object(banner);
    }
    if brand.as_object().is_some_and(|fields| fields.len() > 1) {
        subject["brand"] = brand;
    }
    if let Some(background) = &profile.background {
        subject["wyvrn"] = json!({ "background": image_object(background) });
    }
    let signer = signing_secret(authentication_secret_jwk, Some(verification_method))?;
    let proof_contexts = || vec!["https://www.w3.org/ns/credentials/v2".to_string(), PROFILE_CONTEXT.to_string()];
    // The credential is the DID's own assertion, and signed as one: a presentation
    // holds verifiable credentials, each good by itself.
    let mut credential = json!({
        "@context": contexts,
        "type": ["VerifiableCredential", PROFILE_CREDENTIAL],
        "issuer": did,
        "validFrom": valid_from,
        "credentialSubject": subject,
    });
    let asserted = SignOptions::new().with_proof_purpose("assertionMethod").with_context(proof_contexts());
    let proof = DataIntegrityProof::sign(&credential, &signer, asserted).await.map_err(|e| e.to_string())?;
    credential["proof"] = serde_json::to_value(proof).map_err(|e| e.to_string())?;
    let mut presentation = json!({
        "@context": contexts,
        "type": ["VerifiablePresentation"],
        "holder": did,
        "verifiableCredential": std::iter::once(credential).chain(profile.credentials.iter().filter(|c| c["credentialSubject"]["id"].as_str() == Some(did)).cloned()).collect::<Vec<_>>(),
    });
    // eddsa-jcs-2022 has the proof carry the @context of what it signs.
    let options = SignOptions::new().with_proof_purpose("authentication").with_context(proof_contexts());
    let proof = DataIntegrityProof::sign(&presentation, &signer, options).await.map_err(|e| e.to_string())?;
    presentation["proof"] = serde_json::to_value(proof).map_err(|e| e.to_string())?;
    Ok(presentation)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = r#"{"kty":"OKP","crv":"Ed25519","d":"nWGxne_9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"}"#;

    #[test]
    fn a_compact_jws_carries_its_header_and_verifies_against_the_key() {
        let header = json!({ "alg": "EdDSA", "typ": "dc+sd-jwt", "kid": "did:x#key-1" });
        let jws = sign_compact(&header, br#"{"did":"did:x"}"#, SECRET).unwrap();
        let parts: Vec<&str> = jws.split('.').collect();
        assert_eq!(parts.len(), 3);
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        assert_eq!(serde_json::from_slice::<Value>(&b64.decode(parts[0]).unwrap()).unwrap(), header);
        let key = public_multikey(SECRET).unwrap();
        let detached = json!({ "protected": parts[0], "signature": parts[2] });
        assert_eq!(crate::keys::signer_among(br#"{"did":"did:x"}"#, &detached, std::slice::from_ref(&key)), Some(&key));
        assert!(sign_compact(&json!({ "alg": "ES256" }), b"x", SECRET).is_err());
    }
}
