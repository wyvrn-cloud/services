//! The protocol: what each `did-hosting/1.0` message does.

use affinidi_data_integrity::{DataIntegrityProof, VerifyOptions};
use base64::Engine;
use didcomm_agent::Received;
use didcomm_core::resolver::DIDResolver;
use serde_json::{json, Value};

use crate::config::HostConfig;
use crate::keys;
use crate::log::{self, Checked};
use crate::names::{Name, Reserved};
use crate::store::{Published, Record, Store};

pub const PIURI: &str = "https://wyvrn.app/did-hosting/1.0";

const RESERVE: &str = "https://wyvrn.app/did-hosting/1.0/reserve";
const RESERVATION: &str = "https://wyvrn.app/did-hosting/1.0/reservation";
const PUBLISH: &str = "https://wyvrn.app/did-hosting/1.0/publish";
const PUBLISHED: &str = "https://wyvrn.app/did-hosting/1.0/published";
const PUBLISH_WHOIS: &str = "https://wyvrn.app/did-hosting/1.0/publish-whois";
const STATUS_REQUEST: &str = "https://wyvrn.app/did-hosting/1.0/status-request";
const STATUS: &str = "https://wyvrn.app/did-hosting/1.0/status";
const ASSIGN: &str = "https://wyvrn.app/did-hosting/1.0/assign";
const FREE: &str = "https://wyvrn.app/did-hosting/1.0/free";

/// How a name stands, as the protocol says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Free,
    Reserved,
    Held,
    Published,
    Stale,
    Deactivated,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Reserved => "reserved",
            Self::Held => "held",
            Self::Published => "published",
            Self::Stale => "stale",
            Self::Deactivated => "deactivated",
        }
    }
}

/// A refusal: the `problem-report` to answer with.
struct Refusal {
    code: &'static str,
    comment: String,
    args: Vec<String>,
}

impl Refusal {
    fn new(code: &'static str, comment: impl Into<String>) -> Self {
        Self { code, comment: comment.into(), args: Vec::new() }
    }

    fn with(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }
}

