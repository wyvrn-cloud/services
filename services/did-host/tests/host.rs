//! The host run for real: its HTTP server, holders that write real `did:webvh`
//! logs and talk to it over DIDComm, and a resolver that fetches what it serves.

use std::sync::Arc;

use affinidi_data_integrity::{DataIntegrityProof, SignOptions};
use affinidi_secrets_resolver::secrets::Secret;
use base64::Engine;
use didcomm_agent::{Agent, Identity};
use didwebvh_rs::{log_entry::LogEntryMethods, parameters::Parameters, DIDWebVHState, Multibase};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use wyvrn_hosted_did::{Fetch, Standing};
use wyvrn_did_host::{
    host::Host,
    server::{self, AppState},
    store::Store,
};

const DOMAIN: &str = "id.example";
const P: &str = "https://wyvrn.app/did-hosting/1.0/";
const DAY: i64 = 24 * 60 * 60;

struct Running {
    did: String,
    url: String,
    state: Arc<AppState>,
}

async fn start_host(admins: &[String]) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let admins = admins.iter().map(|did| format!("{did:?}")).collect::<Vec<_>>().join(", ");
    let config: wyvrn_did_host::config::Config = toml::from_str(&format!(
        "[server]\nbind = \"unused\"\npublic_url = \"{url}\"\n[identity]\npath = \"unused\"\n[store]\npath = \"unused\"\n\
         [host]\ndomain = \"{DOMAIN}\"\nadmins = [{admins}]\nreserved_names = [\"admin\"]\nreserved_namespaces = [\"official\"]\n"
    ))
    .unwrap();
    let (agent, did_document) = server::build_agent(&config, Identity::generate().unwrap()).unwrap();
    let did = agent.did();
    let host = Host::new(Store::in_memory().unwrap(), config.host.clone(), didcomm_quickstart::default_resolver());
    let state = Arc::new(AppState { agent, host, did_document });
    let app = server::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Running { did, url, state }
}

fn key_agreement_of(identity: &Identity) -> String {
    let document = identity.did_document("did:example:x", "https://example.invalid/");
    document["verificationMethod"][1]["publicKeyMultibase"].as_str().unwrap().to_string()
}

/// An agent whose `did:peer:4` lists, beside its own key-agreement key, `others`:
/// the keys of its other devices -- or keys that aren't its own at all.
fn agent_listing(identity: Identity, others: &[String]) -> Agent {
    use didcomm_resolver_peer::{peer4, KeyPurpose};
    let document = identity.did_document("did:example:x", "https://example.invalid/");
    let authentication = document["verificationMethod"][0]["publicKeyMultibase"].as_str().unwrap().to_string();
    let own = key_agreement_of(&identity);
    // `#key-2` is the one the agent holds, as `Agent::with_did` expects.
    let mut keys = vec![(KeyPurpose::Authentication, authentication.as_str()), (KeyPurpose::KeyAgreement, own.as_str())];
    keys.extend(others.iter().map(|key| (KeyPurpose::KeyAgreement, key.as_str())));
    let did = peer4::generate(&keys, &[]).unwrap();
    Agent::with_did(identity, &did)
}

fn signer() -> Secret {
    let mut key = Secret::generate_ed25519(None, None);
    let public = key.get_public_keymultibase().unwrap();
    key.id = format!("did:key:{public}#{public}");
    key
}

/// Someone with an app: a DIDComm identity, the keys their public DID will carry,
/// and the log of it as they have written it so far.
struct Holder {
    agent: Agent,
    /// The document's authentication key: signs what is uploaded, and the whois.
    auth: Secret,
    /// This device's update key: signs log entries.
    update: Secret,
    log: DIDWebVHState,
    name: String,
    host: String,
    /// The key-agreement keys of this holder's other devices, which its document
    /// lists beside this one's.
    other_devices: Vec<String>,
}

impl Holder {
    fn new(host: &Running, name: &str) -> Self {
        Self {
            agent: Agent::new(Identity::generate().unwrap()).unwrap(),
            auth: signer(),
            update: signer(),
            log: DIDWebVHState::default(),
            name: name.to_string(),
            host: host.did.clone(),
            other_devices: Vec::new(),
        }
    }

    fn key_agreement(&self) -> String {
        key_agreement_of(self.agent.identity())
    }

    /// The DID document the holder publishes, with `{SCID}` where the log's hash goes.
    fn document(&self, extra_services: Value) -> Value {
        // Before the first entry there is no hash to put in it yet.
        let did = if self.log.log_entries().is_empty() { format!("did:webvh:{{SCID}}:{DOMAIN}:{}", self.name) } else { self.did() };
        let mut methods = vec![
            json!({ "id": format!("{did}#key-1"), "type": "Multikey", "controller": did, "publicKeyMultibase": self.auth.get_public_keymultibase().unwrap() }),
            json!({ "id": format!("{did}#key-2"), "type": "Multikey", "controller": did, "publicKeyMultibase": self.key_agreement() }),
        ];
        let mut key_agreement = vec![format!("{did}#key-2")];
        for (index, key) in self.other_devices.iter().enumerate() {
            let id = format!("{did}#key-{}", index + 3);
            methods.push(json!({ "id": id, "type": "Multikey", "controller": did, "publicKeyMultibase": key }));
            key_agreement.push(id);
        }
        json!({
            "@context": ["https://www.w3.org/ns/did/v1", "https://w3id.org/security/multikey/v1"],
            "id": did,
            "alsoKnownAs": [format!("did:web:{DOMAIN}:{}", self.name), self.agent.did()],
            "verificationMethod": methods,
            "authentication": [format!("{did}#key-1")],
            "keyAgreement": key_agreement,
            "service": extra_services,
        })
    }

    fn did(&self) -> String {
        let scid = self.log.log_entries()[0].log_entry.get_scid().unwrap().to_string();
        format!("did:webvh:{scid}:{DOMAIN}:{}", self.name)
    }

    fn lines(&self, from: usize) -> String {
        self.log.log_entries()[from..].iter().map(|e| serde_json::to_string(&e.log_entry).unwrap()).collect::<Vec<_>>().join("\n")
    }

    fn version_id(&self) -> String {
        self.log.log_entries().last().unwrap().get_version_id().to_string()
    }

    async fn write_first_entry(&mut self) {
        let parameters = Parameters {
            update_keys: Some(Arc::new(vec![Multibase::new(self.update.get_public_keymultibase().unwrap())])),
            portable: Some(true),
            ..Default::default()
        };
        let document = self.document(json!([]));
        self.log.create_log_entry(None, &document, &parameters, &self.update).await.expect("a first entry");
    }

    async fn ask(&self, message_type: &str, body: Value, attachments: Option<Value>) -> Value {
        let mut message = json!({ "type": format!("{P}{message_type}"), "body": body, "return_route": "all" });
        if let Some(attachments) = attachments {
            message["attachments"] = attachments;
        }
        // (`send`, not `request`: a refusal is an answer here, not an error.)
        self.agent.send(&self.host, &message).await.expect("sent").expect("an answer").message
    }

    /// The log entries from `from` on, as the attachment `publish` carries: signed
    /// by the document's authentication key.
    fn log_attachment(&self, from: usize) -> Value {
        signed_attachment("log", "text/jsonl", self.lines(from).as_bytes(), &self.auth)
    }

    async fn reserve(&self) -> Value {
        self.ask("reserve", json!({ "name": self.name }), None).await
    }

    async fn publish(&self, from: usize, after: Option<&str>) -> Value {
        let mut body = json!({ "name": self.name });
        if let Some(after) = after {
            body["after"] = json!(after);
        }
        self.ask("publish", body, Some(json!([self.log_attachment(from)]))).await
    }

