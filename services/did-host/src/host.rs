//! The protocol: what each `did-hosting/1.0` message does.

use base64::Engine;
use didcomm_agent::Received;
use didcomm_core::resolver::DIDResolver;
use serde_json::{json, Value};

use crate::config::HostConfig;
use wyvrn_hosted_did::{keys, whois};
use crate::log;
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

/// Who sent a message: the DID it is from, and the one key of that DID's it was
/// authenticated with.
pub struct Sender {
    pub did: String,
    key: String,
}

impl Sender {
    /// Whether the sender is the party that `theirs` are the keys of: the key it
    /// wrote with is one of them.
    ///
    /// That one key and nothing else. What its DID lists besides doesn't matter
    /// either way -- a device more or a device fewer than a document has is still
    /// the same holder, and someone else's key listed beside one's own makes
    /// nobody them.
    pub fn holds(&self, theirs: &[String]) -> bool {
        theirs.contains(&self.key)
    }
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

    /// Who a message is from. `kid` is the id of the key it was authenticated with;
    /// `None` if that id is not a key-agreement key of the DID it claims to be of.
    async fn sender(&self, did: &str, kid: &str) -> Option<Sender> {
        let document = match self.resolver.resolve(did).await {
            Ok(document) => document,
            Err(error) => {
                tracing::debug!(%did, %error, "couldn't resolve a sender's DID");
                return None;
            }
        };
        let keys = keys::keys_for(&document, "keyAgreement");
        let fragment = kid.rsplit('#').next().unwrap_or_default();
        let key = document["verificationMethod"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|method| method["id"].as_str().is_some_and(|id| id == kid || id.rsplit('#').next() == Some(fragment)))
            .and_then(|method| method["publicKeyMultibase"].as_str())
            .filter(|key| keys.iter().any(|k| k == key))?;
        Some(Sender { did: did.to_string(), key: key.to_string() })
    }

    async fn is_admin(&self, sender: &Sender) -> bool {
        for admin in &self.config.admins {
            if *admin == sender.did || sender.holds(&self.keys_of(admin).await) {
                return true;
            }
        }
        false
    }

