//! The server run for real: its HTTP endpoint, and members' agents that talk to it
//! over DIDComm from endpoints of their own.

use std::sync::Arc;
use std::time::Duration;

use axum::{body::Bytes, extract::State, routing::post, Router};
use didcomm_agent::{Agent, Identity};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use wyvrn_community::{
    community::Community,
    server::{self, AppState},
    store::Store,
};

const C: &str = "https://wyvrn.app/community/1.0/";
const W: &str = "https://didcomm.org/workflow/1.0/";
const PROBLEM: &str = "https://didcomm.org/report-problem/2.0/problem-report";

/// Someone with an app: an agent reachable at an address of its own, and everything
/// it has been sent there.
struct Peer {
    agent: Arc<Agent>,
    server: String,
    inbox: Mutex<mpsc::UnboundedReceiver<Value>>,
}

impl Peer {
    async fn start(server: &Running) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let agent = Arc::new(Agent::with_endpoint(Identity::generate().unwrap(), &url).unwrap());
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
        Self { agent, server: server.did.clone(), inbox: Mutex::new(inbox) }
    }

    fn did(&self) -> String {
        self.agent.did()
    }

    /// Says something to the server and returns what it answers.
    async fn ask(&self, message_type: &str, body: Value) -> Value {
        let message = json!({ "type": message_type, "body": body, "return_route": "all" });
        self.agent.send(&self.server, &message).await.expect("sent").expect("an answer").message
    }

    async fn community(&self, kind: &str, body: Value) -> Value {
        self.ask(&format!("{C}{kind}"), body).await
    }

    async fn workflow(&self, kind: &str, body: Value) -> Value {
        self.ask(&format!("{W}{kind}"), body).await
    }

    /// The next message of `message_type` the server sent this peer unasked.
    async fn next_of(&self, message_type: &str) -> Value {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(10), self.inbox.lock().await.recv())
                .await
                .unwrap_or_else(|_| panic!("a {message_type} within ten seconds"))
                .expect("the inbox to be open");
            if message["type"] == message_type {
                return message;
            }
        }
    }

    async fn next_message(&self) -> Value {
        self.next_of(&format!("{C}message")).await["body"].clone()
    }

    async fn nothing_more(&self) {
        if let Ok(got) = tokio::time::timeout(Duration::from_millis(400), self.inbox.lock().await.recv()).await {
            panic!("expected nothing more, got {got:?}");
        }
    }
}

struct Running {
    did: String,
    url: String,
    state: Arc<AppState>,
}

impl Running {
    /// Lets `peers` in: the first as an admin, the rest as members.
    fn admit(&self, peers: &[&Peer]) {
        for (n, peer) in peers.iter().enumerate() {
            let role = if n == 0 { wyvrn_community::store::Role::Admin } else { wyvrn_community::store::Role::Member };
            self.state.community.store.set_member(&peer.did(), role, 1).unwrap();
        }
    }
}

async fn start_server(settings: &str) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let config: wyvrn_community::config::Config = toml::from_str(&format!(
        "[server]\nbind = \"unused\"\npublic_url = \"{url}\"\n[identity]\npath = \"unused\"\n[store]\npath = \"unused\"\n[community]\nname = \"The Workshop\"\n{settings}\n"
    ))
    .unwrap();
    let (agent, did_document) = server::build_agent(&config, Identity::generate().unwrap()).unwrap();
    let did = agent.did();
    let mut community = Community::new(Store::in_memory().unwrap(), config.community.clone(), Some(wyvrn_service::profile::Kept::scratch().unwrap())).unwrap().with_voice(config.voice.clone());
    let assistant = config.assistant.clone().map(wyvrn_community::assistant::Assistant::new);
    community.assistant = assistant.as_ref().map(wyvrn_community::assistant::Assistant::listing);
    let state = AppState::new(agent, Default::default(), community, did_document, didcomm_quickstart::default_resolver());
    let state = Arc::new(match assistant {
        Some(assistant) => state.with_assistant(assistant),
        None => state,
    });
    tokio::spawn(server::run_deliveries(state.clone()));
    let app = server::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Running { did, url, state }
}

fn kind(message: &Value) -> &str {
    message["type"].as_str().unwrap_or_default().rsplit('/').next().unwrap_or_default()
}

/// Opens a channel as `admin` and returns its id.
async fn open_channel(admin: &Peer, name: &str) -> String {
    let made = admin.community("create-channel", json!({ "name": name })).await;
    assert_eq!(kind(&made), "community", "{made}");
    made["body"]["created"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn someone_not_let_in_is_told_so_and_nothing_else() {
    let server = start_server("").await;
    let admin = Peer::start(&server).await;
    let stranger = Peer::start(&server).await;
    server.admit(&[&admin]);
    let channel = open_channel(&admin, "general").await;

    // Asking to join puts them on the waiting list, and says nothing of the community.
    let waiting = stranger.community("join", json!({})).await;
    assert_eq!(kind(&waiting), "waiting", "{waiting}");
    assert_eq!(waiting["body"], json!({}));

    for (message_type, body) in [
        (format!("{C}post"), json!({ "channel": channel, "content": "let me in" })),
        (format!("{C}history-request"), json!({ "channel": channel })),
        (format!("{C}members-request"), json!({})),
        (format!("{W}start"), json!({ "template_id": "answer-question", "instance_id": "i1", "context": { "question": "x" } })),
    ] {
        let answer = stranger.ask(&message_type, body).await;
        assert_eq!(answer["type"], PROBLEM, "{message_type}: {answer}");
        assert_eq!(answer["body"]["code"], "e.p.trust.not-authorized");
        // Nothing of the community in the refusal.
        assert!(!answer.to_string().contains("general") && !answer.to_string().contains("Workshop"));
    }
    assert_eq!(server.state.community.store.channel(&channel).unwrap().unwrap().latest_seq, 0);
}

#[tokio::test]
async fn an_admin_changes_what_the_community_is_called_and_everyone_who_joined_is_told() {
    let server = start_server("").await;
    let (admin, member) = (Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member]);
    member.community("join", json!({})).await;

    // Not a member's to change.
    let refused = member.community("set-profile", json!({ "profile": { "displayName": "Mine now" } })).await;
    assert_eq!(refused["body"]["code"], "e.p.trust.not-authorized");

    let changed = admin.community("set-profile", json!({ "profile": { "displayName": "The Workshop, Reopened", "description": "Tools and talk." } })).await;
    assert_eq!(kind(&changed), "community");
    assert_eq!((changed["body"]["name"].as_str(), changed["body"]["description"].as_str()), (Some("The Workshop, Reopened"), Some("Tools and talk.")));
    let told = member.next_of(&format!("{C}community")).await;
    assert_eq!(told["body"]["name"], "The Workshop, Reopened");

    // A name emptied is what the settings say again; the description is as it was.
    let back = admin.community("set-profile", json!({ "profile": { "displayName": null } })).await;
    assert_eq!((back["body"]["name"].as_str(), back["body"]["description"].as_str()), (Some("The Workshop"), Some("Tools and talk.")));
    assert_eq!(admin.community("set-profile", json!({ "profile": {} })).await["body"]["code"], "e.p.msg.invalid-profile");
}

#[tokio::test]
async fn an_admin_opens_channels_and_everyone_who_joined_is_told() {
    let server = start_server("").await;
    let (admin, member) = (Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member]);

    let joined = member.community("join", json!({})).await;
    assert_eq!(kind(&joined), "community");
    assert_eq!(joined["body"]["name"], "The Workshop");
    assert_eq!(joined["body"]["role"], "member");
    assert_eq!(joined["body"]["joined"], true);
    assert_eq!(joined["body"]["channels"], json!([]));

    // A member can't open one here; an admin can, and the member's app hears of it.
    let refused = member.community("create-channel", json!({ "name": "mine" })).await;
    assert_eq!(refused["body"]["code"], "e.p.trust.not-authorized");
    // A text channel's name is one word in lowercase, as on IRC (any script).
    for bad in ["design review", "Design", "design!", "#design"] {
        let refused = admin.community("create-channel", json!({ "name": bad })).await;
        assert_eq!(refused["body"]["code"], "e.p.msg.invalid-name", "{bad}");
    }
    let channel = open_channel(&admin, " design-review ").await;
    let told = member.next_of(&format!("{C}community")).await;
    assert_eq!(told["body"]["channels"][0]["name"], "design-review");
    assert_eq!(told["body"]["channels"][0]["id"], channel);
    assert_eq!(told["body"]["role"], "member");
    // Who opened it is told to them alone (yours), and to nobody else.
    assert_eq!(told["body"]["channels"][0].get("yours"), None);

    // No two open channels share a name.
    let twice = admin.community("create-channel", json!({ "name": "design-review" })).await;
    assert_eq!(twice["body"]["code"], "e.p.msg.name-taken");
    open_channel(&admin, "にほんご").await;
    open_channel(&admin, "v1.2_notes").await;

    // Renamed, given a topic, and closed: a closed channel takes no more posts.
    let changed = admin.community("update-channel", json!({ "channel": channel, "name": "design", "topic": "What we are building", "closed": true })).await;
    let mut held = changed["body"]["channels"].as_array().unwrap().iter().find(|c| c["id"] == channel.as_str()).unwrap().clone();
    // An admin may do everything a channel gives.
    assert_eq!(held["permissions"].as_array().unwrap().len(), 11);
    held.as_object_mut().unwrap().remove("permissions");
    assert_eq!(held, json!({ "id": channel, "name": "design", "topic": "What we are building", "closed": true, "latest_seq": 0, "yours": true }));
    let late = admin.community("post", json!({ "channel": channel, "content": "too late" })).await;
    assert_eq!(late["body"]["code"], "e.p.msg.closed");
    let renamed = admin.community("update-channel", json!({ "channel": channel, "name": "Design" })).await;
    assert_eq!(renamed["body"]["code"], "e.p.msg.invalid-name");
    // Its name taken while it was closed, it isn't opened again over the new one.
    open_channel(&admin, "design").await;
    let reopened = admin.community("update-channel", json!({ "channel": channel, "closed": false })).await;
    assert_eq!(reopened["body"]["code"], "e.p.msg.name-taken");
    // ...and only an admin, or whoever opened it, changes a channel.
    let meddling = member.community("update-channel", json!({ "channel": channel, "closed": false })).await;
    assert_eq!(meddling["body"]["code"], "e.p.trust.not-authorized");
}