    /// Reserves, writes the first entry, publishes: a DID hosted under the name.
    async fn register(&mut self) -> Value {
        assert_eq!(kind(&self.reserve().await), "reservation");
        self.write_first_entry().await;
        let published = self.publish(0, None).await;
        assert_eq!(kind(&published), "published", "{published}");
        published
    }
}

/// A log entry has to be later than the one before it, to the second.
async fn next_second() {
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
}

fn signed_attachment(id: &str, media_type: &str, bytes: &[u8], key: &Secret) -> Value {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let protected = b64.encode(json!({ "alg": "EdDSA" }).to_string());
    let signing_key = SigningKey::from_bytes(key.get_private_bytes()[..32].try_into().unwrap());
    let signature = signing_key.sign(format!("{protected}.{}", b64.encode(bytes)).as_bytes());
    json!({
        "id": id,
        "media_type": media_type,
        "data": { "base64": b64.encode(bytes), "jws": { "protected": protected, "signature": b64.encode(signature.to_bytes()) } },
    })
}

/// The last part of a message's type: `published`, `problem-report`.
fn kind(message: &Value) -> &str {
    message["type"].as_str().unwrap_or_default().rsplit('/').next().unwrap_or_default()
}

fn refused_with<'a>(message: &'a Value, code: &str) -> &'a Value {
    assert_eq!(kind(message), "problem-report", "{message}");
    assert_eq!(message["body"]["code"], code, "{message}");
    message
}

async fn fetch(host: &Running, path: &str) -> reqwest::Response {
    reqwest::get(format!("{}{path}", host.url)).await.unwrap()
}

#[tokio::test]
async fn a_holder_claims_a_name_publishes_its_did_and_anyone_can_resolve_it() {
    let host = start_host(&[]).await;
    let mut alice = Holder::new(&host, "alice");

    // Before anything is published there is nothing to fetch, held or not.
    let reservation = alice.reserve().await;
    assert_eq!(kind(&reservation), "reservation");
    assert_eq!(reservation["body"]["did_web"], "did:web:id.example:alice");
    assert_eq!(reservation["body"]["host"], DOMAIN);
    assert_eq!(fetch(&host, "alice/did.jsonl").await.status(), 404);

    alice.write_first_entry().await;
    let published = alice.publish(0, None).await;
    assert_eq!(kind(&published), "published", "{published}");
    assert_eq!(published["body"]["did"], alice.did());
    assert_eq!(published["body"]["did_web"], "did:web:id.example:alice");
    assert_eq!(published["body"]["version_id"], alice.version_id());

    // What is served is the log as she wrote it, and it verifies as her DID's.
    let served = fetch(&host, "alice/did.jsonl").await;
    assert_eq!(served.headers()["content-type"], "text/jsonl; charset=utf-8");
    let log = served.text().await.unwrap();
    assert_eq!(log.trim_end(), alice.lines(0));
    let mut resolver = DIDWebVHState::default();
    let (_, metadata) = resolver.resolve_log(&alice.did(), &log, None).await.expect("the served log verifies");
    assert!(metadata.portable);

    // Its parallel did:web: the same keys under the short name, naming the did:webvh.
    let web: Value = fetch(&host, "alice/did.json").await.json().await.unwrap();
    assert_eq!(web["id"], "did:web:id.example:alice");
    assert!(web["alsoKnownAs"].as_array().unwrap().contains(&json!(alice.did())), "{web}");
    assert_eq!(web["keyAgreement"][0], "did:web:id.example:alice#key-2");
    assert_eq!(web["verificationMethod"][1]["publicKeyMultibase"], alice.key_agreement());
    assert!(!web.to_string().contains("did:webvh:{SCID}"));

    // Anyone can ask how the name stands; only she is told it is hers.
    let bob = Holder::new(&host, "bob");
    let asked = bob.ask("status-request", json!({ "name": "alice" }), None).await;
    assert_eq!(asked["body"]["state"], "published");
    assert_eq!(asked["body"]["did"], alice.did());
    assert_eq!(asked["body"]["yours"], false);
    let own = alice.ask("status-request", json!({}), None).await;
    assert_eq!(own["body"]["name"], "alice");
    assert_eq!(own["body"]["yours"], true);
}

#[tokio::test]
async fn a_did_is_updated_by_its_holder_only_and_only_by_continuing_it() {
    let host = start_host(&[]).await;
    let mut alice = Holder::new(&host, "alice");
    alice.register().await;
    let first = alice.version_id();

    // She adds a service: a second entry, sent as what follows the first.
    let with_service = alice.document(json!([{ "id": "#didcomm", "type": "DIDCommMessaging", "serviceEndpoint": { "uri": "https://mediator.example/" } }]));
    next_second().await;
    alice.log.update_document(with_service, &alice.update).await.unwrap();
    let published = alice.publish(1, Some(&first)).await;
    assert_eq!(kind(&published), "published", "{published}");
    assert_eq!(published["body"]["version_id"], alice.version_id());
    let web: Value = fetch(&host, "alice/did.json").await.json().await.unwrap();
    assert_eq!(web["service"][0]["type"], "DIDCommMessaging");
    assert_eq!(fetch(&host, "alice/did.jsonl").await.text().await.unwrap().lines().count(), 2);

    // The same entry again follows nothing: the log has moved on, and says to what.
    let stale = alice.publish(1, Some(&first)).await;
    assert_eq!(refused_with(&stale, "e.p.msg.conflict")["body"]["args"][0], alice.version_id());

    // Mallory can't append: not an entry of her own making (it isn't signed by an
    // update key of Alice's)...
    let mut mallory = Holder::new(&host, "alice");
    mallory.write_first_entry().await;
    let forged = mallory.ask("publish", json!({ "name": "alice", "after": alice.version_id() }), Some(json!([mallory.log_attachment(0)]))).await;
    refused_with(&forged, "e.p.msg.log-invalid");
    // ...and not one of Alice's that she got hold of and sends herself.
    next_second().await;
    alice.log.update_document(alice.document(json!([])), &alice.update).await.unwrap();
    let replayed = mallory.ask("publish", json!({ "name": "alice", "after": published["body"]["version_id"] }), Some(json!([alice.log_attachment(2)]))).await;
    refused_with(&replayed, "e.p.msg.not-holder");
    assert_eq!(fetch(&host, "alice/did.jsonl").await.text().await.unwrap().lines().count(), 2);

    // Nor does a log pass that was changed after it was signed, or sent unsigned.
    let mut carol = Holder::new(&host, "carol");
    carol.reserve().await;
    carol.write_first_entry().await;
    let tampered = carol.lines(0).replace("#key-2", "#key-9");
    let answer = carol.ask("publish", json!({ "name": "carol" }), Some(json!([signed_attachment("log", "text/jsonl", tampered.as_bytes(), &carol.auth)]))).await;
    refused_with(&answer, "e.p.msg.log-invalid");
    let mut unsigned = carol.log_attachment(0);
    unsigned["data"].as_object_mut().unwrap().remove("jws");
    refused_with(&carol.ask("publish", json!({ "name": "carol" }), Some(json!([unsigned]))).await, "e.p.msg.log-invalid");
    // Signed by a key that isn't the document's is as good as unsigned.
    let wrong_key = signed_attachment("log", "text/jsonl", carol.lines(0).as_bytes(), &signer());
    refused_with(&carol.ask("publish", json!({ "name": "carol" }), Some(json!([wrong_key]))).await, "e.p.msg.log-invalid");
    assert_eq!(kind(&carol.publish(0, None).await), "published");
}

