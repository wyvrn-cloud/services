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
    /// The host's own DID, and its state.
    did: String,
    state: Arc<AppState>,
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
    start_host_with_extra(idle_seconds, mysql, "").await
}

/// The same, with more of the host's configuration (`[backups]`, say).
async fn start_host_with_extra(idle_seconds: i64, mysql: &str, extra: &str) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let dir = std::env::temp_dir().join(format!("wyvrn-host-test-{}-{}", std::process::id(), unique()));
    let mut config: Config = toml::from_str(&format!(
        "[server]\nbind = \"unused\"\npublic_url = \"{url}\"\ndid_method = \"peer\"\n[identity]\npath = \"{dir}/identity.json\"\n[store]\npath = \"unused\"\n[storage]\ndir = \"{dir}/communities\"\nmysql = \"{mysql}\"\nshared_database = \"wyvrn_test_shared_{shared}\"\ndatabase_prefix = \"wyvrn_test_c{shared}_\"\n[loading]\nidle_seconds = {idle_seconds}\n[voice]\nrelay_url = \"https://voice.example/\"\n{extra}\n",
        dir = dir.display(),
        shared = unique().replace('-', "_"),
    ))
    .unwrap();
    config.settle();
    let (agent, did_document) = server::build_agent(&config, Identity::generate().unwrap()).unwrap();
    let host = Host::new(config, Registry::scratch().unwrap());
    let did = agent.did();
    let state = Arc::new(AppState { agent, host: host.clone(), did_document, resolver: didcomm_quickstart::default_resolver(), admin: Default::default(), open: wyvrn_community_host::creating::open_pages(), made: Default::default() });
    let app = server::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Running { url, host, did, state, _dir: dir }
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
        // (The owner by member id, as everyone in a community is named.)
        assert_eq!(joined["body"]["owner"], joined["body"]["you"], "{name}: {joined}");
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
        assert_eq!((joined["body"]["owner"].as_str(), joined["body"]["channels"][0]["id"].as_str()), (joined["body"]["you"].as_str(), Some(channel.as_str())), "{joined}");
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

    // A member's picture, at the address the member list gives: a route of the
    // community's own with a path of its own, which once failed under the host.
    let message = json!({
        "type": "https://didcomm.org/user-profile/1.0/profile",
        "body": { "profile": { "displayName": "Owner", "displayPicture": "#pfp" } },
        "attachments": [{ "id": "pfp", "media_type": "image/png", "data": { "base64": PIXEL } }],
    });
    owner.agent.send(&did, &message).await.expect("sent");
    let listed = owner.community(&did, "members-request", json!({})).await;
    let picture = listed["body"]["members"][0]["profile"]["picture"].as_str().expect("a picture's address").to_string();
    assert!(picture.starts_with(&format!("{}c/gamma/m/", running.url)), "{picture}");
    let fetched = http.get(&picture).send().await.unwrap();
    assert_eq!(fetched.status(), 200);
    assert_eq!(fetched.headers()["content-type"], "image/png");
    assert_eq!(http.get(format!("{}c/gamma/m/nobody/picture/nothing", running.url)).send().await.unwrap().status(), 404);
    assert_eq!(http.get(format!("{}c/", running.url)).send().await.unwrap().status(), 404);
    assert_eq!(http.get(format!("{}nothing-here", running.url)).send().await.unwrap().status(), 404);
}

/// A one-pixel PNG.
const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

