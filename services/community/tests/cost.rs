//! What a request costs the server in statements to its database. Each one is a
//! round trip to MySQL, wherever that is: at 25 ms apart (seen 2026-11-02 between
//! community.wyvrn.app and its database), 200 statements is five seconds.
//!
//! A test binary of its own, with one test, so the process-wide count
//! (`wyvrn_service::db::statements_said`) is this test's alone. Run with
//! `--nocapture` to see the table.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{body::Bytes, extract::State, routing::post, Router};
use didcomm_agent::{Agent, Identity};
use serde_json::{json, Value};
use wyvrn_community::{
    community::Community,
    server::{self, AppState},
    store::{Role, Store},
    web::{self, Web},
};
use wyvrn_service::db::{statement_tally, statements_said, tally_statements};

const C: &str = "https://wyvrn.app/community/1.0/";

/// The official community's size when this was measured.
const MEMBERS: usize = 20;
const CHANNELS: usize = 4;

struct Peer {
    agent: Arc<Agent>,
    server: String,
}

impl Peer {
    async fn start(server: &str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let agent = Arc::new(Agent::with_endpoint(Identity::generate().unwrap(), &url).unwrap());
        let app = Router::new()
            .route(
                "/",
                post(|State(agent): State<Arc<Agent>>, body: Bytes| async move {
                    let _ = agent.receive(&body).await;
                }),
            )
            .with_state(agent.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { agent, server: server.to_string() }
    }

    fn did(&self) -> String {
        self.agent.did()
    }

    async fn community(&self, kind: &str, body: Value) -> Value {
        let message = json!({ "type": format!("{C}{kind}"), "body": body, "return_route": "all" });
        self.agent.send(&self.server, &message).await.expect("sent").expect("an answer").message
    }
}

async fn start() -> (String, Arc<AppState>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let config: wyvrn_community::config::Config = toml::from_str(&format!(
        "[server]\nbind = \"unused\"\npublic_url = \"{url}\"\n[identity]\npath = \"unused\"\n[store]\npath = \"unused\"\n[community]\nname = \"Costs\"\n[web]\nrequests_per_minute = 1000\n"
    ))
    .unwrap();
    let (agent, did_document) = server::build_agent(&config, Identity::generate().unwrap()).unwrap();
    let did = agent.did();
    let store = Store::in_memory().unwrap();
    let web = Web::new(&config).unwrap().map(Arc::new);
    web::settle_new(&store).unwrap();
    let mut community = Community::new(store, config.community.clone(), Some(wyvrn_service::profile::Kept::scratch().unwrap())).unwrap();
    community.public_url = url.clone();
    community.web_url = web.as_ref().map(|w| w.url.to_string());
    let mut state = AppState::new(agent, Default::default(), community, did_document, didcomm_quickstart::default_resolver());
    state.web = web;
    let state = Arc::new(state);
    tokio::spawn(server::run_deliveries(state.clone()));
    let app = server::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap() });
    (did, state)
}

/// Statements said while `ask` runs, until its answer is back, and which. Whatever
/// the last request left going in the background is let finish first.
async fn cost(ask: impl std::future::Future<Output = Value>) -> (u64, Value, Vec<(String, u64)>) {
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    tally_statements(true);
    let before = statements_said();
    let answer = ask.await;
    let said = statements_said() - before;
    let tally = statement_tally();
    tally_statements(false);
    (said, answer, tally)
}

#[tokio::test]
async fn what_each_request_costs_in_statements() {
    let (did, state) = start().await;
    let mut peers = Vec::new();
    for n in 0..MEMBERS {
        let peer = Peer::start(&did).await;
        state.community.store.set_member(&peer.did(), if n == 0 { Role::Admin } else { Role::Member }, 1).unwrap();
        peers.push(peer);
    }
    let admin = &peers[0];
    let member = &peers[1];
    let mut channels = Vec::new();
    for n in 0..CHANNELS {
        let made = admin.community("create-channel", json!({ "name": format!("channel-{n}") })).await;
        channels.push(made["body"]["created"].as_str().expect("a channel made").to_string());
    }
    for peer in &peers {
        peer.community("join", json!({})).await;
        peer.community("set-profile", json!({ "profile": { "displayName": format!("Person {}", &peer.did()[12..18]) } })).await;
    }
    for n in 0..30 {
        let peer = &peers[n % MEMBERS];
        peer.community("post", json!({ "channel": channels[n % CHANNELS], "id": uuid::Uuid::new_v4().to_string(), "content": format!("post {n}") })).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let mut rows = Vec::new();
    let mut measure = |name: &str, (statements, answer, tally): (u64, Value, Vec<(String, u64)>)| {
        let kind = answer["type"].as_str().unwrap_or_default().rsplit('/').next().unwrap_or_default().to_string();
        assert!(!kind.contains("problem"), "{name}: {answer}");
        if std::env::var("COST_DETAIL").is_ok() {
            println!("\n{name}: {statements}");
            for (sql, n) in tally.iter().take(12) {
                println!("  {n:>4}  {}", sql.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(150).collect::<String>());
            }
        }
        rows.push((name.to_string(), statements, kind));
    };
    measure("members-request (admin)", cost(admin.community("members-request", json!({}))).await);
    measure("members-request (member)", cost(member.community("members-request", json!({}))).await);
    measure("join (already in: the community)", cost(member.community("join", json!({}))).await);
    measure("changes-request", cost(member.community("changes-request", json!({ "after_rev": 0 }))).await);
    measure("history-request", cost(member.community("history-request", json!({ "channel": channels[0] }))).await);
    measure(
        "post",
        cost(member.community("post", json!({ "channel": channels[0], "id": uuid::Uuid::new_v4().to_string(), "content": "measured" }))).await,
    );
    measure("set-web (admin)", cost(admin.community("set-web", json!({ "page": true }))).await);
    // What is sent to everyone else afterwards, now in the background.
    let before = statements_said();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let after = statements_said() - before;

    println!("\n{MEMBERS} members, {CHANNELS} channels:");
    for (name, statements, kind) in &rows {
        println!("  {name:<34} {statements:>5} statements  -> {kind}");
    }
    println!("  (then in the background:   {after:>5} statements telling everyone else)");
}