#[tokio::test]
async fn names_are_first_come_first_served_one_to_a_holder_and_well_formed() {
    let host = start_host(&[]).await;
    let mut alice = Holder::new(&host, "alice");

    for bad in ["Al", "ALICE", "al--ice", "-alice", "alíce", "a:b:c", ""] {
        refused_with(&alice.ask("reserve", json!({ "name": bad }), None).await, "e.p.msg.name-invalid");
    }

    // While Alice is claiming it, it is hers to publish under and nobody else's.
    alice.reserve().await;
    let mut eve = Holder::new(&host, "alice");
    refused_with(&eve.reserve().await, "e.p.msg.name-taken");
    eve.write_first_entry().await;
    refused_with(&eve.publish(0, None).await, "e.p.msg.not-held");
    // Publishing without having asked for the name at all is the same.
    let mut dave = Holder::new(&host, "dave");
    dave.write_first_entry().await;
    refused_with(&dave.publish(0, None).await, "e.p.msg.not-held");

    alice.write_first_entry().await;
    assert_eq!(kind(&alice.publish(0, None).await), "published");
    refused_with(&eve.reserve().await, "e.p.msg.name-taken");

    // Alice has her name. She can't have another: not asked for with the same keys...
    refused_with(&alice.ask("reserve", json!({ "name": "alice-two" }), None).await, "e.p.msg.key-in-use");
    // ...and not by a second identity whose document carries a key of the first.
    let mut second = Holder::new(&host, "alice-two");
    second.auth = alice.auth.clone();
    assert_eq!(kind(&second.reserve().await), "reservation");
    second.write_first_entry().await;
    refused_with(&second.publish(0, None).await, "e.p.msg.key-in-use");

    // A DID that may not be moved later is asked to say it may.
    let mut fixed = Holder::new(&host, "fixed");
    fixed.reserve().await;
    let parameters = Parameters { update_keys: Some(Arc::new(vec![Multibase::new(fixed.update.get_public_keymultibase().unwrap())])), ..Default::default() };
    let document = fixed.document(json!([]));
    fixed.log.create_log_entry(None, &document, &parameters, &fixed.update).await.unwrap();
    let answer = fixed.publish(0, None).await;
    assert!(refused_with(&answer, "e.p.msg.log-invalid")["body"]["comment"].as_str().unwrap().contains("portable"));
}

#[tokio::test]
async fn a_reserved_name_is_claimed_only_by_whoever_an_admin_assigns_it_to() {
    let admin = Agent::new(Identity::generate().unwrap()).unwrap();
    let host = start_host(&[admin.did()]).await;
    let as_admin = |message_type: &'static str, body: Value| {
        let (admin, to) = (&admin, host.did.clone());
        async move {
            let message = json!({ "type": format!("{P}{message_type}"), "body": body, "return_route": "all" });
            admin.send(&to, &message).await.unwrap().expect("an answer").message
        }
    };

    // Reserved by configuration, a single name and a whole namespace: nobody's yet.
    let mut news = Holder::new(&host, "official:news");
    refused_with(&news.reserve().await, "e.p.msg.name-reserved");
    refused_with(&Holder::new(&host, "admin").reserve().await, "e.p.msg.name-reserved");
    assert_eq!(news.ask("status-request", json!({ "name": "official:news" }), None).await["body"]["state"], "reserved");

    // Only an admin says who may claim one.
    refused_with(&news.ask("assign", json!({ "name": "official:news", "holder": news.agent.did() }), None).await, "e.p.msg.not-admin");
    refused_with(&news.ask("free", json!({ "name": "official:news" }), None).await, "e.p.msg.not-admin");

    let assigned = as_admin("assign", json!({ "name": "official:news", "holder": news.agent.did() })).await;
    assert_eq!(assigned["body"]["state"], "reserved");
    // Assigned to her, it is still not anybody else's...
    refused_with(&Holder::new(&host, "official:news").reserve().await, "e.p.msg.name-reserved");
    // ...and she claims it the way any name is claimed.
    assert_eq!(news.ask("status-request", json!({ "name": "official:news" }), None).await["body"]["yours"], true);
    news.register().await;
    assert_eq!(fetch(&host, "official/news/did.jsonl").await.status(), 200);
    let web: Value = fetch(&host, "official/news/did.json").await.json().await.unwrap();
    assert_eq!(web["id"], "did:web:id.example:official:news");

    // Once published it is hers: an admin can't hand it to someone else over her...
    refused_with(&as_admin("assign", json!({ "name": "official:news", "holder": admin.did() })).await, "e.p.msg.name-taken");
    // ...only free it, which ends the hosting and leaves the name reserved again.
    let freed = as_admin("free", json!({ "name": "official:news", "comment": "test" })).await;
    assert_eq!(freed["body"]["state"], "reserved");
    assert_eq!(fetch(&host, "official/news/did.jsonl").await.status(), 404);
    assert_eq!(fetch(&host, "official/news/did.json").await.status(), 404);

    // An admin can set aside a name that wasn't, and a freed ordinary name is free.
    assert_eq!(as_admin("assign", json!({ "name": "famous", "holder": null })).await["body"]["state"], "reserved");
    refused_with(&Holder::new(&host, "famous").reserve().await, "e.p.msg.name-reserved");
    let mut zed = Holder::new(&host, "zed");
    zed.register().await;
    assert_eq!(as_admin("free", json!({ "name": "zed" })).await["body"]["state"], "free");
    // Her keys are no longer in anything hosted: she may claim a name again.
    assert_eq!(kind(&zed.reserve().await), "reservation");
}

