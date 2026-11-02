//! The host run for real: communities under one address, each its own server, and
//! members' agents that talk to them over DIDComm from endpoints of their own.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{body::Bytes, extract::State, routing::post, Router};
use didcomm_agent::{Agent, Identity};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use wyvrn_community_host::{
    config::Config,
    registry::{Registry, State as Standing, Storage},
    server::{self, AppState},
    tenants::Host,
};

const C: &str = "https://wyvrn.app/community/1.0/";
const V: &str = "https://wyvrn.app/community-voice/1.0/";

/// Someone with an app, writing to one community.
struct Peer {
    agent: Arc<Agent>,
    inbox: Mutex<mpsc::UnboundedReceiver<Value>>,
}

impl Peer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let agent = Arc::new(Agent::with_endpoint(Identity::generate().unwrap(), &url).unwrap());
        let (sent, inbox) = mpsc::unbounded_channel();
        let app = Router::new()
            .route(
                "/",
                post(|State((agent, sent)): State<(Arc<Agent>, mpsc::UnboundedSender<Value>)>, body: Bytes| async move {
                    if let Ok(received) = agent.receive(&body).await {
                        let _ = sent.send(received.message);
                    }
                }),
            )
            .with_state((agent.clone(), sent));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { agent, inbox: Mutex::new(inbox) }
    }

    fn did(&self) -> String {
        self.agent.did()
    }

    async fn ask(&self, to: &str, message_type: &str, body: Value) -> Value {
        let message = json!({ "type": message_type, "body": body, "return_route": "all" });
        self.agent.send(to, &message).await.expect("sent").expect("an answer").message
    }

    async fn community(&self, to: &str, kind: &str, body: Value) -> Value {
        self.ask(to, &format!("{C}{kind}"), body).await
    }
}

struct Running {
    url: String,
    host: Arc<Host>,
    _dir: PathBuf,
}

impl Running {
    /// The DID of the community called `name`: the did:web at its path.
    async fn did_of(&self, name: &str) -> String {
        self.host.tenant(name).await.unwrap().expect("a community by that name").state.agent.did()
    }

    async fn voice_auth(&self, token: &str) -> (u16, Value) {
        let response = reqwest::Client::new()
            .post(format!("{}voice/auth", self.url))
            .json(&json!({ "id": "s1", "event": "connect", "node": "n", "transport": "quic", "path": "/", "query": format!("jwt={token}") }))
            .send()
            .await
            .unwrap();
        (response.status().as_u16(), response.json::<Value>().await.unwrap_or(Value::Null))
    }
}

async fn start_host(idle_seconds: i64) -> Running {
    start_host_with(idle_seconds, "").await
}

/// A host, with `mysql` its MySQL server (no database), or none.
async fn start_host_with(idle_seconds: i64, mysql: &str) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let dir = std::env::temp_dir().join(format!("wyvrn-host-test-{}-{}", std::process::id(), unique()));
    let mut config: Config = toml::from_str(&format!(
        "[server]\nbind = \"unused\"\npublic_url = \"{url}\"\ndid_method = \"peer\"\n[identity]\npath = \"{dir}/identity.json\"\n[store]\npath = \"unused\"\n[storage]\ndir = \"{dir}/communities\"\nmysql = \"{mysql}\"\nshared_database = \"wyvrn_test_shared_{shared}\"\ndatabase_prefix = \"wyvrn_test_c{shared}_\"\n[loading]\nidle_seconds = {idle_seconds}\n[voice]\nrelay_url = \"https://voice.example/\"\n",
        dir = dir.display(),
        shared = unique().replace('-', "_"),
    ))
    .unwrap();
    config.settle();
    let (agent, did_document) = server::build_agent(&config, Identity::generate().unwrap()).unwrap();
    let host = Host::new(config, Registry::scratch().unwrap());
    let state = Arc::new(AppState { agent, host: host.clone(), did_document, resolver: didcomm_quickstart::default_resolver(), admin: Default::default() });
    let app = server::router(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Running { url, host, _dir: dir }
}

/// A number no other host in this run has.
fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    format!("{nanos}-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn kind(message: &Value) -> &str {
    message["type"].as_str().unwrap_or_default().rsplit('/').next().unwrap_or_default()
}

