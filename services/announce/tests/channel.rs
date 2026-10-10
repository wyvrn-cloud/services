//! The channel run for real: its HTTP server, and agents that talk to it over DIDComm
//! from endpoints of their own.

use std::sync::Arc;
use std::time::Duration;

use axum::{body::Bytes, extract::State, routing::post, Router};
use didcomm_agent::{Agent, Identity};
use didcomm_crypto_askar::{AskarCryptoService, AskarSigningKey};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex, Notify};
use wyvrn_announce::{
    channel::{self, Channel, ChannelInfo},
    server::{self, AppState},
    store::Store,
};

const BASIC_MESSAGE: &str = "https://didcomm.org/basicmessage/2.0/message";

/// Someone with an app: an agent reachable at an address of its own, and everything
/// it has been sent there.
struct Peer {
    agent: Arc<Agent>,
    inbox: Mutex<mpsc::UnboundedReceiver<Value>>,
}

impl Peer {
    async fn start() -> Self {
        Self::start_as(Identity::generate().unwrap()).await
    }

    async fn start_as(identity: Identity) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let agent = Arc::new(Agent::with_endpoint(identity, &url).unwrap());
        let (sent, inbox) = mpsc::unbounded_channel();
        let app = Router::new()
            .route(
                "/",
                post(|State((agent, sent)): State<(Arc<Agent>, mpsc::UnboundedSender<Value>)>, body: Bytes| async move {
                    let received = agent.receive(&body).await.expect("a message this peer can read");
                    let _ = sent.send(received.message);
                }),
            )
            .with_state((agent.clone(), sent));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { agent, inbox: Mutex::new(inbox) }
    }

    fn did(&self) -> String {
        self.agent.did()
    }

    async fn send(&self, to: &str, message_type: &str, body: Value) {
        self.agent.send(to, &json!({"type": message_type, "body": body})).await.unwrap();
    }

    /// The next message sent to this peer.
    async fn next(&self) -> Value {
        tokio::time::timeout(Duration::from_secs(10), self.inbox.lock().await.recv())
            .await
            .expect("a message within ten seconds")
            .expect("the inbox to be open")
    }

    /// The next message of `message_type`, passing over any others.
    async fn next_of(&self, message_type: &str) -> Value {
        loop {
            let message = self.next().await;
            if message["type"] == message_type {
                return message;
            }
        }
    }

    async fn nothing_more(&self) {
        let waited = tokio::time::timeout(Duration::from_millis(400), self.inbox.lock().await.recv()).await;
        assert!(waited.is_err(), "expected nothing more, got {:?}", waited.unwrap());
    }
}

struct Running {
    did: String,
    url: String,
    state: Arc<AppState>,
}

async fn start_channel(admins: &[String]) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let config: wyvrn_announce::config::Config = toml::from_str(&format!(
        "[server]\nbind = \"unused\"\npublic_url = \"{url}\"\n[identity]\npath = \"unused\"\n[store]\npath = \"unused\"\n"
    ))
    .unwrap();
    let (agent, did_document) = server::build_agent(&config, Identity::generate().unwrap()).unwrap();
    let channel = Channel {
        store: Store::in_memory().unwrap(),
        info: ChannelInfo {
            name: "Wyvrn Updates".into(),
            description: "Release notes".into(),
            url: url.clone(),
            picture: None,
            show_authors: false,
            max_post_length: 16_000,
        },
    };
    channel.name_admins(admins, server::now()).unwrap();
    let did = agent.did();
    let state = Arc::new(AppState {
        agent,
        channel,
        did_document,
        resolver: didcomm_quickstart::default_resolver(),
        deliver: Notify::new(),
    });
    tokio::spawn(server::run_deliveries(state.clone()));
    let app = server::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Running { did, url, state }
}

