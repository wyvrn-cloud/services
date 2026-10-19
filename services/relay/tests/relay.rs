//! The relay run for real: its server, a client agent holding a WebSocket to it,
//! and a stand-in endpoint behind it to post to and open a socket on.

use std::sync::Arc;

use axum::{
    extract::ws::{Message, WebSocketUpgrade},
    routing::get,
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use didcomm_agent::{Agent, AgentError, Features, Identity};
use serde_json::{json, Value};
use wyvrn_relay::{
    config::RelayConfig,
    relay::{Carries, Relay, PIURI},
    server::{self, AppState},
    store::Store,
};

/// Answers a post with `pong:` and what was posted, and echoes a WebSocket.
async fn start_endpoint() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/", axum::routing::post(|body: axum::body::Bytes| async move { [b"pong:".as_slice(), &body].concat() }))
        .route(
            "/ws",
            get(|upgrade: WebSocketUpgrade| async move {
                upgrade.on_upgrade(|mut socket| async move {
                    while let Some(Ok(Message::Text(text))) = socket.recv().await {
                        if socket.send(Message::Text(format!("echo:{text}").into())).await.is_err() {
                            break;
                        }
                    }
                })
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("127.0.0.1:{}", address.port())
}

/// The relay, allowed to reach this machine (where the stand-in is), and its DID.
async fn start_relay(carries: Carries) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws = format!("ws://{}/", listener.local_addr().unwrap());
    // A did:peer whose one endpoint is the socket, so a client finds it there.
    let agent = Agent::with_endpoint(Identity::generate().unwrap(), &ws).unwrap().with_features(Features::standard().with_protocol(PIURI, &["relay"]));
    let did = agent.did();
    let config = RelayConfig { carries, allow: vec!["127.0.0.1".into()], max_bytes: 1024 * 1024, ..Default::default() };
    let state = Arc::new(AppState { agent, relay: Relay::new(config, Store::scratch().unwrap()).unwrap(), did_document: None });
    let app = server::router(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    did
}

fn client() -> Agent {
    // Answers come back on the socket, so its own endpoint is never used.
    Agent::with_endpoint(Identity::generate().unwrap(), "http://127.0.0.1:9/").unwrap()
}

fn message(kind: &str, body: Value) -> Value {
    json!({ "id": uuid::Uuid::new_v4().to_string(), "type": format!("{PIURI}/{kind}"), "body": body })
}

fn attachment_text(message: &Value) -> String {
    String::from_utf8(wyvrn_relay::relay::attached(message).unwrap()).unwrap()
}

#[tokio::test]
async fn a_granted_client_posts_through_the_relay_and_opens_a_socket_through_it() {
    let endpoint = start_endpoint().await;
    let relay = start_relay(Carries::All).await;
    let alice = client();
    let mut socket = alice.connect_websocket(&relay).await.unwrap();

    // What it carries, asked as discover-features asks.
    let asked = json!({ "id": uuid::Uuid::new_v4().to_string(), "type": "https://didcomm.org/discover-features/2.0/queries",
        "body": { "queries": [{ "feature-type": "protocol", "match": PIURI }] } });
    let disclosed = socket.request(&asked).await.unwrap();
    let disclosures = disclosed.message["body"]["disclosures"].as_array().unwrap().clone();
    assert!(disclosures.contains(&json!({ "feature-type": "goal-code", "id": "app.wyvrn.relay.all" })), "{disclosures:?}");

    // Nothing is carried before a grant.
    let early = message("send", json!({ "endpoint": format!("http://{endpoint}/"), "content_type": "text/plain" }));
    match socket.request(&early).await {
        Err(AgentError::Problem { code, .. }) => assert_eq!(code, "e.p.req.not-granted"),
        other => panic!("{other:?}"),
    }

    let grant = socket.request(&message("request", json!({}))).await.unwrap();
    assert_eq!(grant.message_type(), format!("{PIURI}/grant"));
    assert_eq!(grant.message["body"]["carries"], "all");

    let mut send = message("send", json!({ "endpoint": format!("http://{endpoint}/"), "content_type": "text/plain" }));
    send["attachments"] = json!([{ "id": "message", "data": { "base64": STANDARD.encode("hello") } }]);
    let sent = socket.request(&send).await.unwrap();
    assert_eq!(sent.message["body"]["status"], 200);
    assert_eq!(attachment_text(&sent.message), "pong:hello");

    let open = message("open", json!({ "endpoint": format!("ws://{endpoint}/ws") }));
    let opened = socket.request(&open).await.unwrap();
    assert_eq!(opened.message_type(), format!("{PIURI}/opened"));
    let mut frame = json!({ "id": uuid::Uuid::new_v4().to_string(), "type": format!("{PIURI}/frame"), "thid": open["id"], "body": { "text": true } });
    frame["attachments"] = json!([{ "id": "frame", "data": { "base64": STANDARD.encode("ping") } }]);
    socket.send(&frame).await.unwrap();
    let back = socket.next_message().await.unwrap().unwrap();
    assert_eq!(back.message_type(), format!("{PIURI}/frame"));
    assert_eq!(back.message["thid"], open["id"]);
    assert_eq!(attachment_text(&back.message), "echo:ping");

    // Somewhere this machine's operator didn't allow is refused.
    let mut elsewhere = message("send", json!({ "endpoint": "http://10.255.255.1/", "content_type": "text/plain" }));
    elsewhere["attachments"] = json!([{ "id": "message", "data": { "base64": STANDARD.encode("x") } }]);
    match socket.request(&elsewhere).await {
        Err(AgentError::Problem { code, .. }) => assert_eq!(code, "e.p.msg.endpoint-refused"),
        other => panic!("{other:?}"),
    }

    let released = socket.request(&message("release", json!({}))).await.unwrap();
    assert_eq!(released.message_type(), format!("{PIURI}/released"));
}

#[tokio::test]
async fn a_relay_that_carries_sends_hands_back_answers_and_opens_nothing() {
    let endpoint = start_endpoint().await;
    let relay = start_relay(Carries::Sends).await;
    let bob = client();
    let mut socket = bob.connect_websocket(&relay).await.unwrap();
    socket.request(&message("request", json!({}))).await.unwrap();

    let mut send = message("send", json!({ "endpoint": format!("http://{endpoint}/"), "content_type": "text/plain" }));
    send["attachments"] = json!([{ "id": "message", "data": { "base64": STANDARD.encode("hello") } }]);
    let sent = socket.request(&send).await.unwrap();
    assert_eq!(sent.message["body"]["status"], 200);
    // A service may answer on the same connection, so the answer comes back.
    assert_eq!(attachment_text(&sent.message), "pong:hello");

    match socket.request(&message("open", json!({ "endpoint": format!("ws://{endpoint}/ws") }))).await {
        Err(AgentError::Problem { code, .. }) => assert_eq!(code, "e.p.req.not-carried"),
        other => panic!("{other:?}"),
    }
}

