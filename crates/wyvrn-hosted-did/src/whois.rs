//! A hosted DID's whois: a presentation it signed, and the profile in it.

use affinidi_data_integrity::{DataIntegrityProof, VerifyOptions};
use serde_json::Value;

use crate::keys;

/// The profile's own context, and the type it gives a profile credential.
pub const PROFILE_CONTEXT: &str = "https://wyvrn.app/ns/profile/v1";
pub const PROFILE_CREDENTIAL: &str = "ProfileCredential";

/// Checks a whois against the DID that is said to have published it: the
/// presentation is held by `did`, signed by one of the authentication keys in
/// `document` (the DID's own, already verified), and every credential in it is
/// about `did`. Returns the presentation.
pub fn check(bytes: &[u8], did: &str, document: &Value) -> Result<Value, String> {
    let presentation: Value = serde_json::from_slice(bytes).map_err(|_| "The whois is not JSON.".to_string())?;
    let holder = presentation["holder"].as_str().or(presentation["holder"]["id"].as_str());
    if holder != Some(did) {
        return Err(format!("The presentation's holder is not {did}."));
    }
    // did:webvh: at least one credential in a whois is about the DID. Others may be
    // about something else of the holder's, and are nothing to this reader.
    if !credentials(&presentation).iter().any(|credential| subjects(credential).iter().any(|subject| subject["id"].as_str() == Some(did))) {
        return Err(format!("A whois has to hold a credential about {did}."));
    }

    // A credential the DID issued and signed has to be what it signed. (One it
    // issued unsigned rests on the presentation's own proof, which is the DID's too.)
    for credential in credentials(&presentation) {
        let issuer = credential["issuer"].as_str().or(credential["issuer"]["id"].as_str());
        if issuer == Some(did) && !credential["proof"].is_null() && !signed_by(credential, document, "assertionMethod").unwrap_or(false) {
            return Err("A credential in the whois says the DID issued it, and isn't signed by an assertion key of the DID.".to_string());
        }
    }
    if !signed_by(&presentation, document, "authentication")? {
        return Err("The presentation's proof doesn't verify against an authentication key of the DID.".to_string());
    }
    Ok(presentation)
}

/// Whether `signed` (a presentation or a credential) carries a proof made by a key
/// that `document` lists under `relationship`. `Err` if it carries no proof at all.
pub(crate) fn signed_by(signed: &Value, document: &Value, relationship: &str) -> Result<bool, String> {
    let proofs: Vec<Value> = match &signed["proof"] {
        Value::Array(many) => many.clone(),
        Value::Object(_) => vec![signed["proof"].clone()],
        _ => return Err("It is not signed.".to_string()),
    };
    let mut unsigned = signed.clone();
    if let Some(object) = unsigned.as_object_mut() {
        object.remove("proof");
    }
    let methods = document["verificationMethod"].as_array().cloned().unwrap_or_default();
    let allowed = keys::keys_for(document, relationship);
    for proof in proofs {
        let Ok(proof) = serde_json::from_value::<DataIntegrityProof>(proof) else { continue };
        // A proof is for one purpose, and made for a document of one @context: the
        // signed thing's has to begin with the proof's (eddsa-jcs-2022, verification).
        if proof.proof_purpose != relationship || !begins_with_context(signed, proof.context.as_deref()) {
            continue;
        }
        // The key the proof names, if the document lists it under the relationship.
        let named = proof.verification_method.clone();
        let fragment = named.rsplit('#').next().unwrap_or_default();
        let key = methods
            .iter()
            .find(|method| method["id"].as_str().is_some_and(|id| id == named || id.rsplit('#').next() == Some(fragment)))
            .and_then(|method| method["publicKeyMultibase"].as_str())
            .filter(|key| allowed.iter().any(|a| a == key))
            .and_then(keys::ed25519_from_multikey);
        let Some(key) = key else { continue };
        if proof.verify_with_public_key(&unsigned, &key, VerifyOptions::default()).is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn begins_with_context(signed: &Value, of_proof: Option<&[String]>) -> bool {
    let Some(of_proof) = of_proof else { return true };
    let of_signed: Vec<&str> = match &signed["@context"] {
        Value::String(one) => vec![one.as_str()],
        Value::Array(many) => many.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    of_proof.len() <= of_signed.len() && of_proof.iter().zip(&of_signed).all(|(a, b)| a == b)
}

fn credentials(presentation: &Value) -> Vec<&Value> {
    match &presentation["verifiableCredential"] {
        Value::Array(many) => many.iter().collect(),
        Value::Null => Vec::new(),
        one => vec![one],
    }
}

fn subjects(credential: &Value) -> Vec<&Value> {
    match &credential["credentialSubject"] {
        Value::Array(many) => many.iter().collect(),
        one => vec![one],
    }
}

fn has_type(node: &Value, wanted: &str) -> bool {
    match &node["type"] {
        Value::String(one) => one == wanted,
        Value::Array(many) => many.iter().any(|t| t == wanted),
        _ => false,
    }
}

/// A picture in a profile: schema.org's `ImageObject`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Where it is: a `data:` URL (it is right here), or `https:`.
    pub url: String,
    pub media_type: Option<String>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    /// The hex SHA-256 of the file. An `https:` picture is only to be shown if what
    /// is fetched matches it.
    pub sha256: Option<String>,
}

impl Image {
    fn of(node: &Value) -> Option<Self> {
        let url = node["contentUrl"].as_str()?;
        let inline = url.starts_with("data:");
        let sha256 = node["sha256"].as_str().map(str::to_lowercase).filter(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()));
        // Nothing but a picture that is here, or one elsewhere that can be checked.
        if !(inline || (url.starts_with("https://") && sha256.is_some())) {
            return None;
        }
        Some(Self {
            url: url.to_string(),
            media_type: node["encodingFormat"].as_str().map(str::to_string),
            width: node["width"].as_u64(),
            height: node["height"].as_u64(),
            sha256,
        })
    }

    /// Whether showing it fetches nothing.
    pub fn is_inline(&self) -> bool {
        self.url.starts_with("data:")
    }
}

/// Who a DID says it is. All of it is the DID's own word.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    /// `Person` or `Organization`, if it says.
    pub kind: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    /// The profile picture.
    pub image: Option<Image>,
    /// An organisation's logo, where that differs from its picture.
    pub logo: Option<Image>,
    pub banner: Option<Image>,
    pub background: Option<Image>,
}

