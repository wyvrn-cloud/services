//! The service run for real: its HTTP server, an uploader that talks to it over
//! DIDComm and puts bytes over HTTP, and someone fetching what it serves.

use std::sync::Arc;

use didcomm_agent::{Agent, Identity};
use serde_json::{json, Value};
use wyvrn_files::{
    hash,
    host::{Host, PIURI},
    server::{self, AppState},
    store::Store,
};

struct Running {
    did: String,
    url: String,
}

/// The service, with `uploader` allowed and small limits: 8-byte chunks, 100
/// bytes a file, 300 bytes in all.
async fn start_service(uploader: &str) -> Running {
    start_with(uploader, "").await.0
}

/// The same, with more `[service]` settings, and the state to change its rules by.
async fn start_with(uploader: &str, more: &str) -> (Running, Arc<AppState>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let config: wyvrn_files::config::Config = toml::from_str(&format!(
        "[server]\nbind = \"unused\"\npublic_url = \"{url}\"\n[identity]\npath = \"unused\"\n[store]\npath = \"unused\"\ndir = \"unused\"\n\
         [service]\nuploaders = [\"{uploader}\"]\nchunk_bytes = 8\nmax_file_bytes = 100\nquota_bytes = 300\n{more}"
    ))
    .unwrap();
    let (agent, did_document) = server::build_agent(&config, Identity::generate().unwrap()).unwrap();
    let did = agent.did();
    let host = Host::new(Store::scratch().unwrap(), config.service.clone(), &url);
    let state = Arc::new(AppState::new(agent, host, did_document));
    let app = server::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (Running { did, url }, state)
}

struct Uploader {
    agent: Agent,
    service: String,
    http: reqwest::Client,
}

impl Uploader {
    fn new(agent: Agent, service: &Running) -> Self {
        Self { agent, service: service.did.clone(), http: reqwest::Client::new() }
    }

    async fn ask(&self, kind: &str, body: Value) -> Value {
        let message = json!({ "type": format!("{PIURI}/{kind}"), "body": body, "return_route": "all" });
        // (`send`, not `request`: a refusal is an answer here, not an error.)
        self.agent.send(&self.service, &message).await.expect("sent").expect("an answer").message
    }

    async fn upload(&self, name: &str, bytes: &[u8]) -> Value {
        self.ask("upload", json!({ "name": name, "media_type": "text/plain", "byte_count": bytes.len(), "hash": hash::of_bytes(bytes) })).await
    }

    async fn put(&self, upload_url: &str, offset: usize, chunk: &[u8]) -> reqwest::Response {
        self.http.put(upload_url).header("Upload-Offset", offset.to_string()).header("Content-Type", "application/offset+octet-stream").body(chunk.to_vec()).send().await.unwrap()
    }

    /// Puts every byte, `chunk` at a time, from wherever the host says it stands.
    async fn put_all(&self, upload_url: &str, bytes: &[u8], chunk: usize) {
        let standing = self.http.head(upload_url).send().await.unwrap();
        assert_eq!(standing.status(), 200);
        let mut offset: usize = standing.headers()["upload-offset"].to_str().unwrap().parse().unwrap();
        while offset < bytes.len() {
            let end = (offset + chunk).min(bytes.len());
            let response = self.put(upload_url, offset, &bytes[offset..end]).await;
            assert_eq!(response.status(), 204, "{}", response.text().await.unwrap());
            offset = end;
        }
    }
}

fn kind(message: &Value) -> &str {
    message["type"].as_str().unwrap().rsplit('/').next().unwrap()
}

