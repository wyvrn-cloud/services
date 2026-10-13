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

    /// Nothing more of what the channel is for. (Being asked for a profile, which it
    /// does of a subscriber it has no name for, is beside the point of every test
    /// that asks this.)
    async fn nothing_more(&self) {
        loop {
            let waited = tokio::time::timeout(Duration::from_millis(400), self.inbox.lock().await.recv()).await;
            match waited {
                Ok(Some(message)) if message["type"] == "https://didcomm.org/user-profile/1.0/request-profile" => continue,
                Ok(other) => panic!("expected nothing more, got {other:?}"),
                Err(_) => return,
            }
        }
    }
}

struct Running {
    did: String,
    url: String,
    state: Arc<AppState>,
}

async fn start_channel(admins: &[String]) -> Running {
    start_channel_resolving(admins, didcomm_quickstart::default_resolver()).await
}

/// A resolver that also knows the documents it is given: for a DID that would be
/// fetched from a host.
struct Knowing {
    documents: std::collections::HashMap<String, Value>,
    otherwise: Box<dyn didcomm_core::resolver::DIDResolver>,
}

#[async_trait::async_trait]
impl didcomm_core::resolver::DIDResolver for Knowing {
    async fn resolve(&self, did: &str) -> Result<Value, didcomm_core::resolver::ResolutionError> {
        match self.documents.get(did) {
            Some(document) => Ok(document.clone()),
            None => self.otherwise.resolve(did).await,
        }
    }

    async fn is_resolvable(&self, did: &str) -> bool {
        self.documents.contains_key(did) || self.otherwise.is_resolvable(did).await
    }
}

async fn start_channel_resolving(admins: &[String], resolver: Box<dyn didcomm_core::resolver::DIDResolver>) -> Running {
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
        resolver,
        admins: admins.to_vec(),
        deliver: Notify::new(),
        reacted: Default::default(),
        react: Notify::new(),
    });
    tokio::spawn(server::run_deliveries(state.clone()));
    tokio::spawn(server::run_reaction_notices(state.clone()));
    let app = server::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Running { did, url, state }
}

#[tokio::test]
async fn an_admin_named_by_a_public_address_is_one_when_they_write_from_their_own_did() {
    const ADDRESS: &str = "did:webvh:QmScid:id.example:ada";
    let ada = Peer::start().await;
    let someone = Peer::start().await;
    // Ada's public address: another DID, whose document lists the key her app
    // writes with among its key-agreement keys.
    let resolver = didcomm_quickstart::default_resolver();
    let own = resolver.resolve(&ada.did()).await.unwrap();
    let agreement = wyvrn_hosted_did::keys::keys_for(&own, "keyAgreement");
    let address = json!({
        "id": ADDRESS,
        "verificationMethod": [{ "id": format!("{ADDRESS}#device-1"), "type": "Multikey", "controller": ADDRESS, "publicKeyMultibase": agreement[0] }],
        "keyAgreement": [format!("{ADDRESS}#device-1")],
    });
    let knowing = Knowing { documents: [(ADDRESS.to_string(), address)].into(), otherwise: resolver };
    let channel = start_channel_resolving(&[ADDRESS.to_string()], Box::new(knowing)).await;

    // Someone else is nobody: the address names Ada's key, not theirs.
    someone.send(&channel.did, channel::SUBSCRIBE, json!({})).await;
    assert_eq!(someone.next_of(channel::SUBSCRIPTION).await["body"]["role"], "subscriber");
    someone.send(&channel.did, channel::POST, json!({"content": "let me in"})).await;
    assert_eq!(someone.next_of(channel::PROBLEM_REPORT).await["body"]["code"], "e.p.trust.not-authorized");

    // Ada writes from her own DID, and is the admin the address was named as.
    ada.send(&channel.did, channel::SUBSCRIBE, json!({})).await;
    assert_eq!(ada.next_of(channel::SUBSCRIPTION).await["body"]["role"], "admin");
    ada.send(&channel.did, channel::POST, json!({"content": "From the address's holder."})).await;
    assert_eq!(someone.next_of(channel::ANNOUNCEMENT).await["body"]["content"], "From the address's holder.");

    // She is in the list once, as the DID she writes from: the address isn't a
    // second subscriber.
    ada.send(&channel.did, channel::SUBSCRIBERS_REQUEST, json!({})).await;
    let listed = ada.next_of(channel::SUBSCRIBERS).await;
    let dids: Vec<&str> = listed["body"]["subscribers"].as_array().unwrap().iter().filter_map(|member| member["did"].as_str()).collect();
    assert_eq!(dids.len(), 2, "{dids:?}");
    assert!(dids.contains(&ada.did().as_str()) && !dids.contains(&ADDRESS), "{dids:?}");
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

#[tokio::test]
async fn a_subscribers_reaction_is_counted_for_every_subscriber_and_on_the_web_page() {
    let admin = Peer::start().await;
    let reader = Peer::start().await;
    let channel = start_channel(&[admin.did()]).await;
    for peer in [&admin, &reader] {
        peer.send(&channel.did, channel::SUBSCRIBE, json!({})).await;
        peer.next_of(channel::SUBSCRIPTION).await;
    }
    admin.send(&channel.did, channel::POST, json!({"content": "Version 2 is out"})).await;
    let post = reader.next_of(channel::ANNOUNCEMENT).await;
    let post_id = post["id"].as_str().unwrap();

    // The channel says it takes reactions, the way any app that does says so.
    let disclosed = reader
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
    assert!(disclosed.message["body"]["disclosures"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["id"] == channel::MESSAGE_REACTIONS_PIURI));

    let react = |emoji: Value, at: i64| {
        json!({
            "type": channel::MESSAGE_REACTIONS,
            "created_time": at,
            "body": {"to": {"id": post_id, "author": channel.did}, "emoji": emoji},
        })
    };
    reader.agent.send(&channel.did, &react(json!(["🎉"]), server::now())).await.unwrap();

    // Both are told how many, and each what is theirs. A subscriber isn't told
    // who; an admin, who can list the subscribers anyway, is.
    let told_reader = reader.next_of(channel::REACTIONS).await;
    assert_eq!(told_reader["body"]["id"], post_id);
    assert_eq!(told_reader["body"]["counts"], json!({"🎉": 1}));
    assert_eq!(told_reader["body"]["yours"], json!(["🎉"]));
    assert!(told_reader["body"].get("who").is_none());
    let told_admin = admin.next_of(channel::REACTIONS).await;
    assert_eq!(told_admin["body"]["counts"], json!({"🎉": 1}));
    assert_eq!(told_admin["body"]["yours"], json!([]));
    assert_eq!(told_admin["body"]["who"], json!({"🎉": [reader.did()]}));

    let page = reqwest::get(format!("{}posts/1", channel.url)).await.unwrap().text().await.unwrap();
    assert!(page.contains("🎉 <span>1</span>"), "{page}");

    // Taking it back is told too, and leaves the page as it was.
    reader.agent.send(&channel.did, &react(json!([]), server::now() + 1)).await.unwrap();
    assert_eq!(admin.next_of(channel::REACTIONS).await["body"]["counts"], json!({}));
    let page = reqwest::get(format!("{}posts/1", channel.url)).await.unwrap().text().await.unwrap();
    assert!(!page.contains("class=\"reactions\""), "{page}");
}
