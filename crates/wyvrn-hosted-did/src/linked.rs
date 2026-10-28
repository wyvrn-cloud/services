//! Linked accounts in a whois: credentials a verification service issued, saying
//! an account elsewhere is the DID's holder's (linked-accounts/1.0, "In the
//! whois"). Finding them in a presentation, and checking one, with the fetching
//! (the issuer's document, its status list) left to the caller.

use std::io::Read;

use base64::Engine;
use chrono::DateTime;
use serde_json::Value;

use crate::whois::signed_by;

pub const LINKED_ACCOUNT_CREDENTIAL: &str = "LinkedAccountCredential";

fn has_type(value: &Value, wanted: &str) -> bool {
    match &value["type"] {
        Value::String(one) => one == wanted,
        Value::Array(many) => many.iter().any(|t| t == wanted),
        _ => false,
    }
}

fn issuer_of(credential: &Value) -> Option<&str> {
    credential["issuer"].as_str().or(credential["issuer"]["id"].as_str())
}

/// The linked-account credentials in a whois `presentation` that are about `did`.
/// Not yet checked: see [`check`].
pub fn of(presentation: &Value, did: &str) -> Vec<Value> {
    let credentials = match &presentation["verifiableCredential"] {
        Value::Array(many) => many.clone(),
        Value::Object(_) => vec![presentation["verifiableCredential"].clone()],
        _ => Vec::new(),
    };
    credentials.into_iter().filter(|c| has_type(c, LINKED_ACCOUNT_CREDENTIAL) && c["credentialSubject"]["id"].as_str() == Some(did) && issuer_of(c).is_some()).collect()
}

/// The status list a credential points to, if it has one: its address and index.
pub fn status_of(credential: &Value) -> Option<(String, u64)> {
    let status = &credential["credentialStatus"];
    if status["type"] != "BitstringStatusListEntry" || status["statusPurpose"] != "revocation" {
        return None;
    }
    let url = status["statusListCredential"].as_str()?.to_string();
    let index = status["statusListIndex"].as_str()?.parse().ok()?;
    Some((url, index))
}

fn bit(encoded: &str, index: u64) -> Result<bool, String> {
    let packed = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded.strip_prefix('u').ok_or("The status list isn't encoded as it should be.")?)
        .map_err(|_| "The status list isn't encoded as it should be.")?;
    let mut bits = Vec::new();
    flate2::read::GzDecoder::new(packed.as_slice()).read_to_end(&mut bits).map_err(|_| "The status list isn't compressed as it should be.")?;
    let byte = bits.get((index / 8) as usize).ok_or("The credential's place is past the end of its status list.")?;
    Ok(byte & (0x80 >> (index % 8)) != 0)
}