#[tokio::test]
async fn a_post_is_answered_reacted_to_changed_pinned_and_taken_back_and_everyone_is_told() {
    let server = start_server("").await;
    let (admin, member, away) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member, &away]);
    let channel = open_channel(&admin, "general").await;
    admin.community("join", json!({})).await;
    member.community("join", json!({})).await;
    let changed = format!("{C}changed");

    admin.community("post", json!({ "channel": channel, "id": "p1", "content": "Shall we ship?" })).await;
    // An answer names what it answers, and calls on whom it likes among the members.
    let answer = member.community("post", json!({ "channel": channel, "id": "p2", "content": "Yes @admin", "reply_to": { "id": "p1", "author": admin.did(), "snippet": "Shall we ship?" }, "mentions": [{ "who": admin.did(), "name": " admin " }, { "who": "did:example:nobody", "name": "x" }, { "who": "everyone" }] })).await;
    assert_eq!(kind(&answer), "posted", "{answer}");
    admin.next_message().await;
    let got = admin.next_message().await;
    assert_eq!(got["id"], "p2");
    assert_eq!(got["reply_to"], json!({ "id": "p1", "author": admin.did(), "snippet": "Shall we ship?" }));
    // Someone who isn't a member is left out, and only an admin calls on everyone.
    assert_eq!(got["mentions"], json!([{ "who": admin.did(), "name": "admin" }]));
    member.next_message().await;
    member.next_message().await;
    assert_eq!(member.community("post", json!({ "channel": channel, "content": "to nothing", "reply_to": { "id": "nope", "author": admin.did() } })).await["body"]["code"], "e.p.msg.unknown-post");
    // ...nor to a post said to be someone else's.
    assert_eq!(member.community("post", json!({ "channel": channel, "content": "to nothing", "reply_to": { "id": "p1", "author": member.did() } })).await["body"]["code"], "e.p.msg.unknown-post");

    // A reaction is all of one member's emoji on a post; everyone is told who put what.
    let reacted = member.ask("https://wyvrn.app/message-reactions/1.0/reactions", json!({ "to": { "id": "p1" }, "emoji": ["👍", "🎉"] })).await;
    assert_eq!(kind(&reacted), "changed", "{reacted}");
    assert_eq!(reacted["body"]["reactions"], json!([{ "emoji": "🎉", "by": [member.did()] }, { "emoji": "👍", "by": [member.did()] }]));
    assert_eq!(admin.next_of(&changed).await["body"]["reactions"].as_array().unwrap().len(), 2);
    member.next_of(&changed).await;
    admin.ask("https://wyvrn.app/message-reactions/1.0/reactions", json!({ "to": { "id": "p1" }, "emoji": ["👍"] })).await;
    let now = member.ask("https://wyvrn.app/message-reactions/1.0/reactions", json!({ "to": { "id": "p1" }, "emoji": ["👍"] })).await;
    // (Two who reacted in the same second are in no particular order.)
    let by = |post: &Value| {
        let mut by: Vec<String> = post["reactions"][0]["by"].as_array().unwrap().iter().map(|did| did.as_str().unwrap().to_string()).collect();
        by.sort();
        by
    };
    let mut both = vec![member.did(), admin.did()];
    both.sort();
    assert_eq!((now["body"]["reactions"][0]["emoji"].as_str(), by(&now["body"])), (Some("👍"), both.clone()));
    assert_eq!(member.ask("https://wyvrn.app/message-reactions/1.0/reactions", json!({ "to": { "id": "p1" }, "emoji": ["thumbs up"] })).await["body"]["code"], "e.p.msg.invalid-reaction");
    assert_eq!(member.ask("https://wyvrn.app/message-reactions/1.0/reactions", json!({ "to": { "id": "nope" }, "emoji": ["👍"] })).await["body"]["code"], "e.p.msg.unknown-post");

    // A post is changed by whoever wrote it, and nobody else.
    assert_eq!(admin.ask("https://wyvrn.app/message-edits/1.0/edit", json!({ "to": { "id": "p2" }, "content": "No" })).await["body"]["code"], "e.p.trust.not-authorized");
    let edited = member.ask("https://wyvrn.app/message-edits/1.0/edit", json!({ "to": { "id": "p2" }, "content": "Yes, on Friday" })).await;
    assert_eq!(edited["body"]["content"], "Yes, on Friday");
    assert!(edited["body"]["edited_at"].is_i64());
    // (Who it calls on is as it was, said again or not.)
    assert_eq!(edited["body"]["mentions"], json!([{ "who": admin.did(), "name": "admin" }]));

    // Pinned by an admin, or by whoever opened the channel.
    assert_eq!(member.ask("https://wyvrn.app/message-pins/1.0/pin", json!({ "to": { "id": "p1" } })).await["body"]["code"], "e.p.trust.not-authorized");
    assert_eq!(admin.ask("https://wyvrn.app/message-pins/1.0/pin", json!({ "to": { "id": "p1" } })).await["body"]["pinned"], true);
    assert_eq!(admin.ask("https://wyvrn.app/message-pins/1.0/unpin", json!({ "to": { "id": "p1" } })).await["body"].get("pinned"), None);

    // Taken back, its place is kept and what it said is gone.
    let gone = member.ask("https://wyvrn.app/message-edits/1.0/delete", json!({ "to": { "id": "p2" } })).await;
    assert_eq!((gone["body"]["deleted"].as_bool(), gone["body"]["content"].as_str(), gone["body"]["seq"].as_i64()), (Some(true), Some(""), Some(2)));
    assert_eq!(gone["body"].get("removed_by"), None);
    assert_eq!(member.ask("https://wyvrn.app/message-edits/1.0/edit", json!({ "to": { "id": "p2" }, "content": "again" })).await["body"]["code"], "e.p.msg.deleted");

    // Someone else's is removed by an admin, or whoever opened the channel, and
    // says who by; by nobody else.
    member.community("post", json!({ "channel": channel, "id": "p3", "content": "Buy my thing" })).await;
    assert_eq!(member.ask("https://wyvrn.app/message-edits/1.0/delete", json!({ "to": { "id": "p1" } })).await["body"]["code"], "e.p.trust.not-authorized");
    let removed = admin.ask("https://wyvrn.app/message-edits/1.0/delete", json!({ "to": { "id": "p3" } })).await;
    assert_eq!((removed["body"]["deleted"].as_bool(), removed["body"]["content"].as_str()), (Some(true), Some("")));
    assert_eq!(removed["body"]["removed_by"], admin.did());
    // ...and an edit is still its author's alone.
    assert_eq!(admin.ask("https://wyvrn.app/message-edits/1.0/edit", json!({ "to": { "id": "p1" }, "content": "Mine" })).await["body"]["content"], "Mine");

    // Someone who was away asks what changed since what they hold, and is given
    // each post once, as it now stands.
    let community = away.community("join", json!({})).await;
    let latest = community["body"]["latest_rev"].as_i64().unwrap();
    assert!(latest > 0);
    let missed = away.community("changes-request", json!({ "after_rev": 0 })).await;
    assert_eq!(kind(&missed), "changes");
    let posts = missed["body"]["messages"].as_array().unwrap();
    assert_eq!(posts.iter().map(|post| post["id"].as_str().unwrap()).collect::<Vec<_>>(), ["p2", "p3", "p1"]);
    assert_eq!(by(&posts[2]), both);
    assert_eq!(posts[0]["deleted"], true);
    assert_eq!(posts[1]["removed_by"], admin.did());
    assert_eq!((missed["body"]["rev"].as_i64(), missed["body"]["more"].as_bool()), (Some(latest), Some(false)));
    assert_eq!(away.community("changes-request", json!({ "after_rev": latest })).await["body"]["messages"], json!([]));
}

#[tokio::test]
async fn a_thread_grows_from_a_post_and_what_is_said_in_it_is_read_there() {
    let server = start_server("").await;
    let (admin, member, bystander) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member, &bystander]);
    let channel = open_channel(&admin, "general").await;
    admin.community("post", json!({ "channel": channel, "id": "root", "content": "Where shall we meet?" })).await;
    member.community("post", json!({ "channel": channel, "id": "a1", "content": "The usual place", "reply_to": { "id": "root" } })).await;

    // Only someone who wrote in the replies it grows from starts one.
    assert_eq!(bystander.community("create-thread", json!({ "to": { "id": "root" } })).await["body"]["code"], "e.p.trust.not-authorized");
    let grown = member.community("create-thread", json!({ "to": { "id": "root" }, "name": "  Meeting   place " })).await;
    assert_eq!(kind(&grown), "changed", "{grown}");
    assert_eq!(grown["body"]["grew"]["name"], "Meeting place");
    assert_eq!(grown["body"]["grew"]["count"], 0);
    assert_eq!(grown["body"]["grew"]["created_by"], member.did());

    // Said in the thread, a post is the thread's, and the post it grew from counts it.
    let said = admin.community("post", json!({ "channel": channel, "id": "t1", "content": "By the fountain", "thread": "root" })).await;
    assert_eq!(kind(&said), "posted", "{said}");
    // ...as is an answer to it, with nothing said of the thread.
    member.community("post", json!({ "channel": channel, "id": "t2", "content": "Fine", "reply_to": { "id": "t1" } })).await;
    assert_eq!(admin.community("post", json!({ "channel": channel, "content": "x", "thread": "a1" })).await["body"]["code"], "e.p.msg.unknown-thread");
    assert_eq!(admin.community("create-thread", json!({ "to": { "id": "t1" } })).await["body"]["code"], "e.p.msg.in-thread");

    let ids = |page: &Value| page["body"]["messages"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    // The channel reads as it did before the thread, with the count on what it grew from.
    let main = bystander.community("history-request", json!({ "channel": channel })).await;
    assert_eq!(ids(&main), ["root", "a1"]);
    assert_eq!(main["body"]["messages"][0]["grew"]["count"], 2);
    let thread = bystander.community("history-request", json!({ "channel": channel, "thread": "root" })).await;
    assert_eq!(ids(&thread), ["t1", "t2"]);
    assert_eq!(thread["body"]["messages"][1]["thread"], "root");
}

#[tokio::test]
async fn members_may_open_channels_where_the_community_says_so() {
    let server = start_server("members_may_create_channels = true").await;
    let (admin, member) = (Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member]);
    let channel = open_channel(&member, "a-session").await;
    // Whoever opened it may close it.
    let closed = member.community("update-channel", json!({ "channel": channel, "closed": true })).await;
    assert_eq!(closed["body"]["channels"][0]["closed"], true);
}

