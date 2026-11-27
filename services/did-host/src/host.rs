//! The protocol: what each `did-hosting/1.0` message does.

use base64::Engine;
use didcomm_agent::Received;
use didcomm_core::resolver::DIDResolver;
use serde_json::{json, Value};

use crate::config::HostConfig;
use wyvrn_hosted_did::{keys, whois};
use crate::log;
use crate::names::{Name, Reserved};
use crate::store::{Pending, Published, Record, Store};
use crate::witness::{self, Witness};

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
const PENDING: &str = "https://wyvrn.app/did-hosting/1.0/pending";
const COSIGN: &str = "https://wyvrn.app/did-hosting/1.0/cosign";
const WITHDRAW: &str = "https://wyvrn.app/did-hosting/1.0/withdraw";

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

/// Whether `did` discloses `kind`'s protocol in `kind`'s role, asked as
/// discover-features/2.0 asks. Only an agent that answers at once can say: a
/// service, posted to with `return_route`. One that can't be asked is not taken
/// for that kind.
async fn is_of_kind(agent: &didcomm_agent::Agent, did: &str, kind: &crate::config::Kind) -> bool {
    let queries = json!({
        "type": "https://didcomm.org/discover-features/2.0/queries",
        "body": { "queries": [{ "feature-type": "protocol", "match": kind.protocol }] },
    });
    match agent.request(did, &queries).await {
        Ok(answer) => answer.message["body"]["disclosures"].as_array().into_iter().flatten().any(|disclosure| {
            disclosure["id"] == kind.protocol.as_str() && disclosure["roles"].as_array().is_some_and(|roles| roles.iter().any(|role| role == kind.role.as_str()))
        }),
        Err(error) => {
            tracing::debug!(%did, %error, "couldn't ask what an agent is");
            false
        }
    }
}

pub struct Host {
    pub store: Store,
    pub config: HostConfig,
    reserved: Reserved,
    resolver: Box<dyn DIDResolver>,
    /// The host's witness, where it witnesses the logs it hosts.
    pub witness: Option<Witness>,
    /// What is to be sent to holders, for the server to send.
    outbox: std::sync::Mutex<Vec<Notice>>,
}

