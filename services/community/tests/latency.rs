//! How long a request takes with the database far away: the production host's
//! MySQL is 23 ms from it (2026-11-26), and every round trip to it is paid in
//! full. Runs only where WYVRN_TEST_DATABASE_URL names a MySQL server
//! (`mysql://user:password@host:port`), best behind something that delays each
//! round trip as the real one does. Run with `--nocapture` to see the table:
//! each request's time, the statements the store said, and the round trips
//! MySQL counted (prepares and closes included).
//!
//! A test binary of its own, so the process-wide statement count is its alone.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{body::Bytes, extract::State, routing::post, Router};
use didcomm_agent::{Agent, Identity};
use mysql::prelude::Queryable;
use serde_json::{json, Value};
use wyvrn_community::{
    community::Community,
    server::{self, AppState},
    store::{Role, Store},
};
use wyvrn_service::db::{statement_tally, statements_said, tally_statements, Location};

const C: &str = "https://wyvrn.app/community/1.0/";
/// The community the burst was measured in (#gen3r4-ル, Frosty's Pillow Fort).
const MEMBERS: usize = 6;

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

    async fn community(&self, kind: &str, body: Value) -> Value {
        let message = json!({ "type": format!("{C}{kind}"), "body": body, "return_route": "all" });
        self.agent.send(&self.server, &message).await.expect("sent").expect("an answer").message
    }
}

/// MySQL's own count of what it was asked: every round trip, prepares included.
struct RoundTrips(mysql::Conn);

impl RoundTrips {
    fn each(&mut self) -> Vec<(String, u64)> {
        let rows: Vec<(String, String)> = self.0.query("SHOW GLOBAL STATUS WHERE Variable_name IN ('Com_stmt_prepare', 'Com_stmt_execute', 'Com_stmt_close', 'Com_stmt_reset', 'Questions')").unwrap();
        rows.into_iter().map(|(name, n)| (name, n.parse().unwrap_or(0))).collect()
    }

    fn now(&mut self) -> u64 {
        // `Questions`: everything a client asked and waited for, executions of
        // prepared statements included (prepares and closes aren't), and this.
        let rows: Vec<(String, String)> = self.0.query("SHOW GLOBAL STATUS WHERE Variable_name IN ('Questions', 'Com_stmt_prepare')").unwrap();
        rows.iter().map(|(_, n)| n.parse::<u64>().unwrap_or(0)).sum::<u64>() - 1
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_post_with_the_database_far_away() {
    let Some(url) = std::env::var("WYVRN_TEST_DATABASE_URL").ok().filter(|url| !url.is_empty()) else { return };
    let server_url = url.trim_end_matches('/').to_string();
    let database = format!("wyvrn_test_latency_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    let mut trips = RoundTrips(mysql::Conn::new(mysql::Opts::from_url(&server_url).unwrap()).unwrap());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let config: wyvrn_community::config::Config = toml::from_str(&format!(
        "[server]\nbind = \"unused\"\npublic_url = \"{url}\"\n[identity]\npath = \"unused\"\n[store]\npath = \"unused\"\n[community]\nname = \"Far away\"\n"
    ))
    .unwrap();
    let (agent, did_document) = server::build_agent(&config, Identity::generate().unwrap()).unwrap();
    let did = agent.did();
    // As the host keeps a community: tables of its own on a shared server.
    let store = Store::open(&Location::Shared { server: server_url.clone(), database: database.clone(), prefix: "c1_".into() }).unwrap();
    let community = Community::new(store, config.community.clone(), Some(wyvrn_service::profile::Kept::scratch().unwrap())).unwrap();
    let state = Arc::new(AppState::new(agent, Default::default(), community, did_document, didcomm_quickstart::default_resolver()));
    tokio::spawn(server::run_deliveries(state.clone()));
    let app = server::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap() });

    let mut peers = Vec::new();
    for n in 0..MEMBERS {
        let peer = Peer::start(&did).await;
        state.community.store.set_member(&peer.agent.did(), if n == 0 { Role::Admin } else { Role::Member }, 1).unwrap();
        peers.push(peer);
    }
    let made = peers[0].community("create-channel", json!({ "name": "general" })).await;
    let channel = made["body"]["created"].as_str().expect("a channel made").to_string();
    for peer in &peers {
        peer.community("join", json!({})).await;
        peer.community("set-profile", json!({ "profile": { "displayName": format!("Person {}", &peer.agent.did()[12..18]) } })).await;
    }
    for n in 0..10 {
        peers[n % MEMBERS].community("post", json!({ "channel": channel, "id": uuid::Uuid::new_v4().to_string(), "content": format!("before {n}") })).await;
    }
    tokio::time::sleep(Duration::from_secs(3)).await;

    let member = &peers[1];
    let post = |n: usize| member.community("post", json!({ "channel": channel, "id": uuid::Uuid::new_v4().to_string(), "content": format!("measured {n}") }));
    let mut rows = Vec::new();

    // One post, with nothing else going on.
    tally_statements(true);
    let (said, sent) = (statements_said(), trips.now());
    let pieces = trips.each();
    let started = Instant::now();
    let answer = post(0).await;
    if std::env::var("COST_DETAIL").is_ok() {
        let after = trips.each();
        println!("round trips by kind: {:?}", pieces.iter().zip(&after).map(|((name, a), (_, b))| format!("{name}={}", b - a)).collect::<Vec<_>>());
    }
    let took = started.elapsed();
    let (said, sent) = (statements_said() - said, trips.now() - sent);
    let tally = statement_tally();
    tally_statements(false);
    assert!(answer["type"].as_str().unwrap_or_default().ends_with("/posted"), "{answer}");
    rows.push(format!("one post:            {:>6} ms  {said:>4} statements  {sent:>4} round trips", took.as_millis()));
    if std::env::var("COST_DETAIL").is_ok() {
        for (sql, n) in tally.iter().take(25) {
            println!("  {n:>4}  {}", sql.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(150).collect::<String>());
        }
    }
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Three, each sent once the one before is answered, as the app sends them,
    // while what each sets going in the background (telling everyone) runs.
    tally_statements(true);
    let (said, sent) = (statements_said(), trips.now());
    for n in 1..=3 {
        let started = Instant::now();
        let answer = post(n).await;
        assert!(answer["type"].as_str().unwrap_or_default().ends_with("/posted"), "{answer}");
        rows.push(format!("post {n} of a burst:   {:>6} ms", started.elapsed().as_millis()));
    }
    tokio::time::sleep(Duration::from_secs(5)).await;
    let (said, sent) = (statements_said() - said, trips.now() - sent);
    let tally = statement_tally();
    tally_statements(false);
    if std::env::var("COST_DETAIL").is_ok() {
        println!("\nthe burst and what it set going:");
        for (sql, n) in tally.iter().take(25) {
            println!("  {n:>4}  {}", sql.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(150).collect::<String>());
        }
    }
    rows.push(format!("the burst and all it set going: {said} statements, {sent} round trips"));

    println!("\n{MEMBERS} members, the database behind {}:", server_url.rsplit('@').next().unwrap_or_default());
    for row in &rows {
        println!("  {row}");
    }
    let _ = trips.0.query_drop(format!("DROP DATABASE {database}"));
}