#[tokio::test]
async fn a_post_reaches_everyone_who_joined_its_author_included_and_can_be_read_back() {
    let server = start_server("").await;
    let (admin, member, away) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member, &away]);
    let channel = open_channel(&admin, "general").await;
    admin.community("join", json!({})).await;
    member.community("join", json!({})).await;

    let posted = member.community("post", json!({ "channel": channel, "id": "p1", "content": "  hello everyone  " })).await;
    assert_eq!(kind(&posted), "posted", "{posted}");
    assert_eq!(posted["body"], json!({ "channel": channel, "seq": 1, "id": "p1" }));
    for peer in [&admin, &member] {
        let got = peer.next_message().await;
        assert_eq!(got["content"], "hello everyone");
        assert_eq!(got["seq"], 1);
        assert_eq!(got["id"], "p1");
        assert_eq!(got["channel"], channel);
        assert_eq!(got["author"]["did"], member.did());
    }
    // Someone let in who hasn't joined is sent nothing.
    away.nothing_more().await;

    // Sent again (a retry), it is the same post, not a second.
    let again = member.community("post", json!({ "channel": channel, "id": "p1", "content": "hello everyone" })).await;
    assert_eq!(again["body"]["seq"], 1);
    // ...and nobody else can use its id.
    assert_eq!(admin.community("post", json!({ "channel": channel, "id": "p1", "content": "mine now" })).await["body"]["code"], "e.p.msg.id-taken");

    for n in 2..=5 {
        admin.community("post", json!({ "channel": channel, "content": format!("post {n}") })).await;
    }
    // Read back by someone who wasn't there, newest page first, each page oldest first.
    let page = away.community("history-request", json!({ "channel": channel, "limit": 2 })).await;
    assert_eq!(kind(&page), "history");
    let seqs = |page: &Value| page["body"]["messages"].as_array().unwrap().iter().map(|m| m["seq"].as_i64().unwrap()).collect::<Vec<_>>();
    assert_eq!(seqs(&page), [4, 5]);
    assert_eq!(page["body"]["more"], true);
    let earlier = away.community("history-request", json!({ "channel": channel, "before": 4, "limit": 10 })).await;
    assert_eq!(seqs(&earlier), [1, 2, 3]);
    assert_eq!(earlier["body"]["more"], false);

    // Nothing empty, and nothing for a channel that isn't there.
    assert_eq!(member.community("post", json!({ "channel": channel, "content": "   " })).await["body"]["code"], "e.p.msg.empty");
    assert_eq!(member.community("post", json!({ "channel": "nope", "content": "hi" })).await["body"]["code"], "e.p.msg.unknown-channel");
}

#[tokio::test]
async fn an_admin_decides_who_is_let_in() {
    let server = start_server("").await;
    let (admin, member, newcomer) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member]);

    // Asking puts them on the waiting list; only an admin lets them in.
    assert_eq!(kind(&newcomer.community("join", json!({})).await), "waiting");
    assert_eq!(member.community("set-member", json!({ "did": newcomer.did(), "role": "member" })).await["body"]["code"], "e.p.trust.not-authorized");
    let listed = admin.community("set-member", json!({ "did": newcomer.did(), "role": "member" })).await;
    assert_eq!(kind(&listed), "members");
    assert_eq!(listed["body"]["members"].as_array().unwrap().len(), 3);
    assert_eq!(kind(&newcomer.community("join", json!({})).await), "community");

    // Taken out again, they are refused again, and asking to join puts them back
    // on the waiting list.
    admin.community("set-member", json!({ "did": newcomer.did(), "role": null })).await;
    assert_eq!(newcomer.community("members-request", json!({})).await["type"], PROBLEM);
    assert_eq!(kind(&newcomer.community("join", json!({})).await), "waiting");

    // A community keeps its owner, the first admin: nobody steps them down or takes them out.
    assert_eq!(admin.community("set-member", json!({ "did": admin.did(), "role": "member" })).await["body"]["code"], "e.p.msg.owner");
    assert_eq!(admin.community("set-member", json!({ "did": admin.did(), "role": null })).await["body"]["code"], "e.p.msg.owner");
    admin.community("set-member", json!({ "did": member.did(), "role": "admin" })).await;
    assert_eq!(member.community("set-member", json!({ "did": admin.did(), "role": null })).await["body"]["code"], "e.p.msg.owner");
    // An admin who isn't the owner ranks no higher than another admin.
    let listed = admin.community("set-member", json!({ "did": newcomer.did(), "role": "admin" })).await;
    assert_eq!(kind(&listed), "members");
    assert_eq!(member.community("set-member", json!({ "did": newcomer.did(), "role": null })).await["body"]["code"], "e.p.trust.not-authorized");
    assert_eq!(kind(&admin.community("set-member", json!({ "did": member.did(), "role": "member" })).await), "members");
}

#[tokio::test]
async fn a_question_is_answered_as_a_form_by_whoever_gets_there_first() {
    let server = start_server("").await;
    let (admin, bot, other) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &bot, &other]);
    let channel = open_channel(&admin, "session").await;
    for peer in [&admin, &bot, &other] {
        peer.community("join", json!({})).await;
    }

    let question = json!({ "questions": [
        { "header": "Library", "text": "Which library should we use?", "options": [
            { "label": "Zod", "description": "Schemas with inference" }, { "label": "Valibot" } ] },
        { "text": "Which checks should run?", "multiple": true, "options": [ { "label": "Lint" }, { "label": "Tests" }, { "label": "Build" } ] },
    ] });
    let asked = bot.community("post", json!({ "channel": channel, "id": "q1", "content": "I need two decisions.", "question": question })).await;
    assert_eq!(kind(&asked), "posted", "{asked}");
    let seen = admin.next_message().await;
    assert_eq!(seen["question"]["questions"][0]["options"][0], json!({ "label": "Zod", "description": "Schemas with inference" }));
    assert!(seen["question"].get("answered_by").is_none());

    // The admin's app opens the form: the first question, a button for each answer.
    let first = admin.workflow("start", json!({ "template_id": "answer-question", "instance_id": "a1", "context": { "question": "q1" } })).await;
    assert_eq!(kind(&first), "status", "{first}");
    assert_eq!(first["body"]["allowed_events"], json!(["answer"]));
    let ui = first["body"]["ui"].to_string();
    assert!(ui.contains("Library (1 of 2)") && ui.contains("Which library should we use?") && ui.contains("Schemas with inference"), "{ui}");
    assert!(first["body"]["ui"].as_array().unwrap().iter().any(|item| item["type"] == "submit-button" && item["input"]["choice"] == "Valibot"));
    // Starting again finds the one under way; an answer that isn't offered is refused.
    let again = admin.workflow("start", json!({ "template_id": "answer-question", "instance_id": "a2", "context": { "question": "q1" } })).await;
    assert_eq!(again["body"]["code"], "multiplicity_violation");
    assert_eq!(again["body"]["args"]["instance_id"], "a1");
    let wrong = admin.workflow("advance", json!({ "instance_id": "a1", "event": "answer", "input": { "choice": "Joi" } })).await;
    assert_eq!(wrong["body"]["code"], "action_failed");

    // Someone else starts on it too, and is part-way through.
    let theirs = other.workflow("start", json!({ "template_id": "answer-question", "instance_id": "b1", "context": { "question": "q1" } })).await;
    assert_eq!(kind(&theirs), "status");

    // The second question takes several answers, and one of the answerer's own.
    let second = admin.workflow("advance", json!({ "instance_id": "a1", "event": "answer", "input": { "choice": "Zod" } })).await;
    assert_eq!(kind(&second), "status", "{second}");
    assert!(second["body"]["ui"].to_string().contains("check-box"));
    let nothing = admin.workflow("advance", json!({ "instance_id": "a1", "event": "answer", "input": {} })).await;
    assert_eq!(nothing["body"]["code"], "action_failed");
    let done = admin.workflow("advance", json!({ "instance_id": "a1", "event": "answer", "input": { "option-0": true, "option-2": "true", "other": "and format" } })).await;
    assert_eq!(kind(&done), "complete", "{done}");

    // The answer is a post in the channel, in the admin's name: text anyone can
    // read, and the answers as data for whoever asked.
    let answer = bot.next_of(&format!("{C}message")).await["body"].clone();
    let answer = if answer["id"] == "q1" { bot.next_message().await } else { answer };
    assert_eq!(answer["author"]["did"], admin.did());
    assert_eq!(answer["content"], "**Library** Zod\n**Which checks should run?** Lint, Build, and format");
    assert_eq!(answer["answer"], json!({ "to": "q1", "answers": [ { "chosen": ["Zod"] }, { "chosen": ["Lint", "Build"], "other": "and format" } ] }));

    // Whoever was part-way through is told it has been answered, and can't answer it.
    let beaten = other.workflow("advance", json!({ "instance_id": "b1", "event": "answer", "input": { "choice": "Valibot" } })).await;
    assert!(beaten["body"]["ui"].to_string().contains("This was answered by"), "{beaten}");
    assert_eq!(beaten["body"]["allowed_events"], json!([]));
    // Read back later, the question says who answered it.
    let history = other.community("history-request", json!({ "channel": channel })).await;
    assert_eq!(history["body"]["messages"][0]["question"]["answered_by"], admin.did());
    assert_eq!(history["body"]["messages"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_question_that_makes_no_sense_is_not_posted() {
    let server = start_server("").await;
    let admin = Peer::start(&server).await;
    server.admit(&[&admin]);
    let channel = open_channel(&admin, "general").await;
    for question in [
        json!({ "questions": [] }),
        json!({ "questions": [{ "text": "Pick", "options": [{ "label": "only one" }] }] }),
        json!({ "questions": [{ "text": "", "options": [{ "label": "a" }, { "label": "b" }] }] }),
        json!({ "questions": [{ "text": "Pick", "options": [{ "label": "Same" }, { "label": "same" }] }] }),
    ] {
        let refused = admin.community("post", json!({ "channel": channel, "content": "a question", "question": question })).await;
        assert_eq!(refused["body"]["code"], "e.p.msg.invalid-question", "{refused}");
    }
    assert_eq!(server.state.community.store.channel(&channel).unwrap().unwrap().latest_seq, 0);
}


#[tokio::test]
async fn whoever_asks_waits_for_an_admin_who_lets_them_in_or_turns_them_away() {
    let server = start_server("").await;
    let (admin, member) = (Peer::start(&server).await, Peer::start(&server).await);
    let (alice, bob) = (Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member]);
    // The admin is sent things unasked; the member, joined too, isn't told who waits.
    server.state.community.store.set_joined(&admin.did(), true).unwrap();
    server.state.community.store.set_joined(&member.did(), true).unwrap();

    assert_eq!(kind(&alice.community("join", json!({})).await), "waiting");
    // The admin hears of it at once, and how many wait now.
    let asked = admin.next_of(&format!("{C}asked-to-join")).await;
    assert_eq!(asked["body"]["did"], alice.did());
    assert_eq!(admin.next_of(&format!("{C}community")).await["body"]["waiting"], 1);
    // What they call themselves, for the admins who decide.
    alice.agent.send(&alice.server, &json!({ "type": "https://didcomm.org/user-profile/1.0/profile", "body": { "profile": { "displayName": "Alice" } } })).await.unwrap();
    assert_eq!(kind(&bob.community("join", json!({})).await), "waiting");
    assert_eq!(admin.next_of(&format!("{C}asked-to-join")).await["body"]["did"], bob.did());
    assert_eq!(admin.next_of(&format!("{C}community")).await["body"]["waiting"], 2);
    // Asking again is no news.
    assert_eq!(kind(&bob.community("join", json!({})).await), "waiting");
    admin.nothing_more().await;
    member.nothing_more().await;

    // An admin is told who is waiting, oldest first; a member isn't.
    let listed = admin.community("members-request", json!({})).await;
    let waiting = listed["body"]["waiting"].as_array().unwrap();
    // (Both asked in the same second here, so which is first is by DID.)
    let mut dids: Vec<String> = waiting.iter().map(|w| w["did"].as_str().unwrap().to_string()).collect();
    dids.sort();
    let mut expected = vec![alice.did(), bob.did()];
    expected.sort();
    assert_eq!(dids, expected);
    assert!(waiting.windows(2).all(|pair| pair[0]["asked_at"].as_i64() <= pair[1]["asked_at"].as_i64()));
    let alice_listed = waiting.iter().find(|w| w["did"] == alice.did()).unwrap();
    assert_eq!(alice_listed["name"], "Alice");
    assert!(member.community("members-request", json!({})).await["body"].get("waiting").is_none());

    // Let in from the list: joined, and told what the community is.
    let answer = admin.community("set-member", json!({ "did": alice.did(), "role": "member" })).await;
    assert_eq!(answer["body"]["waiting"].as_array().unwrap().len(), 1);
    let told = alice.next_of(&format!("{C}community")).await;
    assert_eq!((told["body"]["name"].as_str(), told["body"]["joined"].as_bool()), (Some("The Workshop"), Some(true)));
    assert!(told["body"].get("waiting").is_none(), "a member isn't told how many wait");
    assert_eq!(admin.next_of(&format!("{C}community")).await["body"]["waiting"], 1);
    let let_in = server.state.community.store.member(&alice.did()).unwrap().unwrap();
    // ...under the name she gave while waiting.
    assert!(let_in.joined);
    assert_eq!(let_in.name.as_deref(), Some("Alice"));

    // Turned away: off the list, not let in, and told so, with nothing of the community.
    let answer = admin.community("set-member", json!({ "did": bob.did(), "role": null })).await;
    assert_eq!(answer["body"]["waiting"], json!([]));
    assert!(server.state.community.store.member(&bob.did()).unwrap().is_none());
    assert_eq!(bob.next_of(&format!("{C}removed")).await["body"], json!({ "was": "waiting" }));
    bob.nothing_more().await;
    assert_eq!(admin.next_of(&format!("{C}community")).await["body"]["waiting"], 0);

    // Taken out once in: told so too.
    admin.community("set-member", json!({ "did": alice.did(), "role": null })).await;
    assert_eq!(alice.next_of(&format!("{C}removed")).await["body"], json!({ "was": "member" }));

    // Asking and taking it back.
    assert_eq!(kind(&bob.community("join", json!({})).await), "waiting");
    bob.agent.send(&bob.server, &json!({ "type": format!("{C}leave"), "body": {} })).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(server.state.community.store.waiting().unwrap().len(), 0);
    assert_eq!(admin.next_of(&format!("{C}asked-to-join")).await["body"]["did"], bob.did());
    // (The count went to 1 and back to 0.)
    assert_eq!(admin.next_of(&format!("{C}community")).await["body"]["waiting"], 1);
    assert_eq!(admin.next_of(&format!("{C}community")).await["body"]["waiting"], 0);
}

#[tokio::test]
async fn an_invite_lets_in_while_it_lasts_when_admission_is_by_invite() {
    let server = start_server("").await;
    let admin = Peer::start(&server).await;
    let (alice, bob, carol) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin]);

    // By default an admin lets people in: a code is noted beside them, not used.
    let made = admin.community("create-invite", json!({ "uses": 1, "note": "for Alice" })).await;
    assert_eq!(kind(&made), "invites", "{made}");
    let code = made["body"]["created"].as_str().unwrap().to_string();
    assert_eq!(made["body"]["invites"][0]["note"], "for Alice");
    assert_eq!(kind(&alice.community("join", json!({ "invite": code })).await), "waiting");
    let waiting = admin.community("members-request", json!({})).await["body"]["waiting"][0].clone();
    assert_eq!((waiting["invite"]["code"].as_str(), waiting["invite"]["created_by"]["did"].as_str()), (Some(code.as_str()), Some(admin.did().as_str())));
    assert_eq!(server.state.community.store.invite(&code).unwrap().unwrap().uses_left, Some(1));

    // By invite: the code lets in once, and everyone is told how admission stands.
    let set = admin.community("set-admission", json!({ "admission": "invite" })).await;
    assert_eq!(set["body"]["admission"], "invite");
    let joined = alice.community("join", json!({ "invite": code })).await;
    assert_eq!((kind(&joined), joined["body"]["joined"].as_bool()), ("community", Some(true)));
    assert!(server.state.community.store.waiting().unwrap().is_empty());
    assert_eq!(kind(&bob.community("join", json!({ "invite": code })).await), "waiting");
    assert_eq!(kind(&carol.community("join", json!({})).await), "waiting");
    // Used up, it is no longer listed.
    assert_eq!(admin.community("invites-request", json!({})).await["body"]["invites"], json!([]));

    // One that has expired lets nobody in, and can't be made.
    let past = admin.community("create-invite", json!({ "expires_at": 1 })).await;
    assert_eq!(past["body"]["code"], "e.p.msg.invalid-invite");
    assert_eq!(admin.community("create-invite", json!({ "uses": 0 })).await["body"]["code"], "e.p.msg.invalid-invite");

    // Open: everyone, invite or not.
    admin.community("set-admission", json!({ "admission": "open" })).await;
    assert_eq!(kind(&carol.community("join", json!({})).await), "community");
    assert_eq!(admin.community("set-admission", json!({ "admission": "everyone" })).await["body"]["code"], "e.p.msg.invalid-admission");
    assert_eq!(admin.community("set-admission", json!({})).await["body"]["code"], "e.p.msg.invalid-admission");
}

