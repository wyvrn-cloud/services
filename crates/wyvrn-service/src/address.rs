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
    /// The key the document signs and asserts as, to sign the website's half with.
    signing_jwk: String,
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
                    Published { document, did: did.clone(), origin, signing_jwk }
                }
                None => Published { document, did: did.clone(), origin: String::new(), signing_jwk },
            };
            (Agent::with_did(identity, &did), Some(published))
        }
    })
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
/// `/.well-known/did-configuration.json`. A service that isn't a `did:web` has
/// neither, and says so.
pub fn routes<S: Clone + Send + Sync + 'static>(did: String, published: Option<Published>) -> Router<S> {
    let document = published.as_ref().map(|published| published.document.clone());
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
    Router::new()
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
