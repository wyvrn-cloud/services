//! The host as its logs' witness (did-hosting/1.0, "The host as a witness"), run
//! for real at `localhost`: holders that write witnessed logs and are told, at
//! their hosted DID, of what is held back; readers that take only what the host
//! has witnessed.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::{body::Bytes, extract::State, routing::post, Router};
use base64::Engine;
use didcomm_agent::{Agent, Identity};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use wyvrn_did_host::{
    host::Host,
    server::{self, AppState},
    store::Store,
    witness::{self, Witness},
};
use wyvrn_hosted_did::{recovery, write, Fetch};

const P: &str = "https://wyvrn.app/did-hosting/1.0/";
const HOUR: i64 = 60 * 60;

fn kind(message: &Value) -> &str {
    message["type"].as_str().unwrap_or_default().rsplit('/').next().unwrap_or_default()
}

fn refused_with<'a>(message: &'a Value, code: &str) -> &'a Value {
    assert_eq!(kind(message), "problem-report", "{message}");
    assert_eq!(message["body"]["code"], code, "{message}");
    message
}

fn jwk(seed: u8) -> String {
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    json!({ "kty": "OKP", "crv": "Ed25519", "d": b64.encode(key.to_bytes()), "x": b64.encode(key.verifying_key().to_bytes()) }).to_string()
}

struct Running {
    did: String,
    url: String,
    /// As the DIDs it hosts write it: `localhost%3A<port>`.
    domain: String,
    config: wyvrn_did_host::config::Config,
    store_path: PathBuf,
    identity: String,
    state: Arc<AppState>,
}

/// The host over the store at `store_path`, witnessing as the key kept there.
fn host_at(store_path: &PathBuf, config: &wyvrn_did_host::config::Config, identity: &str) -> Host {
    let store = Store::open(&wyvrn_service::db::Location::Sqlite(store_path.clone())).unwrap();
    let witness = Witness::load_or_make(&store, &witness::bound_to(&Identity::from_json(identity).unwrap())).unwrap();
    Host::new(store, config.host.clone(), didcomm_quickstart::default_resolver()).with_witness(witness)
}

/// The same host started again over what it kept.
fn host_over(running: &Running) -> Host {
    host_at(&running.store_path, &running.config, &running.identity)
}

/// A host that witnesses, at `localhost:<port>`, keeping what it keeps in a SQLite
/// file (so that it can be started again over it).
async fn start_host() -> Running {
    didcomm_resolver_webvh::allow_private_hosts();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}/");
    let domain = format!("localhost%3A{port}");
    let config: wyvrn_did_host::config::Config = toml::from_str(&format!(
        "[server]\nbind = \"unused\"\npublic_url = \"{url}\"\n[identity]\npath = \"unused\"\n[store]\npath = \"unused\"\n[host]\ndomain = \"{domain}\"\n"
    ))
    .unwrap();
    let identity = Identity::generate().unwrap().to_json().unwrap();
    let (agent, did_document) = server::build_agent(&config, Identity::from_json(&identity).unwrap()).unwrap();
    let did = agent.did();
    let store_path = std::env::temp_dir().join(format!("did-host-witness-{}.sqlite", uuid::Uuid::new_v4()));
    let host = host_at(&store_path, &config, &identity);
    let state = Arc::new(AppState { agent, host, did_document, admin: Default::default() });
    let app = server::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Running { did, url, domain, config, store_path, identity, state }
}

/// Another device's key-agreement key: a real X25519 key, as a sender encrypts to
/// every key a document lists.
fn device_key() -> String {
    let document = Identity::generate().unwrap().did_document("did:example:x", "https://example.invalid/");
    document["verificationMethod"][1]["publicKeyMultibase"].as_str().unwrap().to_string()
}

/// Someone with an app at an endpoint of its own: its `did:peer:4`, and, once it
/// has one, its hosted `did:webvh` -- the same keys, the same way in.
struct Holder {
    peer: Arc<Agent>,
    identity: String,
    address: Arc<RwLock<Option<Arc<Agent>>>>,
    inbox: Mutex<mpsc::UnboundedReceiver<Value>>,
    url: String,
    /// The device's Ed25519 key: the document's authentication key, and its update key.
    jwk: String,
    name: String,
    domain: String,
    host: String,
    witness: String,
    /// The log as written so far.
    did: String,
    log: String,
    /// Other devices' key-agreement keys, and recovery keys `(id, multikey)`.
    devices: Vec<String>,
    recovery: Vec<(String, String)>,
    /// Other trusted devices' signing keys (Ed25519 JWKs): update keys beside this one's.
    trusted: Vec<String>,
}