#[tokio::test]
async fn members_make_invites_only_when_an_admin_allows_and_revoke_only_their_own() {
    let server = start_server("").await;
    let (admin, member, other) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member, &other]);

    assert_eq!(member.community("create-invite", json!({})).await["body"]["code"], "e.p.trust.not-authorized");
    assert_eq!(member.community("set-admission", json!({ "members_may_invite": true })).await["body"]["code"], "e.p.trust.not-authorized");
    let set = admin.community("set-admission", json!({ "members_may_invite": true })).await;
    assert_eq!(set["body"]["members_may_invite"], true);

    let admins = admin.community("create-invite", json!({})).await["body"]["created"].as_str().unwrap().to_string();
    let theirs = member.community("create-invite", json!({ "uses": 3 })).await;
    let code = theirs["body"]["created"].as_str().unwrap().to_string();
    // A member is told of their own invites; an admin of everyone's.
    assert_eq!(theirs["body"]["invites"].as_array().unwrap().len(), 1);
    assert_eq!(admin.community("invites-request", json!({})).await["body"]["invites"].as_array().unwrap().len(), 2);
    assert_eq!(other.community("invites-request", json!({})).await["body"]["invites"], json!([]));

    assert_eq!(member.community("revoke-invite", json!({ "code": admins })).await["body"]["code"], "e.p.trust.not-authorized");
    assert_eq!(other.community("revoke-invite", json!({ "code": code })).await["body"]["code"], "e.p.trust.not-authorized");
    assert_eq!(admin.community("revoke-invite", json!({ "code": code })).await["body"]["invites"].as_array().unwrap().len(), 1);
    assert_eq!(member.community("revoke-invite", json!({ "code": code })).await["body"]["code"], "e.p.msg.unknown-invite");
}

#[tokio::test]
async fn a_member_who_leaves_is_sent_nothing_and_listed_as_left_until_they_come_back() {
    let server = start_server("").await;
    let (admin, member) = (Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member]);
    member.community("join", json!({})).await;
    let channel = open_channel(&admin, "general").await;
    member.next_of(&format!("{C}community")).await;

    let left = member.community("leave", json!({})).await;
    assert_eq!((kind(&left), left["body"]["joined"].as_bool()), ("community", Some(false)));
    admin.community("post", json!({ "channel": channel, "content": "while you were out" })).await;
    member.nothing_more().await;
    let listed = admin.community("members-request", json!({})).await;
    let entry = listed["body"]["members"].as_array().unwrap().iter().find(|m| m["did"] == member.did()).unwrap().clone();
    assert!(entry["left_at"].as_i64().is_some(), "{entry}");

    // Back at once, under any admission, and no longer listed as left.
    admin.community("set-admission", json!({ "admission": "admin" })).await;
    let back = member.community("join", json!({})).await;
    assert_eq!(back["body"]["joined"], true);
    let listed = admin.community("members-request", json!({})).await;
    let entry = listed["body"]["members"].as_array().unwrap().iter().find(|m| m["did"] == member.did()).unwrap().clone();
    assert!(entry.get("left_at").is_none(), "{entry}");
}


#[tokio::test]
async fn rules_that_are_required_are_accepted_before_joining_and_before_posting() {
    let server = start_server("").await;
    let (admin, member, stranger) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member]);
    member.community("join", json!({})).await;
    let channel = open_channel(&admin, "general").await;
    member.next_of(&format!("{C}community")).await;

    assert_eq!(member.community("set-rules", json!({ "rules": "Be kind." })).await["body"]["code"], "e.p.trust.not-authorized");
    assert_eq!(admin.community("set-rules", json!({ "required": true })).await["body"]["code"], "e.p.msg.invalid-rules");
    let set = admin.community("set-rules", json!({ "rules": "Be kind.", "required": true })).await;
    assert_eq!((set["body"]["rules"].as_str(), set["body"]["rules_version"].as_i64(), set["body"]["rules_required"].as_bool()), (Some("Be kind."), Some(1), Some(true)));
    // Whoever set them has accepted them; everyone joined is told.
    assert_eq!(set["body"]["rules_accepted"], 1);
    let told = member.next_of(&format!("{C}community")).await;
    assert_eq!((told["body"]["rules_version"].as_i64(), told["body"].get("rules_accepted")), (Some(1), None));

    // A member who hasn't accepted them can't post until they do.
    let refused = member.community("post", json!({ "channel": channel, "content": "hello" })).await;
    assert_eq!(refused["body"]["code"], "e.p.msg.rules-not-accepted");
    assert_eq!(member.community("accept-rules", json!({ "rules_version": 7 })).await["body"]["code"], "e.p.msg.invalid-rules");
    let accepted = member.community("accept-rules", json!({ "rules_version": 1 })).await;
    assert_eq!(accepted["body"]["rules_accepted"], 1);
    assert_eq!(kind(&member.community("post", json!({ "channel": channel, "content": "hello" })).await), "posted");

    // Someone outside is shown them, not put on the waiting list, until they agree.
    admin.community("set-admission", json!({ "admission": "open" })).await;
    let rules = stranger.community("join", json!({})).await;
    assert_eq!(kind(&rules), "rules", "{rules}");
    assert_eq!((rules["body"]["rules"].as_str(), rules["body"]["rules_version"].as_i64(), rules["body"]["name"].as_str()), (Some("Be kind."), Some(1), Some("The Workshop")));
    assert!(server.state.community.store.waiting().unwrap().is_empty());
    assert_eq!(kind(&stranger.community("join", json!({ "accept_rules": 0 })).await), "rules");
    let joined = stranger.community("join", json!({ "accept_rules": 1 })).await;
    assert_eq!((kind(&joined), joined["body"]["rules_accepted"].as_i64()), ("community", Some(1)));

    // A new text is a new version, owed again; taking a requirement off is not.
    let changed = admin.community("set-rules", json!({ "rules": "Be kind. No spam." })).await;
    assert_eq!(changed["body"]["rules_version"], 2);
    assert_eq!(member.community("post", json!({ "channel": channel, "content": "again" })).await["body"]["code"], "e.p.msg.rules-not-accepted");
    admin.community("set-rules", json!({ "required": false })).await;
    assert_eq!(kind(&member.community("post", json!({ "channel": channel, "content": "again" })).await), "posted");
    // Taken away entirely, nothing is said of them.
    let gone = admin.community("set-rules", json!({ "rules": "" })).await;
    assert!(gone["body"].get("rules").is_none(), "{gone}");
}