#[tokio::test]
async fn an_admins_post_reaches_every_subscriber_and_the_web_page_and_nobody_else_can_post() {
    let admin = Peer::start().await;
    let reader = Peer::start().await;
    let channel = start_channel(&[admin.did()]).await;

    // An app finds out what this is by asking.
    let disclosed = admin
        .agent
        .request(
            &channel.did,
            &json!({
                "type": "https://didcomm.org/discover-features/2.0/queries",
                "body": {"queries": [{"feature-type": "protocol", "match": "*"}]},
            }),
        )
        .await
        .unwrap();
    let protocols: Vec<&str> =
        disclosed.message["body"]["disclosures"].as_array().unwrap().iter().filter_map(|d| d["id"].as_str()).collect();
    assert!(protocols.contains(&channel::PIURI), "{protocols:?}");

    reader.send(&channel.did, channel::SUBSCRIBE, json!({})).await;
    let subscription = reader.next_of(channel::SUBSCRIPTION).await;
    assert_eq!(subscription["body"]["subscribed"], true);
    assert_eq!(subscription["body"]["role"], "subscriber");
    assert_eq!(subscription["body"]["channel"]["name"], "Wyvrn Updates");

    admin.send(&channel.did, channel::SUBSCRIBE, json!({})).await;
    assert_eq!(admin.next_of(channel::SUBSCRIPTION).await["body"]["role"], "admin");

    // A subscriber can neither post nor write back.
    reader.send(&channel.did, channel::POST, json!({"content": "let me in"})).await;
    assert_eq!(reader.next_of(channel::PROBLEM_REPORT).await["body"]["code"], "e.p.trust.not-authorized");
    reader.send(&channel.did, BASIC_MESSAGE, json!({"content": "nice update!"})).await;
    assert_eq!(reader.next_of(channel::PROBLEM_REPORT).await["body"]["code"], "e.p.msg.not-accepted");
    admin.nothing_more().await;

    admin.send(&channel.did, channel::POST, json!({"content": "# 0.0.9\n\nNow with announcements."})).await;
    let to_reader = reader.next_of(channel::ANNOUNCEMENT).await;
    assert_eq!(to_reader["body"]["seq"], 1);
    assert_eq!(to_reader["body"]["content"], "# 0.0.9\n\nNow with announcements.");
    assert_eq!(to_reader["from"], channel.did.as_str());
    // The author gets it too: in answer to the post, and as a subscriber, the same one.
    let first = admin.next_of(channel::ANNOUNCEMENT).await;
    let second = admin.next_of(channel::ANNOUNCEMENT).await;
    assert_eq!(first["id"], to_reader["id"]);
    assert_eq!(second["id"], to_reader["id"]);
    // And each subscriber once.
    reader.nothing_more().await;

    // The log, over DIDComm and on the web.
    let history = reader
        .agent
        .request(&channel.did, &json!({"type": channel::HISTORY_REQUEST, "body": {}}))
        .await
        .unwrap();
    assert_eq!(history.message["type"], channel::HISTORY);
    assert_eq!(history.message["body"]["announcements"][0]["id"], to_reader["id"]);

    let http = reqwest::Client::new();
    let page = http.get(&channel.url).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("<h1>0.0.9</h1>"), "{page}");
    assert!(page.contains("Now with announcements."));
    let single = http.get(format!("{}posts/1", channel.url)).send().await.unwrap();
    assert!(single.status().is_success());
    assert_eq!(http.get(format!("{}posts/2", channel.url)).send().await.unwrap().status(), 404);
    let listed: Value = http.get(format!("{}api/posts", channel.url)).send().await.unwrap().json().await.unwrap();
    assert_eq!(listed["posts"][0]["seq"], 1);
    let feed = http.get(format!("{}feed.xml", channel.url)).send().await.unwrap().text().await.unwrap();
    assert!(feed.contains("<title>0.0.9</title>"));
}

