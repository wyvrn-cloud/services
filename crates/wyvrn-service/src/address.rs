//! A service's DID, and publishing it.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use didcomm_agent::{Agent, Identity};
use serde::Deserialize;
use serde_json::Value;

/// How a service is named.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DidMethod {
    /// A `did:peer:4` made from the identity's keys and the public URL. Needs
    /// nothing published, and changes if either does.
    #[default]
    Peer,
    /// A `did:web` at the public URL, which has to serve the DID document
    /// ([`routes`]) over HTTPS.
    Web,
}

impl DidMethod {
    /// From a setting's text: `"peer"` or `"web"`, in any case. `setting` names
    /// where it came from, for the error.
    pub fn parse(value: &str, setting: &str) -> anyhow::Result<Self> {
        match value.to_lowercase().as_str() {
            "peer" => Ok(Self::Peer),
            "web" => Ok(Self::Web),
            other => anyhow::bail!("{setting} is {other:?}; expected \"peer\" or \"web\""),
        }
    }
}

/// What a service that is a `did:web` publishes about itself.
#[derive(Clone)]
pub struct Published {
    /// Its DID document. It names the service's own website as its holder's (a
    /// `LinkedDomains` service), which [`routes`] then says back.
    pub document: Value,
    did: String,
    origin: String,
    /// Where it is, ending in `/`.
    public_url: String,
    /// The key the document signs and asserts as, to sign the website's half with.
    signing_jwk: String,
    /// Who the service says it is, if it says ([`Published::with_profile`]).
    profile: Option<wyvrn_hosted_did::write::ProfileFields>,
}

/// Who a service says it is, to someone looking its DID up before writing to it:
/// the `[profile]` section of its configuration. All of it is optional; a service
/// fills in what it is called anyway ([`ProfileConfig::or`]).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProfileConfig {
    pub name: Option<String>,
    pub description: Option<String>,
    /// A picture's file: PNG, JPEG, WebP or GIF, at most [`MAX_PICTURE_BYTES`].
    pub picture: Option<std::path::PathBuf>,
    /// A wide picture shown across the top of the profile (about 3 to 1), and one
    /// shown behind it. Files as `picture` is, of the same kinds and limit each.
    pub banner: Option<std::path::PathBuf>,
    pub background: Option<std::path::PathBuf>,
}

/// A profile is fetched whole by whoever looks the service up, pictures and all.
pub const MAX_PICTURE_BYTES: usize = 200 * 1024;

/// A picture's file as it goes into a profile: inside it, so that showing it
/// fetches nothing more.
fn picture_object(path: &std::path::Path, what: &str) -> anyhow::Result<Value> {
    use base64::Engine;
    let media_type = match path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => anyhow::bail!("the profile {what} {} is not a .png, .jpg, .webp or .gif", path.display()),
    };
    let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("reading the profile {what} {}: {e}", path.display()))?;
    if bytes.len() > MAX_PICTURE_BYTES {
        anyhow::bail!("the profile {what} {} is {} KB; at most {} KB", path.display(), bytes.len() / 1024, MAX_PICTURE_BYTES / 1024);
    }
    let url = format!("data:{media_type};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes));
    Ok(serde_json::json!({ "contentUrl": url, "encodingFormat": media_type }))
}

impl ProfileConfig {
    /// What the configuration says, overridden by the environment
    /// (`<prefix>_PROFILE_NAME`, `<prefix>_PROFILE_DESCRIPTION`,
    /// `<prefix>_PROFILE_PICTURE`, `<prefix>_PROFILE_BANNER`,
    /// `<prefix>_PROFILE_BACKGROUND`), and failing both, `name` and `description`:
    /// what the service is called anyway.
    pub fn or(&self, prefix: &str, name: &str, description: &str) -> Self {
        let env = |setting: &str| std::env::var(format!("{prefix}_PROFILE_{setting}")).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let said = |value: Option<String>, otherwise: &str| value.or_else(|| Some(otherwise.to_string())).filter(|v| !v.trim().is_empty());
        Self {
            name: said(env("NAME").or_else(|| self.name.clone()), name),
            description: said(env("DESCRIPTION").or_else(|| self.description.clone()), description),
            picture: env("PICTURE").map(Into::into).or_else(|| self.picture.clone()),
            banner: env("BANNER").map(Into::into).or_else(|| self.banner.clone()),
            background: env("BACKGROUND").map(Into::into).or_else(|| self.background.clone()),
        }
    }