#[tokio::test]
async fn a_member_reports_a_post_or_a_member_and_only_admins_are_told() {
    let server = start_server("").await;
    let (admin, alice, bob) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &alice, &bob]);
    admin.community("join", json!({})).await;
    let channel = open_channel(&admin, "general").await;
    let posted = bob.community("post", json!({ "channel": channel, "id": "spam-1", "content": "buy now" })).await;
    assert_eq!(kind(&posted), "posted");
    admin.next_message().await;

    assert_eq!(alice.community("report", json!({ "post": "spam-1", "reason": "rude" })).await["body"]["code"], "e.p.msg.invalid-report");
    assert_eq!(bob.community("report", json!({ "post": "spam-1", "reason": "spam" })).await["body"]["code"], "e.p.msg.invalid-report");
    assert_eq!(alice.community("report", json!({ "post": "nope", "reason": "spam" })).await["body"]["code"], "e.p.msg.unknown-post");
    assert_eq!(alice.community("report", json!({ "post": "spam-1", "member": bob.did(), "reason": "spam" })).await["body"]["code"], "e.p.msg.invalid-report");

    let received = alice.community("report", json!({ "channel": channel, "post": "spam-1", "reason": "spam", "note": "third time today" })).await;
    assert_eq!(kind(&received), "report-received", "{received}");
    // The admin is sent the open reports, with who made each and the post as it stood.
    let told = admin.next_of(&format!("{C}reports")).await;
    let report = told["body"]["reports"][0].clone();
    assert_eq!((report["reason"].as_str(), report["note"].as_str(), report["by"]["did"].as_str()), (Some("spam"), Some("third time today"), Some(alice.did().as_str())));
    assert_eq!((report["post"]["content"].as_str(), report["post"]["author"]["did"].as_str(), report["post"]["removed"].as_bool()), (Some("buy now"), Some(bob.did().as_str()), Some(false)));

    // Reporting it again replaces the first; a member is reported apart.
    alice.community("report", json!({ "post": "spam-1", "reason": "other" })).await;
    alice.community("report", json!({ "member": bob.did(), "reason": "harassment" })).await;
    let open = admin.community("reports-request", json!({})).await;
    assert_eq!(open["body"]["reports"].as_array().unwrap().len(), 2, "{open}");

    // Only admins may see them. Removed since, the copy stays and says so.
    assert_eq!(alice.community("reports-request", json!({})).await["body"]["code"], "e.p.trust.not-authorized");
    admin.ask("https://wyvrn.app/message-edits/1.0/delete", json!({ "to": { "id": "spam-1", "author": bob.did() } })).await;
    let after = admin.community("reports-request", json!({})).await;
    let post_report = after["body"]["reports"].as_array().unwrap().iter().find(|r| r.get("post").is_some()).unwrap().clone();
    assert_eq!((post_report["post"]["content"].as_str(), post_report["post"]["removed"].as_bool()), (Some("buy now"), Some(true)));

    // Dealt with, it is off the open list and on the full one.
    let id = post_report["id"].as_str().unwrap();
    let resolved = admin.community("resolve-report", json!({ "id": id })).await;
    assert_eq!(resolved["body"]["reports"].as_array().unwrap().len(), 1);
    let all = admin.community("reports-request", json!({ "resolved": true })).await;
    let done = all["body"]["reports"].as_array().unwrap().iter().find(|r| r["id"] == id).unwrap().clone();
    assert_eq!(done["resolved_by"], admin.did());
    assert_eq!(admin.community("resolve-report", json!({ "id": "nope" })).await["body"]["code"], "e.p.msg.unknown-report");
}

#[tokio::test]
async fn an_admin_opens_voice_channels_which_take_no_posts() {
    let server = start_server("[voice]\nrelay_url = \"https://voice.example/\"").await;
    let (admin, member) = (Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member]);
    member.community("join", json!({})).await;

    // Only an admin, and only valid settings.
    let refused = member.community("create-channel", json!({ "name": "lounge", "kind": "voice" })).await;
    assert_eq!(refused["body"]["code"], "e.p.trust.not-authorized");
    let bad = admin.community("create-channel", json!({ "name": "lounge", "kind": "voice", "voice": { "speak": "nobody" } })).await;
    assert_eq!(bad["body"]["code"], "e.p.msg.invalid-voice");
    let made = admin.community("create-channel", json!({ "name": "lounge", "kind": "voice", "voice": { "screen": "admins", "limit": 25 } })).await;
    assert_eq!(kind(&made), "community", "{made}");
    let id = made["body"]["created"].as_str().unwrap().to_string();
    let listed = made["body"]["channels"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap().clone();
    assert_eq!((listed["kind"].as_str(), listed["voice"]["screen"].as_str(), listed["voice"]["limit"].as_i64()), (Some("voice"), Some("admins"), Some(25)));
    assert_eq!(made["body"]["voice"]["transport"], "moq");
    member.next_of(&format!("{C}community")).await;

    // It takes no posts.
    assert_eq!(member.community("post", json!({ "channel": id, "content": "hi" })).await["body"]["code"], "e.p.msg.voice-channel");

    // Its settings change a field at a time; the community's defaults by set-voice.
    let changed = admin.community("update-channel", json!({ "channel": id, "voice": { "transport": "webrtc", "limit": null } })).await;
    let listed = changed["body"]["channels"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap().clone();
    assert_eq!((listed["voice"]["transport"].as_str(), listed["voice"]["screen"].as_str(), listed["voice"].get("limit")), (Some("webrtc"), Some("admins"), None));
    let text = open_channel(&admin, "general").await;
    let set = admin.community("set-voice", json!({ "transport": "webrtc", "invites_to": text })).await;
    assert_eq!((set["body"]["voice"]["transport"].as_str(), set["body"]["voice"]["invites_to"].as_str()), (Some("webrtc"), Some(text.as_str())));
    assert_eq!(admin.community("set-voice", json!({ "invites_to": id })).await["body"]["code"], "e.p.msg.unknown-channel");
    assert_eq!(member.community("set-voice", json!({ "transport": "moq" })).await["body"]["code"], "e.p.trust.not-authorized");
}

#[tokio::test]
async fn a_server_without_voice_has_no_voice_channels() {
    let server = start_server("").await;
    let admin = Peer::start(&server).await;
    server.admit(&[&admin]);
    assert_eq!(admin.community("create-channel", json!({ "name": "lounge", "kind": "voice" })).await["body"]["code"], "e.p.msg.unsupported");
    let community = admin.community("join", json!({})).await;
    assert!(community["body"].get("voice").is_none(), "{community}");
}

// --- Voice channels (community-voice/1.0) ------------------------------------

const V: &str = "https://wyvrn.app/community-voice/1.0/";

/// A member's live connection to the server: DIDComm over a WebSocket at the
/// server's address, packed by the member's own agent.
struct Live {
    socket: reqwest_websocket::WebSocket,
}

impl Live {
    async fn open(peer: &Peer, server: &Running) -> Self {
        let url = server.url.replacen("http://", "ws://", 1);
        let response = reqwest_websocket::Upgrade::upgrade(reqwest::Client::new().get(&url)).send().await.unwrap();
        let socket = response.into_websocket().await.unwrap();
        let _ = peer;
        Self { socket }
    }

    async fn send(&mut self, peer: &Peer, kind: &str, body: Value) {
        use futures_util::SinkExt;
        let message = json!({ "id": uuid::Uuid::new_v4().to_string(), "type": format!("{V}{kind}"), "body": body, "return_route": "all" });
        let packed = peer.agent.messaging().pack(&message, &peer.server, Some(&peer.did())).await.unwrap();
        let frame = match String::from_utf8(packed.message) {
            Ok(text) => reqwest_websocket::Message::Text(text),
            Err(bytes) => reqwest_websocket::Message::Binary(bytes.into_bytes().into()),
        };
        self.socket.send(frame).await.unwrap();
    }

    /// The next message of `kind` on the connection.
    async fn next_of(&mut self, peer: &Peer, kind: &str) -> Value {
        use futures_util::StreamExt;
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(10), self.socket.next()).await.unwrap_or_else(|_| panic!("a {kind} within ten seconds")).unwrap().unwrap();
            let bytes = match frame {
                reqwest_websocket::Message::Text(text) => text.into_bytes(),
                reqwest_websocket::Message::Binary(bytes) => bytes.to_vec(),
                _ => continue,
            };
            let received = peer.agent.receive(&bytes).await.unwrap();
            if received.message["type"] == format!("{V}{kind}") {
                return received.message;
            }
        }
    }
}

async fn voice_server() -> Running {
    start_server("[voice]\nrelay_url = \"https://voice.example/\"\nrelay_fingerprint = \"ab12\"").await
}

async fn voice_channel(admin: &Peer, name: &str, voice: Value) -> String {
    let made = admin.community("create-channel", json!({ "name": name, "kind": "voice", "voice": voice })).await;
    assert_eq!(kind(&made), "community", "{made}");
    made["body"]["created"].as_str().unwrap().to_string()
}

async fn voice(peer: &Peer, kind: &str, body: Value) -> Value {
    peer.ask(&format!("{V}{kind}"), body).await
}

/// Says something the server answers with nothing (state, leave, keys, invite,
/// moderate), or with a refusal, which is returned.
async fn tell(peer: &Peer, kind: &str, body: Value) -> Option<Value> {
    let message = json!({ "type": format!("{V}{kind}"), "body": body, "return_route": "all" });
    peer.agent.send(&peer.server, &message).await.expect("sent").map(|answer| answer.message)
}

