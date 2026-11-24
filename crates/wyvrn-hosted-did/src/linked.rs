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

/// The most a linked account's picture may be, in bytes (linked-accounts/1.0,
/// "The picture").
pub const MAX_PICTURE_BYTES: usize = 8 * 1024;
/// The media types a linked account's picture may be.
pub const PICTURE_TYPES: [&str; 2] = ["image/webp", "image/jpeg"];

/// A linked account's picture as its credential names it: not carried inside it,
/// but referred to (`wyvrn.image`) and integrity-protected by a `relatedResource`
/// entry for the same URL (VC Data Model 2.0, "Integrity of Related Resources"),
/// whose `digestSRI` is its SHA-384. The proof covers the entry like any other
/// part of the credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// Where its issuer serves it: the entry's `id`.
    pub url: String,
    pub media_type: String,
    pub sha384: [u8; 48],
}

impl Picture {
    /// The name a copy is kept and served under, wherever it is: the digest in
    /// base64url without padding (64 characters).
    pub fn name(&self) -> String {
        digest_name(&self.sha384)
    }

    /// Whether `bytes` are this picture: its digest, at most [`MAX_PICTURE_BYTES`],
    /// and a picture of its media type.
    pub fn matches(&self, bytes: &[u8]) -> bool {
        bytes.len() <= MAX_PICTURE_BYTES && sha384(bytes) == self.sha384 && media_type_of(bytes) == Some(self.media_type.as_str())
    }
}

/// The SHA-384 of `bytes`.
pub fn sha384(bytes: &[u8]) -> [u8; 48] {
    use sha2::Digest;
    sha2::Sha384::digest(bytes).into()
}

/// `digestSRI` for `bytes`: `sha384-<base64>` (Subresource Integrity).
pub fn digest_sri(bytes: &[u8]) -> String {
    format!("sha384-{}", base64::engine::general_purpose::STANDARD.encode(sha384(bytes)))
}

/// A digest as a name: base64url, no padding.
pub fn digest_name(sha384: &[u8; 48]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha384)
}

/// The SHA-384 a name ([`digest_name`]) says.
pub fn sha384_of_name(name: &str) -> Option<[u8; 48]> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(name).ok()?.try_into().ok()
}

/// The SHA-384 a `digestSRI` value says. It may list several hash expressions
/// (Subresource Integrity), each perhaps with options after a `?`: the SHA-384
/// one is taken.
pub fn sha384_of_sri(sri: &str) -> Option<[u8; 48]> {
    let expression = sri.split_whitespace().find_map(|expression| expression.strip_prefix("sha384-"))?;
    let base64 = expression.split('?').next().unwrap_or_default().trim_end_matches('=');
    base64::engine::general_purpose::STANDARD_NO_PAD.decode(base64).ok()?.try_into().ok()
}

