//! `outbound-relay/1.0`'s relay side, apart from the socket it arrives on: who
//! is granted, what a `send` posts and what it hands back, and opening a
//! connection for a client.

use std::time::Duration;

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::RelayConfig;
use crate::guard::Guard;
use crate::store::Store;

pub const PIURI: &str = "https://wyvrn.app/outbound-relay/1.0";
pub const REQUEST: &str = "https://wyvrn.app/outbound-relay/1.0/request";
pub const GRANT: &str = "https://wyvrn.app/outbound-relay/1.0/grant";
pub const SEND: &str = "https://wyvrn.app/outbound-relay/1.0/send";
pub const SENT: &str = "https://wyvrn.app/outbound-relay/1.0/sent";
pub const OPEN: &str = "https://wyvrn.app/outbound-relay/1.0/open";
pub const OPENED: &str = "https://wyvrn.app/outbound-relay/1.0/opened";
pub const FRAME: &str = "https://wyvrn.app/outbound-relay/1.0/frame";
pub const CLOSE: &str = "https://wyvrn.app/outbound-relay/1.0/close";
pub const RELEASE: &str = "https://wyvrn.app/outbound-relay/1.0/release";
pub const RELEASED: &str = "https://wyvrn.app/outbound-relay/1.0/released";
pub const PROBLEM_REPORT: &str = "https://didcomm.org/report-problem/2.0/problem-report";

/// What a relay carries, as its operator set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Carries {
    Sends,
    All,
}

impl Carries {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sends => "sends",
            Self::All => "all",
        }
    }

    /// The goal code it is disclosed as.
    pub fn goal_code(self) -> &'static str {
        match self {
            Self::Sends => "app.wyvrn.relay.sends",
            Self::All => "app.wyvrn.relay.all",
        }
    }
}

/// A refusal, as a problem-report's code and words.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub code: &'static str,
    pub comment: String,
    pub args: Vec<String>,
}

impl Refusal {
    fn new(code: &'static str, comment: impl Into<String>) -> Self {
        Self { code, comment: comment.into(), args: Vec::new() }
    }
    pub fn not_allowed() -> Self {
        Self::new("e.p.req.not-allowed", "this relay doesn't take you")
    }
    pub fn not_granted() -> Self {
        Self::new("e.p.req.not-granted", "ask this relay first (request)")
    }
    pub fn not_carried(what: &str) -> Self {
        Self::new("e.p.req.not-carried", format!("this relay doesn't carry {what}"))
    }
    pub fn endpoint(why: String) -> Self {
        Self::new("e.p.msg.endpoint-refused", why)
    }
    pub fn too_large(limit: usize) -> Self {
        Self { code: "e.p.msg.too-large", comment: format!("past this relay's limit of {limit} bytes"), args: vec![limit.to_string()] }
    }
    pub fn unreachable(why: impl std::fmt::Display) -> Self {
        Self::new("e.p.xfer.unreachable", why.to_string())
    }
    pub fn malformed(why: &str) -> Self {
        Self::new("e.p.msg.malformed", why)
    }

    /// The problem-report body, pointing at the message refused.
    pub fn body(&self) -> Value {
        json!({ "code": self.code, "comment": self.comment, "args": self.args })
    }
}

/// The relay's working parts.
pub struct Relay {
    pub config: RelayConfig,
    pub store: Store,
    pub guard: Guard,
    /// For posts: every connection it makes goes through the guard's resolver.
    http: reqwest::Client,
    /// For WebSockets: the same, without the whole-request timeout, which would cap
    /// how long a socket may stay open.
    sockets: reqwest::Client,
}

/// What a `send` came to: the `sent` body, and the answer's bytes to attach.
pub struct Posted {
    pub body: Value,
    pub answer: Option<Vec<u8>>,
}

/// The one attachment's bytes (`data.base64`).
pub fn attached(message: &Value) -> Option<Vec<u8>> {
    decode(message["attachments"].as_array()?.first()?["data"]["base64"].as_str()?)
}

/// Base64 however it is written: the DIDComm library hands attachments on as
/// base64url without padding, whatever the sender wrote.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let plain: String = text.trim_end_matches('=').chars().map(|c| match c {
        '-' => '+',
        '_' => '/',
        c => c,
    }).collect();
    base64::engine::general_purpose::STANDARD_NO_PAD.decode(plain).ok()
}

impl Relay {
    pub fn new(config: RelayConfig, store: Store) -> anyhow::Result<Self> {
        let guard = Guard::new(&config.allow);
        let resolver = std::sync::Arc::new(guard.clone());
        let http = reqwest::Client::builder()
            .dns_resolver(resolver.clone())
            .timeout(Duration::from_secs(config.timeout_seconds))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let sockets = reqwest::Client::builder().dns_resolver(resolver).connect_timeout(Duration::from_secs(config.timeout_seconds)).build()?;
        Ok(Self { config, store, guard, http, sockets })
    }

    /// Whether `did` may register at all.
    fn takes(&self, did: &str) -> bool {
        self.config.open || self.config.clients.iter().any(|client| client == did)
    }

    pub fn granted(&self, did: &str) -> Result<(), Refusal> {
        match self.store.granted(did) {
            Ok(true) if self.takes(did) => Ok(()),
            _ => Err(Refusal::not_granted()),
        }
    }

    pub fn request(&self, did: &str, now: i64) -> Result<Value, Refusal> {
        if !self.takes(did) {
            return Err(Refusal::not_allowed());
        }
        self.store.grant(did, now).map_err(Refusal::unreachable)?;
        tracing::info!(client = %did, "granted");
        Ok(json!({ "carries": self.config.carries.as_str(), "max_bytes": self.config.max_bytes }))
    }