    /// Whether one of `keys` has been shown to be held by the holder of a DID
    /// under a name other than `except`: one name to a holder.
    ///
    /// "Shown to be held", not merely listed: a document can list any public key,
    /// and a key someone else's document lists must not stop its real owner from
    /// having a name. A key counts as a name's once its owner has acted as that
    /// name's holder with it: published under it, or asked after it by name. Both
    /// take the document listing the key (the holder's word) and a message about
    /// that name authenticated with it (the key owner's).
    fn in_use(&self, keys: &[String], except: &str) -> anyhow::Result<bool> {
        for key in keys {
            if self.store.name_proven_with(key, except)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Notes that a holder has been heard from: by a key it has shown to be its
    /// own, that its document still lists.
    pub async fn note_sender(&self, received: &Received, kid: &str, now: i64) -> anyhow::Result<()> {
        let Some(did) = received.sender.as_deref() else { return Ok(()) };
        let Some(sender) = self.sender(did, kid).await else { return Ok(()) };
        for name in self.store.names_proven_with(&sender.key)? {
            if self.store.get(&name)?.is_some_and(|record| sender.holds(&record.holder_keys)) {
                self.store.heard(&name, now)?;
            }
        }
        Ok(())
    }

    /// The sender has acted as `name`'s holder with the key it wrote with.
    fn shown_to_hold(&self, sender: &Sender, name: &str) -> anyhow::Result<()> {
        self.store.prove(name, &sender.key)
    }

    /// The name whose DID the sender is the holder of, as its document stands.
    fn name_of(&self, sender: &Sender) -> anyhow::Result<Option<String>> {
        for name in self.store.names_with_key(&sender.key)? {
            if self.store.get(&name)?.is_some_and(|record| record.published != Published::No && sender.holds(&record.holder_keys)) {
                return Ok(Some(name));
            }
        }
        Ok(None)
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

    /// Acts on one message, authenticated by its sender with the key `kid` names.
    /// `None` for a message that isn't this protocol's, or whose sender's DID
    /// can't be read to see what that key is.
    pub async fn handle(&self, received: &Received, kid: &str, now: i64) -> anyhow::Result<Option<Value>> {
        let Some(did) = received.sender.as_deref() else {
            return Ok(None);
        };
        let Some(sender) = self.sender(did, kid).await else {
            return Ok(None);
        };
        let sender = &sender;
        let body = &received.message["body"];
        let answer = match received.message_type() {
            RESERVE => self.reserve(sender, body, now).await?,
            PUBLISH => self.publish(sender, &received.message, now).await?,
            PUBLISH_WHOIS => self.publish_whois(sender, &received.message, now)?,
            STATUS_REQUEST => self.status(sender, body, now).await?,
            ASSIGN => self.assign(sender, body, now).await?,
            FREE => self.free(sender, body, now).await?,
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

    async fn reserve(&self, sender: &Sender, body: &Value, now: i64) -> anyhow::Result<Answer> {
        let name = match self.name(body) {
            Ok(name) => name,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let key = name.as_did_path();
        let record = self.store.get(&key)?;
        match self.state_of(&name, record.as_ref(), now) {
            State::Free => {}
            State::Held => {
                if !record.as_ref().is_some_and(|r| sender.holds(&r.hold_keys)) {
                    return Ok(Err(Refusal::new("e.p.msg.name-taken", "Someone else is claiming that name right now.")));
                }
            }
            State::Reserved => {
                let assigned = match record.as_ref().and_then(|r| r.assigned.as_deref()) {
                    Some(did) => self.keys_of(did).await,
                    None => Vec::new(),
                };
                if !sender.holds(&assigned) {
                    return Ok(Err(Refusal::new("e.p.msg.name-reserved", "That name is set aside.")));
                }
            }
            State::Published | State::Stale | State::Deactivated => {
                return Ok(Err(Refusal::new("e.p.msg.name-taken", "That name is taken.")));
            }
        }
        // One name per holder: a key already in a document here can't claim another.
        if self.in_use(std::slice::from_ref(&sender.key), &key)? {
            return Ok(Err(Refusal::new("e.p.msg.key-in-use", "You already have a DID hosted here under another name.")));
        }
        let expires = now + self.config.hold_seconds;
        self.store.hold(&key, std::slice::from_ref(&sender.key), expires)?;
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

    async fn publish(&self, sender: &Sender, message: &Value, now: i64) -> anyhow::Result<Answer> {
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
            (State::Held, _) if record.as_ref().is_some_and(|r| sender.holds(&r.hold_keys)) => added.to_string(),
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
        // 5: the sender is one of the document's own: the key it wrote with is a
        // key-agreement key of it. On an update that may be a device the entry itself
        // has just added or removed, so either document's will do -- that the entry
        // is validly signed is what makes it the holder's.
        let mut holder_keys = checked.key_agreement();
        let mut allowed = holder_keys.clone();
        if let Some(hosted) = hosted {
            allowed.extend(hosted.holder_keys.iter().cloned());
        }
        if !sender.holds(&allowed) {
            return Ok(Err(Refusal::new("e.p.msg.not-holder", "The key you wrote with is not a key-agreement key of that document.")));
        }
        let authentication = checked.authentication();
        let Some(signed_with) = jws.and_then(|jws| keys::signer_among(&bytes, jws, &authentication)).cloned() else {
            return Ok(Err(Refusal::new("e.p.msg.log-invalid", "The attachment is not signed (jws) by an authentication key of the document.")));
        };
        // 6: its keys are nobody else's -- none that another name's holder has shown
        // it holds, in that name's document now or at any time before. Remembering
        // every version is what stops a key taken out of one document (a device
        // removed) from being free to take a second name.
        let mut all_keys: Vec<String> =
            full_log.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()).flat_map(|entry| keys::all_keys(&entry["state"])).collect();
        all_keys.sort();
        all_keys.dedup();
        let mut to_check = keys::all_keys(&checked.document);
        to_check.push(sender.key.clone());
        to_check.push(signed_with.clone());
        if self.in_use(&to_check, &key)? {
            return Ok(Err(Refusal::new("e.p.msg.key-in-use", "A key of that document belongs to a DID hosted here under another name.")));
        }

        holder_keys.sort();
        let published = if checked.deactivated { Published::Deactivated } else { Published::Yes };
        self.store.publish(
            &key,
            published,
            &checked.did,
            &checked.version_id,
            &full_log,
            &serde_json::to_string(&with_whois_service(checked.web_document.clone(), &self.config.domain, &name))?,
            &holder_keys,
            &all_keys,
            now,
        )?;
        // The keys this was written with and signed with are now known to be the
        // holder's.
        self.shown_to_hold(sender, &key)?;
        self.store.prove(&key, &signed_with)?;
        tracing::info!(name = %key, did = %checked.did, version = %checked.version_id, "published");
        Ok(Ok((
            PUBLISHED,
            json!({ "name": key, "did": checked.did, "did_web": checked.did_web, "version_id": checked.version_id }),
        )))
    }

    fn publish_whois(&self, sender: &Sender, message: &Value, now: i64) -> anyhow::Result<Answer> {
        let name = match self.name(&message["body"]) {
            Ok(name) => name,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let key = name.as_did_path();
        let record = self.store.get(&key)?;
        let Some(record) = record.filter(|r| matches!(r.published, Published::Yes | Published::Stale) && sender.holds(&r.holder_keys)) else {
            return Ok(Err(Refusal::new("e.p.msg.not-holder", "You don't hold a DID under that name.")));
        };
        let did = record.did.clone().unwrap_or_default();
        let whois = match attachment(message, "whois") {
            None => None,
            Some((bytes, jws)) => {
                // Signed like everything uploaded here: by an authentication key of
                // the document, over the bytes as sent.
                let authentication = keys::keys_for(&record.log.as_deref().and_then(last_document).unwrap_or_default(), "authentication");
                let Some(signed_with) = jws.and_then(|jws| keys::signer_among(&bytes, jws, &authentication)).cloned() else {
                    return Ok(Err(Refusal::new("e.p.msg.whois-invalid", "The attachment is not signed (jws) by an authentication key of the DID.")));
                };
                self.store.prove(&key, &signed_with)?;
                if bytes.len() > self.config.max_whois_bytes {
                    return Ok(Err(Refusal::new("e.p.msg.too-large", "The whois is over this host's limit of {1} bytes.").with(self.config.max_whois_bytes.to_string())));
                }
                let document: Value = record.log.as_deref().and_then(|log| last_document(log)).unwrap_or_default();
                if let Err(why) = whois::check(&bytes, &did, &document) {
                    return Ok(Err(Refusal::new("e.p.msg.whois-invalid", why)));
                }
                Some(bytes)
            }
        };
        // The did:web says where its whois is, published or not: did:webvh has it so.
        let web_document = record.web_document.as_deref().and_then(|text| serde_json::from_str::<Value>(text).ok()).unwrap_or_default();
        let web_document = with_whois_service(web_document, &self.config.domain, &name);
        self.store.set_whois(&key, whois.as_deref(), &serde_json::to_string(&web_document)?)?;
        self.shown_to_hold(sender, &key)?;
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

    async fn status(&self, sender: &Sender, body: &Value, now: i64) -> anyhow::Result<Answer> {
        let name = match body.get("name") {
            Some(_) => match self.name(body) {
                Ok(name) => name,
                Err(refusal) => return Ok(Err(refusal)),
            },
            // None asked for: the name the sender holds, if any.
            None => match self.name_of(sender)?.and_then(|name| Name::parse(&name, usize::MAX)) {
                Some(name) => name,
                None => return Ok(Ok((STATUS, json!({ "state": "free", "yours": false })))),
            },
        };
        let asked_by_name = body.get("name").is_some();
        let record = self.store.get(&name.as_did_path())?;
        let yours = match self.state_of(&name, record.as_ref(), now) {
            State::Published | State::Stale | State::Deactivated => record.as_ref().is_some_and(|r| sender.holds(&r.holder_keys)),
            State::Held => record.as_ref().is_some_and(|r| sender.holds(&r.hold_keys)),
            State::Reserved => match record.as_ref().and_then(|r| r.assigned.as_deref()) {
                Some(did) => did == sender.did || sender.holds(&self.keys_of(did).await),
                None => false,
            },
            State::Free => false,
        };
        // Asking after one's own DID by its name is a device saying it is that
        // holder's: how one that never publishes (the second phone) comes to count.
        if yours && asked_by_name && record.as_ref().is_some_and(|r| r.published != Published::No) {
            self.shown_to_hold(sender, &name.as_did_path())?;
        }
        Ok(Ok((STATUS, self.status_body(&name, record.as_ref(), yours, now))))
    }

    async fn assign(&self, sender: &Sender, body: &Value, now: i64) -> anyhow::Result<Answer> {
        if !self.is_admin(sender).await {
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
        tracing::info!(name = %key, holder = holder.unwrap_or("nobody"), by = %sender.did, "assigned");
        let record = self.store.get(&key)?;
        Ok(Ok((STATUS, self.status_body(&name, record.as_ref(), false, now))))
    }

    async fn free(&self, sender: &Sender, body: &Value, now: i64) -> anyhow::Result<Answer> {
        if !self.is_admin(sender).await {
            return Ok(Err(Refusal::new("e.p.msg.not-admin", "Only this host's admins can free a name.")));
        }
        let name = match self.name(body) {
            Ok(name) => name,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let key = name.as_did_path();
        let was = self.store.get(&key)?.and_then(|r| r.did);
        self.store.free(&key)?;
        tracing::info!(name = %key, did = was.as_deref().unwrap_or("-"), by = %sender.did, why = body["comment"].as_str().unwrap_or("-"), "freed");
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
/// The parallel `did:web` document with its `#whois` service, which `did:webvh`
/// has it carry whether or not a whois is published (a reader finds out by asking).
fn with_whois_service(web_document: Value, host: &str, name: &Name) -> Value {
    let mut web_document = web_document;
    let did_web = log::did_web_for(host, name);
    let id = format!("{did_web}#whois");
    let mut services: Vec<Value> = web_document["service"].as_array().cloned().unwrap_or_default();
    services.retain(|service| service["id"] != id.as_str() && service["id"] != "#whois");
    services.push(json!({
        "id": id,
        "type": "LinkedVerifiablePresentation",
        "serviceEndpoint": format!("https://{}/{}/whois.vp", host.replace("%3A", ":"), name.as_url_path()),
    }));
    web_document["service"] = Value::Array(services);
    web_document
}