#[tokio::test]
async fn a_file_is_put_in_chunks_completed_fetched_kept_and_let_go() {
    let alice = Agent::new(Identity::generate().unwrap()).unwrap();
    let service = start_service(&alice.did()).await;
    let alice = Uploader::new(alice, &service);
    let bytes = b"The quick brown fox jumps over the lazy dog.";

    let granted = alice.upload("fox.txt", bytes).await;
    assert_eq!(kind(&granted), "upload-granted", "{granted}");
    let id = granted["body"]["file"].as_str().unwrap().to_string();
    let upload_url = granted["body"]["upload_url"].as_str().unwrap().to_string();
    assert_eq!(upload_url, format!("{}u/{id}", service.url));
    assert_eq!(granted["body"]["chunk_bytes"], 8);

    // The first chunk lands; a chunk at the wrong offset is refused and the
    // right one named; a chunk too long for the host is refused unread.
    assert_eq!(alice.put(&upload_url, 0, &bytes[..8]).await.status(), 204);
    let wrong = alice.put(&upload_url, 0, &bytes[..8]).await;
    assert_eq!((wrong.status().as_u16(), wrong.headers()["upload-offset"].to_str().unwrap()), (409, "8"));
    assert_eq!(alice.put(&upload_url, 8, &bytes[8..20]).await.status(), 413);
    // Asked again over DIDComm, the same file is granted, standing where it stands.
    let again = alice.upload("fox.txt", bytes).await;
    assert_eq!((again["body"]["file"].as_str(), again["body"]["offset"].as_u64()), (Some(id.as_str()), Some(8)));
    // Not fetchable yet, and not complete yet.
    assert_eq!(reqwest::get(format!("{}f/{id}", service.url)).await.unwrap().status(), 404);
    let early = alice.ask("complete", json!({ "file": id })).await;
    assert_eq!((kind(&early), early["body"]["code"].as_str(), early["body"]["args"][0].as_str()), ("problem-report", Some("e.p.msg.incomplete"), Some("8")));

    // The rest, resumed from what the host holds.
    alice.put_all(&upload_url, bytes, 8).await;
    let completed = alice.ask("complete", json!({ "file": id })).await;
    assert_eq!(kind(&completed), "completed", "{completed}");
    let url = completed["body"]["url"].as_str().unwrap().to_string();
    assert_eq!(url, format!("{}f/{id}", service.url));
    assert_eq!((completed["body"]["hash"].as_str().unwrap(), completed["body"]["byte_count"].as_u64().unwrap()), (hash::of_bytes(bytes).as_str(), bytes.len() as u64));
    assert_eq!(alice.put(&upload_url, 44, b"").await.status(), 410);

    // Fetched by anyone with the link: whole, by range, and not twice.
    let whole = reqwest::get(&url).await.unwrap();
    assert_eq!(whole.status(), 200);
    assert_eq!(whole.headers()["content-type"].to_str().unwrap(), "text/plain");
    assert_eq!(whole.headers()["content-disposition"].to_str().unwrap(), "attachment; filename=\"fox.txt\"");
    assert_eq!(whole.headers()["accept-ranges"].to_str().unwrap(), "bytes");
    let tag = whole.headers()["etag"].to_str().unwrap().to_string();
    assert_eq!(tag, format!("\"{}\"", hash::of_bytes(bytes)));
    assert_eq!(whole.bytes().await.unwrap().as_ref(), bytes);
    let part = reqwest::Client::new().get(&url).header("Range", "bytes=4-8").send().await.unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(part.bytes().await.unwrap().as_ref(), b"quick");
    let head = reqwest::Client::new().head(&url).send().await.unwrap();
    assert_eq!((head.status().as_u16(), head.headers()["content-length"].to_str().unwrap()), (200, "44"));
    assert_eq!(reqwest::Client::new().get(&url).header("If-None-Match", &tag).send().await.unwrap().status(), 304);

    // Listed, kept longer, let go.
    let listing = alice.ask("list", json!({})).await;
    assert_eq!(kind(&listing), "listing");
    assert_eq!((listing["body"]["used_bytes"].as_u64(), listing["body"]["files"][0]["state"].as_str(), listing["body"]["files"][0]["url"].as_str()), (Some(44), Some("stored"), Some(url.as_str())));
    let extended = alice.ask("extend", json!({ "file": id, "keep_seconds": 30 * 86400 })).await;
    assert_eq!(kind(&extended), "extended");
    assert!(extended["body"]["expires_time"].as_i64().unwrap() > completed["body"]["expires_time"].as_i64().unwrap());
    assert_eq!(kind(&alice.ask("delete", json!({ "file": id })).await), "deleted");
    assert_eq!(reqwest::get(&url).await.unwrap().status(), 404);
    assert_eq!(alice.ask("list", json!({})).await["body"]["files"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn only_an_uploader_is_served_and_only_what_was_promised_is_kept() {
    let alice = Agent::new(Identity::generate().unwrap()).unwrap();
    let service = start_service(&alice.did()).await;
    let alice = Uploader::new(alice, &service);

    // A stranger is refused, by name; an anonymous message at the door.
    let mallory = Uploader::new(Agent::new(Identity::generate().unwrap()).unwrap(), &service);
    let refused = mallory.upload("x.txt", b"x").await;
    assert_eq!((kind(&refused), refused["body"]["code"].as_str()), ("problem-report", Some("e.p.msg.not-allowed")));
    let anonymous = mallory.agent.messaging().pack(&json!({ "type": format!("{PIURI}/list"), "body": {}, "return_route": "all" }), &service.did, None).await.unwrap();
    let at_the_door = reqwest::Client::new().post(&service.url).body(anonymous.message).send().await.unwrap();
    assert_eq!(at_the_door.status(), 403);
    // Both still get trust pings answered, as any agent does.
    let ping = json!({ "type": "https://didcomm.org/trust-ping/2.0/ping", "body": { "response_requested": true }, "return_route": "all" });
    let pong = mallory.agent.send(&service.did, &ping).await.unwrap().unwrap().message;
    assert!(pong["type"].as_str().unwrap().ends_with("trust-ping/2.0/ping-response"), "{pong}");

    // Bytes that don't hash to what was promised are discarded at complete.
    let granted = alice.upload("lie.txt", b"honest").await;
    let id = granted["body"]["file"].as_str().unwrap().to_string();
    let upload_url = granted["body"]["upload_url"].as_str().unwrap().to_string();
    alice.put_all(&upload_url, b"HONEST", 8).await;
    let discarded = alice.ask("complete", json!({ "file": id })).await;
    assert_eq!(discarded["body"]["code"].as_str(), Some("e.p.msg.hash-mismatch"));
    assert_eq!(reqwest::Client::new().head(&upload_url).send().await.unwrap().status(), 404);

    // The limits: a file too large, then the quota.
    let big = vec![b'x'; 101];
    assert_eq!(alice.upload("big", &big).await["body"]["code"].as_str(), Some("e.p.msg.too-large"));
    for n in 0..3 {
        let bytes = vec![b'a' + n; 100];
        assert_eq!(kind(&alice.upload(&format!("{n}"), &bytes).await), "upload-granted");
    }
    let over = alice.upload("one more", b"z").await;
    assert_eq!((over["body"]["code"].as_str(), over["body"]["args"][0].as_str()), (Some("e.p.msg.quota"), Some("300")));
    // What nobody was granted is nowhere.
    assert_eq!(reqwest::Client::new().head(format!("{}u/{}", service.url, "A".repeat(27))).send().await.unwrap().status(), 404);
    assert_eq!(reqwest::get(format!("{}f/not-an-id", service.url)).await.unwrap().status(), 404);
}

#[tokio::test]
async fn sign_up_is_open_until_locked_down_and_anonymous_uploads_can_be_refused() {
    let admin = Agent::new(Identity::generate().unwrap()).unwrap();
    let (service, state) = start_with("did:example:nobody", &format!("admins = [\"{}\"]\nopen_signup = true\n", admin.did())).await;
    let stranger = || Uploader::new(Agent::new(Identity::generate().unwrap()).unwrap(), &service);

    // Open: anyone who authenticates is an uploader by asking.
    let carol = stranger();
    assert_eq!(kind(&carol.upload("a.txt", b"hello").await), "upload-granted");
    assert_eq!(state.host.store.uploader(&carol.agent.did()).unwrap().unwrap().source, "signup");

    // Locked down: nobody new, and those already in carry on.
    let mut policy = state.policy().unwrap();
    policy.locked = true;
    state.host.store.set_policy(&policy).unwrap();
    let dave = stranger();
    let refused = dave.upload("b.txt", b"hi").await;
    assert_eq!(refused["body"]["comment"], "This service isn't taking new uploaders right now.");
    assert_eq!(kind(&carol.upload("c.txt", b"more").await), "upload-granted");
    // An admin always may.
    assert_eq!(kind(&Uploader::new(admin, &service).upload("d.txt", b"mine").await), "upload-granted");

    // No profile, no files: asked for one, and let in once it has come.
    policy.locked = false;
    policy.require_profile = true;
    state.host.store.set_policy(&policy).unwrap();
    let refused = dave.upload("b.txt", b"hi").await;
    assert_eq!(refused["body"]["code"], "e.p.msg.not-allowed");
    assert!(refused["body"]["comment"].as_str().unwrap().contains("profile"));
    let profile = json!({ "type": "https://didcomm.org/user-profile/1.0/profile", "body": { "profile": { "displayName": "Dave" } } });
    dave.agent.send(&service.did, &profile).await.unwrap();
    assert_eq!(kind(&dave.upload("b.txt", b"hi").await), "upload-granted");
    assert_eq!(state.host.store.uploader(&dave.agent.did()).unwrap().unwrap().name.as_deref(), Some("Dave"));

    // Blocked: out, whatever else.
    let mut blocked = state.host.store.uploader(&dave.agent.did()).unwrap().unwrap();
    blocked.blocked = true;
    state.host.store.put_uploader(&blocked).unwrap();
    assert_eq!(dave.upload("e.txt", b"again").await["body"]["code"], "e.p.msg.not-allowed");
}