#[tokio::test]
async fn two_communities_in_one_host_are_two_servers_that_know_nothing_of_each_other() {
    let running = start_host(600).await;
    let (ann, bea) = (Peer::start().await, Peer::start().await);
    running.host.registry.create("alpha", "Alpha", Storage::Sqlite, &ann.did(), "test", 1).unwrap();
    running.host.registry.create("beta", "Beta", Storage::Sqlite, &bea.did(), "test", 1).unwrap();
    // (did:peer here, which names each community's address: did:web wants HTTPS.)
    let (alpha, beta) = (running.did_of("alpha").await, running.did_of("beta").await);
    assert_ne!(alpha, beta);

    // Each owner is their own community's admin, and nobody in the other.
    let joined = ann.community(&alpha, "join", json!({})).await;
    // (The owner by member id, as everyone in a community is named: she is it.)
    assert_eq!((kind(&joined), joined["body"]["name"].as_str(), joined["body"]["owner"].as_str()), ("community", Some("Alpha"), joined["body"]["you"].as_str()), "{joined}");
    let made = ann.community(&alpha, "create-channel", json!({ "name": "general" })).await;
    let channel = made["body"]["created"].as_str().unwrap().to_string();
    ann.community(&alpha, "post", json!({ "channel": channel, "id": "a1", "content": "alpha only" })).await;
    assert_eq!(kind(&bea.community(&alpha, "join", json!({})).await), "waiting");
    let in_beta = bea.community(&beta, "join", json!({})).await;
    assert_eq!(in_beta["body"]["channels"], json!([]), "{in_beta}");
    assert_eq!(ann.community(&beta, "history-request", json!({ "channel": channel })).await["body"]["code"], "e.p.trust.not-authorized");

    // Their stores are their own folders.
    for name in ["alpha", "beta"] {
        assert!(running.host.folder(name).join("community.sqlite").exists(), "{name}");
    }
    // The host answers for itself at its root, and for nobody it doesn't keep.
    assert!(reqwest::get(&running.url).await.unwrap().text().await.unwrap().contains("keeps Wyvrn communities"));
    assert_eq!(reqwest::get(format!("{}c/nobody/", running.url)).await.unwrap().status(), 404);
}

#[tokio::test]
async fn a_voice_token_is_answered_by_its_own_community_alone() {
    let running = start_host(600).await;
    let (ann, bea) = (Peer::start().await, Peer::start().await);
    running.host.registry.create("alpha", "Alpha", Storage::Sqlite, &ann.did(), "test", 1).unwrap();
    running.host.registry.create("beta", "Beta", Storage::Sqlite, &bea.did(), "test", 1).unwrap();
    let (alpha, beta) = (running.did_of("alpha").await, running.did_of("beta").await);
    let lounge = ann.community(&alpha, "create-channel", json!({ "name": "lounge", "kind": "voice" })).await["body"]["created"].as_str().unwrap().to_string();
    let joined = ann.ask(&alpha, &format!("{V}join"), json!({ "channel": lounge })).await;
    let token = joined["body"]["transport"]["token"].as_str().expect("a token").to_string();
    assert!(token.starts_with("alpha."), "{token}");

    let (status, grant) = running.voice_auth(&token).await;
    assert_eq!(status, 200, "{grant}");
    // The same token passed off as another community's is nobody's.
    let (_, rest) = token.split_once('.').unwrap();
    bea.community(&beta, "join", json!({})).await;
    assert_eq!(running.voice_auth(&format!("beta.{rest}")).await.0, 403);
    assert_eq!(running.voice_auth("nobody.x").await.0, 403);
    assert_eq!(running.voice_auth("no-prefix").await.0, 403);
}

