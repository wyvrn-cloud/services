//! A reply sent to the sender's own address: their request is answered before it
//! is sent, so it is never held open by it, and never says what their endpoint
//! answered (security review WYV-92).

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
async fn the_sender_is_answered_before_the_reply_is_sent_and_never_told_what_its_endpoint_said() {
    const MARKER: &str = "internal-secret-7f3a";
    // The "sender's endpoint": slow, then refusing with something private.
    let reached = Arc::new(tokio::sync::Notify::new());
    let told = reached.clone();
    let endpoint = listen(Router::new().route(
        "/",
        post(move || {
            let told = told.clone();
            async move {
                told.notify_one();
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                (StatusCode::INTERNAL_SERVER_ERROR, MARKER)
            }
        }),
    ))
    .await;

    // The service: answers every message, to the sender's own endpoint.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let service = Arc::new(Echo { agent: Agent::with_endpoint(Identity::generate().unwrap(), &url).unwrap() });
    let service_did = service.agent.did();
    let router = Router::new().route("/", post(|State(service): State<Arc<Echo>>, body: Bytes| http::receive(service, body))).with_state(service);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let sender = Agent::with_endpoint(Identity::generate().unwrap(), &endpoint).unwrap();
    // No return_route: the reply goes to the sender's endpoint, after the send is answered.
    let ping = json!({ "type": "https://didcomm.org/trust-ping/2.0/ping", "body": { "response_requested": true } });
    let started = std::time::Instant::now();
    let answered = sender.send(&service_did, &ping).await;
    assert!(answered.is_ok(), "{answered:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(3), "the send waited on the reply: {:?}", started.elapsed());
    assert!(!format!("{answered:?}").contains(MARKER));
    // And the reply is still sent.
    tokio::time::timeout(std::time::Duration::from_secs(10), reached.notified()).await.expect("the reply was sent");
}