/// The profile in a whois that [`check`] has passed for `did`: the credential of
/// the profile type that `did` issued about itself. `None` if there is none.
pub fn profile(presentation: &Value, did: &str) -> Option<Profile> {
    let credential = credentials(presentation).into_iter().find(|credential| {
        let issuer = credential["issuer"].as_str().or(credential["issuer"]["id"].as_str());
        has_type(credential, PROFILE_CREDENTIAL) && issuer == Some(did)
    })?;
    let subject = subjects(credential).into_iter().find(|subject| subject["id"].as_str() == Some(did))?;
    let text = |value: &Value, limit: usize| value.as_str().map(|s| s.trim().chars().take(limit).collect::<String>()).filter(|s| !s.is_empty());
    let kind = ["Person", "Organization"].into_iter().find(|kind| has_type(subject, kind)).map(str::to_string);
    Some(Profile {
        kind,
        name: text(&subject["name"], 80),
        description: text(&subject["description"], 500),
        image: Image::of(&subject["image"]),
        logo: Image::of(&subject["brand"]["logo"]),
        banner: Image::of(&subject["brand"]["image"]),
        background: Image::of(&subject["wyvrn"]["background"]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DID: &str = "did:webvh:Qm:id.example:alice";

    fn whois(subject: Value) -> Value {
        json!({ "holder": DID, "verifiableCredential": [
            { "type": ["VerifiableCredential"], "issuer": "did:web:registrar.example", "credentialSubject": { "id": DID, "name": "Not this one" } },
            { "type": ["VerifiableCredential", "ProfileCredential"], "issuer": DID, "credentialSubject": subject },
        ]})
    }

    #[test]
    fn a_profile_is_read_from_the_credential_the_did_issued_about_itself() {
        let hash = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        let read = profile(
            &whois(json!({
                "id": DID, "type": "Person", "name": "  Alice ", "description": "Writes about gardens.",
                "image": { "type": "ImageObject", "contentUrl": "data:image/webp;base64,AAAA", "encodingFormat": "image/webp", "width": 256, "height": 256 },
                "brand": { "type": "Brand", "image": { "contentUrl": "https://alice.example/banner.webp", "sha256": hash.to_uppercase() } },
                "wyvrn": { "background": { "contentUrl": "https://alice.example/bg.webp", "sha256": hash } },
            })),
            DID,
        )
        .unwrap();
        assert_eq!(read.kind.as_deref(), Some("Person"));
        assert_eq!(read.name.as_deref(), Some("Alice"));
        assert_eq!(read.description.as_deref(), Some("Writes about gardens."));
        let image = read.image.unwrap();
        assert!(image.is_inline());
        assert_eq!((image.width, image.media_type.as_deref()), (Some(256), Some("image/webp")));
        assert_eq!(read.banner.unwrap().sha256.as_deref(), Some(hash));
        assert_eq!(read.background.unwrap().url, "https://alice.example/bg.webp");
        assert_eq!(read.logo, None);
    }

    #[test]
    fn a_picture_elsewhere_is_only_taken_with_a_hash_to_check_it_by() {
        let read = profile(
            &whois(json!({
                "id": DID,
                "image": { "contentUrl": "https://tracker.example/pixel.png" },
                "brand": { "image": { "contentUrl": "http://alice.example/banner.webp", "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08" } },
                "wyvrn": { "background": { "contentUrl": "https://alice.example/bg.webp", "sha256": "not-a-hash" } },
            })),
            DID,
        )
        .unwrap();
        assert_eq!((read.image, read.banner, read.background), (None, None, None));
    }

    #[test]
    fn there_is_no_profile_without_a_credential_of_the_dids_own() {
        assert_eq!(profile(&json!({ "holder": DID, "verifiableCredential": [] }), DID), None);
        // One somebody else issued, even of the profile type, is not the DID's word.
        let others = json!({ "verifiableCredential": [{ "type": ["ProfileCredential"], "issuer": "did:web:other.example", "credentialSubject": { "id": DID, "name": "x" } }] });
        assert_eq!(profile(&others, DID), None);
    }

    /// A whois as an app writes one, against the document it is signed as.
    #[tokio::test]
    async fn a_written_whois_checks_out_and_a_changed_one_does_not() {
        const SECRET: &str = r#"{"kty":"OKP","crv":"Ed25519","d":"nWGxne_9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"}"#;
        let key = crate::write::public_multikey(SECRET).unwrap();
        let method = format!("{DID}#key-1");
        let document = json!({
            "id": DID,
            "verificationMethod": [{ "id": method, "type": "Multikey", "controller": DID, "publicKeyMultibase": key }],
            "authentication": [method],
            "assertionMethod": [method],
        });
        let fields = crate::write::ProfileFields { name: Some("Alice".to_string()), ..Default::default() };
        let written = crate::write::whois(DID, &fields, SECRET, &method, "2026-10-11T12:00:00Z").await.unwrap();

        let checked = check(written.to_string().as_bytes(), DID, &document).unwrap();
        assert_eq!(profile(&checked, DID).unwrap().name.as_deref(), Some("Alice"));
        // Each proof is for its own purpose, and carries the @context of what it signs.
        assert_eq!(written["proof"]["proofPurpose"], "authentication");
        assert_eq!(written["proof"]["@context"], written["@context"]);
        let credential = &written["verifiableCredential"][0];
        assert_eq!(credential["proof"]["proofPurpose"], "assertionMethod");
        assert_eq!(credential["proof"]["@context"], credential["@context"]);

        // A name changed after signing breaks the credential's proof, and the
        // presentation's with it.
        let mut renamed = written.clone();
        renamed["verifiableCredential"][0]["credentialSubject"]["name"] = json!("Mallory");
        assert!(check(renamed.to_string().as_bytes(), DID, &document).is_err());
        // A document that doesn't list the key for the purpose doesn't vouch for it.
        let mut no_assertions = document.clone();
        no_assertions.as_object_mut().unwrap().remove("assertionMethod");
        assert!(check(written.to_string().as_bytes(), DID, &no_assertions).unwrap_err().contains("assertion key"));
        let mut no_authentication = document.clone();
        no_authentication.as_object_mut().unwrap().remove("authentication");
        assert!(check(written.to_string().as_bytes(), DID, &no_authentication).unwrap_err().contains("authentication key"));
        // The presentation's proof is not a credential's: one purpose doesn't stand in for another.
        let mut swapped = written.clone();
        swapped["proof"] = written["verifiableCredential"][0]["proof"].clone();
        assert!(check(swapped.to_string().as_bytes(), DID, &document).is_err());
    }
}
