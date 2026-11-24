//! What a member list says of a member's linked accounts (community/1.0,
//! "Linked accounts in the member list"): not the credentials, which a member's
//! profile card asks for (`member-request`), but a summary of each the server
//! checked when the profile came (claims.rs, `verified_accounts`), with the
//! picture it refers to served here by its digest, as members' pictures are. And
//! the pictures themselves: taken from the profile's attachments only where a
//! credential kept refers to them, or from a credential that carries its picture
//! inline (as linked-accounts/1.0 first had it).

use base64::Engine;
use serde_json::{json, Map, Value};
use wyvrn_hosted_did::linked;
use wyvrn_service::params;

use crate::store::Store;

/// The platform a credential's account is on, as linked-accounts/1.0 names it.
fn platform_of(homepage: &str, said: Option<&str>) -> Option<&'static str> {
    if said == Some("mastodon") {
        return Some("mastodon");
    }
    Some(match homepage {
        "https://bsky.app" => "bluesky",
        "https://www.twitch.tv" => "twitch",
        "https://discord.com" | "https://discord.gg" => "discord",
        "https://www.youtube.com" => "youtube",
        _ => return None,
    })
}

/// A short line of text a credential says, fit to show: at most `most`
/// characters, and none that are control characters or turn text around.
fn shown_text(value: &Value, most: usize) -> Option<String> {
    let text = value.as_str()?.trim();
    let bidi = |c: char| matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    (!text.is_empty() && text.chars().count() <= most && !text.chars().any(|c| c.is_control() || bidi(c))).then(|| text.to_string())
}

/// A picture a credential carries inline (`wyvrn.image`, a `data:` URI of a WebP
/// or JPEG of at most 8 KiB): `(name, media type, bytes)`.
pub fn inline_picture(credential: &Value) -> Option<(String, String, Vec<u8>)> {
    let uri = credential["credentialSubject"]["account"]["wyvrn"]["image"].as_str()?;
    let (head, data) = uri.strip_prefix("data:")?.split_once(";base64,")?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
    let kind = linked::media_type_of(&bytes).filter(|kind| *kind == head && bytes.len() <= linked::MAX_PICTURE_BYTES)?;
    Some((linked::digest_name(&linked::sha384(&bytes)), kind.to_string(), bytes))
}

/// The name of the picture `credential` shows, whichever way it has one.
fn picture_name(credential: &Value) -> Option<String> {
    linked::picture_of(credential).map(|picture| picture.name()).or_else(|| inline_picture(credential).map(|(name, _, _)| name))
}

/// A summary of `credential`, checked at `checked_at`: `{ platform?, homepage,
/// account, handle?, displayName?, picture?, followers?, checkedAt, credential,
/// issuer, validUntil? }`. `picture` is the name of a picture kept here (one of
/// `pictures`), which the list gives as its address. `None` for what isn't a
/// linked-account credential.
pub fn summary(credential: &Value, checked_at: i64, pictures: &[String]) -> Option<Value> {
    let (homepage, account) = crate::claims::account_of(credential)?;
    let wyvrn = &credential["credentialSubject"]["account"]["wyvrn"];
    let mut said = Map::new();
    if let Some(platform) = platform_of(&homepage, wyvrn["platform"].as_str()) {
        said.insert("platform".into(), json!(platform));
    }
    said.insert("homepage".into(), json!(homepage));
    said.insert("account".into(), json!(account));
    if let Some(handle) = shown_text(&wyvrn["handle"], 300) {
        said.insert("handle".into(), json!(handle));
    }
    if let Some(name) = shown_text(&wyvrn["displayName"], 64) {
        said.insert("displayName".into(), json!(name));
    }
    if let Some(name) = picture_name(credential).filter(|name| pictures.contains(name)) {
        said.insert("picture".into(), json!(name));
    }
    if let Some(followers) = wyvrn["followers"].as_u64() {
        said.insert("followers".into(), json!(followers));
    }
    said.insert("checkedAt".into(), json!(checked_at));
    if let Some(id) = credential["id"].as_str().filter(|id| id.len() <= 200) {
        said.insert("credential".into(), json!(id));
    }
    if let Some(issuer) = credential["issuer"].as_str().or(credential["issuer"]["id"].as_str()).filter(|issuer| issuer.len() <= 1024) {
        said.insert("issuer".into(), json!(issuer));
    }
    if let Some(until) = credential["validUntil"].as_str().filter(|until| until.len() <= 40) {
        said.insert("validUntil".into(), json!(until));
    }
    Some(Value::Object(said))
}