impl Holder {
    async fn start(host: &Running, name: &str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
        let identity = Identity::generate().unwrap();
        let kept = identity.to_json().unwrap();
        let jwk = serde_json::from_str::<Value>(&kept).unwrap()["verification_key"].to_string();
        let peer = Arc::new(Agent::with_endpoint(identity, &url).unwrap());
        let address: Arc<RwLock<Option<Arc<Agent>>>> = Default::default();
        let (sent, inbox) = mpsc::unbounded_channel();
        let app = Router::new()
            .route(
                "/",
                post(|State((peer, address, sent)): State<(Arc<Agent>, Arc<RwLock<Option<Arc<Agent>>>>, mpsc::UnboundedSender<Value>)>, body: Bytes| async move {
                    let as_address = address.read().unwrap().clone();
                    let received = match as_address {
                        Some(agent) => match agent.receive(&body).await {
                            Ok(received) => Ok(received),
                            Err(_) => peer.receive(&body).await,
                        },
                        None => peer.receive(&body).await,
                    };
                    let received = received.expect("a message this holder can read");
                    let _ = sent.send(json!({ "to_address": received.message["to"], "message": received.message }));
                }),
            )
            .with_state((peer.clone(), address.clone(), sent));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            peer,
            identity: kept,
            address,
            inbox: Mutex::new(inbox),
            url,
            jwk,
            name: name.to_string(),
            domain: host.domain.clone(),
            host: host.did.clone(),
            witness: String::new(),
            did: String::new(),
            log: String::new(),
            devices: Vec::new(),
            recovery: Vec::new(),
            trusted: Vec::new(),
        }
    }

    fn auth_key(&self) -> String {
        write::public_multikey(&self.jwk).unwrap()
    }

    fn document(&self) -> Value {
        let did = if self.did.is_empty() { format!("did:webvh:{{SCID}}:{}:{}", self.domain, self.name) } else { self.did.clone() };
        let identity = Identity::from_json(&self.identity).unwrap();
        let mut document = identity.did_document(&did, &self.url);
        document["alsoKnownAs"] = json!([format!("did:web:{}:{}", self.domain, self.name), self.peer.did()]);
        let methods = document["verificationMethod"].as_array_mut().unwrap();
        for key in &self.devices {
            methods.push(json!({ "id": format!("{did}#{key}"), "type": "Multikey", "controller": did, "publicKeyMultibase": key }));
        }
        for (id, key) in &self.recovery {
            methods.push(json!({ "id": format!("{did}#{id}"), "type": "Multikey", "controller": did, "publicKeyMultibase": key }));
        }
        let agreement = document["keyAgreement"].as_array_mut().unwrap();
        agreement.extend(self.devices.iter().map(|key| json!(format!("{did}#{key}"))));
        document["assertionMethod"] = json!([format!("{did}#key-1")]);
        document
    }

    async fn ask(&self, message_type: &str, body: Value, attachments: Option<Value>) -> Value {
        let mut message = json!({ "type": format!("{P}{message_type}"), "body": body, "return_route": "all" });
        if let Some(attachments) = attachments {
            message["attachments"] = attachments;
        }
        self.peer.send(&self.host, &message).await.expect("sent").expect("an answer").message
    }

    fn signed(&self, id: &str, media_type: &str, bytes: &[u8]) -> Value {
        signed_by(&self.jwk, id, media_type, bytes)
    }

    /// The next entry, written (not yet sent): the document as it now is.
    async fn write(&mut self, witnessed: bool) -> write::Written {
        if self.did.is_empty() {
            let options = write::Options { witness: witnessed.then(|| self.witness.clone()), ttl: Some(600) };
            let mut update_keys = vec![self.auth_key()];
            update_keys.extend(self.trusted.iter().map(|jwk| write::public_multikey(jwk).unwrap()));
            write::create_with(&self.document(), &self.jwk, &update_keys, &options).await.unwrap()
        } else {
            tokio::time::sleep(Duration::from_millis(1100)).await;
            write::append(&self.did, &self.log, &self.document(), &self.jwk, None).await.unwrap()
        }
    }

    /// Sends `written` after `after`, with a recovery key's co-signature if given.
    async fn publish(&self, written: &write::Written, after: Option<&str>, recovery_proof: Option<Value>) -> Value {
        let mut body = json!({ "name": self.name });
        if let Some(after) = after {
            body["after"] = json!(after);
        }
        if let Some(proof) = recovery_proof {
            body["recovery_proof"] = proof;
        }
        self.ask("publish", body, Some(json!([self.signed("log", "text/jsonl", written.entry.as_bytes())]))).await
    }

    /// Takes `written` as the log from now on, as an app keeps its own.
    fn keep(&mut self, written: &write::Written) {
        if self.did.is_empty() {
            *self.address.write().unwrap() = Some(Arc::new(Agent::with_did(Identity::from_json(&self.identity).unwrap(), &written.did)));
        }
        self.did = written.did.clone();
        self.log = written.log.clone();
    }

    /// Reserves, writes the first entry naming the host's witness, and publishes it.
    async fn register(&mut self) -> write::Written {
        let reservation = self.ask("reserve", json!({ "name": self.name }), None).await;
        assert_eq!(kind(&reservation), "reservation", "{reservation}");
        self.witness = reservation["body"]["witness"].as_str().expect("the host says it witnesses").to_string();
        let first = self.write(true).await;
        let published = self.publish(&first, None, None).await;
        assert_eq!(kind(&published), "published", "{published}");
        assert_eq!(published["body"]["witness"], self.witness);
        self.keep(&first);
        first
    }

    /// The next `pending` the host sent, to the hosted DID.
    async fn next_pending(&self) -> Value {
        loop {
            let got = tokio::time::timeout(Duration::from_secs(10), self.inbox.lock().await.recv()).await.expect("a pending within ten seconds").unwrap();
            if got["message"]["type"] == format!("{P}pending") {
                assert_eq!(got["to_address"], json!([self.did]), "sent to the hosted DID: {got}");
                return got["message"]["body"].clone();
            }
        }
    }
}

