//! A linked domain: a DID saying a website is its holder's, and the website
//! saying so back. This is DIF's [Well Known DID Configuration]: the DID's
//! document has a `LinkedDomains` service naming an origin, and that origin serves
//! `/.well-known/did-configuration.json`, holding a credential the DID issued
//! about itself and that origin.
//!
//! Neither half means anything alone. A document can name any domain, and a
//! domain can serve a file naming any DID: only someone who controls both the
//! DID's keys and the domain's web server can make the two agree.
//!
//! [Well Known DID Configuration]: https://identity.foundation/well-known-did-configuration/

use affinidi_data_integrity::{DataIntegrityProof, SignOptions};
use chrono::DateTime;
use serde_json::{json, Value};

use crate::whois::signed_by;
use crate::write::signing_secret;

pub const CONFIGURATION_CONTEXT: &str = "https://identity.foundation/.well-known/did-configuration/v1";
/// Where on a linked origin the file is.
pub const WELL_KNOWN_PATH: &str = "/.well-known/did-configuration.json";

/// A website as an origin: `https://alice.example`, from that, from
/// `alice.example`, or from either with a trailing slash. `None` for anything
/// that isn't a bare HTTPS origin: a path, a query, a login, plain `http`.
pub fn origin_of(text: &str) -> Option<String> {
    let text = text.trim();
    let host = match text.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => rest,
        Some(_) => return None,
        None => text,
    };
    let host = host.strip_suffix('/').unwrap_or(host).to_ascii_lowercase();
    let (name, port) = match host.split_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (host.as_str(), None),
    };
    let label = |label: &str| !label.is_empty() && !label.starts_with('-') && !label.ends_with('-') && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if name.is_empty() || name.len() > 253 || !name.split('.').all(label) {
        return None;
    }
    match port {
        // The port HTTPS has anyway is not part of an origin.
        Some("443") => Some(format!("https://{name}")),
        Some(port) if !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()) => Some(format!("https://{name}:{port}")),
        Some(_) => None,
        None => Some(format!("https://{name}")),
    }
}

/// The origins a document says are its holder's: its `LinkedDomains` services, in
/// either form the specification gives (`"https://a.example"`, or
/// `{"origins": [...]}`). Claims only: see [`check`].
pub fn linked_origins(document: &Value) -> Vec<String> {
    let mut origins = Vec::new();
    for service in document["service"].as_array().into_iter().flatten() {
        let is_linked = match &service["type"] {
            Value::String(one) => one == "LinkedDomains",
            Value::Array(many) => many.iter().any(|t| t == "LinkedDomains"),
            _ => false,
        };
        if !is_linked {
            continue;
        }
        let named: Vec<&Value> = match &service["serviceEndpoint"] {
            Value::Object(endpoint) => endpoint.get("origins").and_then(Value::as_array).into_iter().flatten().collect(),
            Value::Array(many) => many.iter().collect(),
            one => vec![one],
        };
        for origin in named.into_iter().filter_map(Value::as_str).filter_map(origin_of) {
            if !origins.contains(&origin) {
                origins.push(origin);
            }
        }
    }
    origins
}

/// A Domain Linkage Credential: `did` saying, of itself, that `origin` is its
/// holder's. Signed with `secret_jwk`, which `verification_method` (`<did>#key-1`)
/// names under `assertionMethod` in the DID's document. `issued` and `expires` are
/// RFC 3339 times; the specification requires both.
pub async fn credential(did: &str, origin: &str, secret_jwk: &str, verification_method: &str, issued: &str, expires: &str) -> Result<Value, String> {
    let origin = origin_of(origin).ok_or("That is not a website's address: it should look like https://example.com")?;
    let contexts = ["https://www.w3.org/2018/credentials/v1", CONFIGURATION_CONTEXT];
    let mut credential = json!({
        "@context": contexts,
        "issuer": did,
        "issuanceDate": issued,
        "expirationDate": expires,
        "type": ["VerifiableCredential", "DomainLinkageCredential"],
        "credentialSubject": { "id": did, "origin": origin },
    });
    let signer = signing_secret(secret_jwk, Some(verification_method))?;
    // eddsa-jcs-2022 has the proof carry the @context of what it signs.
    let options = SignOptions::new().with_proof_purpose("assertionMethod").with_context(contexts.iter().map(|c| c.to_string()).collect());
    let proof = DataIntegrityProof::sign(&credential, &signer, options).await.map_err(|e| e.to_string())?;
    credential["proof"] = serde_json::to_value(proof).map_err(|e| e.to_string())?;
    Ok(credential)
}