#[tokio::test]
async fn someone_who_unsubscribes_is_sent_nothing_more() {
    let admin = Peer::start().await;
    let reader = Peer::start().await;
    let channel = start_channel(&[admin.did()]).await;
    reader.send(&channel.did, channel::SUBSCRIBE, json!({})).await;
    reader.next_of(channel::SUBSCRIPTION).await;
    reader.send(&channel.did, channel::UNSUBSCRIBE, json!({})).await;
    assert_eq!(reader.next_of(channel::SUBSCRIPTION).await["body"]["subscribed"], false);

    admin.send(&channel.did, channel::POST, json!({"content": "anyone there?"})).await;
    admin.next_of(channel::ANNOUNCEMENT).await;
    reader.nothing_more().await;
}

#[tokio::test]
async fn a_subscriber_who_cannot_be_reached_is_left_where_they_were_to_be_tried_again() {
    let admin = Peer::start().await;
    let channel = start_channel(&[admin.did()]).await;
    // Subscribed, at an address nothing is listening on.
    let away = Identity::generate().unwrap();
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_url = format!("http://{}/", dead.local_addr().unwrap());
    drop(dead);
    let away_did = away.did(&dead_url).unwrap();
    channel.state.channel.store.subscribe(&away_did, server::now()).unwrap();

    admin.send(&channel.did, channel::POST, json!({"content": "one"})).await;
    admin.next_of(channel::ANNOUNCEMENT).await;
    server::deliver_pending(&channel.state).await.unwrap();
    assert_eq!(channel.state.channel.store.member(&away_did).unwrap().unwrap().delivered_seq, 0);
    // The post is logged all the same.
    assert_eq!(channel.state.channel.store.latest_seq().unwrap(), 1);
}

/// A `from_prior` saying `prior` is now `new`, signed with `signer`'s key as `prior`'s.
async fn from_prior(signer: &Identity, prior: &str, new: &str) -> String {
    let key = AskarSigningKey::new(format!("{prior}#key-1"), signer.verification_key().clone());
    didcomm_core::rotation::build_from_prior(&AskarCryptoService, prior, new, &key, server::now()).await.unwrap()
}

#[tokio::test]
async fn an_admin_who_changes_did_stays_one_and_the_did_left_behind_is_nobody() {
    let before_identity = Identity::generate().unwrap();
    let signer = Identity::from_json(&before_identity.to_json().unwrap()).unwrap();
    let before = Peer::start_as(before_identity).await;
    let after = Peer::start().await;
    let channel = start_channel(&[before.did()]).await;
    before.send(&channel.did, channel::SUBSCRIBE, json!({})).await;
    assert_eq!(before.next_of(channel::SUBSCRIPTION).await["body"]["role"], "admin");

    // Someone else claiming to be what that admin became: signed with their own key.
    let impostor = Peer::start().await;
    let forged = from_prior(impostor.agent.identity(), &before.did(), &impostor.did()).await;
    impostor
        .agent
        .send(&channel.did, &json!({"type": channel::EMPTY, "from_prior": forged, "body": {}}))
        .await
        .unwrap();
    impostor.send(&channel.did, channel::POST, json!({"content": "mine now"})).await;
    assert_eq!(impostor.next_of(channel::PROBLEM_REPORT).await["body"]["code"], "e.p.trust.not-authorized");

    // The real change: told on a message that says nothing else.
    let notice = from_prior(&signer, &before.did(), &after.did()).await;
    after
        .agent
        .send(&channel.did, &json!({"type": channel::EMPTY, "from_prior": notice, "body": {}}))
        .await
        .unwrap();
    let told = after.next_of(channel::SUBSCRIPTION).await;
    assert_eq!((told["body"]["subscribed"].clone(), told["body"]["role"].clone()), (json!(true), json!("admin")));

    after.send(&channel.did, channel::POST, json!({"content": "still me"})).await;
    assert_eq!(after.next_of(channel::ANNOUNCEMENT).await["body"]["content"], "still me");

    before.send(&channel.did, channel::POST, json!({"content": "a device you removed"})).await;
    assert_eq!(before.next_of(channel::PROBLEM_REPORT).await["body"]["code"], "e.p.trust.superseded");
    assert_eq!(channel.state.channel.store.latest_seq().unwrap(), 1);
    // And posts go to the DID they have now, not the one they left.
    before.nothing_more().await;
}
