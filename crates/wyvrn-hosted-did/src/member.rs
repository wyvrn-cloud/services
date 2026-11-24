//! A member's own word that a public address is theirs in a community
//! (community/1.0, "A member's statement"): a Verifiable Presentation held by the
//! address, carrying no credential, proved by one of the address's
//! `authentication` keys (`eddsa-jcs-2022`, purpose `authentication`) with the
//! community's DID as its `domain` and the member's member id there as its
//! `challenge`. The proof's `created` is when it was made.
//!
//! Without it, that a member id is a public address's rests on the community
//! server's word (it checked the key the profile was written with). With it,
//! anyone can check by resolving the address themselves, and a statement moved
//! to another member, or to another community, or signed by any other key, does
//! not check out.

use serde_json::{json, Value};

use crate::presentation;

pub const VC_CONTEXT: &str = "https://www.w3.org/ns/credentials/v2";

/// The statement before it is proved: `{ @context, type, holder }`.
pub fn unsigned(address: &str) -> Value {
    json!({ "@context": [VC_CONTEXT], "type": ["VerifiablePresentation"], "holder": address })
}

/// `address`'s statement that it is `member` in `community`, proved by the
/// Ed25519 key `secret_jwk`, which `verification_method` names under the
/// address's `authentication`, at `created` (RFC 3339).
pub fn sign(address: &str, community: &str, member: &str, secret_jwk: &str, verification_method: &str, created: &str) -> Result<Value, String> {
    presentation::sign(&unsigned(address), secret_jwk, verification_method, member, community, created)
}

/// Checks `statement`: held by `address`, carrying no credential, and proved for
/// `community` and `member` by a key `address_document` (the address's, as
/// resolved by whoever checks) lists under `authentication`. Gives when it was
/// made (`proof.created`). `Err` says, for a person, what didn't hold.
pub fn check(statement: &Value, address_document: &Value, address: &str, community: &str, member: &str) -> Result<String, String> {
    let holder = statement["holder"].as_str().or(statement["holder"]["id"].as_str());
    if holder != Some(address) {
        return Err("It is another address's.".to_string());
    }
    if address_document["id"].as_str() != Some(address) {
        return Err("The address's document is not the address's.".to_string());
    }
    let types = &statement["type"];
    if !(types == "VerifiablePresentation" || types.as_array().is_some_and(|types| types.iter().any(|t| t == "VerifiablePresentation"))) {
        return Err("It is not a presentation.".to_string());
    }
    if !statement["verifiableCredential"].is_null() {
        return Err("It carries more than the statement.".to_string());
    }
    if member.is_empty() || community.is_empty() {
        return Err("There is no member to check it for.".to_string());
    }
    presentation::check(statement, address_document, member, community)?;
    Ok(statement["proof"]["created"].as_str().unwrap_or_default().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::public_multikey;

    const JWK: &str = r#"{"kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo","d":"nWGxne_9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A"}"#;
    const OTHER_JWK: &str = r#"{"kty":"OKP","crv":"Ed25519","x":"Eiz-h2xFuAXq2ccNPhiRAhGgbSW6kiyd2L4i27k_9MQ","d":"8R_2Lm6V9OjXzY4jMqf2bz3KXQxhHq0-sy8w0ckCJZw"}"#;
    const ADDRESS: &str = "did:webvh:Qm:id.example:alice";
    const COMMUNITY: &str = "did:web:community.example";
    const MEMBER: &str = "m0123456789abcdef0123456789";

    fn document() -> Value {
        json!({ "id": ADDRESS, "verificationMethod": [{ "id": format!("{ADDRESS}#key-1"), "type": "Multikey", "controller": ADDRESS, "publicKeyMultibase": public_multikey(JWK).unwrap() }], "authentication": [format!("{ADDRESS}#key-1")] })
    }

    #[test]
    fn a_statement_checks_out_only_for_its_member_community_address_and_key() {
        let statement = sign(ADDRESS, COMMUNITY, MEMBER, JWK, &format!("{ADDRESS}#key-1"), "2026-11-23T00:00:00Z").unwrap();
        assert_eq!(check(&statement, &document(), ADDRESS, COMMUNITY, MEMBER), Ok("2026-11-23T00:00:00Z".to_string()));
        // Moved to another member, or another community.
        assert!(check(&statement, &document(), ADDRESS, COMMUNITY, "m-someone-else").is_err());
        assert!(check(&statement, &document(), ADDRESS, "did:web:elsewhere.example", MEMBER).is_err());
        // Said to be another address's.
        let mut other = document();
        other["id"] = json!("did:webvh:Qm:id.example:mallory");
        assert!(check(&statement, &other, "did:webvh:Qm:id.example:mallory", COMMUNITY, MEMBER).is_err());
        // Signed by a key the address doesn't sign in with.
        let forged = sign(ADDRESS, COMMUNITY, MEMBER, OTHER_JWK, &format!("{ADDRESS}#key-1"), "2026-11-23T00:00:00Z").unwrap();
        assert!(check(&forged, &document(), ADDRESS, COMMUNITY, MEMBER).is_err());
        let mut no_auth = document();
        no_auth["authentication"] = json!([]);
        assert!(check(&statement, &no_auth, ADDRESS, COMMUNITY, MEMBER).is_err());
        // Changed after it was signed: its time, or a credential slipped in.
        let mut later = statement.clone();
        later["proof"]["created"] = json!("2030-01-01T00:00:00Z");
        assert!(check(&later, &document(), ADDRESS, COMMUNITY, MEMBER).is_err());
        let mut carrying = statement.clone();
        carrying["verifiableCredential"] = json!([{ "id": "urn:uuid:1" }]);
        assert!(check(&carrying, &document(), ADDRESS, COMMUNITY, MEMBER).is_err());
        // Small: it travels in every member entry that isn't brief.
        assert!(statement.to_string().len() < 700, "{}", statement.to_string().len());
    }
}
