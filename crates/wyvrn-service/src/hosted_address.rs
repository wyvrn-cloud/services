//! A service holding a public address at a DID host (did-hosting/1.0), as a
//! person's app does.
//!
//! A service is the `did:web` of its own domain: whatever that domain serves
//! today. Given an address at a DID host (`did:web:id.example:official:news`), it
//! also has a `did:webvh` there, whose history is its own and which the host can't
//! rewrite. The two name each other, so whoever looks either up finds one party,
//! known by the address. That is what lets an app mark a service by the name its
//! DID host reserved for it, and not by a list of domains the app was built with.
//!
//! The address carries the same keys and the same way in. Once the service holds
//! it, it is the DID the service speaks as ([`crate::voice`]), told by
//! [`Holding::when_held`]: what is sent to it is read, and what the service sends
//! unasked comes from it.
//!
//! The name has to be free, or set aside for this service by one of the host's
//! admins (`wyvrn-did-host assign official:news did:web:news.example`).

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use base64::Engine;
use serde_json::{json, Value};
use wyvrn_hosted_did::urls::Location as Place;
use wyvrn_hosted_did::write;

use crate::address::Published;
use crate::db::{Db, Location, Schema, MYSQL_TABLE};
use crate::params;

const DID_HOSTING: &str = "https://wyvrn.app/did-hosting/1.0";

/// Asking a DID something as the service, and having its answer: the service's
/// agent, by whatever route it reaches people.
pub type Ask = Box<dyn Fn(String, Value) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>> + Send + Sync>;

/// Told that the service holds its address: the address's `did:webvh`, and its own
/// `did:web`. For [`crate::Voice::take_up`].
pub type Held = Box<dyn Fn(String, String) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync>;

/// The address a service is set to hold, taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    /// As the setting has it: `did:web:id.example:official:news`.
    pub did_web: String,
    /// The host's own DID: `did:web:id.example`.
    pub host_did: String,
    /// Its domain, as DIDs write it (a port is `%3A`).
    host: String,
    /// `official:news`.
    pub name: String,
}

impl Address {
    /// From `<prefix>_PUBLIC_ADDRESS`, if it is set.
    pub fn from_env(prefix: &str) -> anyhow::Result<Option<Self>> {
        let setting = format!("{prefix}_PUBLIC_ADDRESS");
        match std::env::var(&setting).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
            Some(value) => Self::parse(&value).map(Some).with_context(|| setting),
            None => Ok(None),
        }
    }

    pub fn parse(did_web: &str) -> anyhow::Result<Self> {
        let place = Place::of(did_web).filter(|place| place.scid.is_none() && !place.path.is_empty());
        let Some(place) = place else { bail!("{did_web:?} is not an address at a DID host: did:web:<host>:<name>") };
        let host = did_web["did:web:".len()..].split(':').next().unwrap_or_default().to_string();
        Ok(Self { did_web: did_web.to_string(), host_did: format!("did:web:{host}"), host, name: place.path.join(":") })
    }

    /// The address's `did:webvh`, before its log exists to give it its hash.
    fn template(&self) -> String {
        format!("did:webvh:{{SCID}}:{}:{}", self.host, self.name)
    }
}

fn schema() -> Schema {
    static MYSQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let mysql = MYSQL.get_or_init(|| format!("CREATE TABLE IF NOT EXISTS wyvrn_address (id BIGINT NOT NULL PRIMARY KEY, did_web TEXT NOT NULL, did TEXT NOT NULL, log MEDIUMTEXT NOT NULL) {MYSQL_TABLE}"));
    Schema { sqlite: "CREATE TABLE IF NOT EXISTS wyvrn_address (id INTEGER PRIMARY KEY, did_web TEXT NOT NULL, did TEXT NOT NULL, log TEXT NOT NULL)", mysql }
}

/// The address's log, kept with the rest of what the service keeps: only its
/// holder can continue it, and has to have all of it to do so.
pub struct Kept {
    db: Db,
}