#[tokio::test]
async fn a_whois_is_published_by_the_holder_signed_by_the_did_and_about_nothing_else() {
    let host = start_host(&[]).await;
    let mut alice = Holder::new(&host, "alice");
    alice.register().await;
    let did = alice.did();

    let presentation = |holder: &str, subject: &str| {
        json!({
            "@context": ["https://www.w3.org/ns/credentials/v2", "https://wyvrn.app/ns/profile/v1"],
            "type": ["VerifiablePresentation"],
            "holder": holder,
            "verifiableCredential": [{
                "@context": ["https://www.w3.org/ns/credentials/v2", "https://wyvrn.app/ns/profile/v1"],
                "type": ["VerifiableCredential", "ProfileCredential"],
                "issuer": holder,
                "credentialSubject": { "id": subject, "type": "Person", "name": "Alice", "description": "Writes about gardens." },
            }],
        })
    };
    // Signed as the DID's own authentication key, by the suite that needs no
    // JSON-LD processing to check.
    let sign = |mut presentation: Value, key: &Secret, did: &str| {
        let mut key = key.clone();
        key.id = format!("{did}#key-1");
        async move {
            let proof = DataIntegrityProof::sign(&presentation, &key, SignOptions::new().with_proof_purpose("authentication")).await.unwrap();
            presentation["proof"] = serde_json::to_value(proof).unwrap();
            presentation
        }
    };
    // Uploaded like the log: the attachment itself signed by the DID's key.
    let send = |holder: &Holder, presentation: &Value| {
        let attachment = json!([signed_attachment("whois", "application/vp", presentation.to_string().as_bytes(), &alice.auth)]);
        let holder_name = holder.name.clone();
        async move { (holder_name, attachment) }
    };

    assert_eq!(fetch(&host, "alice/whois.vp").await.status(), 404);

    let whois = sign(presentation(&did, &did), &alice.auth, &did).await;
    let (name, attachment) = send(&alice, &whois).await;
    let answer = alice.ask("publish-whois", json!({ "name": name }), Some(attachment)).await;
    assert_eq!(kind(&answer), "status", "{answer}");
    assert_eq!(answer["body"]["whois"], true);

    let served = fetch(&host, "alice/whois.vp").await;
    assert_eq!(served.headers()["content-type"], "application/vp");
    assert_eq!(served.json::<Value>().await.unwrap(), whois);
    // The did:web says where it is; the did:webvh has it implicitly.
    let web: Value = fetch(&host, "alice/did.json").await.json().await.unwrap();
    let service = web["service"].as_array().unwrap().iter().find(|s| s["type"] == "LinkedVerifiablePresentation").expect("a whois service");
    assert_eq!(service["id"], "did:web:id.example:alice#whois");
    assert_eq!(service["serviceEndpoint"], "https://id.example/alice/whois.vp");

    // Not signed, signed by someone else's key, held by someone else, or about
    // someone else: none of these is her saying who she is.
    let unsigned = presentation(&did, &did);
    let (name, attachment) = send(&alice, &unsigned).await;
    refused_with(&alice.ask("publish-whois", json!({ "name": name }), Some(attachment)).await, "e.p.msg.whois-invalid");
    let by_another = sign(presentation(&did, &did), &signer(), &did).await;
    let (name, attachment) = send(&alice, &by_another).await;
    refused_with(&alice.ask("publish-whois", json!({ "name": name }), Some(attachment)).await, "e.p.msg.whois-invalid");
    let of_another = sign(presentation("did:web:someone.example", "did:web:someone.example"), &alice.auth, &did).await;
    let (name, attachment) = send(&alice, &of_another).await;
    refused_with(&alice.ask("publish-whois", json!({ "name": name }), Some(attachment)).await, "e.p.msg.whois-invalid");
    let about_another = sign(presentation(&did, "did:web:someone.example"), &alice.auth, &did).await;
    let (name, attachment) = send(&alice, &about_another).await;
    refused_with(&alice.ask("publish-whois", json!({ "name": name }), Some(attachment)).await, "e.p.msg.whois-invalid");
    // A good presentation in an attachment that isn't signed, or is signed by
    // some other key: every upload has to be the DID's own, on the outside too.
    let bare = json!([{ "id": "whois", "media_type": "application/vp", "data": { "base64": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(whois.to_string()) } }]);
    refused_with(&alice.ask("publish-whois", json!({ "name": "alice" }), Some(bare)).await, "e.p.msg.whois-invalid");
    let by_stranger = json!([signed_attachment("whois", "application/vp", whois.to_string().as_bytes(), &signer())]);
    refused_with(&alice.ask("publish-whois", json!({ "name": "alice" }), Some(by_stranger)).await, "e.p.msg.whois-invalid");
    // Tampered with after signing.
    let mut changed = whois.clone();
    changed["verifiableCredential"][0]["credentialSubject"]["name"] = json!("Mallory");
    let (name, attachment) = send(&alice, &changed).await;
    refused_with(&alice.ask("publish-whois", json!({ "name": name }), Some(attachment)).await, "e.p.msg.whois-invalid");
    // And nobody else can set hers, even with one that is properly hers.
    let mallory = Holder::new(&host, "mallory");
    let (_, attachment) = send(&alice, &whois).await;
    refused_with(&mallory.ask("publish-whois", json!({ "name": "alice" }), Some(attachment)).await, "e.p.msg.not-holder");
    assert_eq!(fetch(&host, "alice/whois.vp").await.json::<Value>().await.unwrap(), whois);

    // With no attachment she takes it down. The did:web goes on saying where one
    // would be, as did:webvh has every such document do: asking there finds none.
    let removed = alice.ask("publish-whois", json!({ "name": "alice" }), None).await;
    assert_eq!(removed["body"]["whois"], false);
    assert_eq!(fetch(&host, "alice/whois.vp").await.status(), 404);
    let web: Value = fetch(&host, "alice/did.json").await.json().await.unwrap();
    assert!(web.to_string().contains("LinkedVerifiablePresentation"));
}

#[tokio::test]
async fn a_holder_ends_its_did_and_a_silent_one_goes_stale_until_it_speaks() {
    let host = start_host(&[]).await;
    let mut alice = Holder::new(&host, "alice");
    alice.register().await;
    let mut bob = Holder::new(&host, "bob");
    bob.register().await;

    // Alice ends hers: the end is published like any entry, and stays to be seen.
    let before = alice.version_id();
    next_second().await;
    alice.log.deactivate(&alice.update).await.unwrap();
    assert_eq!(kind(&alice.publish(1, Some(&before)).await), "published");
    assert_eq!(alice.ask("status-request", json!({ "name": "alice" }), None).await["body"]["state"], "deactivated");
    assert_eq!(fetch(&host, "alice/did.jsonl").await.text().await.unwrap().lines().count(), 2);
    // Nobody claims the name while it stands.
    refused_with(&Holder::new(&host, "alice").reserve().await, "e.p.msg.name-taken");

    // Bob says nothing for a year: the host asks. A week more: stale, still served.
    let state_of_bob = || async { bob.ask("status-request", json!({ "name": "bob" }), None).await["body"]["state"].clone() };
    let registered = wyvrn_service::now();
    server::check_staleness(&host.state, registered + 364 * DAY).await.unwrap();
    assert!(host.state.host.store.get("bob").unwrap().unwrap().asked_at.is_none());
    server::check_staleness(&host.state, registered + 366 * DAY).await.unwrap();
    assert!(host.state.host.store.get("bob").unwrap().unwrap().asked_at.is_some());
    server::check_staleness(&host.state, registered + 370 * DAY).await.unwrap();
    assert_eq!(host.state.host.store.get("bob").unwrap().unwrap().published, wyvrn_did_host::store::Published::Yes);
    server::check_staleness(&host.state, registered + 374 * DAY).await.unwrap();
    assert_eq!(host.state.host.store.get("bob").unwrap().unwrap().published, wyvrn_did_host::store::Published::Stale);
    assert_eq!(fetch(&host, "bob/did.jsonl").await.status(), 200);

    // Anything from Bob is an answer: he is there, and the name is his as before.
    assert_eq!(state_of_bob().await, "published");
    assert!(host.state.host.store.get("bob").unwrap().unwrap().asked_at.is_none());
}

/// Fetching as a resolver would, with the network left out: `https://id.example/…`
/// is answered by the host under test, unless a prepared answer stands in for it.
struct FromHost<'a> {
    host: &'a Running,
    instead: std::collections::HashMap<String, Option<Vec<u8>>>,
    asked: std::sync::Mutex<Vec<String>>,
}

impl<'a> FromHost<'a> {
    fn new(host: &'a Running) -> Self {
        Self { host, instead: Default::default(), asked: Default::default() }
    }

    fn with(mut self, url: &str, body: Option<&str>) -> Self {
        self.instead.insert(url.to_string(), body.map(|text| text.as_bytes().to_vec()));
        self
    }
}

impl Fetch for FromHost<'_> {
    async fn get(&self, url: &str) -> Result<Option<Vec<u8>>, String> {
        self.asked.lock().unwrap().push(url.to_string());
        if let Some(prepared) = self.instead.get(url) {
            return Ok(prepared.clone());
        }
        let path = url.strip_prefix(&format!("https://{DOMAIN}/")).ok_or_else(|| format!("{url} is not at the host"))?;
        let response = fetch(self.host, path).await;
        match response.status().as_u16() {
            200 => Ok(Some(response.bytes().await.unwrap().to_vec())),
            404 => Ok(None),
            other => Err(format!("{url}: {other}")),
        }
    }
}