/// The file an origin serves at [`WELL_KNOWN_PATH`], holding `credentials`.
pub fn configuration(credentials: Vec<Value>) -> Value {
    json!({ "@context": CONFIGURATION_CONTEXT, "linked_dids": credentials })
}

/// Checks that `origin` is linked to `did`, both ways: `document` (the DID's own,
/// already verified) names the origin, and `bytes`, fetched from the origin's
/// [`WELL_KNOWN_PATH`], hold a credential that `did` issued about itself and that
/// origin, signed by one of the document's `assertionMethod` keys, and in date at
/// `now` (RFC 3339). `Err` says, for a person, which of those didn't hold.
pub fn check(bytes: &[u8], origin: &str, did: &str, document: &Value, now: &str) -> Result<(), String> {
    let origin = origin_of(origin).ok_or("That is not a website's address.")?;
    let site = origin.trim_start_matches("https://");
    if !linked_origins(document).contains(&origin) {
        return Err(format!("This address doesn't say {site} is its website."));
    }
    let now = DateTime::parse_from_rfc3339(now).map_err(|_| "The time to check at is not a time.".to_string())?;
    let file: Value = serde_json::from_slice(bytes).map_err(|_| format!("What {site} serves there is not JSON."))?;
    let Some(linked) = file["linked_dids"].as_array() else {
        return Err(format!("What {site} serves there is not a DID configuration."));
    };

    // The reason to give if nothing passes: the one from the credential that got furthest.
    let mut furthest = (0, format!("{site} doesn't name this address."));
    let mut fell = |stage: u8, why: String| {
        if stage > furthest.0 {
            furthest = (stage, why);
        }
    };
    for credential in linked {
        // A credential in the JWT form is a string. Only the signed-JSON form is read.
        if credential.is_string() {
            fell(1, format!("{site} names DIDs in a form this app doesn't read (JWT)."));
            continue;
        }
        let subject = &credential["credentialSubject"];
        let issuer = credential["issuer"].as_str().or(credential["issuer"]["id"].as_str());
        if subject["id"].as_str() != Some(did) || issuer != Some(did) {
            continue;
        }
        if subject["origin"].as_str().and_then(origin_of).as_deref() != Some(origin.as_str()) {
            fell(2, format!("{site} names this address, but for another website."));
            continue;
        }
        let in_date = |field: &str| credential[field].as_str().and_then(|time| DateTime::parse_from_rfc3339(time).ok());
        match (in_date("issuanceDate"), in_date("expirationDate")) {
            (Some(issued), Some(expires)) if issued <= now && now < expires => {}
            (Some(_), Some(expires)) if now >= expires => {
                fell(3, format!("What {site} serves for this address has expired."));
                continue;
            }
            _ => {
                fell(3, format!("What {site} serves for this address isn't dated as it has to be."));
                continue;
            }
        }
        if signed_by(credential, document, "assertionMethod").unwrap_or(false) {
            return Ok(());
        }
        fell(4, format!("What {site} serves for this address isn't signed by a key the address has now."));
    }
    Err(furthest.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DID: &str = "did:webvh:Qm:id.example:alice";
    /// An Ed25519 key, and the document that lists it for assertions.
    const SECRET: &str = r#"{"kty":"OKP","crv":"Ed25519","d":"nWGxne_9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"}"#;

    fn document(origins: Value) -> Value {
        let key = crate::write::public_multikey(SECRET).unwrap();
        json!({
            "id": DID,
            "verificationMethod": [{ "id": format!("{DID}#key-1"), "type": "Multikey", "controller": DID, "publicKeyMultibase": key }],
            "authentication": [format!("{DID}#key-1")],
            "assertionMethod": [format!("{DID}#key-1")],
            "service": [{ "id": format!("{DID}#linked-domains"), "type": "LinkedDomains", "serviceEndpoint": origins }],
        })
    }

    async fn file(did: &str, origin: &str, expires: &str) -> Vec<u8> {
        let made = credential(did, origin, SECRET, &format!("{did}#key-1"), "2026-01-01T00:00:00Z", expires).await.unwrap();
        configuration(vec![made]).to_string().into_bytes()
    }

    const NOW: &str = "2026-10-11T12:00:00Z";

    #[test]
    fn a_website_is_an_https_origin_and_nothing_more() {
        assert_eq!(origin_of("alice.example").as_deref(), Some("https://alice.example"));
        assert_eq!(origin_of(" HTTPS://Alice.Example/ ").as_deref(), Some("https://alice.example"));
        assert_eq!(origin_of("https://alice.example:8443").as_deref(), Some("https://alice.example:8443"));
        assert_eq!(origin_of("https://alice.example:443").as_deref(), Some("https://alice.example"));
        for not in ["http://alice.example", "https://alice.example/about", "https://user@alice.example", "https://", "alice..example", "alice.example?x=1", "-a.example"] {
            assert_eq!(origin_of(not), None, "{not}");
        }
    }

    #[test]
    fn a_document_names_its_websites_in_either_form() {
        assert_eq!(linked_origins(&document(json!("https://alice.example/"))), ["https://alice.example"]);
        assert_eq!(
            linked_origins(&document(json!({ "origins": ["https://alice.example", "https://shop.example", "http://plain.example"] }))),
            ["https://alice.example", "https://shop.example"]
        );
        assert!(linked_origins(&json!({ "service": [{ "type": "DIDCommMessaging", "serviceEndpoint": "https://alice.example" }] })).is_empty());
    }

    #[tokio::test]
    async fn a_domain_and_a_did_that_name_each_other_are_linked() {
        let document = document(json!("https://alice.example"));
        let served = file(DID, "https://alice.example", "2031-01-01T00:00:00Z").await;
        assert_eq!(check(&served, "alice.example", DID, &document, NOW), Ok(()));
        // The file has the shape the specification gives it, and nothing else in it.
        let parsed: Value = serde_json::from_slice(&served).unwrap();
        assert_eq!(parsed["@context"], CONFIGURATION_CONTEXT);
        assert_eq!(parsed.as_object().unwrap().len(), 2);
        let made = &parsed["linked_dids"][0];
        assert!(made.get("id").is_none());
        assert_eq!(made["proof"]["proofPurpose"], "assertionMethod");
        assert_eq!(made["proof"]["@context"], made["@context"]);
    }

    #[tokio::test]
    async fn neither_half_is_enough_alone() {
        let served = file(DID, "https://alice.example", "2031-01-01T00:00:00Z").await;
        // The website names the DID; the DID doesn't name the website.
        let silent = document(json!("https://elsewhere.example"));
        assert_eq!(check(&served, "https://alice.example", DID, &silent, NOW).unwrap_err(), "This address doesn't say alice.example is its website.");
        // The DID names the website; the website names somebody else.
        let document = document(json!("https://alice.example"));
        let others = file("did:webvh:Qm:id.example:mallory", "https://alice.example", "2031-01-01T00:00:00Z").await;
        assert_eq!(check(&others, "https://alice.example", DID, &document, NOW).unwrap_err(), "alice.example doesn't name this address.");
        // ...or serves a file made for another website: copied from one that is hers.
        let copied = file(DID, "https://shop.example", "2031-01-01T00:00:00Z").await;
        assert_eq!(check(&copied, "https://alice.example", DID, &document, NOW).unwrap_err(), "alice.example names this address, but for another website.");
    }

    #[tokio::test]
    async fn a_credential_has_to_be_in_date_and_signed_by_a_key_the_did_has() {
        let document = document(json!("https://alice.example"));
        let old = file(DID, "https://alice.example", "2026-06-01T00:00:00Z").await;
        assert_eq!(check(&old, "https://alice.example", DID, &document, NOW).unwrap_err(), "What alice.example serves for this address has expired.");

        // Changed after it was signed.
        let mut tampered: Value = serde_json::from_slice(&file(DID, "https://alice.example", "2031-01-01T00:00:00Z").await).unwrap();
        tampered["linked_dids"][0]["expirationDate"] = json!("2099-01-01T00:00:00Z");
        let unsigned = "What alice.example serves for this address isn't signed by a key the address has now.";
        assert_eq!(check(tampered.to_string().as_bytes(), "https://alice.example", DID, &document, NOW).unwrap_err(), unsigned);

        // Signed by a key the document has, but not for assertions.
        let served = file(DID, "https://alice.example", "2031-01-01T00:00:00Z").await;
        let mut no_assertions = document.clone();
        no_assertions.as_object_mut().unwrap().remove("assertionMethod");
        assert_eq!(check(&served, "https://alice.example", DID, &no_assertions, NOW).unwrap_err(), unsigned);

        assert_eq!(check(b"<html>", "https://alice.example", DID, &document, NOW).unwrap_err(), "What alice.example serves there is not JSON.");
        assert_eq!(check(b"{}", "https://alice.example", DID, &document, NOW).unwrap_err(), "What alice.example serves there is not a DID configuration.");
        let jwt = json!({ "@context": CONFIGURATION_CONTEXT, "linked_dids": ["eyJhbGciOiJFZERTQSJ9.e30.sig"] }).to_string();
        assert_eq!(check(jwt.as_bytes(), "https://alice.example", DID, &document, NOW).unwrap_err(), "alice.example names DIDs in a form this app doesn't read (JWT).");
    }
}