/// A log as kept: the `did:webvh` it is of, and every entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Log {
    pub did: String,
    pub log: String,
}

impl Kept {
    pub fn open(location: &Location, service: &str) -> anyhow::Result<Self> {
        Ok(Self { db: Db::open(location, &schema(), service).with_context(|| format!("the public address in {location}"))? })
    }

    pub fn scratch() -> anyhow::Result<Self> {
        Ok(Self { db: Db::scratch(&schema())? })
    }

    /// The log kept for `address`. One kept for another address is not this one's.
    pub fn load(&self, address: &Address) -> anyhow::Result<Option<Log>> {
        Ok(self
            .db
            .lock()
            .query_row("SELECT did_web, did, log FROM wyvrn_address WHERE id = 1", params![], |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?, row.get::<String>(2)?)))?
            .filter(|(did_web, _, _)| did_web == &address.did_web)
            .map(|(_, did, log)| Log { did, log }))
    }

    pub fn save(&self, address: &Address, log: &Log) -> anyhow::Result<()> {
        let mut session = self.db.lock();
        session.execute("DELETE FROM wyvrn_address WHERE id = 1", params![])?;
        session.execute("INSERT INTO wyvrn_address (id, did_web, did, log) VALUES (1, ?1, ?2, ?3)", params![address.did_web.clone(), log.did.clone(), log.log.clone()])?;
        Ok(())
    }
}

/// The document the address carries: the service's own, under the address's name.
/// The same keys under the same key numbers and the same way in, saying what else
/// it goes by: its short form at the host, and the service's `did:web`.
///
/// Without the two things the service says of itself as its `did:web`: where its
/// whois is (the address's is at the host, signed as the address), and the website
/// it links (which [`Published::did_configuration`] says for the address as well).
pub fn address_document(service_document: &Value, service_did: &str, did: &str, address: &Address) -> Value {
    let renamed = service_document.to_string().replace(&format!("\"{service_did}"), &format!("\"{did}"));
    let mut document: Value = serde_json::from_str(&renamed).unwrap_or_else(|_| service_document.clone());
    document["id"] = json!(did);
    document["alsoKnownAs"] = json!([address.did_web, service_did]);
    if let Some(services) = document["service"].as_array_mut() {
        services.retain(|service| !service["id"].as_str().is_some_and(|id| id.ends_with("#whois")));
    }
    document
}

fn signed(id: &str, media_type: &str, bytes: &[u8], signing_jwk: &str) -> anyhow::Result<Value> {
    let jws = write::sign_attachment(bytes, signing_jwk).map_err(|why| anyhow!(why))?;
    Ok(json!([{ "id": id, "media_type": media_type, "data": { "base64": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes), "jws": jws } }]))
}

fn rfc3339(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0).unwrap_or_default().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A service's hold on its address.
pub struct Holding {
    pub address: Address,
    kept: Kept,
    published: Published,
    ask: Ask,
    held: Option<Held>,
}

impl Holding {
    /// With what is kept already taken up: a service that has held the address
    /// before says so (in its `did:web` document) from its first request.
    pub fn new(address: Address, kept: Kept, published: Published, ask: Ask) -> anyhow::Result<Self> {
        if let Some(log) = kept.load(&address)? {
            published.known_also_as(Some(log.did));
        }
        Ok(Self { address, kept, published, ask, held: None })
    }

    /// What to do once the address is held: at start, for an address held before
    /// (so the service speaks as it even while its host can't be reached), and
    /// whenever it is taken up or brought up to date.
    pub fn when_held(mut self, held: Held) -> Self {
        self.held = Some(held);
        self
    }

    async fn tell_held(&self, did: &str) {
        let Some(held) = &self.held else { return };
        if let Err(error) = held(did.to_string(), self.address.did_web.clone()).await {
            tracing::warn!(%did, error = %format!("{error:#}"), "couldn't speak as this service's public address");
        }
    }