async fn profile_whois(holder: &Holder, name: &str) -> Value {
    let did = holder.did();
    let mut presentation = json!({
        "@context": ["https://www.w3.org/ns/credentials/v2", "https://wyvrn.app/ns/profile/v1"],
        "type": ["VerifiablePresentation"],
        "holder": did,
        "verifiableCredential": [{
            "@context": ["https://www.w3.org/ns/credentials/v2", "https://wyvrn.app/ns/profile/v1"],
            "type": ["VerifiableCredential", "ProfileCredential"],
            "issuer": did,
            "credentialSubject": {
                "id": did, "type": "Person", "name": name, "description": "Writes about gardens.",
                "image": { "type": "ImageObject", "contentUrl": "data:image/webp;base64,UklGRg", "encodingFormat": "image/webp", "width": 256, "height": 256 },
            },
        }],
    });
    let mut key = holder.auth.clone();
    key.id = format!("{did}#key-1");
    let proof = DataIntegrityProof::sign(&presentation, &key, SignOptions::new().with_proof_purpose("authentication")).await.unwrap();
    presentation["proof"] = serde_json::to_value(proof).unwrap();
    presentation
}

async fn publish_whois(holder: &Holder, presentation: &Value) {
    let attachment = json!([signed_attachment("whois", "application/vp", presentation.to_string().as_bytes(), &holder.auth)]);
    let answer = holder.ask("publish-whois", json!({ "name": holder.name }), Some(attachment)).await;
    assert_eq!(kind(&answer), "status", "{answer}");
}

#[tokio::test]
async fn someone_given_the_short_name_reaches_the_verified_did_and_its_profile() {
    let host = start_host(&[]).await;
    let mut alice = Holder::new(&host, "alice");
    alice.register().await;
    publish_whois(&alice, &profile_whois(&alice, "Alice").await).await;
    let web = "did:web:id.example:alice";

    // Bob is given the short name. He ends up knowing Alice by the did:webvh, with
    // its history verified, and reads who she says she is -- having sent her nothing.
    let fetcher = FromHost::new(&host);
    let resolved = wyvrn_hosted_did::resolve(web, &fetcher).await.unwrap();
    assert_eq!(resolved.did, alice.did());
    assert_eq!(resolved.did_web.as_deref(), Some(web));
    assert_eq!(resolved.standing, Standing::Verified);
    let profile = wyvrn_hosted_did::whois_of(&resolved, &fetcher).await.unwrap().expect("a profile");
    assert_eq!(profile.name.as_deref(), Some("Alice"));
    assert!(profile.image.unwrap().is_inline());
    // Three files, all at the host the name is at: nobody else learns Bob looked.
    assert_eq!(
        *fetcher.asked.lock().unwrap(),
        ["https://id.example/alice/did.json", "https://id.example/alice/did.jsonl", "https://id.example/alice/whois.vp"]
    );

    // Given the did:webvh itself, the same party, and the short name it goes by.
    let direct = wyvrn_hosted_did::resolve(&alice.did(), &FromHost::new(&host)).await.unwrap();
    assert_eq!((direct.did.as_str(), direct.did_web.as_deref(), direct.standing), (alice.did().as_str(), Some(web), Standing::Verified));

    // A message from the did:peer:4 her document names is hers: it names the short
    // name back, and holds a key of the document. Someone else's doesn't.
    let peer_document = |agent: &Agent, also: &[&str]| {
        let mut document = agent.identity().did_document(&agent.did(), "https://example.invalid/");
        document["alsoKnownAs"] = json!(also);
        document
    };
    assert!(wyvrn_hosted_did::verify::peer_is_holder(&peer_document(&alice.agent, &[web]), web, &resolved.document));
    let mallory = Agent::new(Identity::generate().unwrap()).unwrap();
    assert!(!wyvrn_hosted_did::verify::peer_is_holder(&peer_document(&mallory, &[web]), web, &resolved.document));
}

#[tokio::test]
async fn a_host_cannot_pass_off_another_history_under_the_short_name() {
    let host = start_host(&[]).await;
    let mut alice = Holder::new(&host, "alice");
    alice.register().await;
    let web = "did:web:id.example:alice";
    let real_log = fetch(&host, "alice/did.jsonl").await.text().await.unwrap();
    let real_web = fetch(&host, "alice/did.json").await.text().await.unwrap();

    // A log altered after it was signed: the did:webvh doesn't verify, so all that
    // is left is the did:web, on the host's word and marked as such.
    let altered = FromHost::new(&host).with("https://id.example/alice/did.jsonl", Some(&real_log.replace("#key-2", "#key-9")));
    let resolved = wyvrn_hosted_did::resolve(web, &altered).await.unwrap();
    assert_eq!((resolved.did.as_str(), resolved.standing), (web, Standing::HostsWord));
    // Asked for by its did:webvh, there is nothing to fall back to.
    assert!(wyvrn_hosted_did::resolve(&alice.did(), &altered).await.is_err());

    // The log gone: the same.
    let missing = FromHost::new(&host).with("https://id.example/alice/did.jsonl", None);
    assert_eq!(wyvrn_hosted_did::resolve(web, &missing).await.unwrap().standing, Standing::HostsWord);

    // Mallory's own, perfectly valid did:webvh, which the host's did:web for
    // "alice" is made to point at. It is looked at, as a did:webvh named from
    // anywhere is, and not accepted: it doesn't name this did:web back, as no
    // did:webvh that isn't this one's holder's will.
    let mut mallory = Holder::new(&host, "mallory");
    mallory.register().await;
    let pointing_away = real_web.replace(&alice.did(), &mallory.did());
    let elsewhere = FromHost::new(&host).with("https://id.example/alice/did.json", Some(&pointing_away));
    let resolved = wyvrn_hosted_did::resolve(web, &elsewhere).await.unwrap();
    assert_eq!((resolved.did.as_str(), resolved.standing), (web, Standing::HostsWord));

    // A document served for the name that is some other DID's is not taken at all.
    let not_its_own = FromHost::new(&host).with("https://id.example/alice/did.json", Some(&real_web.replace(web, "did:web:id.example:alicia")));
    assert!(wyvrn_hosted_did::resolve(web, &not_its_own).await.is_err());

    // A whois that isn't hers: signed by Mallory for Mallory, served at Alice's.
    publish_whois(&mallory, &profile_whois(&mallory, "Alice").await).await;
    let mallorys = fetch(&host, "mallory/whois.vp").await.text().await.unwrap();
    let swapped = FromHost::new(&host).with("https://id.example/alice/whois.vp", Some(&mallorys));
    let resolved = wyvrn_hosted_did::resolve(web, &swapped).await.unwrap();
    assert_eq!(resolved.standing, Standing::Verified);
    assert!(wyvrn_hosted_did::whois_of(&resolved, &swapped).await.is_err());
}

#[tokio::test]
async fn a_name_given_to_someone_else_leads_to_a_different_did() {
    let admin = Agent::new(Identity::generate().unwrap()).unwrap();
    let host = start_host(&[admin.did()]).await;
    let web = "did:web:id.example:alice";
    let mut first = Holder::new(&host, "alice");
    first.register().await;
    let before = wyvrn_hosted_did::resolve(web, &FromHost::new(&host)).await.unwrap();

    let free = json!({ "type": format!("{P}free"), "body": { "name": "alice" }, "return_route": "all" });
    admin.send(&host.did, &free).await.unwrap().unwrap();
    assert!(wyvrn_hosted_did::resolve(web, &FromHost::new(&host)).await.is_err());

    let mut second = Holder::new(&host, "alice");
    second.register().await;
    let after = wyvrn_hosted_did::resolve(web, &FromHost::new(&host)).await.unwrap();
    // The short name is the same; who it names is not, and that shows: an app that
    // kept the did:webvh it first resolved sees a different one.
    assert_eq!(before.did_web, after.did_web);
    assert_eq!((before.standing, after.standing), (Standing::Verified, Standing::Verified));
    assert_ne!(before.did, after.did);
    assert_eq!(after.did, second.did());
}

