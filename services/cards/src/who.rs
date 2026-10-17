//! Who a link is to.

use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::time::Duration;

use base64::Engine;
use serde_json::Value;
use wyvrn_hosted_did::read::{self, Files, Step};
use wyvrn_hosted_did::urls::Location;
use wyvrn_hosted_did::{Image, Standing};

use crate::config::Config;

/// Something said of an address beside its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Official,
    /// Its history verified: a `did:webvh`.
    Verified,
    /// A bare `did:web`: whatever its host says today.
    HostsWord,
    Ended,
}

/// What a card shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Who {
    /// The DID the link names.
    pub did: String,
    /// What it is, in words that can't be made up: `alice via id.example` for an
    /// address, `Contact invite` for an invite.
    pub label: String,
    /// The rest is its own word.
    pub name: Option<String>,
    pub description: Option<String>,
    /// Pictures, each a `data:` URL of a bitmap.
    pub picture: Option<String>,
    pub banner: Option<String>,
    pub background: Option<String>,
    pub marks: Vec<Mark>,
}

impl Who {
    /// What to call it: its name, or failing that what it is.
    pub fn title(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.label)
    }

    /// A short mark of everything its card shows: the same while the card would be
    /// drawn the same, and another once anything on it changes.
    pub fn version(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        for part in [Some(&self.did), Some(&self.label), self.name.as_ref(), self.description.as_ref(), self.picture.as_ref(), self.banner.as_ref(), self.background.as_ref()] {
            // Each part's length first, so that no two lists of parts run together the same.
            let part = part.map(String::as_str).unwrap_or_default();
            hash.update((part.len() as u64).to_be_bytes());
            hash.update(part.as_bytes());
        }
        hash.update(format!("{:?}", self.marks).as_bytes());
        hash.finalize().iter().take(6).map(|byte| format!("{byte:02x}")).collect()
    }
}