    fn fields(&self) -> anyhow::Result<wyvrn_hosted_did::write::ProfileFields> {
        let pictured = |path: &Option<std::path::PathBuf>, what: &str| path.as_deref().map(|path| picture_object(path, what)).transpose();
        Ok(wyvrn_hosted_did::write::ProfileFields {
            // A service is nobody's person.
            kind: Some("Organization".to_string()),
            name: self.name.clone(),
            description: self.description.clone(),
            image: pictured(&self.picture, "picture")?,
            banner: pictured(&self.banner, "banner")?,
            background: pictured(&self.background, "background")?,
            ..Default::default()
        })
    }
}

const DAY: i64 = 24 * 60 * 60;

fn rfc3339(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0).unwrap_or_default().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

impl Published {
    /// The service's `/.well-known/did-configuration.json` (DIF Well Known DID
    /// Configuration): the service's DID saying, signed, that the website it is
    /// served from is its own. With the document naming the website, that is both
    /// halves of a linked domain, which an app can check and show.
    ///
    /// Made when asked for, dated from the start of the day and good for a year,
    /// so that a service left running never serves one that has run out.
    pub async fn did_configuration(&self, now: i64) -> anyhow::Result<Value> {
        let issued = now - now.rem_euclid(DAY);
        let credential = wyvrn_hosted_did::linkage::credential(&self.did, &self.origin, &self.signing_jwk, &format!("{}#key-1", self.did), &rfc3339(issued), &rfc3339(issued + 366 * DAY))
            .await
            .map_err(|why| anyhow::anyhow!(why))?;
        Ok(wyvrn_hosted_did::linkage::configuration(vec![credential]))
    }

    /// Has the service say who it is: its document names a whois (did:webvh's
    /// `#whois`, a `LinkedVerifiablePresentation`) at its own address, which
    /// [`routes`] serves. An app that looks the DID up shows it before anything is
    /// sent. Over HTTPS only, as the website's link is: tried locally over HTTP, the
    /// document is published without it.
    pub fn with_profile(mut self, profile: &ProfileConfig) -> anyhow::Result<Self> {
        if self.origin.is_empty() || (profile.name.is_none() && profile.description.is_none() && profile.picture.is_none()) {
            return Ok(self);
        }
        self.profile = Some(profile.fields()?);
        let endpoint = format!("{}whois.vp", self.public_url);
        if let Some(services) = self.document["service"].as_array_mut() {
            services.push(serde_json::json!({ "id": format!("{}#whois", self.did), "type": "LinkedVerifiablePresentation", "serviceEndpoint": endpoint }));
        }
        Ok(self)
    }

    /// The service's whois: its profile, as a presentation it signs. Dated from the
    /// start of the day, as its website's link is.
    pub async fn whois(&self, now: i64) -> anyhow::Result<Option<Value>> {
        let Some(profile) = &self.profile else { return Ok(None) };
        let issued = now - now.rem_euclid(DAY);
        let whois = wyvrn_hosted_did::write::whois(&self.did, profile, &self.signing_jwk, &format!("{}#key-1", self.did), &rfc3339(issued)).await.map_err(|why| anyhow::anyhow!(why))?;
        Ok(Some(whois))
    }
}

/// The agent for an identity reached at `public_url`, and what to publish for it
/// if it is a `did:web`.
pub fn build_agent(identity: Identity, public_url: &str, method: DidMethod) -> anyhow::Result<(Agent, Option<Published>)> {
    Ok(match method {
        DidMethod::Peer => (Agent::with_endpoint(identity, public_url)?, None),
        DidMethod::Web => {
            let did = did_web_for(public_url)?;
            let url = reqwest::Url::parse(public_url)?;
            let mut document = identity.did_document(&did, public_url);
            // A website can only be linked over HTTPS. One tried locally over HTTP
            // publishes its document without the link.
            let origin = wyvrn_hosted_did::linkage::origin_of(&url.origin().ascii_serialization());
            // The identity as it is saved holds each key as a JWK, secret and all.
            let saved: Value = serde_json::from_str(&identity.to_json()?)?;
            let signing_jwk = saved["verification_key"].to_string();
            let published = match origin {
                Some(origin) => {
                    // What it signs as, it also asserts as: that is what the
                    // website's half is checked against.
                    document["assertionMethod"] = serde_json::json!([format!("{did}#key-1")]);
                    if let Some(services) = document["service"].as_array_mut() {
                        services.push(serde_json::json!({ "id": format!("{did}#linked-domains"), "type": "LinkedDomains", "serviceEndpoint": origin }));
                    }
                    Published { document, did: did.clone(), origin, public_url: with_slash(public_url), signing_jwk, profile: None }
                }
                None => Published { document, did: did.clone(), origin: String::new(), public_url: with_slash(public_url), signing_jwk, profile: None },
            };
            (Agent::with_did(identity, &did), Some(published))
        }
    })
}