/// A message for a holder: `pending`, to the hosted DID it is about.
#[derive(Debug, Clone)]
pub struct Notice {
    /// Whom to send it to, in order: the hosted `did:webvh` (as its witnessed log
    /// has it), then the `did:peer:4` that log names, should that fail.
    pub to: Vec<String>,
    pub message: Value,
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
        Self { store, config, reserved, resolver, witness: None, outbox: Default::default() }
    }

    /// The host witnessing every log it hosts, as `witness` (did-hosting/1.0, "The
    /// host as a witness").
    pub fn with_witness(mut self, witness: Witness) -> Self {
        self.witness = Some(witness);
        self
    }

    /// What is to be sent to holders now, taken.
    pub fn take_notices(&self) -> Vec<Notice> {
        std::mem::take(&mut *self.outbox.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
    }

    /// Tells `record`'s holder about the entry pending under it: at its hosted DID,
    /// as the log witnessed so far has it -- so every device it had before the
    /// change, not only the ones the entry keeps.
    fn tell(&self, record: &Record, version_id: &str, state: &str, publish_time: Option<i64>, entry: Option<&str>) {
        let Some(did) = record.did.clone() else { return };
        let mut to = vec![did];
        let witnessed = record.log.as_deref().and_then(last_document);
        if let Some(peer) = witnessed.as_ref().and_then(|document| wyvrn_hosted_did::verify::also_known_as(document).into_iter().find(|aka| aka.starts_with("did:peer:")).map(str::to_string)) {
            to.push(peer);
        }
        let mut body = json!({ "name": record.name, "version_id": version_id, "state": state });
        if let Some(time) = publish_time {
            body["publish_time"] = json!(time);
        }
        if let Some(entry) = entry {
            body["entry"] = json!(entry);
        }
        let message = json!({ "type": PENDING, "body": body });
        self.outbox.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(Notice { to, message });
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

    /// Whether a message from `did`, authenticated with `kid`, is an admin's:
    /// what lets them into the host's admin pages (crate::admin).
    pub async fn is_admin_by(&self, did: &str, kid: &str) -> bool {
        match self.sender(did, kid).await {
            Some(sender) => self.is_admin(&sender).await,
            None => false,
        }
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

    /// Whether a free name may be claimed by `did`: in a namespace new names may be
    /// claimed in, and of the kind that namespace is held for, if it is held for
    /// one. A name an admin assigned isn't asked about: that is the admin's choice.
    async fn claimable_by(&self, name: &Name, did: &str, agent: &didcomm_agent::Agent) -> Result<(), Refusal> {
        let open = &self.config.open_namespaces;
        let namespace = (name.labels().len() > 1).then(|| name.labels()[0].as_str());
        if !open.is_empty() && !namespace.is_some_and(|namespace| open.iter().any(|label| label == namespace)) {
            let named = open.iter().map(|label| format!("{label}:")).collect::<Vec<_>>().join(", ");
            return Err(Refusal::new("e.p.msg.name-invalid", format!("New names here are in a namespace: {named}.")));
        }
        let Some(kind) = namespace.and_then(|namespace| self.config.kinds.get(namespace)) else { return Ok(()) };
        if is_of_kind(agent, did, kind).await {
            Ok(())
        } else {
            Err(Refusal::new("e.p.msg.wrong-kind", format!("Names in {}: are for agents that disclose {} as {}.", name.labels()[0], kind.protocol, kind.role)))
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
    ///
    /// `agent` is the host's own, which asks a sender what it is when a namespace
    /// is held for one kind of agent.
    pub async fn handle(&self, received: &Received, kid: &str, now: i64, agent: &didcomm_agent::Agent) -> anyhow::Result<Option<Value>> {
        let Some(did) = received.sender.as_deref() else {
            return Ok(None);
        };
        let Some(sender) = self.sender(did, kid).await else {
            return Ok(None);
        };
        let sender = &sender;
        let body = &received.message["body"];
        let answer = match received.message_type() {
            RESERVE => self.reserve(sender, body, now, agent).await?,
            PUBLISH => self.publish(sender, &received.message, now).await?,
            PUBLISH_WHOIS => self.publish_whois(sender, &received.message, now)?,
            STATUS_REQUEST => self.status(sender, body, now).await?,
            COSIGN => self.cosign(sender, &received.message, now).await?,
            WITHDRAW => self.withdraw(sender, &received.message, now)?,
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

    async fn reserve(&self, sender: &Sender, body: &Value, now: i64, agent: &didcomm_agent::Agent) -> anyhow::Result<Answer> {
        let name = match self.name(body) {
            Ok(name) => name,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let key = name.as_did_path();
        let record = self.store.get(&key)?;
        match self.state_of(&name, record.as_ref(), now) {
            State::Free => {
                if let Err(refusal) = self.claimable_by(&name, &sender.did, agent).await {
                    return Ok(Err(refusal));
                }
            }
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
        let mut reservation = json!({
            "name": key,
            "host": self.config.domain,
            "did_web": log::did_web_for(&self.config.domain, &name),
            "expires_time": expires,
        });
        if let Some(witness) = &self.witness {
            reservation["witness"] = json!(witness.did);
        }
        Ok(Ok((RESERVATION, reservation)))
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
        // An entry held back is not built on: it may yet be undone.
        let pending = match hosted {
            Some(_) => self.store.pending(&key)?,
            None => None,
        };
        let full_log = match (state, hosted) {
            (State::Published | State::Stale, Some(hosted)) => {
                let last = hosted.version_id.clone().unwrap_or_default();
                if let Some(pending) = pending.as_ref().filter(|pending| message["body"]["after"].as_str() == Some(pending.version_id.as_str())) {
                    return Ok(Err(Refusal::new("e.p.msg.pending", "{1} is held back, and may yet be undone: write after the last witnessed entry, or wait until it is published.").with(&pending.version_id)));
                }
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
        let newest: Value = serde_json::from_str(added.lines().last().unwrap_or_default()).unwrap_or_default();
        let newest_version = newest["versionId"].as_str().unwrap_or_default().to_string();

        // Witnessed as it would be if published now: what was witnessed before, and a
        // proof for the newest entry. (Nothing of it is served unless it is.)
        let proofs = match &self.witness {
            Some(witness) if !newest_version.is_empty() => Some(witness.proofs_with(self.store.witness_proofs(&key)?.as_deref().filter(|_| hosted.is_some()), &newest_version).await?),
            _ => None,
        };
        // 1-4: a valid log, of this name's DID, whose document names its did:web.
        let checked = match log::check(&self.config.domain, &name, &full_log, proofs.as_deref()).await {
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
        // A host that witnesses names itself the only witness of every log it hosts,
        // from its first entry, or the next entry of one published before it did. A
        // log already witnessed here changes that only as a change held back: how a
        // holder moving to another host takes it out.
        if let Some(witness) = &self.witness {
            let ours = witness.parameter();
            let was = hosted.and_then(|hosted| hosted.log.as_deref()).map(witness::in_force).unwrap_or_default();
            if witness::in_force(&full_log).witness != ours && was.witness != ours {
                return Ok(Err(Refusal::new(
                    "e.p.msg.log-invalid",
                    "This host witnesses the logs it hosts: the log's witness parameter names {1} as its only witness, threshold 1.",
                )
                .with(&witness.did)));
            }
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
        let entry = Pending {
            name: key.clone(),
            version_id: checked.version_id.clone(),
            after: hosted.and_then(|hosted| hosted.version_id.clone()).unwrap_or_default(),
            entries: added.to_string(),
            did: checked.did.clone(),
            log: full_log.clone(),
            web_document: serde_json::to_string(&with_whois_service(checked.web_document.clone(), &self.config.domain, &name))?,
            holder_keys,
            all_keys,
            proven: vec![sender.key.clone(), signed_with],
            deactivated: checked.deactivated,
            accepted_at: now,
            publish_at: now + witness::HOLD_SECONDS,
            soon_sent: false,
        };

        // Held back, at a host that witnesses: an entry that takes something away,
        // unless a recovery key the last witnessed entry lists co-signs it.
        if let (Some(_), Some(hosted)) = (&self.witness, hosted) {
            let witnessed = hosted.log.as_deref().unwrap_or_default();
            let co_signed = match message["body"].get("recovery_proof").filter(|proof| !proof.is_null()) {
                Some(proof) => match wyvrn_hosted_did::recovery::check_address_entry(proof, &last_document(witnessed).unwrap_or_default(), &entry.version_id) {
                    Ok(_) => true,
                    Err(why) => return Ok(Err(Refusal::new("e.p.msg.recovery-invalid", why))),
                },
                None => false,
            };
            let held_back = witness::held_back_because(witnessed, added).filter(|_| !co_signed);
            match (held_back, &pending) {
                // One pending entry a name: this one, too, would have to wait.
                (Some(_), Some(pending)) => {
                    return Ok(Err(Refusal::new(
                        "e.p.msg.pending",
                        "{1} is held back already, until {2} (seconds since the epoch): write after it once it is published, withdraw it, or co-sign this entry with a recovery key.",
                    )
                    .with(&pending.version_id)
                    .with(pending.publish_at.to_string())));
                }
                (Some(why), None) => {
                    self.store.hold_back(&entry)?;
                    tracing::info!(name = %key, version = %entry.version_id, why, "held back");
                    self.tell(hosted, &entry.version_id, "held", Some(entry.publish_at), Some(&entry.entries));
                    let mut body = self.published_body(&name, hosted);
                    body["pending"] = json!({ "version_id": entry.version_id, "accepted_time": entry.accepted_at, "publish_time": entry.publish_at });
                    return Ok(Ok((PUBLISHED, body)));
                }
                // Only adding, or co-signed: witnessed at once, and a pending entry it
                // follows the same witnessed one as is dropped.
                (None, Some(pending)) => {
                    tracing::info!(name = %key, dropped = %pending.version_id, by = %entry.version_id, "a pending entry replaced");
                    self.tell(hosted, &pending.version_id, "dropped", None, None);
                }
                (None, None) => {}
            }
        }

        let published = self.commit(&entry, proofs.as_deref(), now)?;
        Ok(Ok((PUBLISHED, self.published_body(&name, &published))))
    }

    /// Publishes `entry`, witnessed with `proofs` (where the host witnesses):
    /// `did-witness.json` first, then the log. Returns the name as it now stands.
    fn commit(&self, entry: &Pending, proofs: Option<&str>, now: i64) -> anyhow::Result<Record> {
        let published = if entry.deactivated { Published::Deactivated } else { Published::Yes };
        self.store.publish(&entry.name, published, &entry.did, &entry.version_id, &entry.log, &entry.web_document, &entry.holder_keys, &entry.all_keys, proofs, now)?;
        // The keys this was written with and signed with are now known to be the
        // holder's.
        for key in &entry.proven {
            self.store.prove(&entry.name, key)?;
        }
        tracing::info!(name = %entry.name, did = %entry.did, version = %entry.version_id, "published");
        self.store.get(&entry.name)?.ok_or_else(|| anyhow::anyhow!("{} was published and is not there", entry.name))
    }

    /// `published`'s body for `record` as it stands: its last witnessed entry.
    fn published_body(&self, name: &Name, record: &Record) -> Value {
        let mut body = json!({
            "name": name.as_did_path(),
            "did": record.did,
            "did_web": log::did_web_for(&self.config.domain, name),
            "version_id": record.version_id,
        });
        if let Some(witness) = &self.witness {
            body["witness"] = json!(witness.did);
        }
        body
    }

    /// Witnesses and publishes the pending entry of `name`, now.
    async fn publish_pending(&self, record: &Record, pending: &Pending, now: i64) -> anyhow::Result<Record> {
        let proofs = match &self.witness {
            Some(witness) => Some(witness.proofs_with(self.store.witness_proofs(&pending.name)?.as_deref(), &pending.version_id).await?),
            None => None,
        };
        let published = self.commit(pending, proofs.as_deref(), now)?;
        self.tell(record, &pending.version_id, "published", Some(now), None);
        Ok(published)
    }

    /// Publishes every pending entry whose day is up, and tells the holder of each
    /// that is to be published within the hour: what the host does once a minute,
    /// from what it keeps, so that a restart loses nothing.
    pub async fn publish_due(&self, now: i64) -> anyhow::Result<()> {
        for pending in self.store.all_pending()? {
            let Some(record) = self.store.get(&pending.name)? else {
                self.store.drop_pending(&pending.name)?;
                continue;
            };
            // What it followed has moved on (an admin's doing): it can't be published.
            if record.version_id.as_deref() != Some(pending.after.as_str()) {
                self.store.drop_pending(&pending.name)?;
                continue;
            }
            // Held back by a rule that no longer holds it (0.0.28's first host held a
            // recovery key back for being named anew): published now.
            let still_held = witness::held_back_because(record.log.as_deref().unwrap_or_default(), &pending.entries).is_some();
            if now >= pending.publish_at || !still_held {
                self.publish_pending(&record, &pending, now).await?;
            } else if now >= pending.publish_at - witness::SOON_SECONDS && !pending.soon_sent {
                self.store.told_soon(&pending.name)?;
                self.tell(&record, &pending.version_id, "soon", Some(pending.publish_at), Some(&pending.entries));
            }
        }
        Ok(())
    }

    /// The pending entry `body` names, under a name `sender` holds (by the last
    /// witnessed document's keys, or the pending one's).
    fn pending_named(&self, sender: &Sender, body: &Value) -> anyhow::Result<Result<(Name, Record, Pending), Refusal>> {
        let name = match self.name(body) {
            Ok(name) => name,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let Some(record) = self.store.get(&name.as_did_path())?.filter(|r| matches!(r.published, Published::Yes | Published::Stale)) else {
            return Ok(Err(Refusal::new("e.p.msg.not-holder", "You don't hold a DID under that name.")));
        };
        let pending = self.store.pending(&name.as_did_path())?;
        if !sender.holds(&record.holder_keys) && !pending.as_ref().is_some_and(|pending| sender.holds(&pending.holder_keys)) {
            return Ok(Err(Refusal::new("e.p.msg.not-holder", "You don't hold a DID under that name.")));
        }
        match pending {
            Some(pending) if body["version_id"].as_str() == Some(pending.version_id.as_str()) => Ok(Ok((name, record, pending))),
            Some(pending) => Ok(Err(Refusal::new("e.p.msg.pending", "That entry isn't pending: {1} is.").with(pending.version_id))),
            None => Ok(Err(Refusal::new("e.p.msg.pending", "No entry is pending under that name."))),
        }
    }

    /// `cosign`: the pending entry co-signed, and so witnessed and published at once.
    /// By a recovery key the last witnessed entry lists (`recovery_proof`), for any
    /// entry; or, for an entry held back only for giving the log its first recovery
    /// key, by a second trusted device: an attachment `cosign` whose bytes are the
    /// entry's `versionId`, signed (`jws`) by an update key of the last witnessed
    /// entry other than the one that wrote it. One device alone can't, or a thief
    /// holding one could give the address a recovery key of its own at once.
    async fn cosign(&self, sender: &Sender, message: &Value, now: i64) -> anyhow::Result<Answer> {
        let body = &message["body"];
        let (name, record, pending) = match self.pending_named(sender, body)? {
            Ok(found) => found,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let witnessed_log = record.log.as_deref().unwrap_or_default();
        if body.get("recovery_proof").is_some_and(|proof| !proof.is_null()) || attachment(message, "cosign").is_none() {
            let witnessed = last_document(witnessed_log).unwrap_or_default();
            if let Err(why) = wyvrn_hosted_did::recovery::check_address_entry(&body["recovery_proof"], &witnessed, &pending.version_id) {
                return Ok(Err(Refusal::new("e.p.msg.recovery-invalid", why)));
            }
        } else {
            let update_keys = witness::in_force(witnessed_log).update_keys;
            let Some(by) = attachment(message, "cosign")
                .filter(|(bytes, _)| bytes == pending.version_id.as_bytes())
                .and_then(|(bytes, jws)| jws.and_then(|jws| keys::signer_among(&bytes, jws, &update_keys)).cloned())
            else {
                return Ok(Err(Refusal::new(
                    "e.p.msg.cosign-invalid",
                    "A trusted device co-signs with an attachment \"cosign\" carrying the entry's versionId, signed (jws) by an update key of the last witnessed entry.",
                )));
            };
            if pending.proven.contains(&by) || witness::signers_of(&pending.entries).contains(&by) {
                return Ok(Err(Refusal::new("e.p.msg.cosign-invalid", "The device that wrote the entry can't co-sign it: another trusted device of the account has to.")));
            }
            if !witness::only_a_first_recovery_key(witnessed_log, &pending.entries) {
                let why = witness::held_back_because(witnessed_log, &pending.entries).unwrap_or("it takes something away");
                return Ok(Err(Refusal::new("e.p.msg.cosign-invalid", "A trusted device co-signs only an entry that gives the DID its first recovery key, and {1}: co-sign it with a recovery key.").with(why)));
            }
            tracing::info!(name = %name.as_did_path(), version = %pending.version_id, by = %by, "a first recovery key co-signed by a second trusted device");
        }
        let published = self.publish_pending(&record, &pending, now).await?;
        Ok(Ok((PUBLISHED, self.published_body(&name, &published))))
    }

    /// `withdraw`: the holder drops its own pending entry. Authenticated by a key of
    /// the last witnessed entry's `updateKeys`: an attachment `withdraw` whose bytes
    /// are the entry's `versionId`, signed (`jws`) by one, as everything uploaded
    /// here is signed.
    fn withdraw(&self, sender: &Sender, message: &Value, now: i64) -> anyhow::Result<Answer> {
        let (name, record, pending) = match self.pending_named(sender, &message["body"])? {
            Ok(found) => found,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let update_keys = witness::in_force(record.log.as_deref().unwrap_or_default()).update_keys;
        let signed = attachment(message, "withdraw")
            .filter(|(bytes, _)| bytes == pending.version_id.as_bytes())
            .and_then(|(bytes, jws)| jws.and_then(|jws| keys::signer_among(&bytes, jws, &update_keys)).cloned());
        if signed.is_none() {
            return Ok(Err(Refusal::new("e.p.msg.not-holder", "A withdraw carries the entry's versionId as an attachment \"withdraw\", signed (jws) by an update key of the last witnessed entry.")));
        }
        self.store.drop_pending(&name.as_did_path())?;
        tracing::info!(name = %name.as_did_path(), version = %pending.version_id, "a pending entry withdrawn");
        self.tell(&record, &pending.version_id, "dropped", None, None);
        let record = self.store.get(&name.as_did_path())?;
        Ok(Ok((STATUS, self.status_body(&name, record.as_ref(), true, now))))
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
                let document: Value = record.log.as_deref().and_then(last_document).unwrap_or_default();
                match whois::check(&bytes, &did, &document) {
                    Ok(presentation) => Some((bytes, presentation)),
                    Err(why) => return Ok(Err(Refusal::new("e.p.msg.whois-invalid", why))),
                }
            }
        };
        // The pictures its linked accounts refer to, beside it: each the bytes a
        // credential in it names by their digest, and nothing else.
        let mut pictures = Vec::new();
        for attachment in message["attachments"].as_array().into_iter().flatten().filter(|a| a["id"] != "whois") {
            let Some((_, presentation)) = &whois else {
                return Ok(Err(Refusal::new("e.p.msg.whois-invalid", "Pictures go with a whois that refers to them.")));
            };
            let id = attachment["id"].as_str().unwrap_or_default();
            let referred = wyvrn_hosted_did::linked::pictures_of(&wyvrn_hosted_did::linked::of(presentation, &did));
            let Some(bytes) = attachment_bytes(attachment) else {
                return Ok(Err(Refusal::new("e.p.msg.whois-invalid", "An attachment isn't base64.")));
            };
            if bytes.len() > wyvrn_hosted_did::linked::MAX_PICTURE_BYTES {
                return Ok(Err(Refusal::new("e.p.msg.too-large", "A linked account's picture is at most {1} bytes.").with(wyvrn_hosted_did::linked::MAX_PICTURE_BYTES.to_string())));
            }
            let Some(picture) = referred.iter().find(|picture| picture.name() == id && picture.matches(&bytes)) else {
                return Ok(Err(Refusal::new("e.p.msg.whois-invalid", "An attachment isn't a picture a linked account in the whois refers to (by its digest).")));
            };
            if !pictures.iter().any(|(name, _, _): &(String, String, Vec<u8>)| *name == picture.name()) {
                pictures.push((picture.name(), picture.media_type.clone(), bytes));
            }
        }
        let pictures_bytes: usize = pictures.iter().map(|(_, _, bytes)| bytes.len()).sum();
        if pictures.len() > self.config.max_whois_pictures || pictures_bytes > self.config.max_whois_pictures_bytes {
            return Ok(Err(Refusal::new("e.p.msg.too-large", "The pictures are over this host's limit of {1} pictures and {2} bytes.")
                .with(self.config.max_whois_pictures.to_string())
                .with(self.config.max_whois_pictures_bytes.to_string())));
        }
        let whois = whois.map(|(bytes, _)| bytes);
        // The did:web says where its whois is, published or not: did:webvh has it so.
        let web_document = record.web_document.as_deref().and_then(|text| serde_json::from_str::<Value>(text).ok()).unwrap_or_default();
        let web_document = with_whois_service(web_document, &self.config.domain, &name);
        self.store.set_whois(&key, whois.as_deref(), &serde_json::to_string(&web_document)?, &pictures)?;
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
            // The entry held back, to its holder alone: so that a device that missed
            // being told finds it.
            if let Some(pending) = self.store.pending(&record.name).ok().flatten().filter(|_| yours) {
                body["pending"] = json!({ "version_id": pending.version_id, "accepted_time": pending.accepted_at, "publish_time": pending.publish_at, "entry": pending.entries });
            }
        }
        if let Some(witness) = &self.witness {
            body["witness"] = json!(witness.did);
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
    Some((attachment_bytes(attachment)?, attachment["data"].get("jws")))
}

/// An attachment's `data.base64`, as bytes.
fn attachment_bytes(attachment: &Value) -> Option<Vec<u8>> {
    let text = attachment["data"]["base64"].as_str()?;
    // base64url as DIDComm says, and the ordinary alphabet as some senders write it.
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(text.trim_end_matches('=')))
        .ok()
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