/// A `data:` URL that is a bitmap and nothing else: what may be put in a card.
/// Never SVG, which is a document that can carry more than a picture.
pub fn bitmap(url: &str) -> Option<String> {
    let rest = url.strip_prefix("data:")?;
    let (media_type, data) = rest.split_once(";base64,")?;
    let media_type = media_type.to_ascii_lowercase();
    if !matches!(media_type.as_str(), "image/png" | "image/jpeg" | "image/webp" | "image/gif") {
        return None;
    }
    // Re-encoded: what goes into the card is known to be base64 and nothing more.
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(data.trim_end_matches('=')))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(data.trim_end_matches('=')))
        .ok()?;
    (!bytes.is_empty() && bytes.len() <= MAX_PICTURE_BYTES).then(|| format!("data:{media_type};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

/// A picture in a card is one that came inside a profile or an invite.
const MAX_PICTURE_BYTES: usize = 512 * 1024;

fn inline(image: &Option<Image>) -> Option<String> {
    // Only a picture that is right there. One that is elsewhere would have this
    // service fetching whatever address a profile cares to name.
    image.as_ref().filter(|image| image.is_inline()).and_then(|image| bitmap(&image.url))
}

/// Whether `did` is a `did:web` or `did:webvh` as an app would take one: nothing in
/// it but what such a DID is made of.
pub fn is_address(did: &str) -> bool {
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '%');
    did.len() <= 300 && did.chars().all(ok) && Location::of(did).is_some_and(|place| !place.host.contains('%'))
}

/// `alice via id.example`: an address as it is written for a person.
pub fn written(did: &str) -> String {
    match Location::of(did) {
        Some(place) if !place.path.is_empty() => format!("{} via {}", place.path.join(" / "), place.host),
        Some(place) => place.host,
        None => did.to_string(),
    }
}

/// Getting a DID's files: the only part that touches a network.
pub trait Fetch: Send + Sync {
    /// The text at `url`, or `None` if there is nothing there (404).
    fn get<'a>(&'a self, url: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<String>, String>> + Send + 'a>>;
}

/// A DID's document, its log and its whois, and room to spare. More asks than
/// this is a reader going round in circles.
const MAX_FILES: usize = 8;

/// Looks an address up, as an app does before adding it: resolved, its history
/// verified where it has one, and its profile checked against it.
pub async fn of_address(did: &str, fetch: &dyn Fetch, config: &Config) -> Result<Who, String> {
    if !is_address(did) {
        return Err(format!("{did} is not a public address"));
    }
    let mut files = Files::new();
    let found = loop {
        match read::read(did, &files).await {
            Step::Done(found) => break found,
            Step::Failed(why) => return Err(why),
            // Only ever a file of the DID's own, at its own host, over HTTPS.
            Step::Need(url) if url.starts_with("https://") && !files.contains_key(&url) && files.len() < MAX_FILES => {
                let file = fetch.get(&url).await?;
                files.insert(url, file);
            }
            Step::Need(_) => return Err("the address could not be read".to_string()),
        }
    };
    let resolved = &found.resolved;
    let place = Location::of(&resolved.did);
    let verified = resolved.standing == Standing::Verified;
    let official = place.as_ref().is_some_and(|place| {
        let host = place.host.to_lowercase();
        if place.path.is_empty() {
            config.official_domains.contains(&host)
        } else {
            verified && host == config.did_host && place.path.first().is_some_and(|first| first == "official") && place.path.len() > 1
        }
    });
    let mut marks = Vec::new();
    if official {
        marks.push(Mark::Official);
    }
    marks.push(if verified { Mark::Verified } else { Mark::HostsWord });
    if resolved.deactivated {
        marks.push(Mark::Ended);
    }
    let profile = found.profile.as_ref();
    Ok(Who {
        did: did.to_string(),
        label: written(&resolved.did),
        name: profile.and_then(|profile| profile.name.clone()),
        description: profile.and_then(|profile| profile.description.clone()),
        picture: profile.and_then(|profile| inline(&profile.image)),
        banner: profile.and_then(|profile| inline(&profile.banner)),
        background: profile.and_then(|profile| inline(&profile.background)),
        marks,
    })
}

const INVITATION: &str = "https://didcomm.org/out-of-band/2.0/invitation";
const PROFILE: &str = "https://didcomm.org/user-profile/1.0/profile";
const CONTACT_GOAL: &str = "app.wyvrn.contact";

fn text(value: &Value, limit: usize) -> Option<String> {
    value.as_str().map(|s| s.trim().chars().take(limit).collect::<String>()).filter(|s| !s.is_empty())
}

/// Reads a contact invite (an out-of-band/2.0 invitation, as a link carries it in
/// `_oob`) for what it shows: who it is from, and the profile riding along.
/// Nothing in an invite is signed, so none of it is marked as anything.
pub fn of_invite(oob: &str) -> Option<Who> {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let bytes = b64.decode(oob.trim_end_matches('=')).or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(oob.trim_end_matches('='))).ok()?;
    let invitation: Value = serde_json::from_slice(&bytes).ok()?;
    if invitation["type"] != INVITATION || invitation["body"]["goal_code"] != CONTACT_GOAL {
        return None;
    }
    let did = invitation["from"].as_str().filter(|did| did.len() <= 4000 && did.starts_with("did:") && !did.contains(char::is_whitespace))?;
    let attached = invitation["attachments"].as_array().into_iter().flatten().map(|attachment| &attachment["data"]["json"]).find(|message| message["type"] == PROFILE);
    let profile = attached.map(|message| &message["body"]["profile"]);
    let picture = attached.and_then(|message| {
        let reference = message["body"]["profile"]["displayPicture"].as_str()?.rsplit('#').next()?;
        let attachment = message["attachments"].as_array()?.iter().find(|attachment| attachment["id"] == reference)?;
        bitmap(&format!("data:{};base64,{}", attachment["media_type"].as_str()?, attachment["data"]["base64"].as_str()?.replace('-', "+").replace('_', "/")))
    });
    Some(Who {
        did: did.to_string(),
        label: "Contact invite".to_string(),
        name: profile.and_then(|profile| text(&profile["displayName"], 80)),
        description: profile.and_then(|profile| text(&profile["description"], 300)),
        picture,
        banner: None,
        background: None,
        marks: Vec::new(),
    })
}

/// Whether an address is one out on the internet: not this machine, not the
/// network it is on, nothing set aside for anything else.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_broadcast() || v4.is_documentation() || v4.is_unspecified() || v4.is_multicast()
                || a == 0
                // Carrier-grade NAT, and what is reserved.
                || (a == 100 && (64..128).contains(&b))
                || a >= 240)
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public(IpAddr::V4(v4)),
            None => {
                let first = v6.segments()[0];
                !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast()
                    // Unique local, and link local.
                    || (first & 0xfe00) == 0xfc00
                    || (first & 0xffc0) == 0xfe80)
            }
        },
    }
}

/// Names looked up, less every address that isn't out on the internet: a DID names
/// any host it likes, and this service is not a way to reach what is beside it.
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let found = tokio::net::lookup_host((name.as_str(), 0)).await?;
            let public: Vec<std::net::SocketAddr> = found.filter(|address| is_public(address.ip())).collect();
            if public.is_empty() {
                return Err(format!("{} is not a public host", name.as_str()).into());
            }
            Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// A DID's file is small. Anything past this isn't one.
const MAX_FILE_BYTES: usize = 512 * 1024;

/// Fetching over HTTPS, from public hosts only.
pub struct Https(reqwest::Client);