/// An attachment signed (`jws`) by the Ed25519 key `jwk`.
fn signed_by(jwk: &str, id: &str, media_type: &str, bytes: &[u8]) -> Value {
    let jws = write::sign_attachment(bytes, jwk).unwrap();
    json!({ "id": id, "media_type": media_type, "data": { "base64": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes), "jws": jws } })
}

/// Fetching what the host serves, as a reader does (over HTTP, at localhost).
struct Http;

impl Fetch for Http {
    async fn get(&self, url: &str) -> Result<Option<Vec<u8>>, String> {
        let response = reqwest::get(url.replace("https://", "http://")).await.map_err(|e| e.to_string())?;
        if response.status() == 404 {
            return Ok(None);
        }
        Ok(Some(response.bytes().await.map_err(|e| e.to_string())?.to_vec()))
    }
}

async fn served_keys(holder: &Holder) -> Vec<String> {
    let resolved = wyvrn_hosted_did::resolve(&holder.did, &Http).await.expect("the served log verifies, witnessed");
    wyvrn_hosted_did::keys::keys_for(&resolved.document, "keyAgreement")
}

#[tokio::test]
async fn what_only_adds_is_witnessed_at_once_and_a_removal_waits_a_day_while_the_holder_is_told() {
    let host = start_host().await;
    let mut alice = Holder::start(&host, "alice").await;

    // A first entry that doesn't name the host's witness isn't taken.
    let reservation = alice.ask("reserve", json!({ "name": "alice" }), None).await;
    alice.witness = reservation["body"]["witness"].as_str().unwrap().to_string();
    assert!(alice.witness.starts_with("did:key:z6Mk"), "{reservation}");
    let unwitnessed = alice.write(false).await;
    refused_with(&alice.publish(&unwitnessed, None, None).await, "e.p.msg.log-invalid");
    let first = alice.register().await;

    // Served with the witness's proof beside it, fresh every time, and verified.
    let log = reqwest::get(format!("{}alice/did.jsonl", host.url)).await.unwrap();
    assert_eq!(log.headers()["cache-control"], "no-cache");
    let proofs = reqwest::get(format!("{}alice/did-witness.json", host.url)).await.unwrap();
    assert_eq!(proofs.headers()["cache-control"], "no-cache");
    let tag = proofs.headers()["etag"].to_str().unwrap().to_string();
    let file: Value = proofs.json().await.unwrap();
    assert_eq!(file[0]["versionId"], first.version_id);
    assert_eq!(file[0]["proof"][0]["cryptosuite"], "eddsa-jcs-2022");
    assert_eq!(file[0]["proof"][0]["proofPurpose"], "assertionMethod");
    assert_eq!(file[0]["proof"][0]["verificationMethod"], format!("{}#{}", alice.witness, alice.witness.trim_start_matches("did:key:")));
    let again = reqwest::Client::new().get(format!("{}alice/did-witness.json", host.url)).header("if-none-match", &tag).send().await.unwrap();
    assert_eq!(again.status(), 304);
    assert_eq!(served_keys(&alice).await.len(), 1);
    // The DIDComm library's own resolver takes it as well.
    let document = didcomm_quickstart::default_resolver().resolve(&alice.did).await.unwrap();
    assert_eq!(document["id"], alice.did);

    // A device added: witnessed and published at once.
    alice.devices.push(device_key());
    let added = alice.write(true).await;
    let published = alice.publish(&added, Some(&first.version_id), None).await;
    assert_eq!(published["body"]["version_id"], added.version_id, "{published}");
    assert!(published["body"].get("pending").is_none());
    alice.keep(&added);
    assert_eq!(served_keys(&alice).await.len(), 2);

    // It removed again: held back a day. Neither file has it; the holder is told,
    // at its hosted DID, and finds it in status.
    alice.devices.clear();
    let removal = alice.write(true).await;
    let held = alice.publish(&removal, Some(&added.version_id), None).await;
    assert_eq!(kind(&held), "published", "{held}");
    assert_eq!(held["body"]["version_id"], added.version_id, "still the last witnessed: {held}");
    assert_eq!(held["body"]["pending"]["version_id"], removal.version_id);
    let accepted = held["body"]["pending"]["accepted_time"].as_i64().unwrap();
    assert_eq!(held["body"]["pending"]["publish_time"].as_i64().unwrap(), accepted + 24 * HOUR);
    let told = alice.next_pending().await;
    assert_eq!((told["state"].as_str(), told["version_id"].as_str()), (Some("held"), Some(removal.version_id.as_str())), "{told}");
    assert_eq!(told["entry"], removal.entry);
    assert_eq!(served_keys(&alice).await.len(), 2, "a held entry isn't served");
    let status = alice.ask("status-request", json!({ "name": "alice" }), None).await;
    assert_eq!(status["body"]["pending"]["version_id"], removal.version_id, "{status}");
    assert_eq!(status["body"]["pending"]["entry"], removal.entry);
    assert_eq!(status["body"]["witness"], alice.witness);
    // Not to anyone else.
    let stranger = Holder::start(&host, "stranger").await;
    let theirs = stranger.ask("status-request", json!({ "name": "alice" }), None).await;
    assert!(theirs["body"].get("pending").is_none(), "{theirs}");

    // Nothing is built on it.
    alice.keep(&removal);
    alice.devices.push(device_key());
    let on_top = alice.write(true).await;
    refused_with(&alice.publish(&on_top, Some(&removal.version_id), None).await, "e.p.msg.pending");

    // An hour before its day is up, the holder is told again; when it is, published.
    host.state.host.publish_due(accepted + 22 * HOUR).await.unwrap();
    assert!(host.state.host.take_notices().is_empty());
    host.state.host.publish_due(accepted + 23 * HOUR + 1).await.unwrap();
    server::send_notices(&host.state, host.state.host.take_notices());
    assert_eq!(alice.next_pending().await["state"], "soon");
    host.state.host.publish_due(accepted + 23 * HOUR + 30).await.unwrap();
    assert!(host.state.host.take_notices().is_empty(), "told once");
    host.state.host.publish_due(accepted + 24 * HOUR).await.unwrap();
    server::send_notices(&host.state, host.state.host.take_notices());
    let told = alice.next_pending().await;
    assert_eq!((told["state"].as_str(), told["version_id"].as_str()), (Some("published"), Some(removal.version_id.as_str())));
    assert_eq!(served_keys(&alice).await.len(), 1, "witnessed and published");
    let status = alice.ask("status-request", json!({ "name": "alice" }), None).await;
    assert_eq!(status["body"]["version_id"], removal.version_id);
    assert!(status["body"].get("pending").is_none());
    let _ = std::fs::remove_file(&host.store_path);
}