    async fn ask_host(&self, kind: &str, body: Value, attachments: Option<Value>) -> anyhow::Result<Value> {
        let mut message = json!({ "type": format!("{DID_HOSTING}/{kind}"), "body": body });
        if let Some(attachments) = attachments {
            message["attachments"] = attachments;
        }
        let answer = (self.ask)(self.address.host_did.clone(), message).await.with_context(|| format!("asking {} to {kind}", self.address.host_did))?;
        if answer["type"].as_str().is_some_and(|kind| kind.ends_with("/problem-report")) {
            let body = &answer["body"];
            let mut comment = body["comment"].as_str().or(body["code"].as_str()).unwrap_or("it refused").to_string();
            for (index, arg) in body["args"].as_array().into_iter().flatten().enumerate() {
                comment = comment.replace(&format!("{{{}}}", index + 1), arg.as_str().unwrap_or_default());
            }
            bail!("{} would not {kind}: {comment}", self.address.host_did);
        }
        Ok(answer)
    }

    /// Gets the address, or brings it up to date: its log says what the service's
    /// own document says now, and its whois who the service says it is. Returns the
    /// address's `did:webvh`.
    pub async fn hold(&self, now: i64) -> anyhow::Result<String> {
        let signing_jwk = self.published.signing_jwk();
        let service_did = self.published.did().to_string();
        let name = json!({ "name": self.address.name });
        let did = match self.kept.load(&self.address)? {
            None => {
                self.ask_host("reserve", name.clone(), None).await?;
                let document = address_document(&self.published.document, &service_did, &self.address.template(), &self.address);
                let update_keys = [write::public_multikey(signing_jwk).map_err(|why| anyhow!(why))?];
                let written = write::create(&document, signing_jwk, &update_keys).await.map_err(|why| anyhow!(why))?;
                self.ask_host("publish", name.clone(), Some(signed("log", "text/jsonl", written.entry.as_bytes(), signing_jwk)?)).await?;
                self.kept.save(&self.address, &Log { did: written.did.clone(), log: written.log })?;
                tracing::info!(did = %written.did, address = %self.address.did_web, "this service now holds its public address");
                written.did
            }
            Some(held) => {
                let document = address_document(&self.published.document, &service_did, &held.did, &self.address);
                let last: Value = held.log.lines().last().and_then(|line| serde_json::from_str(line).ok()).unwrap_or_default();
                if last["state"] != document {
                    // (An entry is timed to the second, and has to be later than the last.)
                    tokio::time::sleep(Duration::from_millis(1100)).await;
                    let written = write::append(&held.did, &held.log, &document, signing_jwk, None).await.map_err(|why| anyhow!(why))?;
                    let after = last["versionId"].as_str().unwrap_or_default();
                    self.ask_host("publish", json!({ "name": self.address.name, "after": after }), Some(signed("log", "text/jsonl", written.entry.as_bytes(), signing_jwk)?)).await?;
                    self.kept.save(&self.address, &Log { did: held.did.clone(), log: written.log })?;
                    tracing::info!(did = %held.did, "this service's public address says what its document now says");
                }
                held.did
            }
        };
        self.published.known_also_as(Some(did.clone()));
        self.tell_held(&did).await;
        self.publish_whois(&did, now).await?;
        Ok(did)
    }

    /// Publishes who the service says it is, as the address: the same profile its
    /// own whois has, signed as the address's DID.
    pub async fn publish_whois(&self, did: &str, now: i64) -> anyhow::Result<()> {
        let Some(profile) = self.published.profile_now() else { return Ok(()) };
        let signing_jwk = self.published.signing_jwk();
        let whois = write::whois(did, &profile, signing_jwk, &format!("{did}#key-1"), &rfc3339(now - now.rem_euclid(24 * 60 * 60))).await.map_err(|why| anyhow!(why))?;
        self.ask_host("publish-whois", json!({ "name": self.address.name }), Some(signed("whois", "application/vp", whois.to_string().as_bytes(), signing_jwk)?)).await?;
        Ok(())
    }