type Answer = Result<(&'static str, Value), Refusal>;

pub struct Host {
    pub store: Store,
    pub config: HostConfig,
    reserved: Reserved,
    resolver: Box<dyn DIDResolver>,
}

/// Whether whoever authenticated with one of `sender` is the party that holds
/// `theirs`: every key the sender's DID lists is one of theirs.
///
/// "Every", because all that is known of the sender is its DID: the message was
/// authenticated with *one* of that DID's key-agreement keys, and which one is not
/// passed on. If all of them are the party's, whichever it was is. (A DID made up
/// to list someone else's key beside its own does not pass: its own key is not
/// theirs.)
fn is_party(sender: &[String], theirs: &[String]) -> bool {
    !sender.is_empty() && sender.iter().all(|key| theirs.contains(key))
}

impl Host {
    pub fn new(store: Store, config: HostConfig, resolver: Box<dyn DIDResolver>) -> Self {
        let reserved = Reserved::new(&config.reserved_names, &config.reserved_namespaces);
        Self { store, config, reserved, resolver }
    }

    /// A DID's key-agreement keys, or none if it can't be resolved.
    async fn keys_of(&self, did: &str) -> Vec<String> {
        match self.resolver.resolve(did).await {
            Ok(document) => keys::keys_for(&document, "keyAgreement"),
            Err(error) => {
                tracing::debug!(%did, %error, "couldn't resolve a DID to see its keys");
                Vec::new()
            }
        }
    }

    async fn is_admin(&self, sender: &str, sender_keys: &[String]) -> bool {
        for admin in &self.config.admins {
            if admin == sender || is_party(sender_keys, &self.keys_of(admin).await) {
                return true;
            }
        }
        false
    }

    pub fn state_of(&self, name: &Name, record: Option<&Record>, now: i64) -> State {
        match record.map(|r| r.published) {
            Some(Published::Yes) => return State::Published,
            Some(Published::Stale) => return State::Stale,
            Some(Published::Deactivated) => return State::Deactivated,
            Some(Published::No) | None => {}
        }
        if record.is_some_and(|r| !r.hold_keys.is_empty() && r.hold_expires > now) {
            State::Held
        } else if record.is_some_and(|r| r.reserved) || self.reserved.covers(name) {
            State::Reserved
        } else {
            State::Free
        }
    }

    fn name(&self, body: &Value) -> Result<Name, Refusal> {
        let text = body["name"].as_str().unwrap_or_default();
        Name::parse(text, self.config.max_labels).ok_or_else(|| {
            Refusal::new(
                "e.p.msg.name-invalid",
                format!(
                    "A name is up to {} labels joined by ':', each 3 to 32 of a-z, 0-9 and '-', with no hyphen first, last or doubled.",
                    self.config.max_labels
                ),
            )
        })
    }

    /// Acts on one message from an authenticated sender. `None` for a message that
    /// isn't this protocol's.
    pub async fn handle(&self, received: &Received, now: i64) -> anyhow::Result<Option<Value>> {
        let Some(sender) = received.sender.as_deref() else {
            return Ok(None);
        };
        let sender_keys = self.keys_of(sender).await;
        // Anything from a holder says it is still there.
        if let Some(name) = self.store.name_held_by(&sender_keys)? {
            self.store.heard(&name, now)?;
        }
        let body = &received.message["body"];
        let answer = match received.message_type() {
            RESERVE => self.reserve(&sender_keys, body, now).await?,
            PUBLISH => self.publish(&sender_keys, &received.message, now).await?,
            PUBLISH_WHOIS => self.publish_whois(&sender_keys, &received.message, now)?,
            STATUS_REQUEST => self.status(sender, &sender_keys, body, now).await?,
            ASSIGN => self.assign(sender, &sender_keys, body, now).await?,
            FREE => self.free(sender, &sender_keys, body, now).await?,
            _ => return Ok(None),
        };
        Ok(Some(match answer {
            Ok((message_type, body)) => received.reply(message_type, body),
            Err(refusal) => {
                tracing::debug!(code = refusal.code, comment = %refusal.comment, "refused");
                let args: Vec<&str> = refusal.args.iter().map(String::as_str).collect();
                received.problem_report(refusal.code, &refusal.comment, &args)
            }
        }))
    }

    async fn reserve(&self, sender_keys: &[String], body: &Value, now: i64) -> anyhow::Result<Answer> {
        let name = match self.name(body) {
            Ok(name) => name,
            Err(refusal) => return Ok(Err(refusal)),
        };
        if sender_keys.is_empty() {
            return Ok(Err(Refusal::new("e.p.msg.name-invalid", "Your DID has no key-agreement key this host can see.")));
        }
        let key = name.as_did_path();
        let record = self.store.get(&key)?;
        match self.state_of(&name, record.as_ref(), now) {
            State::Free => {}
            State::Held => {
                if !record.as_ref().is_some_and(|r| is_party(sender_keys, &r.hold_keys)) {
                    return Ok(Err(Refusal::new("e.p.msg.name-taken", "Someone else is claiming that name right now.")));
                }
            }
            State::Reserved => {
                let assigned = match record.as_ref().and_then(|r| r.assigned.as_deref()) {
                    Some(did) => self.keys_of(did).await,
                    None => Vec::new(),
                };
                if !is_party(sender_keys, &assigned) {
                    return Ok(Err(Refusal::new("e.p.msg.name-reserved", "That name is set aside.")));
                }
            }
            State::Published | State::Stale | State::Deactivated => {
                return Ok(Err(Refusal::new("e.p.msg.name-taken", "That name is taken.")));
            }
        }
        // One name per holder: a key already in a document here can't claim another.
        for sender_key in sender_keys {
            if self.store.name_with_key(sender_key, &key)?.is_some() {
                return Ok(Err(Refusal::new("e.p.msg.key-in-use", "You already have a DID hosted here under another name.")));
            }
        }
        let expires = now + self.config.hold_seconds;
        self.store.hold(&key, sender_keys, expires)?;
        Ok(Ok((
            RESERVATION,
            json!({
                "name": key,
                "host": self.config.domain,
                "did_web": log::did_web_for(&self.config.domain, &name),
                "expires_time": expires,
            }),
        )))
    }

    async fn publish(&self, sender_keys: &[String], message: &Value, now: i64) -> anyhow::Result<Answer> {
        let name = match self.name(&message["body"]) {
            Ok(name) => name,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let key = name.as_did_path();
        let Some((bytes, jws)) = attachment(message, "log") else {
            return Ok(Err(Refusal::new("e.p.msg.log-invalid", "There is no \"log\" attachment with base64 data.")));
        };
        let Ok(added) = String::from_utf8(bytes.clone()) else {
            return Ok(Err(Refusal::new("e.p.msg.log-invalid", "The log is not text.")));
        };
        let added = added.trim_end_matches('\n');

        let record = self.store.get(&key)?;
        let state = self.state_of(&name, record.as_ref(), now);
        let hosted = record.as_ref().filter(|_| matches!(state, State::Published | State::Stale));
        let full_log = match (state, hosted) {
            (State::Published | State::Stale, Some(hosted)) => {
                let last = hosted.version_id.clone().unwrap_or_default();
                if message["body"]["after"].as_str() != Some(last.as_str()) {
                    return Ok(Err(Refusal::new("e.p.msg.conflict", "The log has moved on: its last entry is {1}.").with(last)));
                }
                format!("{}\n{added}", hosted.log.as_deref().unwrap_or_default())
            }
            (State::Deactivated, _) => return Ok(Err(Refusal::new("e.p.msg.name-taken", "The DID under that name has been ended."))),
            (State::Held, _) if record.as_ref().is_some_and(|r| is_party(sender_keys, &r.hold_keys)) => added.to_string(),
            _ => return Ok(Err(Refusal::new("e.p.msg.not-held", "Reserve the name first: it isn't held for you."))),
        };
        if full_log.len() > self.config.max_log_bytes {
            return Ok(Err(Refusal::new("e.p.msg.too-large", "The log is over this host's limit of {1} bytes.").with(self.config.max_log_bytes.to_string())));
        }

        // 1-4: a valid log, of this name's DID, whose document names its did:web.
        let checked = match log::check(&self.config.domain, &name, &full_log).await {
            Ok(checked) => checked,
            Err(why) => return Ok(Err(Refusal::new("e.p.msg.log-invalid", why))),
        };
        // 2: what is hosted is continued, never replaced.
        if let Some(hosted) = hosted {
            if hosted.did.as_deref() != Some(checked.did.as_str()) {
                return Ok(Err(Refusal::new("e.p.msg.log-invalid", "That is not a continuation of the log hosted here.")));
            }
        } else if self.config.require_portable && !checked.portable {
            return Ok(Err(Refusal::new("e.p.msg.log-invalid", "This host asks for \"portable\": true in a DID's first entry, so that it can be moved later.")));
        }
        // 5: the sender holds the document's keys. On an update the sender may be
        // one the entry itself has just added or removed, so either document's will do:
        // that the entry is validly signed is what makes it the holder's.
        let mut holder_keys = checked.key_agreement();
        let mut allowed = holder_keys.clone();
        if let Some(hosted) = hosted {
            allowed.extend(hosted.holder_keys.iter().cloned());
        }
        if !is_party(sender_keys, &allowed) {
            return Ok(Err(Refusal::new("e.p.msg.not-holder", "Your DID's key-agreement keys are not all in that document.")));
        }
        if !jws.is_some_and(|jws| keys::signed_by_one_of(&bytes, jws, &checked.authentication())) {
            return Ok(Err(Refusal::new("e.p.msg.log-invalid", "The attachment is not signed (jws) by an authentication key of the document.")));
        }
        // 6: its keys are nobody else's.
        let all_keys = keys::all_keys(&checked.document);
        for document_key in &all_keys {
            if self.store.name_with_key(document_key, &key)?.is_some() {
                return Ok(Err(Refusal::new("e.p.msg.key-in-use", "A key of that document is in a DID hosted here under another name.")));
            }
        }

        holder_keys.sort();
        let published = if checked.deactivated { Published::Deactivated } else { Published::Yes };
        self.store.publish(
            &key,
            published,
            &checked.did,
            &checked.version_id,
            &full_log,
            &serde_json::to_string(&with_whois_service(&checked, hosted.is_some_and(|h| h.whois.is_some()), &self.config.domain, &name))?,
            &holder_keys,
            &all_keys,
            now,
        )?;
        tracing::info!(name = %key, did = %checked.did, version = %checked.version_id, "published");
        Ok(Ok((
            PUBLISHED,
            json!({ "name": key, "did": checked.did, "did_web": checked.did_web, "version_id": checked.version_id }),
        )))
    }

    fn publish_whois(&self, sender_keys: &[String], message: &Value, now: i64) -> anyhow::Result<Answer> {
        let name = match self.name(&message["body"]) {
            Ok(name) => name,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let key = name.as_did_path();
        let record = self.store.get(&key)?;
        let Some(record) = record.filter(|r| matches!(r.published, Published::Yes | Published::Stale) && is_party(sender_keys, &r.holder_keys)) else {
            return Ok(Err(Refusal::new("e.p.msg.not-holder", "You don't hold a DID under that name.")));
        };
        let did = record.did.clone().unwrap_or_default();
        let whois = match attachment(message, "whois") {
            None => None,
            Some((bytes, _)) => {
                if bytes.len() > self.config.max_whois_bytes {
                    return Ok(Err(Refusal::new("e.p.msg.too-large", "The whois is over this host's limit of {1} bytes.").with(self.config.max_whois_bytes.to_string())));
                }
                let document: Value = record.log.as_deref().and_then(|log| last_document(log)).unwrap_or_default();
                if let Err(why) = check_whois(&bytes, &did, &document) {
                    return Ok(Err(Refusal::new("e.p.msg.whois-invalid", why)));
                }
                Some(bytes)
            }
        };
        // The did:web says where its whois is; with none, it says nothing of one.
        let web_document = record.web_document.as_deref().and_then(|text| serde_json::from_str::<Value>(text).ok()).unwrap_or_default();
        let web_document = set_whois_service(web_document, whois.is_some(), &self.config.domain, &name);
        self.store.set_whois(&key, whois.as_deref(), &serde_json::to_string(&web_document)?)?;
        let record = self.store.get(&key)?;
        Ok(Ok((STATUS, self.status_body(&name, record.as_ref(), true, now))))
    }

    fn status_body(&self, name: &Name, record: Option<&Record>, yours: bool, now: i64) -> Value {
        let state = self.state_of(name, record, now);
        let mut body = json!({ "name": name.as_did_path(), "state": state.as_str(), "yours": yours });
        if let Some(record) = record.filter(|_| matches!(state, State::Published | State::Stale | State::Deactivated)) {
            body["did"] = json!(record.did);
            body["version_id"] = json!(record.version_id);
            body["whois"] = json!(record.whois.is_some());
        }
        body
    }

    async fn status(&self, sender: &str, sender_keys: &[String], body: &Value, now: i64) -> anyhow::Result<Answer> {
        let name = match body.get("name") {
            Some(_) => match self.name(body) {
                Ok(name) => name,
                Err(refusal) => return Ok(Err(refusal)),
            },
            // None asked for: the name the sender holds, if any.
            None => match self.store.name_held_by(sender_keys)?.and_then(|name| Name::parse(&name, usize::MAX)) {
                Some(name) => name,
                None => return Ok(Ok((STATUS, json!({ "state": "free", "yours": false })))),
            },
        };
        let record = self.store.get(&name.as_did_path())?;
        let yours = match self.state_of(&name, record.as_ref(), now) {
            State::Published | State::Stale | State::Deactivated => record.as_ref().is_some_and(|r| is_party(sender_keys, &r.holder_keys)),
            State::Held => record.as_ref().is_some_and(|r| is_party(sender_keys, &r.hold_keys)),
            State::Reserved => match record.as_ref().and_then(|r| r.assigned.as_deref()) {
                Some(did) => did == sender || is_party(sender_keys, &self.keys_of(did).await),
                None => false,
            },
            State::Free => false,
        };
        Ok(Ok((STATUS, self.status_body(&name, record.as_ref(), yours, now))))
    }

    async fn assign(&self, sender: &str, sender_keys: &[String], body: &Value, now: i64) -> anyhow::Result<Answer> {
        if !self.is_admin(sender, sender_keys).await {
            return Ok(Err(Refusal::new("e.p.msg.not-admin", "Only this host's admins decide who may claim a name.")));
        }
        let text = body["name"].as_str().unwrap_or_default();
        let (text, namespace) = match text.strip_suffix(":*") {
            Some(namespace) => (namespace, true),
            None => (text, false),
        };
        let Some(name) = Name::parse(text, self.config.max_labels) else {
            return Ok(Err(Refusal::new("e.p.msg.name-invalid", "That is not a name, or a namespace followed by ':*'.")));
        };
        let holder = body["holder"].as_str();
        if namespace {
            // A namespace is reserved whole by configuration: nothing here can add one
            // while the host runs, and one can't be given to a holder.
            let refusal = if holder.is_some() { "A namespace can't be assigned to a holder, only its names can." } else { "A namespace is reserved in this host's configuration (reserved_namespaces)." };
            return Ok(Err(Refusal::new("e.p.msg.name-invalid", refusal)));
        }
        let key = name.as_did_path();
        let record = self.store.get(&key)?;
        if matches!(self.state_of(&name, record.as_ref(), now), State::Published | State::Stale | State::Deactivated) {
            return Ok(Err(Refusal::new("e.p.msg.name-taken", "A DID is hosted under that name. Free it first.")));
        }
        self.store.assign(&key, holder)?;
        tracing::info!(name = %key, holder = holder.unwrap_or("nobody"), by = %sender, "assigned");
        let record = self.store.get(&key)?;
        Ok(Ok((STATUS, self.status_body(&name, record.as_ref(), false, now))))
    }

    async fn free(&self, sender: &str, sender_keys: &[String], body: &Value, now: i64) -> anyhow::Result<Answer> {
        if !self.is_admin(sender, sender_keys).await {
            return Ok(Err(Refusal::new("e.p.msg.not-admin", "Only this host's admins can free a name.")));
        }
        let name = match self.name(body) {
            Ok(name) => name,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let key = name.as_did_path();
        let was = self.store.get(&key)?.and_then(|r| r.did);
        self.store.free(&key)?;
        tracing::info!(name = %key, did = was.as_deref().unwrap_or("-"), by = %sender, why = body["comment"].as_str().unwrap_or("-"), "freed");
        Ok(Ok((STATUS, self.status_body(&name, None, false, now))))
    }
}

/// An attachment's bytes, and its `jws` if it has one.
fn attachment<'a>(message: &'a Value, id: &str) -> Option<(Vec<u8>, Option<&'a Value>)> {
    let attachment = message["attachments"].as_array()?.iter().find(|a| a["id"] == id)?;
    let text = attachment["data"]["base64"].as_str()?;
    // base64url as DIDComm says, and the ordinary alphabet as some senders write it.
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(text.trim_end_matches('=')))
        .ok()?;
    Some((bytes, attachment["data"].get("jws")))
}

/// The document a stored log's last entry gives. (The log was checked when stored.)
fn last_document(log: &str) -> Option<Value> {
    let last: Value = serde_json::from_str(log.lines().last()?).ok()?;
    last.get("state").cloned()
}

/// The `#whois` service of a `did:web` document: present when there is a whois,
/// as `did:webvh` asks of a parallel `did:web` (for the `did:webvh` itself the
/// service is implicit).
fn set_whois_service(mut web_document: Value, has_whois: bool, host: &str, name: &Name) -> Value {
    let did_web = log::did_web_for(host, name);
    let id = format!("{did_web}#whois");
    let mut services: Vec<Value> = web_document["service"].as_array().cloned().unwrap_or_default();
    services.retain(|service| service["id"] != id.as_str() && service["id"] != "#whois");
    if has_whois {
        services.push(json!({
            "id": id,
            "type": "LinkedVerifiablePresentation",
            "serviceEndpoint": format!("https://{}/{}/whois.vp", host.replace("%3A", ":"), name.as_url_path()),
        }));
    }
    if services.is_empty() {
        if let Some(object) = web_document.as_object_mut() {
            object.remove("service");
        }
    } else {
        web_document["service"] = Value::Array(services);
    }
    web_document
}

fn with_whois_service(checked: &Checked, has_whois: bool, host: &str, name: &Name) -> Value {
    set_whois_service(checked.web_document.clone(), has_whois, host, name)
}

/// A whois is a presentation held by the DID, signed by one of its authentication
/// keys, and about nothing but the DID.
fn check_whois(bytes: &[u8], did: &str, document: &Value) -> Result<(), String> {
    let presentation: Value = serde_json::from_slice(bytes).map_err(|_| "The whois is not JSON.".to_string())?;
    let holder = presentation["holder"].as_str().or(presentation["holder"]["id"].as_str());
    if holder != Some(did) {
        return Err(format!("The presentation's holder is not {did}."));
    }
    for credential in presentation["verifiableCredential"].as_array().into_iter().flatten() {
        let subject = &credential["credentialSubject"];
        let subjects: Vec<&Value> = match subject {
            Value::Array(many) => many.iter().collect(),
            one => vec![one],
        };
        if !subjects.iter().all(|s| s["id"].as_str() == Some(did)) {
            return Err(format!("Every credential in a whois has to be about {did}."));
        }
    }

    let proofs: Vec<Value> = match &presentation["proof"] {
        Value::Array(many) => many.clone(),
        Value::Object(_) => vec![presentation["proof"].clone()],
        _ => return Err("The presentation is not signed.".to_string()),
    };
    let mut unsigned = presentation.clone();
    unsigned.as_object_mut().map(|object| object.remove("proof"));
    let methods = document["verificationMethod"].as_array().cloned().unwrap_or_default();
    let authentication = keys::keys_for(document, "authentication");
    for proof in proofs {
        let Ok(proof) = serde_json::from_value::<DataIntegrityProof>(proof) else { continue };
        // The key the proof names, if it is one of the DID's authentication keys.
        let method_id = proof.verification_method.clone();
        let fragment = method_id.rsplit('#').next().unwrap_or_default().to_string();
        let key = methods
            .iter()
            .find(|m| m["id"].as_str().is_some_and(|id| id == method_id || id.rsplit('#').next() == Some(fragment.as_str())))
            .and_then(|m| m["publicKeyMultibase"].as_str())
            .filter(|key| authentication.iter().any(|a| a == key))
            .and_then(keys::ed25519_from_multikey);
        let Some(key) = key else { continue };
        if proof.verify_with_public_key(&unsigned, &key, VerifyOptions::default()).is_ok() {
            return Ok(());
        }
    }
    Err("The presentation's proof doesn't verify against an authentication key of the DID.".to_string())
}
