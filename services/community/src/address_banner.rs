//! A member's banner, from the public address their profile names, for a member
//! whose app didn't send one.
//!
//! Once the server has seen that the address is the member's (the key they wrote
//! with is one of its keys: `wyvrn_service::named::holder_among`), what the
//! address publishes about its holder is theirs too. Its whois carries the banner
//! the person chose, as a small picture inline (`brand.image`), read and checked
//! by the same reader the app uses (`wyvrn_hosted_did::read`). The member list
//! shows it behind their name.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};
use wyvrn_hosted_did::read::{self, Files, Step};

/// A DID's document, its log and its whois, and room to spare.
const MAX_FILES: usize = 8;
/// What one file may be: a log grows with each change of its holder's devices.
const MAX_FILE_BYTES: usize = 1024 * 1024;
/// How long an address's banner (or its having none) is taken as read.
const KEPT_FOR: Duration = Duration::from_secs(60 * 60);
/// The id of the attachment it rides in on the profile message.
const ATTACHMENT: &str = "address-banner";

/// The banner the address `address` publishes, as `(media type, base64)`: an inline
/// picture only, of the kinds a profile picture may be. `None` for none, or if the
/// address couldn't be read. `fetch` gets a file's text (`None` for a 404).
pub async fn banner_of<F, Fut>(address: &str, fetch: F) -> Option<(String, String)>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Option<String>, String>>,
{
    let banner = profile_of(address, fetch).await?.banner?;
    let (media_type, data) = banner.url.strip_prefix("data:")?.split_once(";base64,")?;
    let media_type = media_type.to_ascii_lowercase();
    if !matches!(media_type.as_str(), "image/webp" | "image/png" | "image/jpeg" | "image/gif") {
        return None;
    }
    base64::engine::general_purpose::STANDARD.decode(data).ok()?;
    Some((media_type, data.to_string()))
}

/// What the address `address` publishes about its holder (its whois), read and
/// checked as an app reads it.
pub async fn profile_of<F, Fut>(address: &str, fetch: F) -> Option<wyvrn_hosted_did::whois::Profile>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Option<String>, String>>,
{
    let mut files = Files::new();
    let found = loop {
        match read::read(address, &files).await {
            Step::Done(found) => break found,
            Step::Failed(_) => return None,
            // Only ever a file of the address's own, at its own host, over HTTPS.
            Step::Need(url) if url.starts_with("https://") && !files.contains_key(&url) && files.len() < MAX_FILES => {
                let file = fetch(url.clone()).await.ok()?;
                files.insert(url, file);
            }
            Step::Need(_) => return None,
        }
    };
    found.profile
}

/// A file of a public address's, over HTTPS (`None` for a 404), fetched only from
/// a public address (security review WYV-49): the URL comes from a DID someone
/// gave, and the server fetching it is inside its own network. Its host is
/// checked as written and again as resolved, and no redirect is followed.
async fn fetch_public(url: String) -> Result<Option<String>, String> {
    static HTTP: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .dns_resolver(wyvrn_service::guard::Guard::default())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("a client with a timeout builds")
    });
    wyvrn_service::guard::Guard::default().check(&url, &["https"]).await?;
    let response = HTTP.get(&url).send().await.map_err(|e| e.to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!("{url}: {}", response.status()));
    }
    if response.content_length().is_some_and(|length| length as usize > MAX_FILE_BYTES) {
        return Err(format!("{url} is too large"));
    }
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(format!("{url} is too large"));
    }
    String::from_utf8(bytes.to_vec()).map(Some).map_err(|e| e.to_string())
}

/// The linked-account credentials the address `address` publishes now, over HTTPS.
pub async fn linked_of_address(address: &str) -> Option<Vec<Value>> {
    let profile = profile_of(address, fetch_public).await?;
    Some(profile.linked)
}

/// [`banner_of`] over HTTPS, each address at most once an hour.
pub async fn banner_of_address(address: &str) -> Option<(String, String)> {
    static SEEN: std::sync::LazyLock<Mutex<HashMap<String, (Instant, Option<(String, String)>)>>> = std::sync::LazyLock::new(Default::default);
    if let Some((at, banner)) = SEEN.lock().expect("lock").get(address) {
        if at.elapsed() < KEPT_FOR {
            return banner.clone();
        }
    }
    let banner = banner_of(address, fetch_public).await;
    let mut seen = SEEN.lock().expect("lock");
    seen.retain(|_, (at, _)| at.elapsed() < KEPT_FOR);
    seen.insert(address.to_string(), (Instant::now(), banner.clone()));
    banner
}