    /// Holds the address for as long as the service runs: tried until it is had (the
    /// host has to be able to fetch the service's own `did:web` document first, so
    /// not before the service is serving), and its whois published again whenever
    /// an admin changes who the service says it is.
    pub async fn run(self) {
        if let Ok(Some(log)) = self.kept.load(&self.address) {
            self.tell_held(&log.did).await;
        }
        let mut wait = Duration::from_secs(2);
        let did = loop {
            tokio::time::sleep(wait).await;
            match self.hold(crate::now()).await {
                Ok(did) => break did,
                Err(error) => {
                    tracing::warn!(address = %self.address.did_web, error = %format!("{error:#}"), "couldn't take up this service's public address; trying again");
                    wait = (wait * 2).min(Duration::from_secs(10 * 60));
                }
            }
        };
        loop {
            self.published.profile_changed().await;
            if let Err(error) = self.publish_whois(&did, crate::now()).await {
                tracing::warn!(%did, error = %format!("{error:#}"), "couldn't publish this service's changed profile at its public address");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_a_name_at_a_host() {
        let address = Address::parse("did:web:id.example:official:news").unwrap();
        assert_eq!((address.host_did.as_str(), address.name.as_str()), ("did:web:id.example", "official:news"));
        assert_eq!(address.template(), "did:webvh:{SCID}:id.example:official:news");
        let local = Address::parse("did:web:localhost%3A8081:news").unwrap();
        assert_eq!((local.host_did.as_str(), local.template().as_str()), ("did:web:localhost%3A8081", "did:webvh:{SCID}:localhost%3A8081:news"));
        assert!(Address::parse("did:web:id.example").is_err());
        assert!(Address::parse("did:webvh:Qm:id.example:news").is_err());
        assert!(Address::parse("official:news").is_err());
    }

    #[test]
    fn the_address_carries_the_services_document_under_its_own_name() {
        let service = "did:web:news.example";
        let document = json!({
            "id": service,
            "verificationMethod": [{ "id": format!("{service}#key-1"), "controller": service, "publicKeyMultibase": "z6Mk" }],
            "authentication": [format!("{service}#key-1")],
            "service": [
                { "id": format!("{service}#didcomm"), "type": "DIDCommMessaging", "serviceEndpoint": { "uri": "https://news.example/" } },
                { "id": format!("{service}#whois"), "type": "LinkedVerifiablePresentation", "serviceEndpoint": "https://news.example/whois.vp" },
            ],
        });
        let address = Address::parse("did:web:id.example:official:news").unwrap();
        let did = "did:webvh:Qm:id.example:official:news";
        let carried = address_document(&document, service, did, &address);
        assert_eq!(carried["id"], did);
        assert_eq!(carried["alsoKnownAs"], json!(["did:web:id.example:official:news", service]));
        assert_eq!(carried["verificationMethod"][0], json!({ "id": format!("{did}#key-1"), "controller": did, "publicKeyMultibase": "z6Mk" }));
        assert_eq!(carried["authentication"], json!([format!("{did}#key-1")]));
        // Still reached where the service is; its whois is the host's to serve.
        assert_eq!(carried["service"], json!([{ "id": format!("{did}#didcomm"), "type": "DIDCommMessaging", "serviceEndpoint": { "uri": "https://news.example/" } }]));
    }

    #[test]
    fn a_log_is_kept_for_the_address_it_is_of() {
        let kept = Kept::scratch().unwrap();
        let news = Address::parse("did:web:id.example:official:news").unwrap();
        assert_eq!(kept.load(&news).unwrap(), None);
        let log = Log { did: "did:webvh:Qm:id.example:official:news".into(), log: "{}".into() };
        kept.save(&news, &log).unwrap();
        assert_eq!(kept.load(&news).unwrap(), Some(log));
        assert_eq!(kept.load(&Address::parse("did:web:id.example:other").unwrap()).unwrap(), None);
    }
}
