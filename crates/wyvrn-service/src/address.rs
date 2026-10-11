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

/// The agent for an identity reached at `public_url`, and the DID document to
/// publish for it if it is a `did:web`.
pub fn build_agent(identity: Identity, public_url: &str, method: DidMethod) -> anyhow::Result<(Agent, Option<Value>)> {
    Ok(match method {
        DidMethod::Peer => (Agent::with_endpoint(identity, public_url)?, None),
        DidMethod::Web => {
            let did = did_web_for(public_url)?;
            let document = identity.did_document(&did, public_url);
            (Agent::with_did(identity, &did), Some(document))
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
/// root of a host, or under a path, is looked for. A service that isn't a
/// `did:web` has no document, and says so.
pub fn routes<S: Clone + Send + Sync + 'static>(did: String, document: Option<Value>) -> Router<S> {
    let serve_document = move || {
        let document = document.clone();
        async move {
            match document {
                Some(document) => Json(document).into_response(),
                None => not_a_did_web(),
            }
        }
    };
    Router::new()
        .route("/did", get(move || async move { did }))
        .route("/.well-known/did.json", get(serve_document.clone()))
        .route("/did.json", get(serve_document))
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

    #[test]
    fn a_did_method_is_read_from_a_setting() {
        assert_eq!(DidMethod::parse("Web", "X").unwrap(), DidMethod::Web);
        assert_eq!(DidMethod::parse("peer", "X").unwrap(), DidMethod::Peer);
        assert!(DidMethod::parse("key", "X_DID_METHOD").unwrap_err().to_string().contains("X_DID_METHOD"));
    }
}