fn with_slash(url: &str) -> String {
    if url.ends_with('/') { url.to_string() } else { format!("{url}/") }
}

/// The `did:web` for a URL: host (`:port` percent-encoded), then any path segments
/// joined by `:` -- `https://example.com:8443/news/` → `did:web:example.com%3A8443:news`.
pub fn did_web_for(url: &str) -> anyhow::Result<String> {
    let url = reqwest::Url::parse(url)?;
    let host = url.host_str().ok_or_else(|| anyhow::anyhow!("{url} has no host"))?;
    let mut did = format!("did:web:{host}");
    if let Some(port) = url.port() {
        did.push_str(&format!("%3A{port}"));
    }
    for segment in url.path_segments().into_iter().flatten().filter(|s| !s.is_empty()) {
        did.push(':');
        did.push_str(segment);
    }
    Ok(did)
}

/// The routes that say who a service is: `GET /did` (the DID, as text), and its DID
/// document at `/.well-known/did.json` and `/did.json` -- where a `did:web` at the
/// root of a host, or under a path, is looked for -- and, beside it, its
/// `/.well-known/did-configuration.json` and its `/whois.vp` (who it says it is).
/// A service that isn't a `did:web` has none of them, and says so.
pub fn routes<S: Clone + Send + Sync + 'static>(did: String, published: Option<Published>) -> Router<S> {
    let document = published.as_ref().map(|published| published.document.clone());
    let whois_of = published.clone();
    let serve_document = move || {
        let document = document.clone();
        async move {
            match document {
                Some(document) => Json(document).into_response(),
                None => not_a_did_web(),
            }
        }
    };
    let serve_configuration = move || {
        let published = published.clone();
        async move {
            let Some(published) = published.filter(|published| !published.origin.is_empty()) else {
                return not_a_did_web();
            };
            match published.did_configuration(crate::now()).await {
                // Not kept by whoever asks: it is made afresh each day.
                Ok(configuration) => ([(axum::http::header::CACHE_CONTROL, "no-cache")], Json(configuration)).into_response(),
                Err(error) => crate::http::internal(error),
            }
        }
    };
    let serve_whois = move || {
        let published = whois_of.clone();
        async move {
            let Some(published) = published else { return not_a_did_web() };
            match published.whois(crate::now()).await {
                Ok(Some(whois)) => ([(axum::http::header::CONTENT_TYPE, "application/vp"), (axum::http::header::CACHE_CONTROL, "no-cache")], whois.to_string()).into_response(),
                Ok(None) => (StatusCode::NOT_FOUND, "this service publishes no profile").into_response(),
                Err(error) => crate::http::internal(error),
            }
        }
    };
    Router::new()
        .route("/whois.vp", get(serve_whois))
        .route("/did", get(move || async move { did }))
        .route("/.well-known/did.json", get(serve_document.clone()))
        .route("/did.json", get(serve_document))
        .route("/.well-known/did-configuration.json", get(serve_configuration))
}