#[tokio::test]
async fn an_idle_community_is_closed_and_opened_again_when_it_owes_something() {
    let running = start_host(0).await;
    let ann = Peer::start().await;
    running.host.registry.create("alpha", "Alpha", Storage::Sqlite, &ann.did(), "test", 1).unwrap();
    let alpha = running.did_of("alpha").await;
    ann.community(&alpha, "join", json!({})).await;
    let channel = ann.community(&alpha, "create-channel", json!({ "name": "general" })).await["body"]["created"].as_str().unwrap().to_string();
    // A poll, ending in an hour (the shortest a poll may be).
    let ends = wyvrn_service::now() + 3600 + 5;
    let posted = ann.community(&alpha, "post", json!({ "channel": channel, "id": "p1", "content": "Which?", "poll": { "question": "Which?", "answers": [{ "id": "a", "text": "This" }, { "id": "b", "text": "That" }], "ends_at": ends } })).await;
    assert_eq!(kind(&posted), "posted", "{posted}");
    // Sent to everyone joined (its author), so that it owes nobody a delivery.
    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(10), ann.inbox.lock().await.recv()).await.expect("the poll delivered").unwrap();
        if kind(&message) == "message" && message["body"]["id"] == "p1" {
            break;
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Idle, and nothing holds it open: closed, to be opened when the poll ends.
    running.host.housekeeping(wyvrn_service::now()).await;
    assert_eq!(running.host.loaded_count(), 0);
    let record = running.host.registry.get("alpha").unwrap().unwrap();
    assert_eq!(record.wake_at, Some(ends), "{record:?}");
    assert!(running.host.registry.due(ends - 1).unwrap().is_empty());

    // Then it is opened (and, idle again at once, closed with the same errand).
    running.host.housekeeping(ends).await;
    assert_eq!(running.host.registry.get("alpha").unwrap().unwrap().wake_at, Some(ends));
    // Spoken to again, it answers from what it kept.
    let history = ann.community(&alpha, "history-request", json!({ "channel": channel })).await;
    assert_eq!(history["body"]["messages"][0]["id"], "p1", "{history}");

    // Suspended, it answers nothing and isn't opened.
    running.host.set_state("alpha", Standing::Suspended, None).unwrap();
    assert_eq!(running.host.loaded_count(), 0);
    assert_eq!(reqwest::get(format!("{}c/alpha/", running.url)).await.unwrap().status(), 404);
}

