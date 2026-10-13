//! The server run for real: its HTTP endpoint, and members' agents that talk to it
//! over DIDComm from endpoints of their own.

use std::sync::Arc;
use std::time::Duration;

use axum::{body::Bytes, extract::State, routing::post, Router};
use didcomm_agent::{Agent, Identity};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex, Notify};
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
    let community = Community::new(Store::in_memory().unwrap(), config.community.clone(), Some(wyvrn_service::profile::Kept::scratch().unwrap())).unwrap();
    let state = Arc::new(AppState { agent, community, did_document, resolver: didcomm_quickstart::default_resolver(), deliver: Notify::new() });
    tokio::spawn(server::run_deliveries(state.clone()));
    let app = server::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Running { did, state }
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

    for (message_type, body) in [
        (format!("{C}join"), json!({})),
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
    let channel = open_channel(&admin, "  design   review ").await;
    let told = member.next_of(&format!("{C}community")).await;
    assert_eq!(told["body"]["channels"][0]["name"], "design review");
    assert_eq!(told["body"]["channels"][0]["id"], channel);
    assert_eq!(told["body"]["role"], "member");

    // No two open channels share a name, whatever its case.
    let twice = admin.community("create-channel", json!({ "name": "Design Review" })).await;
    assert_eq!(twice["body"]["code"], "e.p.msg.name-taken");

    // Renamed, given a topic, and closed: a closed channel takes no more posts.
    let changed = admin.community("update-channel", json!({ "channel": channel, "name": "design", "topic": "What we are building", "closed": true })).await;
    assert_eq!(changed["body"]["channels"][0], json!({ "id": channel, "name": "design", "topic": "What we are building", "closed": true, "latest_seq": 0 }));
    let late = admin.community("post", json!({ "channel": channel, "content": "too late" })).await;
    assert_eq!(late["body"]["code"], "e.p.msg.closed");
    // ...and only an admin, or whoever opened it, changes a channel.
    let meddling = member.community("update-channel", json!({ "channel": channel, "closed": false })).await;
    assert_eq!(meddling["body"]["code"], "e.p.trust.not-authorized");
}

#[tokio::test]
async fn members_may_open_channels_where_the_community_says_so() {
    let server = start_server("members_may_create_channels = true").await;
    let (admin, member) = (Peer::start(&server).await, Peer::start(&server).await);
    server.admit(&[&admin, &member]);
    let channel = open_channel(&member, "a session").await;
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

    assert_eq!(newcomer.community("join", json!({})).await["type"], PROBLEM);
    assert_eq!(member.community("set-member", json!({ "did": newcomer.did(), "role": "member" })).await["body"]["code"], "e.p.trust.not-authorized");
    let listed = admin.community("set-member", json!({ "did": newcomer.did(), "role": "member" })).await;
    assert_eq!(kind(&listed), "members");
    assert_eq!(listed["body"]["members"].as_array().unwrap().len(), 3);
    assert_eq!(kind(&newcomer.community("join", json!({})).await), "community");

    // Taken out again, they are refused again.
    admin.community("set-member", json!({ "did": newcomer.did(), "role": null })).await;
    assert_eq!(newcomer.community("join", json!({})).await["type"], PROBLEM);

    // A community keeps an admin: the only one can't step down or be removed.
    assert_eq!(admin.community("set-member", json!({ "did": admin.did(), "role": "member" })).await["body"]["code"], "e.p.msg.last-admin");
    assert_eq!(admin.community("set-member", json!({ "did": admin.did(), "role": null })).await["body"]["code"], "e.p.msg.last-admin");
    admin.community("set-member", json!({ "did": member.did(), "role": "admin" })).await;
    assert_eq!(kind(&admin.community("set-member", json!({ "did": admin.did(), "role": "member" })).await), "members");
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