#[tokio::test]
async fn joining_a_voice_channel_gives_a_room_a_token_and_who_else_is_there_and_watchers_are_told() {
    let server = voice_server().await;
    let (alice, bob, carol) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&alice, &bob, &carol]);
    let lounge = voice_channel(&alice, "lounge", json!({})).await;

    // Carol watches over a live connection; asking anywhere else is refused.
    assert_eq!(voice(&carol, "watch-presence", json!({})).await["body"]["code"], "e.p.req.live-only");
    let mut live = Live::open(&carol, &server).await;
    live.send(&carol, "watch-presence", json!({})).await;
    let first = live.next_of(&carol, "presence").await;
    assert_eq!(first["body"]["channels"], json!([{ "channel": lounge, "participants": [] }]));

    // Alice joins the empty room.
    let joined = voice(&alice, "join", json!({ "channel": lounge })).await;
    assert_eq!(kind(&joined), "joined", "{joined}");
    let transport = &joined["body"]["transport"];
    assert_eq!((transport["kind"].as_str(), transport["url"].as_str(), transport["fingerprint"].as_str()), (Some("moq"), Some("https://voice.example/"), Some("ab12")));
    let path = transport["path"].as_str().unwrap().to_string();
    assert!(path.starts_with("c/") && path.ends_with(&lounge) && !path.contains("did:"), "{path}");
    assert_eq!(joined["body"]["participants"], json!([]));
    assert_eq!(joined["body"]["may"], json!({ "speak": true, "video": true, "screen": true }));
    let alice_id = joined["body"]["participant"].as_str().unwrap().to_string();
    let told = live.next_of(&carol, "presence").await;
    assert_eq!(told["body"]["channels"][0]["participants"][0]["did"], alice.did());

    // Bob joins and is told Alice is there; Alice's sharing reaches the watcher.
    let bobs = voice(&bob, "join", json!({ "channel": lounge })).await;
    assert_eq!(bobs["body"]["participants"][0]["participant"], alice_id);
    live.next_of(&carol, "presence").await;
    tell(&alice, "state", json!({ "channel": lounge, "muted": true, "deafened": false, "sharing": { "screen": true } })).await;
    let sharing = live.next_of(&carol, "presence").await;
    let entry = sharing["body"]["channels"][0]["participants"].as_array().unwrap().iter().find(|p| p["did"] == alice.did()).unwrap().clone();
    assert_eq!((entry["muted"].as_bool(), entry["sharing"]["screen"].as_bool()), (Some(true), Some(true)));

    // Leaving tells the watcher too.
    tell(&bob, "leave", json!({ "channel": lounge })).await;
    let after = live.next_of(&carol, "presence").await;
    assert_eq!(after["body"]["channels"][0]["participants"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn the_relay_is_told_what_each_token_may_publish_from_who_is_in_the_room_now() {
    let server = voice_server().await;
    let (admin, member) = (Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member]);
    let stage = voice_channel(&admin, "stage", json!({ "screen": "admins" })).await;
    let joined = voice(&member, "join", json!({ "channel": stage })).await;
    assert_eq!(joined["body"]["may"]["screen"], false);
    let token = joined["body"]["transport"]["token"].as_str().unwrap().to_string();
    let path = joined["body"]["transport"]["path"].as_str().unwrap().to_string();
    let id = joined["body"]["participant"].as_str().unwrap().to_string();

    let ask = |token: String| {
        let url = format!("{}voice/auth", server.url);
        async move {
            let response = reqwest::Client::new()
                .post(url)
                .json(&json!({ "id": "s1", "event": "connect", "node": "n", "transport": "quic", "path": "/", "query": format!("jwt={token}") }))
                .send()
                .await
                .unwrap();
            (response.status().as_u16(), response.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let (status, grant) = ask(token.clone()).await;
    assert_eq!(status, 200, "{grant}");
    let publish: Vec<String> = grant["publish"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect();
    assert!(publish.contains(&format!("{path}/{id}/mic")) && publish.contains(&format!("{path}/{id}/camera")));
    assert!(!publish.iter().any(|p| p.ends_with("/screen")), "{publish:?}");
    assert_eq!(grant["subscribe"], json!([format!("{path}/**")]));
    assert_eq!(grant["revalidate"], 15);
    assert!(grant["expires"].as_i64().unwrap() > wyvrn_service::now() + 15, "{grant}");

    // Muted by an admin, the microphone isn't granted; removed, nothing is.
    let admins = voice(&admin, "join", json!({ "channel": stage })).await;
    assert_eq!(admins["body"]["may"]["screen"], true);
    tell(&admin, "moderate", json!({ "channel": stage, "participant": id, "mute": true })).await;
    let (_, muted) = ask(token.clone()).await;
    assert!(!muted["publish"].as_array().unwrap().iter().any(|p| p.as_str().unwrap().ends_with("/mic")), "{muted}");
    assert_eq!(tell(&member, "moderate", json!({ "channel": stage, "participant": id, "mute": false })).await.unwrap()["body"]["code"], "e.p.trust.not-authorized");
    tell(&admin, "moderate", json!({ "channel": stage, "participant": id, "remove": true })).await;
    assert_eq!(ask(token).await.0, 403);
    assert_eq!(ask("nonsense".into()).await.0, 403);
}

#[tokio::test]
async fn keys_are_passed_on_as_they_are_to_those_in_the_room_and_invitations_ring_or_are_posted() {
    let server = voice_server().await;
    let (alice, bob, carol, dave) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&alice, &bob, &carol, &dave]);
    for peer in [&carol, &dave] {
        peer.community("join", json!({})).await;
    }
    let general = open_channel(&alice, "general").await;
    let lounge = voice_channel(&alice, "lounge", json!({})).await;
    let a = voice(&alice, "join", json!({ "channel": lounge })).await;
    let b = voice(&bob, "join", json!({ "channel": lounge })).await;
    let (a_id, b_id) = (a["body"]["participant"].as_str().unwrap().to_string(), b["body"]["participant"].as_str().unwrap().to_string());

    // Alice's key for Bob, opaque to the server, reaches Bob as it was; one for
    // someone not in the room goes nowhere.
    let jwe = json!({ "protected": "e30", "ciphertext": "opaque" });
    tell(&alice, "keys", json!({ "channel": lounge, "keys": [{ "to": b_id, "kid": "a1", "jwe": jwe }, { "to": "p-nobody", "kid": "a1", "jwe": jwe }] })).await;
    let key = bob.next_of(&format!("{V}keys")).await;
    assert_eq!((key["body"]["from"].as_str(), key["body"]["kid"].as_str(), &key["body"]["jwe"]), (Some(a_id.as_str()), Some("a1"), &jwe));

    // Carol lets the community ring her; Dave doesn't, and is named in a post.
    let set = voice(&dave, "set-preferences", json!({ "ring": false })).await;
    assert_eq!(set["body"]["ring"], false);
    assert_eq!(tell(&carol, "invite", json!({ "channel": lounge, "to": [dave.did()] })).await.unwrap()["body"]["code"], "e.p.trust.not-authorized");
    tell(&alice, "invite", json!({ "channel": lounge, "to": [carol.did(), dave.did()] })).await;
    let ring = carol.next_of(&format!("{V}ring")).await;
    assert_eq!((ring["body"]["channel"].as_str(), ring["body"]["from"]["did"].as_str(), ring["body"]["participants"].as_i64()), (Some(lounge.as_str()), Some(alice.did().as_str()), Some(2)));
    let posted = dave.next_message().await;
    assert_eq!((posted["channel"].as_str(), posted["voice_invite"]["channel"].as_str()), (Some(general.as_str()), Some(lounge.as_str())));
    assert_eq!(posted["mentions"][0]["who"], dave.did());
}

#[tokio::test]
async fn a_voice_channel_has_a_limit_and_one_room_at_a_time() {
    let server = voice_server().await;
    let (alice, bob, carol) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&alice, &bob, &carol]);
    let small = voice_channel(&alice, "small", json!({ "limit": 2 })).await;
    let other = voice_channel(&alice, "other", json!({})).await;
    voice(&alice, "join", json!({ "channel": small })).await;
    voice(&bob, "join", json!({ "channel": small })).await;
    assert_eq!(voice(&carol, "join", json!({ "channel": small })).await["body"]["code"], "e.p.req.room-full");
    // Bob moving to another room makes space.
    let moved = voice(&bob, "join", json!({ "channel": other })).await;
    assert_eq!(kind(&moved), "joined");
    assert_eq!(kind(&voice(&carol, "join", json!({ "channel": small })).await), "joined");
    let stranger = Peer::start(&server).await;
    assert_eq!(voice(&stranger, "join", json!({ "channel": small })).await["body"]["code"], "e.p.trust.not-authorized");
}

#[tokio::test]
async fn a_member_in_a_room_from_two_devices_is_two_participants_and_can_move_to_one() {
    let server = voice_server().await;
    let (alice, bob) = (Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&alice, &bob]);
    let lounge = voice_channel(&alice, "lounge", json!({})).await;
    let mut live = Live::open(&bob, &server).await;
    live.send(&bob, "watch-presence", json!({})).await;
    live.next_of(&bob, "presence").await;

    // Alice's desktop, then her phone: two participants, each told of the other.
    let desk = voice(&alice, "join", json!({ "channel": lounge, "device": "desk" })).await;
    let phone = voice(&alice, "join", json!({ "channel": lounge, "device": "phone" })).await;
    let (desk_id, phone_id) = (desk["body"]["participant"].as_str().unwrap().to_string(), phone["body"]["participant"].as_str().unwrap().to_string());
    assert_ne!(desk_id, phone_id);
    assert_ne!(desk["body"]["transport"]["token"], phone["body"]["transport"]["token"]);
    assert_eq!(phone["body"]["participants"][0]["participant"], desk_id);
    // The desktop asking again is the same participant, with its token.
    let again = voice(&alice, "join", json!({ "channel": lounge, "device": "desk" })).await;
    assert_eq!((again["body"]["participant"].as_str(), &again["body"]["transport"]["token"]), (Some(desk_id.as_str()), &desk["body"]["transport"]["token"]));

    // A key from one device to the other reaches the member, saying which it is for.
    let jwe = json!({ "protected": "e30", "ciphertext": "opaque" });
    tell(&alice, "keys", json!({ "channel": lounge, "participant": desk_id, "keys": [{ "to": phone_id, "kid": "d1", "jwe": jwe }] })).await;
    let key = alice.next_of(&format!("{V}keys")).await;
    assert_eq!((key["body"]["from"].as_str(), key["body"]["to"].as_str()), (Some(desk_id.as_str()), Some(phone_id.as_str())));

    // State names its participant; the other device's is left as it was.
    tell(&alice, "state", json!({ "channel": lounge, "participant": phone_id, "muted": true, "deafened": false, "sharing": {} })).await;
    let mut presence = live.next_of(&bob, "presence").await;
    while presence["body"]["channels"][0]["participants"].as_array().unwrap().iter().all(|p| p["muted"] != true) {
        presence = live.next_of(&bob, "presence").await;
    }
    let muted: Vec<&str> = presence["body"]["channels"][0]["participants"].as_array().unwrap().iter().filter(|p| p["muted"] == true).map(|p| p["participant"].as_str().unwrap()).collect();
    assert_eq!(muted, vec![phone_id.as_str()]);

    // Her laptop joins with replace ("move here"): the other two are out.
    let laptop = voice(&alice, "join", json!({ "channel": lounge, "device": "laptop", "replace": true })).await;
    assert_eq!(laptop["body"]["participants"], json!([]));
    let mut after = live.next_of(&bob, "presence").await;
    while after["body"]["channels"][0]["participants"].as_array().unwrap().len() != 1 {
        after = live.next_of(&bob, "presence").await;
    }
    assert_eq!(after["body"]["channels"][0]["participants"][0]["participant"], laptop["body"]["participant"]);

    // Leaving names the participant; one that sent none leaves with all of its.
    voice(&alice, "join", json!({ "channel": lounge, "device": "desk" })).await;
    tell(&alice, "leave", json!({ "channel": lounge })).await;
    let mut empty = live.next_of(&bob, "presence").await;
    while !empty["body"]["channels"][0]["participants"].as_array().unwrap().is_empty() {
        empty = live.next_of(&bob, "presence").await;
    }
}