/// Which of [`PICTURE_TYPES`] `bytes` are, by their first bytes.
pub fn media_type_of(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The picture `credential` refers to, if it refers to one: `wyvrn.image` an
/// `https:` URL, and a `relatedResource` entry for it with a WebP or JPEG
/// `mediaType` and a SHA-384 `digestSRI`. A credential that carries its picture
/// inline (a `data:` URI, as linked-accounts/1.0 first had it) or names one with
/// no such entry refers to none.
pub fn picture_of(credential: &Value) -> Option<Picture> {
    let url = credential["credentialSubject"]["account"]["wyvrn"]["image"].as_str().filter(|url| url.starts_with("https://") && url.len() <= 2048)?;
    let related = match &credential["relatedResource"] {
        Value::Array(many) => many.iter().collect::<Vec<_>>(),
        one @ Value::Object(_) => vec![one],
        _ => return None,
    };
    let entry = related.into_iter().find(|entry| entry["id"].as_str() == Some(url))?;
    let media_type = entry["mediaType"].as_str().filter(|kind| PICTURE_TYPES.contains(kind))?;
    let sha384 = entry["digestSRI"].as_str().and_then(sha384_of_sri)?;
    Some(Picture { url: url.to_string(), media_type: media_type.to_string(), sha384 })
}

/// The pictures that `credentials` refer to, by name ([`Picture::name`]).
pub fn pictures_of(credentials: &[Value]) -> Vec<Picture> {
    let mut found: Vec<Picture> = Vec::new();
    for picture in credentials.iter().filter_map(picture_of) {
        if !found.iter().any(|kept| kept.sha384 == picture.sha384) {
            found.push(picture);
        }
    }
    found
}

/// The attachments of a message that are pictures `credentials` refer to
/// (linked-accounts/1.0, "Where the picture travels"): each `data.base64` whose
/// bytes match one, as `(picture, bytes)`, one per picture. Anything else among
/// the attachments is left alone.
pub fn attached_pictures(attachments: &Value, credentials: &[Value]) -> Vec<(Picture, Vec<u8>)> {
    let wanted = pictures_of(credentials);
    let mut found: Vec<(Picture, Vec<u8>)> = Vec::new();
    for attachment in attachments.as_array().into_iter().flatten() {
        let Some(text) = attachment["data"]["base64"].as_str() else { continue };
        let text = text.trim_end_matches('=');
        let Some(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text).or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(text)).ok() else { continue };
        if bytes.len() > MAX_PICTURE_BYTES {
            continue;
        }
        if let Some(picture) = wanted.iter().find(|picture| picture.matches(&bytes)) {
            if !found.iter().any(|(kept, _)| kept.sha384 == picture.sha384) {
                found.push((picture.clone(), bytes));
            }
        }
    }
    found
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

    /// A picture of the media type `kind`, as its first bytes say: enough for
    /// [`media_type_of`], whatever follows.
    fn picture_bytes(kind: &str, fill: u8) -> Vec<u8> {
        let mut bytes = if kind == "image/jpeg" { vec![0xff, 0xd8, 0xff, 0xe0] } else { b"RIFF\0\0\0\0WEBPVP8 ".to_vec() };
        bytes.extend(std::iter::repeat_n(fill, 2000));
        bytes
    }

    fn with_picture(bytes: &[u8]) -> Value {
        let url = format!("https://verify.example/pictures/{}.webp", digest_name(&sha384(bytes)));
        let mut c = credential(None);
        c["credentialSubject"]["account"]["wyvrn"]["image"] = json!(url);
        c["relatedResource"] = json!([{ "id": url, "mediaType": "image/webp", "digestSRI": digest_sri(bytes) }]);
        c
    }

    #[tokio::test]
    async fn a_picture_is_referred_to_by_its_digest_and_the_proof_covers_the_reference() {
        let doc = issuer_document();
        let open = signed(status_list(&[])).await;
        let bytes = picture_bytes("image/webp", 7);
        let good = signed(with_picture(&bytes)).await;
        assert_eq!(check(&good, &doc, Some(&open), NOW), Ok(()));
        let picture = picture_of(&good).unwrap();
        assert_eq!(picture.media_type, "image/webp");
        assert_eq!(picture.name().len(), 64);
        assert_eq!(sha384_of_name(&picture.name()), Some(picture.sha384));
        assert!(picture.matches(&bytes));
        // Another picture, or the same one cut short or said to be a JPEG: not it.
        assert!(!picture.matches(&picture_bytes("image/webp", 8)));
        assert!(!picture.matches(&bytes[..bytes.len() - 1]));
        let mut as_jpeg = picture.clone();
        as_jpeg.media_type = "image/jpeg".into();
        assert!(!as_jpeg.matches(&bytes));
        // The digest, the URL and the media type are all signed.
        for (pointer, other) in [
            ("/relatedResource/0/digestSRI", json!(digest_sri(&picture_bytes("image/webp", 9)))),
            ("/relatedResource/0/mediaType", json!("image/jpeg")),
            ("/credentialSubject/account/wyvrn/image", json!("https://elsewhere.example/x.webp")),
        ] {
            let mut altered = good.clone();
            *altered.pointer_mut(pointer).unwrap() = other;
            assert_eq!(check(&altered, &doc, Some(&open), NOW), Err("Its issuer's signature doesn't check out.".to_string()), "{pointer}");
        }
        // A reference with no entry for it, or an entry with no SHA-384, is none.
        let mut unprotected = good.clone();
        unprotected["relatedResource"][0]["id"] = json!("https://verify.example/other");
        assert_eq!(picture_of(&unprotected), None);
        let mut sha256 = good.clone();
        sha256["relatedResource"][0]["digestSRI"] = json!("sha256-n4bQgYhMfWWaL+qgxVrQFaO/TxsrC4Is0V1sFbDwCgg=");
        assert_eq!(picture_of(&sha256), None);
        // An SRI value of several expressions: the SHA-384 one.
        assert_eq!(sha384_of_sri(&format!("sha256-abc {}", digest_sri(&bytes))), Some(sha384(&bytes)));
        // One carried inline, as the profile first had it: a picture, but no reference.
        let mut inline = credential(None);
        inline["credentialSubject"]["account"]["wyvrn"]["image"] = json!("data:image/jpeg;base64,/9j/4AAQ");
        assert_eq!(picture_of(&inline), None);
    }

    #[test]
    fn a_messages_pictures_are_the_attachments_that_match_a_credential() {
        let bytes = picture_bytes("image/webp", 1);
        let credentials = vec![with_picture(&bytes)];
        let other = picture_bytes("image/webp", 2);
        let attachments = json!([
            { "id": "pfp", "media_type": "image/png", "data": { "base64": base64::engine::general_purpose::STANDARD.encode(&other) } },
            { "id": digest_name(&sha384(&bytes)), "media_type": "image/webp", "data": { "base64": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes) } },
            { "id": "again", "media_type": "image/webp", "data": { "base64": base64::engine::general_purpose::STANDARD.encode(&bytes) } },
        ]);
        let found = attached_pictures(&attachments, &credentials);
        assert_eq!(found.len(), 1, "once, and nothing that isn't one");
        assert_eq!(found[0].1, bytes);
        // Too big to be one, whatever its digest.
        let big = { let mut b = picture_bytes("image/webp", 3); b.extend(vec![0u8; MAX_PICTURE_BYTES]); b };
        let attachments = json!([{ "id": "x", "data": { "base64": base64::engine::general_purpose::STANDARD.encode(&big) } }]);
        assert!(attached_pictures(&attachments, &[with_picture(&big)]).is_empty());
    }
}