#[tokio::test]
async fn communities_kept_in_mysql_a_database_each_or_tables_each_keep_apart() {
    let Some(url) = std::env::var("WYVRN_TEST_DATABASE_URL").ok().filter(|url| !url.is_empty()) else { return };
    let running = start_host_with(600, url.trim_end_matches('/')).await;
    let (ann, bea) = (Peer::start().await, Peer::start().await);
    running.host.registry.create("own-db", "Own", Storage::MySqlDatabase, &ann.did(), "test", 1).unwrap();
    running.host.registry.create("shared", "Shared", Storage::MySqlShared, &bea.did(), "test", 1).unwrap();
    running.host.registry.create("shared-too", "Shared too", Storage::MySqlShared, &ann.did(), "test", 1).unwrap();
    for (peer, name) in [(&ann, "own-db"), (&bea, "shared"), (&ann, "shared-too")] {
        let did = running.did_of(name).await;
        let joined = peer.community(&did, "join", json!({})).await;
        assert_eq!(joined["body"]["owner"], peer.did(), "{name}: {joined}");
        let channel = peer.community(&did, "create-channel", json!({ "name": format!("in-{name}") })).await["body"]["created"].as_str().unwrap().to_string();
        assert_eq!(kind(&peer.community(&did, "post", json!({ "channel": channel, "id": format!("{name}-1"), "content": name })).await), "posted");
    }
    // Each has its own channel, and nobody else's; and nothing was kept in a file.
    for (peer, name) in [(&ann, "own-db"), (&bea, "shared"), (&ann, "shared-too")] {
        let did = running.did_of(name).await;
        let channels = peer.community(&did, "join", json!({})).await["body"]["channels"].clone();
        assert_eq!(channels.as_array().unwrap().len(), 1, "{name}: {channels}");
        assert_eq!(channels[0]["name"], format!("in-{name}"));
        assert!(!running.host.folder(name).join("community.sqlite").exists());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_community_moves_between_every_kind_of_storage_and_is_the_same_community() {
    let Some(url) = std::env::var("WYVRN_TEST_DATABASE_URL").ok().filter(|url| !url.is_empty()) else { return };
    let running = start_host_with(600, url.trim_end_matches('/')).await;
    let ann = Peer::start().await;
    running.host.registry.create("mover", "Mover", Storage::Sqlite, &ann.did(), "test", 1).unwrap();
    let did = running.did_of("mover").await;
    ann.community(&did, "join", json!({})).await;
    let channel = ann.community(&did, "create-channel", json!({ "name": "general" })).await["body"]["created"].as_str().unwrap().to_string();
    ann.community(&did, "post", json!({ "channel": channel, "id": "m1", "content": "kept through every move" })).await;

    let mut posts = 1;
    for to in [Storage::MySqlShared, Storage::MySqlDatabase, Storage::Sqlite] {
        let host = running.host.clone();
        let said = tokio::task::spawn_blocking(move || host.move_storage("mover", to)).await.unwrap().unwrap();
        assert!(said.contains(to.as_str()), "{said}");
        assert_eq!(running.host.registry.get("mover").unwrap().unwrap().storage, to);
        // The same keys, so the same DID; the same owner, channel and posts.
        assert_eq!(running.did_of("mover").await, did);
        let joined = ann.community(&did, "join", json!({})).await;
        assert_eq!((joined["body"]["owner"].as_str(), joined["body"]["channels"][0]["id"].as_str()), (Some(ann.did().as_str()), Some(channel.as_str())), "{joined}");
        let id = format!("m{}", posts + 1);
        assert_eq!(kind(&ann.community(&did, "post", json!({ "channel": channel, "id": id, "content": to.as_str() })).await), "posted");
        posts += 1;
        let history = ann.community(&did, "history-request", json!({ "channel": channel })).await;
        assert_eq!(history["body"]["messages"].as_array().unwrap().len(), posts, "{history}");
    }
    // Back in SQLite, where it began: moved into the files it left, it is refused.
    let host = running.host.clone();
    assert!(tokio::task::spawn_blocking(move || host.move_storage("mover", Storage::Sqlite)).await.unwrap().is_err());
}

const W: &str = "https://didcomm.org/workflow/1.0/";

/// Every bit of text a workflow's page shows, and every link, at any depth.
fn page_text(items: &Value, into: &mut Vec<String>) {
    for item in items.as_array().into_iter().flatten() {
        for key in ["text", "label", "url"] {
            if let Some(text) = item[key].as_str() {
                into.push(text.to_string());
            }
        }
        page_text(&item["children"], into);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owner_takes_their_community_to_a_server_of_their_own() {
    let running = start_host(600).await;
    let (owner, member) = (Peer::start().await, Peer::start().await);
    running.host.registry.create("leaving", "Leaving", Storage::Sqlite, &owner.did(), "test", 1).unwrap();
    let did = running.did_of("leaving").await;
    owner.community(&did, "join", json!({})).await;
    let channel = owner.community(&did, "create-channel", json!({ "name": "general" })).await["body"]["created"].as_str().unwrap().to_string();
    owner.community(&did, "post", json!({ "channel": channel, "id": "x1", "content": "goes with it" })).await;
    owner.community(&did, "set-member", json!({ "did": member.did(), "role": "member" })).await;
    member.community(&did, "join", json!({})).await;

    // Only the owner is shown the page.
    let listed = |answer: &Value| answer["body"]["workflows"].as_array().unwrap().iter().any(|workflow| workflow["template_id"] == "admin-move-out");
    assert!(listed(&owner.ask(&did, &format!("{W}discover"), json!({})).await));
    assert!(!listed(&member.ask(&did, &format!("{W}discover"), json!({})).await));
    assert_eq!(member.ask(&did, &format!("{W}start"), json!({ "template_id": "admin-move-out", "instance_id": "m" })).await["body"]["code"], "not_authorized");

    // Begun: read-only, and everyone told.
    owner.ask(&did, &format!("{W}start"), json!({ "template_id": "admin-move-out", "instance_id": "o" })).await;
    let begun = owner.ask(&did, &format!("{W}advance"), json!({ "instance_id": "o", "event": "begin", "input": {} })).await;
    assert_eq!(running.host.registry.get("leaving").unwrap().unwrap().state, Standing::Moving, "{begun}");
    let told = loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(10), member.inbox.lock().await.recv()).await.expect("told it is moving").unwrap();
        if kind(&message) == "community" && message["body"]["moving"] == true {
            break message;
        }
    };
    assert_eq!(told["body"]["name"], "Leaving");
    assert_eq!(member.community(&did, "post", json!({ "channel": channel, "id": "x2", "content": "too late" })).await["body"]["code"], "e.p.req.moving");

    // The export, at a link that works once.
    let exported = owner.ask(&did, &format!("{W}advance"), json!({ "instance_id": "o", "event": "export", "input": {} })).await;
    let mut shown = Vec::new();
    page_text(&exported["body"]["ui"], &mut shown);
    let link = shown.iter().find(|text| text.contains("/export/")).unwrap_or_else(|| panic!("a link in {exported}")).clone();
    let bytes = reqwest::get(&link).await.unwrap().bytes().await.unwrap();
    assert_eq!(reqwest::get(&link).await.unwrap().status(), 404);
    let unpacked = running._dir.join("unpacked");
    tar::Archive::new(flate2::read::GzDecoder::new(&bytes[..])).unpack(&unpacked).unwrap();
    let root = unpacked.join("leaving");
    for file in ["data/identity.json", "data/community.sqlite", "data/stickers.sqlite", "data/files/files.sqlite", "config/default.toml", "README.txt"] {
        assert!(root.join(file).exists(), "{file}");
    }

    // It opens as the single server: the same keys, members and posts, and not moving.
    let mut config = wyvrn_community::config::Config::load(root.join("config/default.toml")).unwrap();
    config.server.public_url = "http://127.0.0.1:1/".into();
    config.server.did_method = wyvrn_service::address::DidMethod::Peer;
    config.identity.path = root.join("data/identity.json");
    let stores = wyvrn_community::serve::Stores {
        community: wyvrn_service::db::Location::Sqlite(root.join("data/community.sqlite")),
        stickers: Some(wyvrn_service::db::Location::Sqlite(root.join("data/stickers.sqlite"))),
        files: Some(wyvrn_service::db::Location::Sqlite(root.join("data/files/files.sqlite"))),
    };
    config.files.as_mut().unwrap().dir = root.join("data/files");
    let opened = wyvrn_community::serve::open(&config, &stores, Default::default()).await.unwrap();
    let community = &opened.state.community;
    assert!(!community.moving().unwrap());
    assert_eq!(community.owner().unwrap().as_deref(), Some(owner.did().as_str()));
    assert!(community.store.member(&member.did()).unwrap().is_some());
    assert_eq!(community.store.post_by_id("x1").unwrap().unwrap().content, "goes with it");
    let hosted = running.host.tenant("leaving").await.unwrap().unwrap();
    assert_eq!(opened.state.agent.identity().to_json().unwrap(), hosted.state.agent.identity().to_json().unwrap());
    drop(hosted);

    // Handed over (here by a host admin: there is no DID host to watch), it stops.
    running.host.set_state("leaving", Standing::Moved, Some(wyvrn_service::now() + 30 * 86_400)).unwrap();
    assert_eq!(running.host.loaded_count(), 0);
    assert_eq!(reqwest::get(format!("{}c/leaving/", running.url)).await.unwrap().status(), 404);
}

#[tokio::test]
async fn each_community_is_on_the_web_at_its_own_path() {
    let running = start_host(600).await;
    let owner = Peer::start().await;
    running.host.registry.create("gamma", "Gamma", Storage::Sqlite, &owner.did(), "test", 1).unwrap();
    let did = running.did_of("gamma").await;
    let joined = owner.community(&did, "join", json!({})).await;
    assert_eq!(joined["body"]["web"]["url"], format!("{}c/gamma/", running.url), "{joined}");
    assert_eq!(joined["body"]["web"]["page"], true, "a community made now has its page on");

    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let page = http.get(format!("{}c/gamma/", running.url)).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert_eq!(page.headers()["x-frame-options"], "DENY");
    assert!(page.headers().get("access-control-allow-origin").is_none());
    let html = page.text().await.unwrap();
    assert!(html.contains("<h1>Gamma</h1>"));
    assert!(html.contains(&format!("<link rel=\"canonical\" href=\"{}c/gamma/\">", running.url)));
    // Without the `/`, to it: the page's links are relative.
    let bare = http.get(format!("{}c/gamma", running.url)).send().await.unwrap();
    assert_eq!(bare.status(), 308);
    assert_eq!(bare.headers()["location"], "gamma/");
    let discovery: Value = http.get(format!("{}c/gamma/.well-known/openid-configuration", running.url)).send().await.unwrap().json().await.unwrap();
    assert_eq!(discovery["issuer"], format!("{}c/gamma", running.url));
}