/// The pictures to keep for `credentials`: each one refers to, from among
/// `attachments` (the profile message's), and each one carries inline.
/// `(name, media type, bytes)`.
pub fn pictures_for(credentials: &[Value], attachments: &Value) -> Vec<(String, String, Vec<u8>)> {
    let mut found: Vec<(String, String, Vec<u8>)> = linked::attached_pictures(attachments, credentials).into_iter().map(|(picture, bytes)| (picture.name(), picture.media_type, bytes)).collect();
    for inline in credentials.iter().filter_map(inline_picture) {
        if !found.iter().any(|(name, _, _)| *name == inline.0) {
            found.push(inline);
        }
    }
    found
}

/// What is kept of a member's linked accounts beyond the credentials.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Kept {
    /// The summaries, as the list gives them but for their pictures' addresses.
    pub summaries: Option<Value>,
    /// Their public address's statement that it is theirs here.
    pub statement: Option<Value>,
}

impl Store {
    pub fn member_linked(&self, did: &str) -> wyvrn_service::db::Result<Kept> {
        let read = |text: Option<String>| text.and_then(|text| serde_json::from_str::<Value>(&text).ok());
        Ok(self
            .db
            .lock()
            .query_row("SELECT summaries, statement FROM member_linked WHERE did = ?1", params![did], |row| Ok(Kept { summaries: read(row.get(0)?), statement: read(row.get(1)?) }))?
            .unwrap_or_default())
    }

    pub fn set_member_linked(&self, did: &str, kept: &Kept, now: i64) -> wyvrn_service::db::Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM member_linked WHERE did = ?1", params![did])?;
        if kept.summaries.is_some() || kept.statement.is_some() {
            db.execute(
                "INSERT INTO member_linked (did, summaries, statement, at) VALUES (?1, ?2, ?3, ?4)",
                params![did, kept.summaries.as_ref().map(Value::to_string), kept.statement.as_ref().map(Value::to_string), now],
            )?;
        }
        Ok(())
    }

    /// Keeps `pictures` as `did`'s linked accounts' pictures, and no others.
    pub fn set_member_linked_pictures(&self, did: &str, pictures: &[(String, String, Vec<u8>)]) -> wyvrn_service::db::Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM member_linked_pictures WHERE did = ?1", params![did])?;
        for (name, media_type, bytes) in pictures {
            db.execute(
                "INSERT OR REPLACE INTO member_linked_pictures (did, picture, media_type, data) VALUES (?1, ?2, ?3, ?4)",
                params![did, name, media_type, base64::engine::general_purpose::STANDARD.encode(bytes)],
            )?;
        }
        Ok(())
    }

    /// The names of `did`'s linked accounts' pictures kept here.
    pub fn member_linked_picture_names(&self, did: &str) -> wyvrn_service::db::Result<Vec<String>> {
        self.db.lock().query("SELECT picture FROM member_linked_pictures WHERE did = ?1", params![did], |row| row.get(0))
    }

    /// One of them: `(media type, base64)`.
    pub fn member_linked_picture(&self, did: &str, name: &str) -> wyvrn_service::db::Result<Option<(String, String)>> {
        self.db.lock().query_row("SELECT media_type, data FROM member_linked_pictures WHERE did = ?1 AND picture = ?2", params![did, name], |row| Ok((row.get(0)?, row.get(1)?)))
    }
}