/// A stand-in for Anthropic's Messages API: answers every request with `answer`,
/// and keeps what it was asked.
async fn stand_in_model(answer: &'static str) -> (String, Arc<tokio::sync::Mutex<Vec<Value>>>) {
    let asked = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let kept = asked.clone();
    let app = axum::Router::new().route(
        "/v1/messages",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let kept = kept.clone();
            async move {
                kept.lock().await.push(body);
                axum::Json(json!({ "content": [{ "type": "text", "text": answer }], "usage": { "input_tokens": 10, "output_tokens": 5 } }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, asked)
}

#[tokio::test]
async fn claude_answers_in_a_room_from_what_was_said_there_within_its_limits() {
    let (api, asked) = stand_in_model("A wyvern has two legs.").await;
    let server = start_server(&format!("[assistant]\napi_url = \"{api}\"\napi_key = \"test-key\"\ndaily_requests_per_member = 1\n")).await;
    let (admin, member) = (Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member]);
    let joined = member.community("join", json!({})).await;
    // Whoever is let in is told what their posts are sent to.
    assert_eq!(joined["body"]["assistant"]["name"], "Claude");
    assert_eq!(joined["body"]["assistant"]["provider"], "Anthropic");
    let room = open_channel(&admin, "wyverns").await;
    member.next_of(&format!("{C}community")).await;

    let posted = member.community("post", json!({ "channel": room, "content": "what is a wyvern?" })).await;
    assert_eq!(kind(&posted), "posted");
    let mine = member.next_message().await;
    assert_eq!(mine["content"], "what is a wyvern?");
    // Claude answers in the room, as the server, answering that post.
    let answer = member.next_message().await;
    assert_eq!(answer["content"], "A wyvern has two legs.");
    assert_eq!(answer["author"]["did"], server.did);
    assert_eq!(answer["author"]["name"], "Claude");
    assert_eq!(answer["reply_to"]["id"], mine["id"]);
    // It was asked with the room's conversation, the asker under their name.
    let request = asked.lock().await.last().cloned().unwrap();
    assert_eq!(request["messages"][0]["role"], "user");
    assert!(request["messages"][0]["content"].as_str().unwrap().ends_with("what is a wyvern?"));
    assert!(request["system"].as_str().unwrap().contains("\"wyverns\""));

    // Over the day's limit, the member is told so in the room, and the model isn't asked.
    member.community("post", json!({ "channel": room, "content": "and a dragon?" })).await;
    member.next_message().await;
    let refused = member.next_message().await;
    assert!(refused["content"].as_str().unwrap().contains("in a day"), "{refused}");
    assert_eq!(asked.lock().await.len(), 1);
}

/// Advances `instance` with `event` and `input`, and returns the answer.
async fn advance(peer: &Peer, instance: &str, event: &str, input: Value) -> Value {
    peer.workflow("advance", json!({ "instance_id": instance, "event": event, "input": input })).await
}

#[tokio::test]
async fn roles_are_made_given_and_ranked_through_workflows() {
    let server = start_server("").await;
    let (owner, artist, other) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&owner, &artist, &other]);
    let channel = open_channel(&owner, "gallery").await;
    for peer in [&owner, &artist, &other] {
        peer.community("join", json!({})).await;
    }

    // A new community has Admin, Moderator and everyone; the first admin owns it.
    let told = owner.community("join", json!({})).await["body"].clone();
    assert_eq!(told["owner"], owner.did());
    let names: Vec<&str> = told["roles"].as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Admin", "Moderator", "everyone"]);
    assert_eq!(told["my_roles"], json!(["admin"]));
    assert_eq!(told["role"], "admin");

    // Only those who manage roles see the workflows, or may start them.
    let listed = artist.workflow("discover", json!({})).await;
    assert!(!listed["body"].to_string().contains("\"roles\""), "{listed}");
    let refused = artist.workflow("start", json!({ "template_id": "roles", "instance_id": "r0" })).await;
    assert_eq!(refused["body"]["code"], "not_authorized");
    assert!(owner.workflow("discover", json!({})).await["body"].to_string().contains("member-roles"));

    // The owner makes a role, Artist, that keeps order in channels.
    let list = owner.workflow("start", json!({ "template_id": "roles", "instance_id": "r1" })).await;
    assert_eq!(list["body"]["state"], "list", "{list}");
    let editing = advance(&owner, "r1", "create", json!({ "name": "Artist" })).await;
    assert_eq!(editing["body"]["state"], "edit", "{editing}");
    assert!(editing["body"]["ui"].to_string().contains("perm-manage_messages"));
    let taken = advance(&owner, "r1", "back", json!({})).await;
    assert_eq!(taken["body"]["state"], "list");
    assert_eq!(advance(&owner, "r1", "create", json!({ "name": "artist" })).await["body"]["code"], "guard_failed");
    let roles = owner.community("join", json!({})).await["body"]["roles"].clone();
    let artist_role = roles.as_array().unwrap().iter().find(|r| r["name"] == "Artist").unwrap()["id"].as_str().unwrap().to_string();
    advance(&owner, "r1", "edit", json!({ "role": artist_role })).await;
    let bad = advance(&owner, "r1", "save", json!({ "name": "Artist", "color": "blue" })).await;
    assert_eq!(bad["body"]["code"], "guard_failed");
    let saved = advance(&owner, "r1", "save", json!({ "name": "Artist", "color": "#9B59B6", "icon": "🎨", "mentionable": true, "perm-manage_messages": true, "perm-post": true })).await;
    assert_eq!(saved["body"]["state"], "list", "{saved}");
    assert!(saved["body"]["ui"].to_string().contains("Saved Artist."));
    // Everyone is told each change; the last says it as saved.
    let shown = loop {
        let told = other.next_of(&format!("{C}community")).await["body"].clone();
        let role = told["roles"].as_array().unwrap().iter().find(|r| r["id"] == artist_role.as_str()).cloned();
        if let Some(role) = role.filter(|role| role.get("color").is_some()) {
            break role;
        }
    };
    assert_eq!(shown, json!({ "id": artist_role, "name": "Artist", "color": "#9b59b6", "icon": "🎨", "mentionable": true, "hoist": false, "self_assignable": false }));
    assert_eq!(advance(&owner, "r1", "done", json!({})).await["type"], format!("{W}complete"));

    // Given the role, a member removes someone else's post, as they couldn't before.
    other.community("post", json!({ "channel": channel, "id": "p1", "content": "spam" })).await;
    let before = artist.ask("https://wyvrn.app/message-edits/1.0/delete", json!({ "to": { "id": "p1", "author": other.did() } })).await;
    assert_eq!(before["body"]["code"], "e.p.trust.not-authorized");
    owner.workflow("start", json!({ "template_id": "member-roles", "instance_id": "m1", "context": { "member": artist.did() } })).await;
    let given = advance(&owner, "m1", "save", json!({ format!("role-{artist_role}"): true })).await;
    assert_eq!(kind(&given), "complete", "{given}");
    let now_told = loop {
        let told = artist.next_of(&format!("{C}community")).await["body"].clone();
        if told["my_roles"] != json!([]) {
            break told;
        }
    };
    assert_eq!(now_told["my_roles"], json!([artist_role]));
    assert_eq!(now_told["role"], "moderator");
    assert!(now_told["channels"][0]["permissions"].as_array().unwrap().contains(&json!("manage_messages")));
    let removed = artist.ask("https://wyvrn.app/message-edits/1.0/delete", json!({ "to": { "id": "p1", "author": other.did() } })).await;
    assert_eq!(removed["body"]["removed_by"], artist.did(), "{removed}");
    // A mentionable role may be mentioned by anyone.
    other.community("post", json!({ "channel": channel, "id": "p2", "content": "@Artist look", "mentions": [{ "who": format!("role:{artist_role}"), "name": "Artist" }, { "who": "everyone", "name": "everyone" }] })).await;
    let mentioned = other.community("history-request", json!({ "channel": channel })).await["body"]["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(mentioned["mentions"], json!([{ "who": format!("role:{artist_role}"), "name": "Artist" }]));

    // Members list who holds what.
    let members = owner.community("members-request", json!({})).await;
    let entry = members["body"]["members"].as_array().unwrap().iter().find(|m| m["did"] == artist.did()).unwrap().clone();
    assert_eq!(entry["roles"], json!([artist_role]));
}

#[tokio::test]
async fn a_channel_can_be_made_read_only_and_rank_limits_moderators() {
    let server = start_server("").await;
    let (owner, moderator, member) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&owner, &moderator, &member]);
    let channel = open_channel(&owner, "news").await;
    for peer in [&owner, &moderator, &member] {
        peer.community("join", json!({})).await;
    }
    owner.community("set-member", json!({ "did": moderator.did(), "role": "moderator" })).await;

    // Read-only: members can't post; a role the channel allows still can.
    let opened = owner.workflow("start", json!({ "template_id": "channel-permissions", "instance_id": "c1", "context": { "channel": channel } })).await;
    assert_eq!(opened["body"]["state"], "roles", "{opened}");
    let locked = advance(&owner, "c1", "read_only", json!({})).await;
    assert!(locked["body"]["ui"].to_string().contains("Let everyone post"), "{locked}");
    assert_eq!(member.community("post", json!({ "channel": channel, "content": "hi" })).await["body"]["code"], "e.p.trust.not-authorized");
    let told = member.community("join", json!({})).await["body"].clone();
    assert_eq!(told["channels"][0]["read_only"], true);
    assert!(!told["channels"][0]["permissions"].as_array().unwrap().contains(&json!("post")));
    advance(&owner, "c1", "edit", json!({ "role": "moderator" })).await;
    assert_eq!(advance(&owner, "c1", "save", json!({ "allow-post": true, "deny-post": true })).await["body"]["code"], "guard_failed");
    let saved = advance(&owner, "c1", "save", json!({ "allow-post": true })).await;
    assert!(saved["body"]["ui"].to_string().contains("Moderator: allows Post"), "{saved}");
    assert_eq!(kind(&moderator.community("post", json!({ "channel": channel, "content": "news" })).await), "posted");
    // Someone who doesn't manage channels can't open it.
    let refused = member.workflow("start", json!({ "template_id": "channel-permissions", "instance_id": "c2", "context": { "channel": channel } })).await;
    assert_eq!(refused["body"]["code"], "not_authorized");

    // A moderator lets in, and takes out a member, but not an admin; and can't
    // manage roles at all.
    assert_eq!(kind(&moderator.community("set-member", json!({ "did": member.did(), "role": null })).await), "members");
    assert_eq!(moderator.community("set-member", json!({ "did": owner.did(), "role": null })).await["body"]["code"], "e.p.msg.owner");
    assert_eq!(moderator.community("set-member", json!({ "did": member.did(), "role": "moderator" })).await["body"]["code"], "e.p.trust.not-authorized");
    assert_eq!(moderator.workflow("start", json!({ "template_id": "roles", "instance_id": "r9" })).await["body"]["code"], "not_authorized");
}