/// Checks `credential` (a linked-account credential found by [`of`]): signed by
/// one of `issuer_document`'s `assertionMethod` keys, in date at `now` (RFC 3339),
/// and not revoked in `status_list` (the credential its `credentialStatus` names,
/// fetched, signed by the same issuer). Whether the issuer is one to trust is
/// the caller's. `Err` says, for a person, what didn't hold.
pub fn check(credential: &Value, issuer_document: &Value, status_list: Option<&Value>, now: &str) -> Result<(), String> {
    let issuer = issuer_of(credential).ok_or("It names no issuer.")?;
    if issuer_document["id"].as_str() != Some(issuer) {
        return Err("The issuer's document is not the issuer's.".to_string());
    }
    if !signed_by(credential, issuer_document, "assertionMethod")? {
        return Err("Its issuer's signature doesn't check out.".to_string());
    }
    let now = DateTime::parse_from_rfc3339(now).map_err(|_| "The time to check at is not a time.".to_string())?;
    let at = |field: &str| credential[field].as_str().map(DateTime::parse_from_rfc3339);
    match at("validFrom") {
        Some(Ok(from)) if from <= now => {}
        Some(Ok(_)) => return Err("It isn't valid yet.".to_string()),
        _ => return Err("It doesn't say when it was issued.".to_string()),
    }
    if let Some(until) = at("validUntil") {
        match until {
            Ok(until) if until > now => {}
            _ => return Err("It has expired.".to_string()),
        }
    }
    let Some((url, index)) = status_of(credential) else { return Ok(()) };
    let list = status_list.ok_or("Its status list couldn't be read.")?;
    if list["id"].as_str() != Some(url.as_str()) || issuer_of(list) != Some(issuer) || !has_type(list, "BitstringStatusListCredential") {
        return Err("Its status list is not the one it names.".to_string());
    }
    if !signed_by(list, issuer_document, "assertionMethod")? {
        return Err("Its status list's signature doesn't check out.".to_string());
    }
    let subject = &list["credentialSubject"];
    if subject["statusPurpose"] != "revocation" {
        return Err("Its status list is not a revocation list.".to_string());
    }
    if bit(subject["encodedList"].as_str().ok_or("Its status list holds no list.")?, index)? {
        return Err("Its issuer has taken it back.".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use affinidi_data_integrity::{DataIntegrityProof, SignOptions};
    use serde_json::json;

    use super::*;
    use crate::write::{public_multikey, signing_secret};

    const ISSUER: &str = "did:web:verify.example";
    const DID: &str = "did:webvh:Qm:id.example:alice";
    const JWK: &str = r#"{"kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo","d":"nWGxne_9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A"}"#;
    const LIST: &str = "https://verify.example/status/1";

    async fn signed(mut value: Value) -> Value {
        let signer = signing_secret(JWK, Some(&format!("{ISSUER}#key-1"))).unwrap();
        let contexts = value["@context"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
        let proof = DataIntegrityProof::sign(&value, &signer, SignOptions::new().with_proof_purpose("assertionMethod").with_context(contexts)).await.unwrap();
        value["proof"] = serde_json::to_value(proof).unwrap();
        value
    }

    fn issuer_document() -> Value {
        json!({ "id": ISSUER, "verificationMethod": [{ "id": format!("{ISSUER}#key-1"), "type": "Multikey", "controller": ISSUER, "publicKeyMultibase": public_multikey(JWK).unwrap() }], "assertionMethod": [format!("{ISSUER}#key-1")] })
    }

    fn credential(until: Option<&str>) -> Value {
        let mut c = json!({
            "@context": ["https://www.w3.org/ns/credentials/v2", "https://wyvrn.app/ns/linked-account/v1"],
            "type": ["VerifiableCredential", LINKED_ACCOUNT_CREDENTIAL],
            "issuer": ISSUER,
            "validFrom": "2026-10-28T00:00:00Z",
            "credentialSubject": { "id": DID, "account": { "type": "OnlineAccount", "accountServiceHomepage": "https://bsky.app", "accountName": "did:plc:abc", "wyvrn": { "handle": "alice.bsky.social" } } },
            "credentialStatus": { "id": format!("{LIST}#42"), "type": "BitstringStatusListEntry", "statusPurpose": "revocation", "statusListIndex": "42", "statusListCredential": LIST },
        });
        if let Some(until) = until {
            c["validUntil"] = json!(until);
        }
        c
    }

    fn status_list(revoked: &[u64]) -> Value {
        let mut bits = vec![0u8; 16384];
        for &i in revoked {
            bits[(i / 8) as usize] |= 0x80 >> (i % 8);
        }
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&bits).unwrap();
        let encoded = format!("u{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(gzip.finish().unwrap()));
        json!({ "@context": ["https://www.w3.org/ns/credentials/v2"], "id": LIST, "type": ["VerifiableCredential", "BitstringStatusListCredential"], "issuer": ISSUER, "validFrom": "2026-10-28T00:00:00Z",
            "credentialSubject": { "id": format!("{LIST}#list"), "type": "BitstringStatusList", "statusPurpose": "revocation", "encodedList": encoded } })
    }

    const NOW: &str = "2026-10-29T00:00:00Z";

    #[tokio::test]
    async fn a_linked_account_checks_out_until_it_is_revoked_changed_or_expired() {
        let doc = issuer_document();
        let good = signed(credential(None)).await;
        let open = signed(status_list(&[7])).await;
        assert_eq!(check(&good, &doc, Some(&open), NOW), Ok(()));
        let revoked = signed(status_list(&[42])).await;
        assert_eq!(check(&good, &doc, Some(&revoked), NOW), Err("Its issuer has taken it back.".to_string()));
        assert!(check(&good, &doc, None, NOW).is_err(), "a status list it names has to be read");
        let mut changed = good.clone();
        changed["credentialSubject"]["account"]["accountName"] = json!("did:plc:someoneelse");
        assert_eq!(check(&changed, &doc, Some(&open), NOW), Err("Its issuer's signature doesn't check out.".to_string()));
        let expired = signed(credential(Some("2026-10-28T12:00:00Z"))).await;
        assert_eq!(check(&expired, &doc, Some(&open), NOW), Err("It has expired.".to_string()));
        let mut forged_list = open.clone();
        forged_list["credentialSubject"]["encodedList"] = revoked["credentialSubject"]["encodedList"].clone();
        assert!(check(&good, &doc, Some(&forged_list), NOW).is_err());
    }

    #[tokio::test]
    async fn the_whois_carries_linked_accounts_about_it_and_lists_them() {
        let good = signed(credential(None)).await;
        let mut someone_elses = good.clone();
        someone_elses["credentialSubject"]["id"] = json!("did:webvh:Qm:id.example:bob");
        let fields = crate::write::ProfileFields { name: Some("Alice".to_string()), credentials: vec![good.clone(), someone_elses], ..Default::default() };
        let whois = crate::write::whois(DID, &fields, JWK, &format!("{DID}#key-1"), "2026-10-28T00:00:00Z").await.unwrap();
        assert_eq!(whois["verifiableCredential"].as_array().unwrap().len(), 2, "the profile and Alice's own link");
        assert_eq!(of(&whois, DID), vec![good]);
        let profile = crate::whois::profile(&whois, DID).unwrap();
        assert_eq!(profile.linked.len(), 1);
    }
}