#[tokio::test]
async fn a_recovery_key_ends_the_wait_and_one_that_isnt_the_addresss_is_refused() {
    let host = start_host().await;
    let mut alice = Holder::start(&host, "carol").await;
    let recovery_jwk = jwk(11);
    alice.recovery.push(("recovery-1a2b".to_string(), write::public_multikey(&recovery_jwk).unwrap()));
    alice.devices.push(device_key());
    let first = alice.register().await;

    // A removal co-signed by a key the address doesn't list, or by the right key
    // over another entry: refused.
    alice.devices.clear();
    let removal = alice.write(true).await;
    let wrong_key = recovery::sign_address_entry(&removal.version_id, &jwk(12), "recovery-1a2b").unwrap();
    refused_with(&alice.publish(&removal, Some(&first.version_id), Some(wrong_key)).await, "e.p.msg.recovery-invalid");
    let wrong_entry = recovery::sign_address_entry(&first.version_id, &recovery_jwk, "recovery-1a2b").unwrap();
    refused_with(&alice.publish(&removal, Some(&first.version_id), Some(wrong_entry)).await, "e.p.msg.recovery-invalid");
    // Co-signed by the address's recovery key: published at once.
    let proof = recovery::sign_address_entry(&removal.version_id, &recovery_jwk, "recovery-1a2b").unwrap();
    let published = alice.publish(&removal, Some(&first.version_id), Some(proof)).await;
    assert_eq!(published["body"]["version_id"], removal.version_id, "{published}");
    assert!(published["body"].get("pending").is_none());
    alice.keep(&removal);
    assert_eq!(served_keys(&alice).await.len(), 1);

    // A recovery key added is held back like one taken away, and `cosign` ends it.
    let new_recovery = jwk(13);
    alice.recovery.push(("recovery-5e6f".to_string(), write::public_multikey(&new_recovery).unwrap()));
    let changed = alice.write(true).await;
    let held = alice.publish(&changed, Some(&removal.version_id), None).await;
    assert_eq!(held["body"]["pending"]["version_id"], changed.version_id, "{held}");
    assert_eq!(alice.next_pending().await["state"], "held");
    // Only by a key the last witnessed entry lists: not the one it adds.
    let by_new = recovery::sign_address_entry(&changed.version_id, &new_recovery, "recovery-5e6f").unwrap();
    refused_with(&alice.ask("cosign", json!({ "name": "carol", "version_id": changed.version_id, "recovery_proof": by_new }), None).await, "e.p.msg.recovery-invalid");
    let proof = recovery::sign_address_entry(&changed.version_id, &recovery_jwk, "recovery-1a2b").unwrap();
    refused_with(&alice.ask("cosign", json!({ "name": "carol", "version_id": first.version_id, "recovery_proof": proof }), None).await, "e.p.msg.pending");
    let cosigned = alice.ask("cosign", json!({ "name": "carol", "version_id": changed.version_id, "recovery_proof": proof }), None).await;
    assert_eq!(kind(&cosigned), "published", "{cosigned}");
    assert_eq!(cosigned["body"]["version_id"], changed.version_id);
    assert_eq!(alice.next_pending().await["state"], "published");
    let _ = std::fs::remove_file(&host.store_path);
}