impl Https {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self(
            reqwest::Client::builder()
                .user_agent("wyvrn-cards/1.0 (+https://wyvrn.app)")
                .https_only(true)
                // A DID's files are where its DID says, not where that place points.
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(6))
                .dns_resolver(std::sync::Arc::new(PublicOnly))
                .build()?,
        ))
    }
}

impl Fetch for Https {
    fn get<'a>(&'a self, url: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<String>, String>> + Send + 'a>> {
        Box::pin(async move {
            let parsed = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
            // A host written as an address never goes through the lookup above.
            let host = parsed.host_str().unwrap_or_default().trim_matches(['[', ']']);
            if host.parse::<IpAddr>().is_ok() || host.eq_ignore_ascii_case("localhost") {
                return Err(format!("{host} is not a public host"));
            }
            let mut response = self.0.get(parsed.clone()).send().await.map_err(|e| format!("couldn't reach {host}: {e}"))?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !response.status().is_success() {
                return Err(format!("couldn't reach {host} ({})", response.status()));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
                bytes.extend_from_slice(&chunk);
                if bytes.len() > MAX_FILE_BYTES {
                    return Err(format!("{host} answered with more than a DID's file would be"));
                }
            }
            String::from_utf8(bytes).map(Some).map_err(|_| format!("{url} is not text"))
        })
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_card_s_version_follows_what_it_shows() {
        let who = Who { did: "did:web:id.example:alice".into(), label: "alice via id.example".into(), name: Some("Alice".into()), description: None, picture: None, banner: None, background: None, marks: vec![] };
        assert_eq!(who.version(), who.clone().version());
        assert_eq!(who.version().len(), 12);
        let with_banner = Who { banner: Some("data:image/png;base64,AAAA".into()), ..who.clone() };
        assert_ne!(who.version(), with_banner.version());
        // Parts don't run into one another: a name moved into the description is a change.
        let moved = Who { name: None, description: Some("Alice".into()), ..who.clone() };
        assert_ne!(who.version(), moved.version());
    }
    use super::*;
    use serde_json::json;

    #[test]
    fn an_address_is_only_what_one_is_made_of() {
        assert!(is_address("did:web:id.wyvrn.app:frostyfrog"));
        assert!(is_address("did:webvh:QmRa6b5q:id.wyvrn.app:official:news"));
        assert!(is_address("did:web:localhost%3A8081:alice"));
        assert!(!is_address("did:peer:4zQm"));
        assert!(!is_address("did:web:id.wyvrn.app:<script>"));
        assert!(!is_address("did:web:id.wyvrn.app/..%2f"));
    }

    #[test]
    fn an_address_is_written_as_a_name_via_a_host() {
        assert_eq!(written("did:web:id.wyvrn.app:frostyfrog"), "frostyfrog via id.wyvrn.app");
        assert_eq!(written("did:webvh:Qm:id.wyvrn.app:official:news"), "official / news via id.wyvrn.app");
        assert_eq!(written("did:web:announce.wyvrn.app"), "announce.wyvrn.app");
    }

    #[test]
    fn only_a_bitmap_goes_into_a_card() {
        assert!(bitmap("data:image/png;base64,iVBORw0KGgo=").is_some());
        assert_eq!(bitmap("data:image/svg+xml;base64,PHN2Zy8+"), None);
        assert_eq!(bitmap("https://example.com/a.png"), None);
        assert_eq!(bitmap("data:image/png;base64,\"/><script>"), None);
    }

    #[test]
    fn an_invite_is_read_for_who_it_is_from() {
        let invitation = json!({
            "type": INVITATION, "id": "1", "from": "did:peer:4zQmExample",
            "body": { "goal_code": CONTACT_GOAL },
            "attachments": [{ "id": "profile", "data": { "json": {
                "type": PROFILE,
                "body": { "profile": { "displayName": " Alice ", "description": "Hello", "displayPicture": "#pfp" } },
                "attachments": [{ "id": "pfp", "media_type": "image/png", "data": { "base64": "iVBORw0KGgo" } }],
            }}}],
        });
        let oob = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(invitation.to_string());
        let who = of_invite(&oob).unwrap();
        assert_eq!((who.did.as_str(), who.label.as_str(), who.name.as_deref(), who.description.as_deref()), ("did:peer:4zQmExample", "Contact invite", Some("Alice"), Some("Hello")));
        assert!(who.picture.unwrap().starts_with("data:image/png;base64,"));
        assert!(who.marks.is_empty());

        let other = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({ "type": INVITATION, "from": "did:peer:4zQm", "body": { "goal_code": "something.else" } }).to_string());
        assert_eq!(of_invite(&other), None);
        assert_eq!(of_invite("not an invite"), None);
    }

    #[test]
    fn nothing_beside_this_service_is_public() {
        for private in ["127.0.0.1", "10.1.2.3", "192.168.1.1", "172.16.0.9", "169.254.169.254", "100.64.0.1", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:10.0.0.1"] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }
}