/// `message` (a user-profile/1.0 `profile`) with `banner` attached as its banner,
/// as an app attaches one.
pub fn with_banner(message: &mut Value, (media_type, base64): (String, String)) {
    let Some(profile) = message["body"]["profile"].as_object_mut() else { return };
    profile.insert("banner".into(), json!(format!("#{ATTACHMENT}")));
    let attachment = json!({ "id": ATTACHMENT, "media_type": media_type, "data": { "base64": base64 } });
    match message.get_mut("attachments").and_then(Value::as_array_mut) {
        Some(attachments) => attachments.push(attachment),
        None => message["attachments"] = json!([attachment]),
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn an_address_is_fetched_from_only_over_https_and_never_from_inside_the_network() {
        for url in ["https://127.0.0.1/.well-known/did.json", "https://10.0.0.1/x", "https://localhost/x", "http://example.com/x"] {
            assert!(super::fetch_public(url.to_string()).await.is_err(), "{url}");
        }
    }

    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use std::collections::HashMap;
    use wyvrn_hosted_did::write;

    fn jwk(seed: u8) -> String {
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        json!({ "kty": "OKP", "crv": "Ed25519", "x": URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes()), "d": URL_SAFE_NO_PAD.encode(key.to_bytes()) }).to_string()
    }

    /// A public address as a person's app publishes it: its log, and a whois with
    /// `banner` (a profile `ImageObject`) where there is one. Answers the address
    /// and its files by URL.
    async fn published(banner: Option<Value>) -> (String, HashMap<String, String>) {
        let (authentication, update) = (jwk(1), jwk(2));
        let document = |did: &str| {
            json!({
                "@context": ["https://www.w3.org/ns/did/v1", "https://w3id.org/security/multikey/v1"],
                "id": did,
                "verificationMethod": [{ "id": format!("{did}#key-1"), "type": "Multikey", "controller": did, "publicKeyMultibase": write::public_multikey(&authentication).unwrap() }],
                "authentication": [format!("{did}#key-1")],
                "assertionMethod": [format!("{did}#key-1")],
            })
        };
        let update_key = write::public_multikey(&update).unwrap();
        let first = write::create(&document("did:webvh:{SCID}:id.example:user:alice"), &update, std::slice::from_ref(&update_key)).await.unwrap();
        let profile: write::ProfileFields = serde_json::from_value(json!({ "kind": "Person", "name": "Alice", "banner": banner })).unwrap();
        let whois = write::whois(&first.did, &profile, &authentication, &format!("{}#key-1", first.did), "2026-11-19T00:00:00Z").await.unwrap();
        let base = "https://id.example/user/alice";
        let files = HashMap::from([(format!("{base}/did.jsonl"), first.log.clone()), (format!("{base}/whois.vp"), whois.to_string())]);
        (first.did, files)
    }

    async fn read_with(address: &str, files: &HashMap<String, String>) -> Option<(String, String)> {
        banner_of(address, |url| {
            let file = files.get(&url).cloned();
            async move { Ok(file) }
        })
        .await
    }

    #[tokio::test]
    async fn the_banner_an_address_publishes_inline_is_read_from_its_whois() {
        let (address, files) = published(Some(json!({ "contentUrl": "data:image/webp;base64,UklGRg==", "encodingFormat": "image/webp" }))).await;
        assert_eq!(read_with(&address, &files).await, Some(("image/webp".to_string(), "UklGRg==".to_string())));
    }

    #[tokio::test]
    async fn no_banner_one_elsewhere_or_an_unreadable_address_is_none() {
        let (address, files) = published(None).await;
        assert_eq!(read_with(&address, &files).await, None);
        // A banner fetched from elsewhere isn't fetched: only one that is in the whois.
        let elsewhere = json!({ "contentUrl": "https://alice.example/banner.webp", "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08" });
        let (address, files) = published(Some(elsewhere)).await;
        assert_eq!(read_with(&address, &files).await, None);
        assert_eq!(read_with(&address, &HashMap::new()).await, None);
        // Not a picture it may be.
        let (address, files) = published(Some(json!({ "contentUrl": "data:image/svg+xml;base64,PHN2Zy8+" }))).await;
        assert_eq!(read_with(&address, &files).await, None);
    }

    #[test]
    fn a_banner_rides_on_the_profile_as_an_app_attaches_one() {
        let mut message = json!({ "type": "https://didcomm.org/user-profile/1.0/profile", "body": { "profile": { "displayName": "Alice", "displayPicture": "#pfp" } }, "attachments": [{ "id": "pfp" }] });
        with_banner(&mut message, ("image/webp".into(), "UklGRg==".into()));
        assert_eq!(message["body"]["profile"]["banner"], "#address-banner");
        assert_eq!(message["attachments"][1], json!({ "id": "address-banner", "media_type": "image/webp", "data": { "base64": "UklGRg==" } }));
    }
}