#[tokio::test]
async fn a_pending_entry_is_withdrawn_or_replaced_by_one_that_only_adds_and_kept_across_a_restart() {
    let host = start_host().await;
    let mut alice = Holder::start(&host, "dave").await;
    let tablet = device_key();
    alice.devices.push(tablet.clone());
    let first = alice.register().await;

    // Held, then withdrawn by the holder: signed by an update key, or not at all.
    alice.devices.clear();
    let removal = alice.write(true).await;
    assert!(alice.publish(&removal, Some(&first.version_id), None).await["body"]["pending"].is_object());
    assert_eq!(alice.next_pending().await["state"], "held");
    let body = json!({ "name": "dave", "version_id": removal.version_id });
    refused_with(&alice.ask("withdraw", body.clone(), None).await, "e.p.msg.not-holder");
    let by_another = {
        let other = Holder { jwk: jwk(21), ..Holder::start(&host, "x").await };
        other.signed("withdraw", "text/plain", removal.version_id.as_bytes())
    };
    refused_with(&alice.ask("withdraw", body.clone(), Some(json!([by_another]))).await, "e.p.msg.not-holder");
    let withdrawn = alice.ask("withdraw", body.clone(), Some(json!([alice.signed("withdraw", "text/plain", removal.version_id.as_bytes())]))).await;
    assert_eq!(kind(&withdrawn), "status", "{withdrawn}");
    assert!(withdrawn["body"].get("pending").is_none());
    assert_eq!(alice.next_pending().await["state"], "dropped");
    refused_with(&alice.ask("withdraw", body, None).await, "e.p.msg.pending");

    // Held again, then replaced by an entry after the last witnessed one that only adds.
    let removal = alice.write(true).await;
    assert!(alice.publish(&removal, Some(&first.version_id), None).await["body"]["pending"].is_object());
    assert_eq!(alice.next_pending().await["state"], "held");
    // Another that takes away has to wait its turn.
    alice.recovery.push(("recovery-9".to_string(), write::public_multikey(&jwk(31)).unwrap()));
    let another = alice.write(true).await;
    refused_with(&alice.publish(&another, Some(&first.version_id), None).await, "e.p.msg.pending");
    alice.recovery.clear();
    alice.devices = vec![tablet, device_key()];
    let adds = alice.write(true).await;
    let published = alice.publish(&adds, Some(&first.version_id), None).await;
    assert_eq!(published["body"]["version_id"], adds.version_id, "{published}");
    assert_eq!(alice.next_pending().await["state"], "dropped");
    alice.keep(&adds);
    assert_eq!(served_keys(&alice).await.len(), 3);

    // Held across a restart: the host started again over the same store has the
    // same witness, and publishes it when its day is up.
    alice.devices.pop();
    let removal = alice.write(true).await;
    let held = alice.publish(&removal, Some(&adds.version_id), None).await;
    let accepted = held["body"]["pending"]["accepted_time"].as_i64().unwrap();
    let restarted = host_over(&host);
    assert_eq!(restarted.witness.as_ref().unwrap().did, alice.witness);
    assert_eq!(restarted.store.pending("dave").unwrap().unwrap().version_id, removal.version_id);
    restarted.publish_due(accepted + 24 * HOUR).await.unwrap();
    assert_eq!(restarted.store.get("dave").unwrap().unwrap().version_id.as_deref(), Some(removal.version_id.as_str()));
    // Served (by the host still running, over the same store) and verified.
    alice.keep(&removal);
    assert_eq!(served_keys(&alice).await.len(), 2);
    let _ = std::fs::remove_file(&host.store_path);
}