    pub fn release(&self, did: &str) -> Result<Value, Refusal> {
        self.store.release(did).map_err(Refusal::unreachable)?;
        Ok(json!({}))
    }

    /// Posts what `send` carries, for `did`.
    pub async fn send(&self, did: &str, message: &Value) -> Result<Posted, Refusal> {
        self.granted(did)?;
        let body = &message["body"];
        let endpoint = body["endpoint"].as_str().ok_or_else(|| Refusal::malformed("send needs an endpoint"))?;
        let content_type = body["content_type"].as_str().unwrap_or("application/didcomm-encrypted+json");
        let url = self.guard.check(endpoint, &["http", "https"]).await.map_err(Refusal::endpoint)?;
        let bytes = attached(message).ok_or_else(|| Refusal::malformed("send carries the message as its one attachment, in base64"))?;
        if bytes.len() > self.config.max_bytes {
            return Err(Refusal::too_large(self.config.max_bytes));
        }
        let response = self.http.post(url).header(reqwest::header::CONTENT_TYPE, content_type).body(bytes).send().await.map_err(|e| Refusal::unreachable(format!("{endpoint}: {e}")))?;
        let status = response.status().as_u16();
        let answer_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
        let answer = response.bytes().await.map_err(|e| Refusal::unreachable(format!("{endpoint}: {e}")))?;
        let mut body = json!({ "status": status });
        // A relay that carries sends says whether it was taken, and nothing of the answer.
        let answer = (self.config.carries == Carries::All && !answer.is_empty()).then(|| answer.to_vec());
        if let (Some(answer_type), Some(_)) = (answer_type, &answer) {
            body["content_type"] = json!(answer_type);
        }
        if answer.as_ref().is_some_and(|answer| answer.len() > self.config.max_bytes) {
            return Err(Refusal::too_large(self.config.max_bytes));
        }
        Ok(Posted { body, answer })
    }

    /// Opens the WebSocket an `open` asks for, for `did`.
    pub async fn open(&self, did: &str, message: &Value) -> Result<reqwest_websocket::WebSocket, Refusal> {
        self.granted(did)?;
        if self.config.carries != Carries::All {
            return Err(Refusal::not_carried("WebSockets"));
        }
        let endpoint = message["body"]["endpoint"].as_str().ok_or_else(|| Refusal::malformed("open needs an endpoint"))?;
        let url = self.guard.check(endpoint, &["ws", "wss"]).await.map_err(Refusal::endpoint)?;
        use reqwest_websocket::Upgrade;
        let response = self.sockets.get(url).upgrade().send().await.map_err(|e| Refusal::unreachable(format!("{endpoint}: {e}")))?;
        response.into_websocket().await.map_err(|e| Refusal::unreachable(format!("{endpoint}: {e}")))
    }

    /// The discover-features disclosures to add to the agent's own: what it
    /// carries, as a goal code, for a query that asks after the protocol or it.
    pub fn disclosures(&self, queries: &Value) -> Vec<Value> {
        let goal = self.config.carries.goal_code();
        let asked = queries["queries"].as_array().into_iter().flatten().any(|query| {
            let pattern = query["match"].as_str().unwrap_or_default();
            match query["feature-type"].as_str() {
                Some("protocol") => didcomm_agent::features::matches(pattern, PIURI),
                Some("goal-code") => didcomm_agent::features::matches(pattern, goal),
                _ => false,
            }
        });
        if asked {
            vec![json!({ "feature-type": "goal-code", "id": goal })]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relay(carries: Carries, open: bool) -> Relay {
        Relay::new(RelayConfig { carries, open, clients: vec!["did:example:listed".into()], ..Default::default() }, Store::scratch().unwrap()).unwrap()
    }

    #[test]
    fn a_closed_relay_grants_only_its_clients() {
        let closed = relay(Carries::All, false);
        assert_eq!(closed.request("did:example:stranger", 1), Err(Refusal::not_allowed()));
        assert_eq!(closed.request("did:example:listed", 1).unwrap()["carries"], "all");
        assert!(closed.granted("did:example:listed").is_ok());
        closed.release("did:example:listed").unwrap();
        assert_eq!(closed.granted("did:example:listed"), Err(Refusal::not_granted()));
    }

    #[tokio::test]
    async fn sends_need_a_grant_and_a_public_endpoint() {
        let open = relay(Carries::Sends, true);
        let message = json!({ "body": { "endpoint": "http://127.0.0.1:1/" }, "attachments": [{ "data": { "base64": "aGk=" } }] });
        assert_eq!(open.send("did:example:a", &message).await.err().unwrap().code, "e.p.req.not-granted");
        open.request("did:example:a", 1).unwrap();
        assert_eq!(open.send("did:example:a", &message).await.err().unwrap().code, "e.p.msg.endpoint-refused");
        let socket = json!({ "body": { "endpoint": "wss://1.1.1.1/" } });
        assert_eq!(open.open("did:example:a", &socket).await.err().unwrap().code, "e.p.req.not-carried");
    }

    #[test]
    fn what_it_carries_is_disclosed_to_whoever_asks_after_the_protocol() {
        let all = relay(Carries::All, true);
        let asked = json!({ "queries": [{ "feature-type": "protocol", "match": "https://wyvrn.app/outbound-relay/*" }] });
        assert_eq!(all.disclosures(&asked), vec![json!({ "feature-type": "goal-code", "id": "app.wyvrn.relay.all" })]);
        let other = json!({ "queries": [{ "feature-type": "protocol", "match": "https://didcomm.org/*" }] });
        assert!(all.disclosures(&other).is_empty());
    }
}
