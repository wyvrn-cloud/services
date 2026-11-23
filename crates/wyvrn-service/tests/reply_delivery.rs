//! A reply that couldn't be delivered: the sender is told so in fixed words, and
//! never what its endpoint answered (security review WYV-92).

use std::sync::Arc;

use axum::{body::Bytes, extract::State, http::StatusCode, routing::post, Router};
use didcomm_agent::{Agent, Identity};
use serde_json::{json, Value};
use wyvrn_service::{http, Incoming, Service};

struct Echo {
    agent: Agent,
}

impl Service for Echo {
    fn agent(&self) -> &Agent {
        &self.agent
    }

    async fn answer(&self, incoming: &Incoming<'_>) -> anyhow::Result<Option<Value>> {
        Ok(Some(incoming.received.reply("https://didcomm.org/trust-ping/2.0/ping-response", json!({}))))
    }
}

async fn listen(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    url
}

#[tokio::test]
async fn what_the_senders_endpoint_answered_is_not_sent_back() {
    const MARKER: &str = "internal-secret-7f3a";
    // The "sender's endpoint": an internal service answering with something private.
    let endpoint = listen(Router::new().route("/", post(|| async { (StatusCode::INTERNAL_SERVER_ERROR, MARKER) }))).await;

    // The service: answers every message, to the sender's own endpoint.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let service = Arc::new(Echo { agent: Agent::with_endpoint(Identity::generate().unwrap(), &url).unwrap() });
    let service_did = service.agent.did();
    let router = Router::new().route("/", post(|State(service): State<Arc<Echo>>, body: Bytes| http::receive(service, body))).with_state(service);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let sender = Agent::with_endpoint(Identity::generate().unwrap(), &endpoint).unwrap();
    // No return_route: the reply goes to the sender's endpoint, which refuses it.
    let ping = json!({ "type": "https://didcomm.org/trust-ping/2.0/ping", "body": { "response_requested": true } });
    let error = sender.send(&service_did, &ping).await.expect_err("the service couldn't deliver its reply").to_string();
    assert!(error.contains("502"), "{error}");
    assert!(error.contains(http::DELIVERY_FAILED), "{error}");
    assert!(!error.contains(MARKER), "the endpoint's answer was sent back: {error}");
}