#[tokio::test]
async fn any_of_a_holders_devices_acts_for_it_whatever_else_its_did_lists() {
    let host = start_host(&[]).await;
    // Alice has a phone and a tablet. Her document lists both; each writes to the
    // host as itself, from a DID that lists only its own key.
    let tablet = Agent::new(Identity::generate().unwrap()).unwrap();
    let mut alice = Holder::new(&host, "alice");
    alice.other_devices = vec![key_agreement_of(tablet.identity())];
    alice.register().await;
    let phone = std::mem::replace(&mut alice.agent, tablet);

    // The tablet: not the device that registered, and its DID has one key where the
    // document has two. It is hers all the same.
    assert_eq!(alice.ask("status-request", json!({}), None).await["body"]["yours"], true);
    publish_whois(&alice, &profile_whois(&alice, "Alice").await).await;

    // The phone again, from a DID that also lists a watch the document doesn't have
    // yet: a key more than the document is no bar either, and it is this device
    // that adds it.
    let watch = key_agreement_of(&Identity::generate().unwrap());
    let phone_identity = Identity::from_json(&phone.identity().to_json().unwrap()).unwrap();
    alice.agent = agent_listing(phone_identity, &[alice.other_devices[0].clone(), watch.clone()]);
    assert_eq!(alice.ask("status-request", json!({}), None).await["body"]["yours"], true);
    let before = alice.version_id();
    alice.other_devices.push(watch);
    next_second().await;
    alice.log.update_document(alice.document(json!([])), &alice.update).await.unwrap();
    assert_eq!(kind(&alice.publish(1, Some(&before)).await), "published");
    let web: Value = fetch(&host, "alice/did.json").await.json().await.unwrap();
    assert_eq!(web["keyAgreement"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn a_device_taken_out_of_a_did_cannot_take_a_second_name() {
    let host = start_host(&[]).await;
    let tablet = Agent::new(Identity::generate().unwrap()).unwrap();
    let tablet_key = key_agreement_of(tablet.identity());
    let mut alice = Holder::new(&host, "alice");
    alice.other_devices = vec![tablet_key.clone()];
    alice.register().await;

    // The tablet is one of hers, and has said so: it asked after her DID by name.
    let as_tablet = |message_type: &'static str, body: Value| {
        let (tablet, to) = (&tablet, host.did.clone());
        async move {
            let message = json!({ "type": format!("{P}{message_type}"), "body": body, "return_route": "all" });
            tablet.send(&to, &message).await.unwrap().expect("an answer").message
        }
    };
    assert_eq!(as_tablet("status-request", json!({ "name": "alice" })).await["body"]["yours"], true);

    // She removes it. It is no longer her DID's...
    let before = alice.version_id();
    alice.other_devices.clear();
    next_second().await;
    alice.log.update_document(alice.document(json!([])), &alice.update).await.unwrap();
    assert_eq!(kind(&alice.publish(1, Some(&before)).await), "published");
    assert_eq!(as_tablet("status-request", json!({ "name": "alice" })).await["body"]["yours"], false);
    // ...and its key is not thereby free to claim another name: it was hers.
    refused_with(&as_tablet("reserve", json!({ "name": "alice-again" })).await, "e.p.msg.key-in-use");
    // Nor can someone else's document list it.
    let mut carol = Holder::new(&host, "carol");
    carol.other_devices = vec![tablet_key];
    carol.reserve().await;
    carol.write_first_entry().await;
    refused_with(&carol.publish(0, None).await, "e.p.msg.key-in-use");
}

#[tokio::test]
async fn listing_someone_elses_key_neither_makes_one_them_nor_keeps_them_out() {
    let host = start_host(&[]).await;
    let mut alice = Holder::new(&host, "alice");
    let alices_key = alice.key_agreement();

    // Mallory puts Alice's public key in her own document, before Alice has a name.
    // It is only a public key: listing it shows nothing, and doesn't use up Alice's.
    let mut mallory = Holder::new(&host, "mallory");
    mallory.other_devices = vec![alices_key.clone()];
    mallory.register().await;
    alice.register().await;

    // A DID made up to list Alice's key beside Mallory's own is still Mallory:
    // what she wrote with is her own key, and that is not in Alice's document.
    let mut eve = Holder::new(&host, "alice");
    eve.agent = agent_listing(Identity::generate().unwrap(), &[alices_key.clone()]);
    assert_eq!(eve.ask("status-request", json!({ "name": "alice" }), None).await["body"]["yours"], false);
    let whois = profile_whois(&alice, "Not Alice").await;
    let attachment = json!([signed_attachment("whois", "application/vp", whois.to_string().as_bytes(), &alice.auth)]);
    refused_with(&eve.ask("publish-whois", json!({ "name": "alice" }), Some(attachment)).await, "e.p.msg.not-holder");

    // The same trick doesn't take a name someone is in the middle of claiming.
    let mut bob = Holder::new(&host, "bob");
    assert_eq!(kind(&bob.reserve().await), "reservation");
    let mut thief = Holder::new(&host, "bob");
    thief.agent = agent_listing(Identity::generate().unwrap(), &[bob.key_agreement()]);
    refused_with(&thief.reserve().await, "e.p.msg.name-taken");
    thief.other_devices = vec![bob.key_agreement()];
    thief.write_first_entry().await;
    refused_with(&thief.publish(0, None).await, "e.p.msg.not-held");
    bob.write_first_entry().await;
    assert_eq!(kind(&bob.publish(0, None).await), "published");

    // Once Alice has shown the key is hers, nobody else's document may carry it.
    let mut late = Holder::new(&host, "late");
    late.other_devices = vec![alices_key];
    late.reserve().await;
    late.write_first_entry().await;
    refused_with(&late.publish(0, None).await, "e.p.msg.key-in-use");
}

#[tokio::test]
async fn a_message_nobody_authenticated_is_turned_away() {
    let host = start_host(&[]).await;
    let alice = Holder::new(&host, "alice");
    let reserve = json!({ "type": format!("{P}reserve"), "body": { "name": "alice" }, "return_route": "all" });
    let post = |packed: Vec<u8>| {
        let url = host.url.clone();
        async move { reqwest::Client::new().post(url).body(packed).send().await.unwrap() }
    };

    // Encrypted to the host but from nobody (anoncrypt): refused outright, with
    // nothing held and nothing said in DIDComm, there being nobody to say it to.
    let anonymous = alice.agent.messaging().pack(&reserve, &host.did, None).await.unwrap().message;
    let response = post(anonymous).await;
    assert_eq!(response.status(), 403);
    assert!(response.text().await.unwrap().contains("authcrypt"));
    assert!(host.state.host.store.get("alice").unwrap().is_none());

    // The same message, authenticated, is taken.
    let authenticated = alice.agent.messaging().pack(&reserve, &host.did, Some(&alice.agent.did())).await.unwrap().message;
    assert_eq!(post(authenticated).await.status(), 200);
    assert!(host.state.host.store.get("alice").unwrap().is_some());

    // Not DIDComm at all.
    assert_eq!(post(b"hello".to_vec()).await.status(), 400);
}

#[tokio::test]
async fn a_message_a_mediator_carried_is_known_by_its_senders_key_like_any_other() {
    let host = start_host(&[]).await;
    let service = Arc::new(server::HostService(host.state.clone()));
    let mut alice = Holder::new(&host, "alice");
    // What a mediator hands over is the sender's sealed message, as sent. Given
    // that, the host does what it would have done had it been posted.
    async fn seal(agent: &Agent, to: &str, message: Value) -> Vec<u8> {
        agent.messaging().pack(&message, to, Some(&agent.did())).await.unwrap().message
    }

    let reserve = json!({ "type": format!("{P}reserve"), "body": { "name": "alice" } });
    wyvrn_service::mediator::deliver(&service, &seal(&alice.agent, &host.did, reserve).await).await;
    assert!(!host.state.host.store.get("alice").unwrap().expect("held").hold_keys.is_empty());

    alice.write_first_entry().await;
    let publish = json!({ "type": format!("{P}publish"), "body": { "name": "alice" }, "attachments": [alice.log_attachment(0)] });
    wyvrn_service::mediator::deliver(&service, &seal(&alice.agent, &host.did, publish).await).await;
    assert_eq!(fetch(&host, "alice/did.jsonl").await.status(), 200);
    assert_eq!(host.state.host.store.get("alice").unwrap().unwrap().did, Some(alice.did()));

    // And it is the key that counts there too: the same name can't be had again
    // by the same holder, and someone else carried in gets no further than posted.
    let again = json!({ "type": format!("{P}reserve"), "body": { "name": "alice-two" } });
    wyvrn_service::mediator::deliver(&service, &seal(&alice.agent, &host.did, again).await).await;
    assert!(host.state.host.store.get("alice-two").unwrap().is_none());

    // One nobody authenticated is dropped, carried as posted.
    let bob = Holder::new(&host, "bob");
    let anonymous = bob.agent.messaging().pack(&json!({ "type": format!("{P}reserve"), "body": { "name": "bob" } }), &host.did, None).await.unwrap().message;
    wyvrn_service::mediator::deliver(&service, &anonymous).await;
    assert!(host.state.host.store.get("bob").unwrap().is_none());
    // Nor does rubbish do anything.
    wyvrn_service::mediator::deliver(&service, b"not a message").await;
}

#[tokio::test]
async fn an_app_registers_and_is_read_back_with_the_functions_it_will_use() {
    use didcomm_resolver_peer::peer4;
    use wyvrn_hosted_did::{read, write};
    let host = start_host(&[]).await;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    // Keys as an app holds them: JWKs.
    let jwk = || {
        let key = SigningKey::from_bytes(&rand_seed());
        json!({ "kty": "OKP", "crv": "Ed25519", "x": b64.encode(key.verifying_key().to_bytes()), "d": b64.encode(key.to_bytes()) }).to_string()
    };
    let (authentication, phone_update, tablet_update) = (jwk(), jwk(), jwk());
    let web = "did:web:id.example:alice";

    // The identity the app writes from: a did:peer:4 that names its short name.
    let identity = Identity::generate().unwrap();
    let key_agreement = key_agreement_of(&identity);
    let peer = peer4::encode(json!({
        "@context": ["https://www.w3.org/ns/did/v1", "https://w3id.org/security/multikey/v1"],
        "alsoKnownAs": [web],
        "verificationMethod": [
            { "id": "#key-1", "type": "Multikey", "publicKeyMultibase": write::public_multikey(&authentication).unwrap() },
            { "id": "#key-2", "type": "Multikey", "publicKeyMultibase": key_agreement },
        ],
        "authentication": ["#key-1"],
        "keyAgreement": ["#key-2"],
    }))
    .unwrap();
    let agent = Agent::with_did(identity, &peer);
    let ask = |message_type: &'static str, body: Value, attachment: Option<Value>| {
        let (agent, to) = (&agent, host.did.clone());
        async move {
            let mut message = json!({ "type": format!("{P}{message_type}"), "body": body, "return_route": "all" });
            if let Some(attachment) = attachment {
                message["attachments"] = json!([attachment]);
            }
            agent.send(&to, &message).await.unwrap().expect("an answer").message
        }
    };
    let attach = |id: &str, media_type: &str, bytes: &[u8]| {
        json!({ "id": id, "media_type": media_type, "data": { "base64": b64.encode(bytes), "jws": write::sign_attachment(bytes, &authentication).unwrap() } })
    };

    // Reserve, write the first entry, publish.
    assert_eq!(kind(&ask("reserve", json!({ "name": "alice" }), None).await), "reservation");
    let document = |did: &str| {
        json!({
            "@context": ["https://www.w3.org/ns/did/v1", "https://w3id.org/security/multikey/v1"],
            "id": did,
            "alsoKnownAs": [web, peer],
            "verificationMethod": [
                { "id": format!("{did}#key-1"), "type": "Multikey", "controller": did, "publicKeyMultibase": write::public_multikey(&authentication).unwrap() },
                { "id": format!("{did}#key-2"), "type": "Multikey", "controller": did, "publicKeyMultibase": key_agreement },
            ],
            "authentication": [format!("{did}#key-1")],
            // What it signs as, it also asserts as: its profile, and a linked website.
            "assertionMethod": [format!("{did}#key-1")],
            "keyAgreement": [format!("{did}#key-2")],
        })
    };
    let phone_key = write::public_multikey(&phone_update).unwrap();
    let first = write::create(&document("did:webvh:{SCID}:id.example:alice"), &phone_update, std::slice::from_ref(&phone_key)).await.unwrap();
    assert!(first.did.starts_with("did:webvh:Qm") && first.did.ends_with(":id.example:alice"), "{}", first.did);
    let published = ask("publish", json!({ "name": "alice" }), Some(attach("log", "text/jsonl", first.entry.as_bytes()))).await;
    assert_eq!(kind(&published), "published", "{published}");
    assert_eq!(published["body"]["did"], first.did);

    // A second device gets an update key of its own; then it, not the phone, writes.
    next_second().await;
    let both = [phone_key.clone(), write::public_multikey(&tablet_update).unwrap()];
    let second = write::append(&first.did, &first.log, &document(&first.did), &phone_update, Some(&both)).await.unwrap();
    let answer = ask("publish", json!({ "name": "alice", "after": first.version_id }), Some(attach("log", "text/jsonl", second.entry.as_bytes()))).await;
    assert_eq!(kind(&answer), "published", "{answer}");
    next_second().await;
    let third = write::append(&first.did, &second.log, &document(&first.did), &tablet_update, None).await.unwrap();
    let answer = ask("publish", json!({ "name": "alice", "after": second.version_id }), Some(attach("log", "text/jsonl", third.entry.as_bytes()))).await;
    assert_eq!(kind(&answer), "published", "{answer}");
    // A key the log never allowed can't continue it, even on the holder's own device.
    next_second().await;
    assert!(write::append(&first.did, &third.log, &document(&first.did), &jwk(), None).await.is_err());

    // Who she is.
    let profile: write::ProfileFields = serde_json::from_value(json!({
        "kind": "Person", "name": "Alice", "description": "Writes about gardens.",
        "image": { "contentUrl": "data:image/webp;base64,UklGRg", "encodingFormat": "image/webp", "width": 256, "height": 256 },
        "background": { "contentUrl": "https://alice.example/bg.webp", "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08" },
    }))
    .unwrap();
    let whois = write::whois(&first.did, &profile, &authentication, &format!("{}#key-1", first.did), "2026-10-11T00:00:00Z").await.unwrap();
    let answer = ask("publish-whois", json!({ "name": "alice" }), Some(attach("whois", "application/vp", whois.to_string().as_bytes()))).await;
    assert_eq!(answer["body"]["whois"], true, "{answer}");

    // Read back as the app reads: asking with what it has, fetching what it is told to.
    let mut files = read::Files::new();
    let mut fetched = Vec::new();
    let read = loop {
        match read::read(web, &files).await {
            read::Step::Need(url) => {
                let path = url.strip_prefix("https://id.example/").expect("only its own host is asked").to_string();
                let response = fetch(&host, &path).await;
                files.insert(url.clone(), (response.status() == 200).then_some(response.text().await.unwrap()));
                fetched.push(path);
            }
            read::Step::Done(read) => break read,
            read::Step::Failed(why) => panic!("{why}"),
        }
    };
    assert_eq!(fetched, ["alice/did.json", "alice/did.jsonl", "alice/whois.vp"]);
    assert_eq!(read.resolved.did, first.did);
    assert_eq!(read.resolved.standing, Standing::Verified);
    assert_eq!(read.peer.as_deref(), Some(peer.as_str()));
    let profile = read.profile.expect("a profile");
    assert_eq!((profile.name.as_deref(), profile.kind.as_deref()), (Some("Alice"), Some("Person")));
    assert!(profile.image.unwrap().is_inline());
    assert_eq!(profile.background.unwrap().url, "https://alice.example/bg.webp");

    // The same DID string with a letter of its document changed is no DID at all,
    // and one that is real but someone else's is not hers.
    assert!(wyvrn_hosted_did::verify::peer4_document(&peer).is_some());
    let mut bent = peer.clone();
    bent.pop();
    bent.push(if peer.ends_with('1') { '2' } else { '1' });
    assert!(wyvrn_hosted_did::verify::peer4_document(&bent).is_none());
    let other = Agent::new(Identity::generate().unwrap()).unwrap().did();
    let theirs = wyvrn_hosted_did::verify::peer4_document(&other).expect("a real did:peer:4 decodes");
    assert!(!wyvrn_hosted_did::verify::peer_is_holder(&theirs, web, &read.resolved.document));
}

fn rand_seed() -> [u8; 32] {
    let key = Secret::generate_ed25519(None, None);
    key.get_private_bytes()[..32].try_into().unwrap()
}

#[tokio::test]
async fn a_service_holds_an_address_and_is_found_by_it_from_its_own_domain() {
    use wyvrn_service::address::{build_agent, DidMethod, ProfileConfig};
    use wyvrn_service::hosted_address::{Address, Holding, Kept};

    let admin = Agent::new(Identity::generate().unwrap()).unwrap();
    let host = start_host(&[admin.did()]).await;

    // A service that is did:web:news.example, saying who it is. Here it writes to
    // the host from a did:peer:4 of the same keys, which the host can look up
    // without news.example being anywhere.
    let identity = Identity::generate().unwrap();
    let asking = Arc::new(Agent::new(Identity::from_json(&identity.to_json().unwrap()).unwrap()).unwrap());
    let (_, published) = build_agent(identity, "https://news.example/", DidMethod::Web).unwrap();
    let profile = ProfileConfig { name: Some("Wyvrn News".into()), description: Some("What is new.".into()), ..Default::default() };
    let published = published.unwrap().with_profile(&profile).unwrap();

    let address = Address::parse(&format!("did:web:{DOMAIN}:official:news")).unwrap();
    let ask = {
        let (asking, host_did) = (asking.clone(), host.did.clone());
        Box::new(move |_to: String, mut message: Value| {
            let (asking, host_did) = (asking.clone(), host_did.clone());
            Box::pin(async move {
                message["return_route"] = json!("all");
                // (The host under test is a did:peer:4 at a local port, not the did:web the address names.)
                Ok(asking.send(&host_did, &message).await?.ok_or_else(|| anyhow::anyhow!("no answer"))?.message)
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Value>> + Send>>
        })
    };
    let holding = Holding::new(address, Kept::scratch().unwrap(), published.clone(), ask).unwrap();

    // The namespace is reserved: not the service's until an admin says so.
    let refused = holding.hold(1_800_000_000).await.unwrap_err();
    assert!(format!("{refused:#}").contains("set aside"), "{refused:#}");
    assert_eq!(published.document_now()["alsoKnownAs"], Value::Null);
    let assign = json!({ "type": format!("{P}assign"), "body": { "name": "official:news", "holder": asking.did() }, "return_route": "all" });
    admin.send(&host.did, &assign).await.unwrap().expect("an answer");

    let did = holding.hold(1_800_000_000).await.unwrap();
    assert!(did.starts_with("did:webvh:") && did.ends_with(&format!(":{DOMAIN}:official:news")), "{did}");
    // Its own document names the address from then on, and its website's link covers both.
    assert_eq!(published.document_now()["alsoKnownAs"], json!([did]));
    let linked = published.did_configuration(1_800_000_000).await.unwrap();
    assert_eq!(linked["linked_dids"].as_array().unwrap().len(), 2);

    // Looked up by its own domain, it is the party the address names: its history
    // verified, and its profile the one published at the host, signed as the address.
    let web_document = published.document_now().to_string();
    let files = || FromHost::new(&host).with("https://news.example/.well-known/did.json", Some(&web_document));
    let found = wyvrn_hosted_did::resolve("did:web:news.example", &files()).await.unwrap();
    assert_eq!((found.did.as_str(), found.standing, found.did_web.as_deref()), (did.as_str(), Standing::Verified, Some("did:web:news.example")));
    assert_eq!(found.document["service"][0]["serviceEndpoint"]["uri"], published.document["service"][0]["serviceEndpoint"]["uri"]);
    let who = wyvrn_hosted_did::whois_of(&found, &files()).await.unwrap().unwrap();
    assert_eq!((who.name.as_deref(), who.description.as_deref()), (Some("Wyvrn News"), Some("What is new.")));
    // ...as it is looked up by the address itself.
    let by_address = wyvrn_hosted_did::resolve(&format!("did:web:{DOMAIN}:official:news"), &files()).await.unwrap();
    assert_eq!((by_address.did.as_str(), by_address.standing), (did.as_str(), Standing::Verified));

    // A did:web that only claims the address is not it: the address doesn't name it back.
    let claiming = json!({ "id": "did:web:impostor.example", "alsoKnownAs": [did] }).to_string();
    let impostor = FromHost::new(&host).with("https://impostor.example/.well-known/did.json", Some(&claiming));
    let alone = wyvrn_hosted_did::resolve("did:web:impostor.example", &impostor).await.unwrap();
    assert_eq!((alone.did.as_str(), alone.standing), ("did:web:impostor.example", Standing::HostsWord));

    // Held again, nothing is written: the log already says what the document says.
    let before = fetch(&host, "official/news/did.jsonl").await.text().await.unwrap();
    assert_eq!(holding.hold(1_800_000_100).await.unwrap(), did);
    assert_eq!(fetch(&host, "official/news/did.jsonl").await.text().await.unwrap(), before);

    // An admin changes what the service is called: the address says so too.
    published.changed(&wyvrn_service::profile::Changed { name: Some("Wyvrn Updates".into()), ..Default::default() });
    holding.publish_whois(&did, 1_800_000_200).await.unwrap();
    let who = wyvrn_hosted_did::whois_of(&found, &files()).await.unwrap().unwrap();
    assert_eq!(who.name.as_deref(), Some("Wyvrn Updates"));
}