fn not_a_did_web() -> Response {
    (StatusCode::NOT_FOUND, "this service's DID is not a did:web").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_service_says_who_it_is_in_a_whois_its_document_names_and_its_key_signed() {
        let (_, published) = build_agent(Identity::generate().unwrap(), "https://updates.example/", DidMethod::Web).unwrap();
        let picture = std::env::temp_dir().join(format!("wyvrn-profile-test-{}.png", std::process::id()));
        std::fs::write(&picture, [0x89, b'P', b'N', b'G']).unwrap();
        let profile = ProfileConfig { picture: Some(picture.clone()), banner: Some(picture.clone()), background: Some(picture.clone()), ..Default::default() }.or("WYVRN_TEST_NOTHING", "Updates", "What is new.");
        let published = published.unwrap().with_profile(&profile).unwrap();
        std::fs::remove_file(&picture).unwrap();

        let named = published.document["service"].as_array().unwrap().iter().find(|service| service["id"] == "did:web:updates.example#whois").unwrap();
        assert_eq!((named["type"].as_str(), named["serviceEndpoint"].as_str()), (Some("LinkedVerifiablePresentation"), Some("https://updates.example/whois.vp")));
        // As an app reads it: checked against the document, and the profile taken out.
        let whois = published.whois(1_800_000_000).await.unwrap().unwrap();
        let checked = wyvrn_hosted_did::whois::check(whois.to_string().as_bytes(), "did:web:updates.example", &published.document).unwrap();
        let read = wyvrn_hosted_did::whois::profile(&checked, "did:web:updates.example").unwrap();
        assert_eq!((read.kind.as_deref(), read.name.as_deref(), read.description.as_deref()), (Some("Organization"), Some("Updates"), Some("What is new.")));
        assert!(read.image.unwrap().url.starts_with("data:image/png;base64,"));
        // Its banner and its background the same way: inside the profile.
        assert!(read.banner.unwrap().is_inline() && read.background.unwrap().is_inline());

        // Over plain HTTP, and with nothing to say, there is none.
        let (_, local) = build_agent(Identity::generate().unwrap(), "http://localhost:8081/", DidMethod::Web).unwrap();
        assert!(local.unwrap().with_profile(&profile).unwrap().whois(0).await.unwrap().is_none());
        let (_, silent) = build_agent(Identity::generate().unwrap(), "https://quiet.example/", DidMethod::Web).unwrap();
        assert!(silent.unwrap().with_profile(&ProfileConfig::default()).unwrap().whois(0).await.unwrap().is_none());
    }

    #[test]
    fn a_did_web_is_the_host_then_the_path() {
        assert_eq!(did_web_for("https://example.com/").unwrap(), "did:web:example.com");
        assert_eq!(did_web_for("https://example.com:8443/news/").unwrap(), "did:web:example.com%3A8443:news");
        assert_eq!(did_web_for("https://example.com/a/b").unwrap(), "did:web:example.com:a:b");
        assert!(did_web_for("not a url").is_err());
    }

    #[tokio::test]
    async fn a_did_web_service_links_its_own_website_both_ways() {
        let identity = Identity::generate().unwrap();
        let (agent, published) = build_agent(identity, "https://id.example/", DidMethod::Web).unwrap();
        let published = published.unwrap();
        let did = agent.did();
        assert_eq!(did, "did:web:id.example");
        // The document names the website, and can sign assertions.
        assert_eq!(wyvrn_hosted_did::linkage::linked_origins(&published.document), ["https://id.example"]);
        assert_eq!(published.document["assertionMethod"], serde_json::json!(["did:web:id.example#key-1"]));
        // The website's half checks out against it, today and most of a year from now.
        let now = 1_791_750_000;
        let served = published.did_configuration(now).await.unwrap().to_string();
        let at = |seconds: i64| wyvrn_hosted_did::linkage::check(served.as_bytes(), "https://id.example", &did, &published.document, &rfc3339(seconds));
        assert_eq!(at(now), Ok(()));
        assert_eq!(at(now + 300 * DAY), Ok(()));
        assert!(at(now + 400 * DAY).unwrap_err().contains("expired"));
        // The same file all day, and a new one the next.
        assert_eq!(published.did_configuration(now + 60).await.unwrap()["linked_dids"][0]["issuanceDate"], published.did_configuration(now).await.unwrap()["linked_dids"][0]["issuanceDate"]);
        assert_ne!(published.did_configuration(now + DAY).await.unwrap()["linked_dids"][0]["issuanceDate"], published.did_configuration(now).await.unwrap()["linked_dids"][0]["issuanceDate"]);

        // A did:peer service has nothing to publish; one tried over HTTP, no website to link.
        assert!(build_agent(Identity::generate().unwrap(), "https://id.example/", DidMethod::Peer).unwrap().1.is_none());
        let local = build_agent(Identity::generate().unwrap(), "http://localhost:8081/", DidMethod::Web).unwrap().1.unwrap();
        assert!(wyvrn_hosted_did::linkage::linked_origins(&local.document).is_empty());
    }

    #[test]
    fn a_did_method_is_read_from_a_setting() {
        assert_eq!(DidMethod::parse("Web", "X").unwrap(), DidMethod::Web);
        assert_eq!(DidMethod::parse("peer", "X").unwrap(), DidMethod::Peer);
        assert!(DidMethod::parse("key", "X_DID_METHOD").unwrap_err().to_string().contains("X_DID_METHOD"));
    }
}