#[tokio::test]
async fn a_community_run_on_its_own_is_brought_in_with_its_keys_and_everything_it_keeps() {
    // Kept in SQLite as wyvrn-community keeps one, beside another service's table
    // in the same database (as a server whose stores share one database has).
    let elsewhere = start_host(600).await;
    let (owner, member) = (Peer::start().await, Peer::start().await);
    elsewhere.host.registry.create("solo", "Solo", Storage::Sqlite, &owner.did(), "test", 1).unwrap();
    let was = elsewhere.did_of("solo").await;
    owner.community(&was, "join", json!({})).await;
    let channel = owner.community(&was, "create-channel", json!({ "name": "general" })).await["body"]["created"].as_str().unwrap().to_string();
    owner.community(&was, "post", json!({ "channel": channel, "id": "s1", "content": "comes along" })).await;
    owner.community(&was, "set-member", json!({ "did": member.did(), "role": "member" })).await;
    let keys = elsewhere.host.tenant("solo").await.unwrap().unwrap().state.agent.identity().to_json().unwrap();
    elsewhere.host.unload("solo");
    let folder = elsewhere.host.folder("solo");
    rusqlite::Connection::open(folder.join("community.sqlite")).unwrap().execute_batch("CREATE TABLE someone_elses (x TEXT); INSERT INTO someone_elses VALUES ('not ours');").unwrap();

    // Brought in, with a second owner named in the host's configuration.
    let running = start_host(600).await;
    let host = running.host.clone();
    let mut config = host.config.clone();
    config.owners.insert("brought".into(), vec![member.did()]);
    let host = Host::new(config, Registry::scratch().unwrap());
    let source = wyvrn_community_host::import::Source {
        community: wyvrn_service::db::Location::Sqlite(folder.join("community.sqlite")),
        stickers: Some(wyvrn_service::db::Location::Sqlite(folder.join("stickers.sqlite"))),
        files: Some(folder.join("files")),
        files_records: None,
    };
    let (in_host, from) = (host.clone(), source);
    let owner_did = owner.did();
    let said = tokio::task::spawn_blocking(move || in_host.import("brought", "Brought", &owner_did, Storage::Sqlite, &from, 2)).await.unwrap().unwrap();
    assert!(said.contains("brought is in"), "{said}");
    assert_eq!(host.registry.get("brought").unwrap().unwrap().state, Standing::Active);

    // The same keys, members and posts; and both owners are owners.
    let tenant = host.tenant("brought").await.unwrap().unwrap();
    assert_eq!(tenant.state.agent.identity().to_json().unwrap(), keys);
    let community = &tenant.state.community;
    assert_eq!(community.store.post_by_id("s1").unwrap().unwrap().content, "comes along");
    assert!(community.store.member(&member.did()).unwrap().is_some());
    assert_eq!(community.owners().unwrap(), [owner.did(), member.did()]);
    assert!(!community.moving().unwrap());

    // A name taken already is refused, and nothing is left of the attempt.
    let again = wyvrn_community_host::import::Source {
        community: wyvrn_service::db::Location::Sqlite(folder.join("community.sqlite")),
        stickers: None,
        files: None,
        files_records: None,
    };
    let owner_did = owner.did();
    let refused = tokio::task::spawn_blocking(move || host.import("brought", "Again", &owner_did, Storage::Sqlite, &again, 3)).await.unwrap();
    assert!(refused.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_community_run_on_its_own_in_mysql_is_brought_into_shared_tables() {
    let Some(url) = std::env::var("WYVRN_TEST_DATABASE_URL").ok().filter(|url| !url.is_empty()) else { return };
    let server = url.trim_end_matches('/');
    // A community as wyvrn-community keeps one in MySQL: its tables in a database
    // of its own, unprefixed, and its keys there too.
    let elsewhere = start_host(600).await;
    let owner = Peer::start().await;
    elsewhere.host.registry.create("solo", "Solo", Storage::Sqlite, &owner.did(), "test", 1).unwrap();
    let was = elsewhere.did_of("solo").await;
    owner.community(&was, "join", json!({})).await;
    let channel = owner.community(&was, "create-channel", json!({ "name": "general" })).await["body"]["created"].as_str().unwrap().to_string();
    owner.community(&was, "post", json!({ "channel": channel, "id": "m1", "content": "from mysql" })).await;
    let keys = Identity::from_json(&elsewhere.host.tenant("solo").await.unwrap().unwrap().state.agent.identity().to_json().unwrap()).unwrap();
    elsewhere.host.unload("solo");
    let folder = elsewhere.host.folder("solo");
    let database = format!("wyvrn_test_solo_{}", unique().replace('-', "_"));
    {
        let mut admin = mysql::Conn::new(mysql::Opts::from_url(server).unwrap()).unwrap();
        mysql::prelude::Queryable::query_drop(&mut admin, format!("CREATE DATABASE {database}")).unwrap();
    }
    let standalone = wyvrn_service::db::Location::MySql(format!("{server}/{database}"));
    wyvrn_service::identity::keep(&standalone, &folder.join("unused.json"), "community", &keys).unwrap();
    wyvrn_community::store::Store::open(&standalone).unwrap();
    wyvrn_service::profile::Kept::open(&standalone, "community").unwrap();
    wyvrn_service::hosted_address::Kept::open(&standalone, "community").unwrap();
    wyvrn_service::db::copy_store(&wyvrn_service::db::Location::Sqlite(folder.join("community.sqlite")), &standalone, "community", &["wyvrn_identity"]).unwrap();

    let running = start_host_with(600, server).await;
    let host = running.host.clone();
    let source = wyvrn_community_host::import::Source { community: standalone, stickers: None, files: None, files_records: None };
    let owner_did = owner.did();
    let in_host = host.clone();
    tokio::task::spawn_blocking(move || in_host.import("from-mysql", "From MySQL", &owner_did, Storage::MySqlShared, &source, 2)).await.unwrap().unwrap();
    let tenant = host.tenant("from-mysql").await.unwrap().unwrap();
    assert_eq!(tenant.state.agent.identity().to_json().unwrap(), keys.to_json().unwrap());
    assert_eq!(tenant.state.community.store.post_by_id("m1").unwrap().unwrap().content, "from mysql");
    assert!(!host.folder("from-mysql").join("community.sqlite").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn someone_makes_a_community_from_their_app_as_far_as_the_hosts_admins_allow() {
    use wyvrn_community_host::creating::Mode;
    let running = start_host(600).await;
    let (alice, bob) = (Peer::start().await, Peer::start().await);
    let offered = |answer: &Value| answer["body"]["workflows"].as_array().unwrap().iter().any(|w| w["template_id"] == "community-create");

    // Closed, as a host starts: nothing offered, nothing made.
    assert!(!offered(&alice.ask(&running.did, &format!("{W}discover"), json!({})).await));
    let refused = alice.ask(&running.did, &format!("{W}start"), json!({ "template_id": "community-create", "instance_id": "a1" })).await;
    assert_eq!(kind(&refused), "problem-report", "{refused}");

    // Open to anyone: made, theirs, and its DID handed back to be added.
    running.state.set_creating_mode(Mode::Anyone).unwrap();
    assert!(offered(&alice.ask(&running.did, &format!("{W}discover"), json!({})).await));
    alice.ask(&running.did, &format!("{W}start"), json!({ "template_id": "community-create", "instance_id": "a1" })).await;
    let bad = alice.ask(&running.did, &format!("{W}advance"), json!({ "instance_id": "a1", "event": "create", "input": { "title": "Bad", "name": "No Spaces" } })).await;
    assert!(bad["body"]["ui"].to_string().contains("lowercase"), "{bad}");
    let made = alice.ask(&running.did, &format!("{W}advance"), json!({ "instance_id": "a1", "event": "create", "input": { "title": "The Kiln", "name": "the-kiln" } })).await;
    let community = made["body"]["artifacts"]["community"].as_str().unwrap_or_else(|| panic!("{made}")).to_string();
    assert_eq!(made["body"]["artifacts"]["name"], "the-kiln");
    assert_eq!(running.host.registry.get("the-kiln").unwrap().unwrap().owner, alice.did());
    assert_eq!(community, running.did_of("the-kiln").await);
    let joined = alice.community(&community, "join", json!({})).await;
    assert_eq!(kind(&joined), "community", "{joined}");
    // And not another straight away.
    let again = alice.ask(&running.did, &format!("{W}advance"), json!({ "instance_id": "a1", "event": "create", "input": { "title": "More", "name": "more-kilns" } })).await;
    assert!(again["body"]["ui"].to_string().contains("wait a little"), "{again}");

    // By code: a wrong one refused, a right one used up.
    running.state.set_creating_mode(Mode::Codes).unwrap();
    running.host.registry.add_code("KILNCODE01", 1, "for Bob", "did:example:admin", 1).unwrap();
    bob.ask(&running.did, &format!("{W}start"), json!({ "template_id": "community-create", "instance_id": "b1" })).await;
    let wrong = bob.ask(&running.did, &format!("{W}advance"), json!({ "instance_id": "b1", "event": "create", "input": { "title": "Bob's", "name": "bobs-place", "code": "NOPE" } })).await;
    assert!(wrong["body"]["ui"].to_string().contains("isn't one"), "{wrong}");
    let right = bob.ask(&running.did, &format!("{W}advance"), json!({ "instance_id": "b1", "event": "create", "input": { "title": "Bob's", "name": "bobs-place", "code": "KILNCODE01" } })).await;
    assert!(right["body"]["artifacts"]["community"].is_string(), "{right}");
    assert!(running.host.registry.codes().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hosted_community_is_held_to_its_posts_a_minute_and_the_room_its_store_has() {
    let running = start_host(600).await;
    let owner = Peer::start().await;
    running.host.registry.create("busy", "Busy", Storage::Sqlite, &owner.did(), "test", 1).unwrap();
    let limits = wyvrn_community_host::config::Limits { posts_per_minute: 2, ..Default::default() };
    running.host.registry.set_limits("busy", Some(&limits)).unwrap();
    let busy = running.did_of("busy").await;
    owner.community(&busy, "join", json!({})).await;
    let channel = owner.community(&busy, "create-channel", json!({ "name": "general" })).await["body"]["created"].as_str().unwrap().to_string();
    for n in 0..2 {
        assert_eq!(kind(&owner.community(&busy, "post", json!({ "channel": channel, "content": format!("post {n}") })).await), "posted");
    }
    // Two a minute, from everyone: the owner too.
    let third = owner.community(&busy, "post", json!({ "channel": channel, "content": "one too many" })).await;
    assert_eq!(third["body"]["code"], "e.p.req.rate-limited", "{third}");

    // Its store, measured, past the room it has: nothing more is posted.
    running.host.registry.create("full", "Full", Storage::Sqlite, &owner.did(), "test", 1).unwrap();
    running.host.registry.set_limits("full", Some(&wyvrn_community_host::config::Limits { store_bytes: 1, ..Default::default() })).unwrap();
    let full = running.did_of("full").await;
    owner.community(&full, "join", json!({})).await;
    let general = owner.community(&full, "create-channel", json!({ "name": "general" })).await["body"]["created"].as_str().unwrap().to_string();
    tokio::task::spawn_blocking({
        let host = running.host.clone();
        move || host.measure()
    })
    .await
    .unwrap();
    let refused = owner.community(&full, "post", json!({ "channel": general, "content": "no room" })).await;
    assert_eq!(refused["body"]["code"], "e.p.msg.quota", "{refused}");
}

#[tokio::test(flavor = "multi_thread")]
async fn kinds_of_channel_are_turned_off_for_every_community_or_for_one() {
    let running = start_host_with_extra(600, "", "[limits]\ndisabled_channel_kinds = [\"wiki\"]").await;
    let owner = Peer::start().await;
    // One that follows the host's, and one with its own: wikis, but no stages.
    running.host.registry.create("plain", "Plain", Storage::Sqlite, &owner.did(), "test", 1).unwrap();
    running.host.registry.create("wikis", "Wikis", Storage::Sqlite, &owner.did(), "test", 1).unwrap();
    let own = wyvrn_community_host::config::Limits { disabled_channel_kinds: Some(vec!["stage".into()]), ..Default::default() };
    running.host.registry.set_limits("wikis", Some(&own)).unwrap();
    let (plain, wikis) = (running.did_of("plain").await, running.did_of("wikis").await);
    for did in [&plain, &wikis] {
        owner.community(did, "join", json!({})).await;
    }
    let wiki = json!({ "name": "handbook", "kind": "wiki" });
    let stage = json!({ "name": "town-hall", "kind": "voice", "voice": { "stage": true } });
    assert_eq!(owner.community(&plain, "create-channel", wiki.clone()).await["body"]["code"], "e.p.msg.kind-off");
    assert_eq!(kind(&owner.community(&plain, "create-channel", stage.clone()).await), "community");
    assert_eq!(kind(&owner.community(&wikis, "create-channel", wiki).await), "community");
    let refused = owner.community(&wikis, "create-channel", stage).await;
    assert_eq!(refused["body"]["code"], "e.p.msg.kind-off", "{refused}");
}

#[tokio::test(flavor = "multi_thread")]
async fn voice_turned_off_ends_a_room_under_way_and_it_cant_be_joined_again() {
    let running = start_host(600).await;
    let ann = Peer::start().await;
    running.host.registry.create("alpha", "Alpha", Storage::Sqlite, &ann.did(), "test", 1).unwrap();
    let alpha = running.did_of("alpha").await;
    let lounge = ann.community(&alpha, "create-channel", json!({ "name": "lounge", "kind": "voice" })).await["body"]["created"].as_str().unwrap().to_string();
    let joined = ann.ask(&alpha, &format!("{V}join"), json!({ "channel": lounge })).await;
    let token = joined["body"]["transport"]["token"].as_str().expect("a token").to_string();
    assert_eq!(running.voice_auth(&token).await.0, 200);

    let off = wyvrn_community_host::config::Limits { disabled_channel_kinds: Some(vec!["voice".into()]), ..Default::default() };
    running.host.change_limits("alpha", None, &off).unwrap();
    assert!(running.host.loaded("alpha").is_none());
    // Ended: the forwarding service is told no, and joining again is refused, saying why.
    assert_eq!(running.voice_auth(&token).await.0, 403);
    let again = ann.ask(&alpha, &format!("{V}join"), json!({ "channel": lounge })).await;
    assert_eq!(again["body"]["code"], "e.p.msg.kind-off", "{again}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_community_is_backed_up_sealed_with_the_hosts_key_and_opens_whole() {
    let dir = std::env::temp_dir().join(format!("wyvrn-host-backup-test-{}", unique()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = dir.join("backup.key");
    std::fs::write(&key, "ab".repeat(32)).unwrap();
    let running = start_host_with_extra(600, "", &format!("[backups]\ndir = \"{}\"\nkeep = 2\nkey = \"{}\"", dir.join("kept").display(), key.display())).await;
    let owner = Peer::start().await;
    running.host.registry.create("kept", "Kept", Storage::Sqlite, &owner.did(), "test", 1).unwrap();
    let kept = running.did_of("kept").await;
    owner.community(&kept, "join", json!({})).await;
    let channel = owner.community(&kept, "create-channel", json!({ "name": "general" })).await["body"]["created"].as_str().unwrap().to_string();
    owner.community(&kept, "post", json!({ "channel": channel, "id": "p1", "content": "keep this safe" })).await;

    // Three backed up, two kept: the newest.
    for at in [100, 200, 300] {
        let host = running.host.clone();
        tokio::task::spawn_blocking(move || host.back_up("kept", at)).await.unwrap().unwrap();
    }
    let listed = running.host.backups("kept");
    assert_eq!(listed.iter().map(|(_, at)| *at).collect::<Vec<_>>(), [300, 200]);

    // Sealed: not an archive as it is; opened with the key, the community whole.
    let sealed = running.host.backups_of("kept").join(&listed[0].0);
    assert!(!std::fs::read(&sealed).unwrap().starts_with(&[0x1f, 0x8b]));
    let opened = dir.join("opened.tar.gz");
    let key_bytes = [0xab_u8; 32];
    wyvrn_community_host::backups::open(&key_bytes, &sealed, &opened).unwrap();
    let out = dir.join("out");
    tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(&opened).unwrap())).unpack(&out).unwrap();
    let store = wyvrn_community::store::Store::open(&wyvrn_service::db::Location::Sqlite(out.join("kept/data/community.sqlite"))).unwrap();
    assert_eq!(store.post_by_id("p1").unwrap().unwrap().content, "keep this safe");
    assert!(out.join("kept/data/identity.json").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_community_at_its_own_domain_once_proved_by_its_txt_record() {
    use wyvrn_community_host::domains;
    // A stand-in DNS-over-HTTPS resolver: what `_wyvrn.club.example.org` says.
    let dns = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dns_url = format!("http://{}/dns-query", dns.local_addr().unwrap());
    let record = Arc::new(std::sync::Mutex::new(String::new()));
    let said = record.clone();
    let resolver = axum::Router::new().route(
        "/dns-query",
        axum::routing::get(move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| {
            let said = said.lock().unwrap().clone();
            async move {
                let answers = if q.get("name").map(String::as_str) == Some("_wyvrn.club.example.org") && !said.is_empty() { json!([{ "type": 16, "data": format!("\"{said}\"") }]) } else { json!([]) };
                axum::Json(json!({ "Status": 0, "Answer": answers }))
            }
        }),
    );
    tokio::spawn(async move { axum::serve(dns, resolver).await.unwrap() });
    let running = start_host_with_extra(600, "", &format!("[domains]\nenabled = true\ncname = \"c.wyvrn.example\"\ndns_over_https = \"{dns_url}\"\n")).await;
    let ann = Peer::start().await;
    running.host.registry.create("club", "The Club", Storage::Sqlite, &ann.did(), "test", 1).unwrap();
    running.host.registry.create("other", "Other", Storage::Sqlite, &ann.did(), "test", 1).unwrap();
    let ask = |domain: &str| reqwest::get(format!("{}domains/ask?domain={domain}", running.url));

    // Claimed, and not yet proved: no certificate, and not its.
    assert!(running.host.registry.claim_domain("club", "club.example.org", "wyvrn-token1", wyvrn_service::now()).unwrap());
    assert!(running.host.registry.claim_domain("other", "club.example.org", "wyvrn-token2", wyvrn_service::now()).unwrap(), "named by another too: whoever proves it first");
    assert_eq!(ask("club.example.org").await.unwrap().status(), 404);
    assert!(domains::txt_records(&dns_url, "_wyvrn.club.example.org").await.unwrap().is_empty());

    // The TXT record set: proved.
    *record.lock().unwrap() = "wyvrn-token1".to_string();
    assert_eq!(domains::txt_records(&dns_url, "_wyvrn.club.example.org").await.unwrap(), ["wyvrn-token1"]);
    assert!(running.host.registry.domain_proved("club", 2).unwrap());
    assert!(!running.host.registry.claim_domain("other", "club.example.org", "wyvrn-token2", wyvrn_service::now()).unwrap(), "proved: another community's");
    assert_eq!(ask("club.example.org").await.unwrap().status(), 200);
    assert_eq!(ask("someone-elses.example.org").await.unwrap().status(), 404);

    // A request for the domain is the community's, at its root.
    let client = reqwest::Client::new();
    let own = client.get(format!("{}.well-known/did.json", running.url)).header("host", "club.example.org").send().await.unwrap();
    let at_path = client.get(format!("{}c/club/.well-known/did.json", running.url)).send().await.unwrap();
    assert_eq!(own.status(), at_path.status());
    let (own, at_path) = (own.text().await.unwrap(), at_path.text().await.unwrap());
    assert_eq!(own, at_path, "the community's own answer, not the host's");
    // The host's own name is still the host's.
    assert!(client.get(&running.url).send().await.unwrap().text().await.unwrap().contains("keeps Wyvrn communities"));
    // And its web view's address is the domain.
    assert_eq!(running.host.settings(&running.host.registry.get("club").unwrap().unwrap()).web.public_url.as_deref(), Some("https://club.example.org/"));
}

#[tokio::test]
async fn a_failed_import_leaves_nothing_behind_and_a_leftover_folder_is_never_taken_over() {
    let running = start_host(600).await;
    let host = running.host.clone();
    // Nothing to bring in: no keys where it says (WYV-46).
    let nowhere = std::env::temp_dir().join(format!("wyvrn-no-community-{}", unique()));
    std::fs::create_dir_all(&nowhere).unwrap();
    let source = wyvrn_community_host::import::Source { community: wyvrn_service::db::Location::Sqlite(nowhere.join("community.sqlite")), stickers: None, files: None, files_records: None };
    let in_host = host.clone();
    let failed = tokio::task::spawn_blocking(move || in_host.import("ghost", "Ghost", "did:example:owner", Storage::Sqlite, &source, 1)).await.unwrap();
    assert!(failed.is_err());
    assert!(host.leftover("ghost").is_none(), "its folder went with it");
    assert!(host.registry.get("ghost").unwrap().is_none());

    // A folder something else left, keys and all, is refused for a new community.
    std::fs::create_dir_all(host.folder("left")).unwrap();
    std::fs::write(host.folder("left").join("identity.json"), "{}").unwrap();
    assert!(host.refuse_leftover("left").is_err());
    let _ = std::fs::remove_dir_all(&nowhere);
}

#[tokio::test]
async fn a_deleted_community_is_purged_once_its_time_to_be_kept_runs_out() {
    let running = start_host(600).await;
    let host = running.host.clone();
    host.registry.create("gone", "Gone", Storage::Sqlite, "did:example:owner", "test", 1).unwrap();
    host.tenant("gone").await.unwrap().unwrap();
    host.unload("gone");
    assert!(host.leftover("gone").is_some());
    host.registry.set_state("gone", Standing::Deleted, Some(1_000)).unwrap();
    // Still being kept (WYV-48).
    host.housekeeping(999).await;
    assert!(host.registry.get("gone").unwrap().is_some());
    assert!(host.leftover("gone").is_some());
    // Its time ran out: gone for good, and the name free again.
    host.housekeeping(1_000).await;
    assert!(host.registry.get("gone").unwrap().is_none());
    assert!(host.leftover("gone").is_none());
    host.registry.create("gone", "Again", Storage::Sqlite, "did:example:other", "test", 2).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_purged_community_kept_in_mysql_loses_its_tables_and_no_one_elses() {
    let Some(url) = std::env::var("WYVRN_TEST_DATABASE_URL").ok().filter(|url| !url.is_empty()) else { return };
    let server = url.trim_end_matches('/');
    let running = start_host_with(600, server).await;
    let host = running.host.clone();
    for name in ["gone", "stays"] {
        host.registry.create(name, name, Storage::MySqlShared, "did:example:owner", "test", 1).unwrap();
        host.tenant(name).await.unwrap().unwrap();
        host.unload(name);
    }
    let shared = host.config.storage.shared_database.clone();
    let tables = |like: String| {
        let mut admin = mysql::Conn::new(mysql::Opts::from_url(server).unwrap()).unwrap();
        mysql::prelude::Queryable::exec::<String, _, _>(&mut admin, "SELECT table_name FROM information_schema.tables WHERE table_schema = ? AND table_name LIKE ?", (shared.clone(), like)).unwrap()
    };
    let (gone, stays) = (host.registry.get("gone").unwrap().unwrap(), host.registry.get("stays").unwrap().unwrap());
    assert!(!tables(format!("c{}\\_%", gone.id)).is_empty());
    host.registry.set_state("gone", Standing::Deleted, Some(10)).unwrap();
    host.housekeeping(10).await;
    assert!(host.registry.get("gone").unwrap().is_none());
    assert!(tables(format!("c{}\\_%", gone.id)).is_empty(), "its tables went");
    assert!(!tables(format!("c{}\\_%", stays.id)).is_empty(), "another's stayed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_past_its_budget_takes_no_more_posts_in_any_community() {
    let running = start_host_with_extra(600, "", "").await;
    let mut config = running.host.config.clone();
    config.storage.budget_bytes = 1;
    let host = Host::new(config, Registry::scratch().unwrap());
    host.registry.create("busy", "Busy", Storage::Sqlite, "did:example:owner", "test", 1).unwrap();
    let tenant = host.tenant("busy").await.unwrap().unwrap();
    // Measured: one byte is the whole budget, and any store is more (WYV-22).
    tokio::task::block_in_place(|| host.measure());
    assert!(host.full.load(std::sync::atomic::Ordering::Relaxed));
    assert!(tenant.state.community.host_full.get().is_some_and(|full| full.load(std::sync::atomic::Ordering::Relaxed)));
}

#[tokio::test]
async fn every_community_trusts_the_linked_account_issuers_the_host_names_or_wyvrn_verify() {
    let plain = start_host(600).await;
    let record = plain.host.registry.create("alpha", "Alpha", Storage::Sqlite, "did:example:ann", "test", 1).unwrap();
    assert_eq!(plain.host.settings(&record).community.linked_account_issuers, [wyvrn_community::claims::DEFAULT_ISSUER]);
    let named = start_host_with_extra(600, "", "[host]\nlinked_account_issuers = [\"did:web:verify.example\", \"did:web:other.example\"]").await;
    let record = named.host.registry.create("beta", "Beta", Storage::Sqlite, "did:example:bea", "test", 1).unwrap();
    assert_eq!(named.host.settings(&record).community.linked_account_issuers, ["did:web:verify.example", "did:web:other.example"]);
}