/// `summaries` as the list gives them, each picture its address: under
/// `<public_url>/m/<member id>/linked/<name>`.
pub fn with_addresses(summaries: &Value, public_url: &str, member: &str) -> Value {
    let mut given = summaries.clone();
    for summary in given.as_array_mut().into_iter().flatten() {
        if let Some(name) = summary["picture"].as_str().map(str::to_string) {
            summary["picture"] = json!(format!("{}/m/{member}/linked/{name}", public_url.trim_end_matches('/')));
        }
    }
    given
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential(image: Option<Value>, related: Option<Value>) -> Value {
        let mut c = json!({
            "id": "urn:uuid:1",
            "type": ["VerifiableCredential", "LinkedAccountCredential"],
            "issuer": "did:web:verify.example",
            "validFrom": "2026-11-01T00:00:00Z",
            "credentialSubject": { "id": "did:webvh:Qm:id.example:alice", "account": { "type": "OnlineAccount", "accountServiceHomepage": "https://bsky.app", "accountName": "did:plc:abc",
                "wyvrn": { "handle": "alice.bsky.social", "displayName": "Alice", "followers": 12 } } },
        });
        if let Some(image) = image {
            c["credentialSubject"]["account"]["wyvrn"]["image"] = image;
        }
        if let Some(related) = related {
            c["relatedResource"] = related;
        }
        c
    }

    #[test]
    fn a_summary_says_what_a_card_shows_and_names_a_picture_only_once_kept() {
        let jpeg = [0xff_u8, 0xd8, 0xff, 0xe0, 9, 9];
        let url = format!("https://verify.example/pictures/{}.jpg", linked::digest_name(&linked::sha384(&jpeg)));
        let c = credential(Some(json!(url)), Some(json!([{ "id": url, "mediaType": "image/jpeg", "digestSRI": linked::digest_sri(&jpeg) }])));
        let name = linked::digest_name(&linked::sha384(&jpeg));
        let summary = summary(&c, 1_800_000_000, &[]).unwrap();
        assert_eq!(
            summary,
            json!({ "platform": "bluesky", "homepage": "https://bsky.app", "account": "did:plc:abc", "handle": "alice.bsky.social", "displayName": "Alice", "followers": 12, "checkedAt": 1_800_000_000, "credential": "urn:uuid:1", "issuer": "did:web:verify.example" })
        );
        let with_picture = super::summary(&c, 1, std::slice::from_ref(&name)).unwrap();
        assert_eq!(with_picture["picture"], name);
        let given = with_addresses(&json!([with_picture]), "https://c.example/", "m1");
        assert_eq!(given[0]["picture"], format!("https://c.example/m/m1/linked/{name}"));
        // A name that turns text around isn't shown; Discord has no followers.
        let mut odd = credential(None, None);
        odd["credentialSubject"]["account"]["wyvrn"]["displayName"] = json!("\u{202E}evil");
        odd["credentialSubject"]["account"]["accountServiceHomepage"] = json!("https://discord.com");
        odd["credentialSubject"]["account"]["wyvrn"].as_object_mut().unwrap().remove("followers");
        let odd = super::summary(&odd, 1, &[]).unwrap();
        assert_eq!((odd["platform"].as_str(), odd.get("displayName"), odd.get("followers")), (Some("discord"), None, None));
    }

    #[test]
    fn pictures_kept_are_the_ones_credentials_name_attached_or_inline() {
        let jpeg = [0xff_u8, 0xd8, 0xff, 0xe0, 1, 2, 3];
        let webp = b"RIFF\0\0\0\0WEBPVP8 x".to_vec();
        let url = format!("https://verify.example/pictures/{}.webp", linked::digest_name(&linked::sha384(&webp)));
        let referring = credential(Some(json!(url)), Some(json!([{ "id": url, "mediaType": "image/webp", "digestSRI": linked::digest_sri(&webp) }])));
        let inline = credential(Some(json!(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpeg)))), None);
        let attachments = json!([
            { "id": "x", "media_type": "image/webp", "data": { "base64": base64::engine::general_purpose::STANDARD.encode(&webp) } },
            { "id": "y", "media_type": "image/jpeg", "data": { "base64": base64::engine::general_purpose::STANDARD.encode([0xff, 0xd8, 0xff, 0]) } },
        ]);
        let kept = pictures_for(&[referring.clone(), inline.clone()], &attachments);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0], (linked::digest_name(&linked::sha384(&webp)), "image/webp".to_string(), webp.clone()));
        assert_eq!(kept[1].2, jpeg.to_vec());
        // A picture that isn't the one named: none.
        let mut wrong = webp.clone();
        wrong.push(0);
        let attachments = json!([{ "id": "x", "data": { "base64": base64::engine::general_purpose::STANDARD.encode(&wrong) } }]);
        assert!(pictures_for(std::slice::from_ref(&referring), &attachments).is_empty());
        // An inline picture that isn't what it says, or too big: none.
        let fake = credential(Some(json!("data:image/webp;base64,/9j/4AAQ")), None);
        let big = credential(Some(json!(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode([&[0xff_u8, 0xd8, 0xff][..], &vec![0; 9000]].concat())))), None);
        assert!(pictures_for(&[fake, big], &json!([])).is_empty());
    }
}