#[tokio::test]
async fn bans_timeouts_slow_mode_and_bulk_removal_are_kept_in_the_audit_log() {
    let server = start_server("max_posts_per_minute = 5").await;
    let (owner, moderator, member, stranger) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&owner, &moderator, &member]);
    let channel = open_channel(&owner, "general").await;
    for peer in [&owner, &moderator, &member] {
        peer.community("join", json!({})).await;
    }
    owner.community("set-member", json!({ "did": moderator.did(), "role": "moderator" })).await;

    // Slow mode: one post a minute, but not for whoever may ignore it.
    let set = owner.community("update-channel", json!({ "channel": channel, "slow_mode": 60 })).await;
    assert_eq!(set["body"]["channels"][0]["slow_mode"], 60, "{set}");
    assert_eq!(member.community("update-channel", json!({ "channel": channel, "slow_mode": 0 })).await["body"]["code"], "e.p.trust.not-authorized");
    assert_eq!(kind(&member.community("post", json!({ "channel": channel, "id": "m1", "content": "one" })).await), "posted");
    let slow = member.community("post", json!({ "channel": channel, "content": "two" })).await;
    assert_eq!(slow["body"]["code"], "e.p.msg.slow-mode", "{slow}");
    assert!(slow["body"]["args"]["retry_after"].as_i64().unwrap() > 0);
    // Sent again after a lost answer, a post is the same post.
    assert_eq!(kind(&member.community("post", json!({ "channel": channel, "id": "m1", "content": "one" })).await), "posted");
    assert_eq!(kind(&moderator.community("post", json!({ "channel": channel, "content": "mods may" })).await), "posted");
    owner.community("update-channel", json!({ "channel": channel, "slow_mode": 0 })).await;

    // A server's own limit: so many a minute, anywhere.
    for n in 0..4 {
        member.community("post", json!({ "channel": channel, "id": format!("r{n}"), "content": "fast" })).await;
    }
    assert_eq!(member.community("post", json!({ "channel": channel, "content": "too fast" })).await["body"]["code"], "e.p.req.rate-limited");

    // A timeout: still in, but quiet. Told until when.
    let timed = moderator.community("timeout", json!({ "did": member.did(), "seconds": 600, "reason": "cool off" })).await;
    let listed = timed["body"]["members"].as_array().unwrap().iter().find(|m| m["did"] == member.did()).unwrap().clone();
    assert!(listed["timed_out_until"].as_i64().is_some(), "{timed}");
    let told = member.community("join", json!({})).await["body"].clone();
    assert!(told["timed_out_until"].as_i64().is_some());
    assert_eq!(member.ask("https://wyvrn.app/message-reactions/1.0/reactions", json!({ "to": { "id": "m1" }, "emoji": ["👍"] })).await["body"]["code"], "e.p.msg.timed-out");
    assert_eq!(moderator.community("timeout", json!({ "did": owner.did(), "seconds": 600 })).await["body"]["code"], "e.p.msg.owner");
    assert_eq!(member.community("timeout", json!({ "did": moderator.did(), "seconds": 600 })).await["body"]["code"], "e.p.trust.not-authorized");
    moderator.community("timeout", json!({ "did": member.did(), "seconds": 0 })).await;
    assert!(member.community("join", json!({})).await["body"].get("timed_out_until").is_none());

    // Bulk removal: the member's posts in the last hour.
    let bulk = moderator.community("bulk-delete", json!({ "channel": channel, "author": member.did(), "seconds": 3600 })).await;
    assert_eq!(bulk["body"]["removed"], 5, "{bulk}");
    let history = owner.community("history-request", json!({ "channel": channel })).await;
    assert!(history["body"]["messages"].as_array().unwrap().iter().filter(|m| m["author"]["did"] == member.did()).all(|m| m["deleted"] == true));

    // Moderators don't ban by default; the owner bans, and the banned stay out.
    assert_eq!(moderator.community("ban", json!({ "did": member.did() })).await["body"]["code"], "e.p.trust.not-authorized");
    let banned = owner.community("ban", json!({ "did": member.did(), "reason": "spam" })).await;
    assert_eq!(banned["body"]["bans"][0]["did"], member.did(), "{banned}");
    assert_eq!(member.next_of(&format!("{C}removed")).await["body"]["was"], "banned");
    assert_eq!(member.community("join", json!({})).await["body"]["code"], "e.p.trust.not-authorized");
    // Someone who never was a member can be banned before they ask.
    owner.community("ban", json!({ "did": stranger.did(), "seconds": 3600 })).await;
    assert_eq!(stranger.community("join", json!({})).await["body"]["code"], "e.p.trust.not-authorized");
    owner.community("unban", json!({ "did": member.did() })).await;
    assert_eq!(kind(&member.community("join", json!({})).await), "waiting");

    // The audit log says who did all that; the member's history gathers what was done to them.
    let audit = owner.community("audit-request", json!({})).await;
    let actions: Vec<&str> = audit["body"]["entries"].as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
    for expected in ["member.unban", "member.ban", "post.bulk-remove", "member.timeout", "channel.update", "member.roles", "channel.create"] {
        assert!(actions.contains(&expected), "{expected} in {actions:?}");
    }
    let only = owner.community("audit-request", json!({ "action": "member.timeout", "limit": 1 })).await;
    assert_eq!(only["body"]["entries"].as_array().unwrap().len(), 1);
    assert_eq!(only["body"]["more"], true);
    assert_eq!(member.community("audit-request", json!({})).await["type"], PROBLEM);
    let history = moderator.community("member-history-request", json!({ "did": member.did() })).await;
    let done: Vec<&str> = history["body"]["actions"].as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
    assert_eq!(done, ["member.unban", "member.ban", "member.timeout", "member.timeout"], "{history}");
}

#[tokio::test]
async fn automod_refuses_holds_and_alerts_and_join_gates_ask_before_letting_in() {
    let server = start_server("").await;
    let (owner, member, newcomer) = (Peer::start(&server).await, Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&owner, &member]);
    let channel = open_channel(&owner, "general").await;
    let log = open_channel(&owner, "mod-log").await;
    for peer in [&owner, &member] {
        peer.community("join", json!({})).await;
    }

    // Two rules: one refuses a word and times out; one holds links.
    owner.workflow("start", json!({ "template_id": "automod", "instance_id": "a1" })).await;
    advance(&owner, "a1", "create", json!({ "name": "No scams" })).await;
    let bad = advance(&owner, "a1", "save", json!({ "name": "No scams", "patterns": "(unclosed" })).await;
    assert_eq!(bad["body"]["code"], "guard_failed", "{bad}");
    let saved = advance(&owner, "a1", "save", json!({ "name": "No scams", "enabled": true, "words": "scam*", "block": true, "alert_channel": "mod-log", "timeout_minutes": "5" })).await;
    assert!(saved["body"]["ui"].to_string().contains("Saved No scams."), "{saved}");
    advance(&owner, "a1", "create", json!({ "name": "Links" })).await;
    advance(&owner, "a1", "save", json!({ "name": "Links", "enabled": true, "links": true, "allowed_domains": "wyvrn.app", "hold": true })).await;
    assert_eq!(kind(&advance(&owner, "a1", "done", json!({})).await), "complete");

    // Refused, said in the log channel, and timed out.
    let refused = member.community("post", json!({ "channel": channel, "content": "this is a SCAMMER" })).await;
    assert_eq!(refused["body"]["code"], "e.p.msg.automod", "{refused}");
    assert_eq!(refused["body"]["args"]["rule"], "No scams");
    let alerted = owner.community("history-request", json!({ "channel": log })).await["body"]["messages"][0].clone();
    assert!(alerted["content"].as_str().unwrap().contains("refused a post"), "{alerted}");
    assert_eq!(alerted["author"]["name"], "AutoMod");
    assert_eq!(member.community("post", json!({ "channel": channel, "content": "sorry" })).await["body"]["code"], "e.p.msg.timed-out");
    owner.community("timeout", json!({ "did": member.did(), "seconds": 0 })).await;

    // Held: answered posted-but-held, sent to nobody; a moderator lets it through.
    assert_eq!(kind(&member.community("post", json!({ "channel": channel, "content": "docs at https://wyvrn.app/x" })).await), "posted");
    let held = member.community("post", json!({ "channel": channel, "id": "h1", "content": "look https://evil.example" })).await;
    assert_eq!(held["body"]["held"], true, "{held}");
    assert!(held["body"].get("seq").is_none());
    let told = owner.next_of(&format!("{C}held")).await;
    assert_eq!(told["body"]["posts"][0]["id"], "h1");
    let seen = owner.community("history-request", json!({ "channel": channel })).await;
    assert!(!seen["body"].to_string().contains("evil.example"));
    let released = owner.community("release-held", json!({ "id": "h1" })).await;
    assert_eq!(released["body"]["posts"], json!([]), "{released}");
    let placed = owner.community("history-request", json!({ "channel": channel })).await;
    assert!(placed["body"].to_string().contains("evil.example"));
    let audit = owner.community("audit-request", json!({})).await.to_string();
    assert!(audit.contains("automod.release") && audit.contains("automod.hold") && audit.contains("automod.block"));

    // Join gates: questions first, then the waiting list shows the answers.
    owner.community("set-admission", json!({ "admission": "admin" })).await;
    owner.workflow("start", json!({ "template_id": "join-gates", "instance_id": "g1" })).await;
    let gated = advance(&owner, "g1", "save", json!({ "post_wait": "10", "q0": "How did you find us?", "q0-required": true, "q1": "Pick one", "q1-options": "Art | Code" })).await;
    assert_eq!(kind(&gated), "complete", "{gated}");
    let asked = newcomer.community("join", json!({})).await;
    assert_eq!(kind(&asked), "join-questions", "{asked}");
    assert_eq!(asked["body"]["questions"][1]["options"], json!(["Art", "Code"]));
    assert_eq!(newcomer.community("join", json!({ "answers": { "q2": "Art" } })).await["body"]["code"], "e.p.msg.invalid-answers");
    assert_eq!(newcomer.community("join", json!({ "answers": { "q1": "A friend", "q2": "Music" } })).await["body"]["code"], "e.p.msg.invalid-answers");
    assert_eq!(kind(&newcomer.community("join", json!({ "answers": { "q1": "A friend", "q2": "Code" } })).await), "waiting");
    let members = owner.community("members-request", json!({})).await;
    assert_eq!(members["body"]["waiting"][0]["answers"], json!([{ "question": "How did you find us?", "answer": "A friend" }, { "question": "Pick one", "answer": "Code" }]));
    // Let in, they wait ten minutes to post; someone with a role doesn't.
    owner.community("set-member", json!({ "did": newcomer.did(), "role": "member" })).await;
    let told = newcomer.next_of(&format!("{C}community")).await;
    assert_eq!(told["body"]["gates"]["post_wait"], 10);
    let early = newcomer.community("post", json!({ "channel": channel, "content": "hi" })).await;
    assert_eq!(early["body"]["code"], "e.p.msg.too-new", "{early}");
    owner.community("set-member", json!({ "did": newcomer.did(), "role": "moderator" })).await;
    assert_eq!(kind(&newcomer.community("post", json!({ "channel": channel, "content": "hi" })).await), "posted");
}