#[tokio::test]
async fn a_service_holding_an_address_names_the_hosts_witness_from_its_first_entry() {
    use wyvrn_service::address::{build_agent, DidMethod};
    use wyvrn_service::hosted_address::{Address, Holding, Kept};
    let host = start_host().await;
    let identity = Identity::generate().unwrap();
    let asking = Arc::new(Agent::new(Identity::from_json(&identity.to_json().unwrap()).unwrap()).unwrap());
    let (_, published) = build_agent(identity, "https://news.example/", DidMethod::Web).unwrap();
    let ask = {
        let (asking, host_did) = (asking.clone(), host.did.clone());
        Box::new(move |_to: String, mut message: Value| {
            let (asking, host_did) = (asking.clone(), host_did.clone());
            Box::pin(async move {
                message["return_route"] = json!("all");
                Ok(asking.send(&host_did, &message).await?.ok_or_else(|| anyhow::anyhow!("no answer"))?.message)
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Value>> + Send>>
        })
    };
    let address = Address::parse(&format!("did:web:{}:news", host.domain)).unwrap();
    let holding = Holding::new(address, Kept::scratch().unwrap(), published.unwrap(), ask).unwrap();
    let did = holding.hold(1_800_000_000).await.unwrap();
    let log = reqwest::get(format!("{}news/did.jsonl", host.url)).await.unwrap().text().await.unwrap();
    let first: Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(first["parameters"]["witness"], host.state.host.witness.as_ref().unwrap().parameter());
    let resolved = wyvrn_hosted_did::resolve(&did, &Http).await.expect("witnessed, and verified so");
    assert_eq!(resolved.did, did);
    let _ = std::fs::remove_file(&host.store_path);
}

/// What is kept of witnessing, in whichever database the tests are given
/// (`WYVRN_TEST_DATABASE_URL` for MySQL): a pending entry, the witness's proofs,
/// the witness key; and nothing of them once the name is freed.
#[tokio::test]
async fn what_witnessing_keeps_is_kept_in_the_store_and_freed_with_the_name() {
    use wyvrn_did_host::store::{Pending, Published};
    let store = Store::in_memory().unwrap();
    let witness = Witness::load_or_make(&store, "did:key:z6Mkhost").unwrap();
    assert_eq!(Witness::load_or_make(&store, "did:key:z6Mkhost").unwrap().did, witness.did, "made once");
    store.publish("alice", Published::Yes, "did:webvh:Qm:id.example:alice", "1-Qm", "{}", "{}", &["z6LSa".into()], &["z6LSa".into()], Some("[1]"), 10).unwrap();
    assert_eq!(store.witness_proofs("alice").unwrap().as_deref(), Some("[1]"));
    let pending = Pending {
        name: "alice".into(),
        version_id: "2-Qm".into(),
        after: "1-Qm".into(),
        entries: "{\"versionId\":\"2-Qm\"}".into(),
        did: "did:webvh:Qm:id.example:alice".into(),
        log: "{}\n{}".repeat(1000),
        web_document: "{}".into(),
        holder_keys: vec!["z6LSa".into()],
        all_keys: vec!["z6LSa".into(), "z6LSb".into()],
        proven: vec!["z6LSa".into()],
        deactivated: true,
        accepted_at: 100,
        publish_at: 100 + witness::HOLD_SECONDS,
        soon_sent: false,
    };
    store.hold_back(&pending).unwrap();
    assert_eq!(store.pending("alice").unwrap().as_ref(), Some(&pending));
    store.told_soon("alice").unwrap();
    assert!(store.all_pending().unwrap()[0].soon_sent);
    // Published over it: the pending entry is gone, the proofs replaced.
    store.publish("alice", Published::Yes, "did:webvh:Qm:id.example:alice", "3-Qm", "{}", "{}", &[], &[], Some("[3]"), 20).unwrap();
    assert!(store.pending("alice").unwrap().is_none());
    assert_eq!(store.witness_proofs("alice").unwrap().as_deref(), Some("[3]"));
    store.hold_back(&pending).unwrap();
    store.drop_pending("alice").unwrap();
    assert!(store.all_pending().unwrap().is_empty());
    store.hold_back(&pending).unwrap();
    store.free("alice").unwrap();
    assert!(store.pending("alice").unwrap().is_none() && store.witness_proofs("alice").unwrap().is_none());
}

#[tokio::test]
async fn a_first_recovery_key_waits_a_day_unless_a_second_trusted_device_co_signs_it() {
    let host = start_host().await;
    let mut alice = Holder::start(&host, "erin").await;
    let laptop = jwk(41);
    alice.trusted.push(laptop.clone());
    let tablet = device_key();
    alice.devices.push(tablet.clone());
    let first = alice.register().await;

    // The first recovery key: held back, with nothing in force to co-sign it with.
    alice.recovery.push(("recovery-77aa".to_string(), write::public_multikey(&jwk(42)).unwrap()));
    let adds = alice.write(true).await;
    let held = alice.publish(&adds, Some(&first.version_id), None).await;
    assert_eq!(held["body"]["pending"]["version_id"], adds.version_id, "{held}");
    assert_eq!(alice.next_pending().await["state"], "held");
    let cosign = |version: &str| json!({ "name": "erin", "version_id": version });

    // Meanwhile what would be held back too is refused, saying until when.
    alice.recovery.push(("recovery-88bb".to_string(), write::public_multikey(&jwk(43)).unwrap()));
    let more = alice.write(true).await;
    let refused = alice.publish(&more, Some(&first.version_id), None).await;
    refused_with(&refused, "e.p.msg.pending");
    assert_eq!(refused["body"]["args"][1], held["body"]["pending"]["publish_time"].as_i64().unwrap().to_string(), "{refused}");
    alice.recovery.pop();

    // Not by the device that wrote it, nor by a key the log doesn't list, nor over
    // another entry; a recovery key it adds can't either.
    let by_writer = alice.signed("cosign", "text/plain", adds.version_id.as_bytes());
    refused_with(&alice.ask("cosign", cosign(&adds.version_id), Some(json!([by_writer]))).await, "e.p.msg.cosign-invalid");
    let by_stranger = signed_by(&jwk(44), "cosign", "text/plain", adds.version_id.as_bytes());
    refused_with(&alice.ask("cosign", cosign(&adds.version_id), Some(json!([by_stranger]))).await, "e.p.msg.cosign-invalid");
    let over_another = signed_by(&laptop, "cosign", "text/plain", first.version_id.as_bytes());
    refused_with(&alice.ask("cosign", cosign(&adds.version_id), Some(json!([over_another]))).await, "e.p.msg.cosign-invalid");
    let by_new = recovery::sign_address_entry(&adds.version_id, &jwk(42), "recovery-77aa").unwrap();
    refused_with(&alice.ask("cosign", json!({ "name": "erin", "version_id": adds.version_id, "recovery_proof": by_new }), None).await, "e.p.msg.recovery-invalid");

    // By the other trusted device: published at once.
    let by_laptop = signed_by(&laptop, "cosign", "text/plain", adds.version_id.as_bytes());
    let cosigned = alice.ask("cosign", cosign(&adds.version_id), Some(json!([by_laptop]))).await;
    assert_eq!(kind(&cosigned), "published", "{cosigned}");
    assert_eq!(cosigned["body"]["version_id"], adds.version_id);
    assert_eq!(alice.next_pending().await["state"], "published");
    alice.keep(&adds);

    // With a recovery key in force, a device co-signs nothing: a removal still waits,
    // or takes the recovery key.
    alice.devices.clear();
    let removal = alice.write(true).await;
    assert!(alice.publish(&removal, Some(&adds.version_id), None).await["body"]["pending"].is_object());
    assert_eq!(alice.next_pending().await["state"], "held");
    let by_laptop = signed_by(&laptop, "cosign", "text/plain", removal.version_id.as_bytes());
    refused_with(&alice.ask("cosign", cosign(&removal.version_id), Some(json!([by_laptop]))).await, "e.p.msg.cosign-invalid");
    let _ = std::fs::remove_file(&host.store_path);
}

#[tokio::test]
async fn a_first_recovery_key_from_an_accounts_one_trusted_device_is_published_at_once() {
    let host = start_host().await;
    let mut alice = Holder::start(&host, "frank").await;
    alice.devices.push(device_key());
    let first = alice.register().await;

    // One trusted device: whoever holds it is the account, and the key goes in now.
    alice.recovery.push(("recovery-99cc".to_string(), write::public_multikey(&jwk(51)).unwrap()));
    let adds = alice.write(true).await;
    let published = alice.publish(&adds, Some(&first.version_id), None).await;
    assert_eq!(kind(&published), "published", "{published}");
    assert_eq!(published["body"]["version_id"], adds.version_id, "{published}");
    assert!(published["body"].get("pending").is_none(), "{published}");
    alice.keep(&adds);

    // A second recovery key, or one changed, still waits the day.
    alice.recovery.push(("recovery-aadd".to_string(), write::public_multikey(&jwk(52)).unwrap()));
    let another = alice.write(true).await;
    assert!(alice.publish(&another, Some(&adds.version_id), None).await["body"]["pending"].is_object());
    let _ = std::fs::remove_file(&host.store_path);
}
