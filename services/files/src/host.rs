//! The `host` role of `files/1.0`: what the service answers each message with,
//! and the rules of the HTTP side (which chunk may be put, what may be fetched).
//! Nothing here knows about HTTP or DIDComm envelopes: it is handed a sender, a
//! message and the time, and answers with a message or a refusal.

use serde_json::{json, Value};

use crate::config::ServiceConfig;
use crate::hash;
use crate::store::{is_id, FileRecord, NewFile, State, Store};

pub const PIURI: &str = "https://wyvrn.app/files/1.0";

/// Why a message was not done as asked: a `problem-report`'s body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub comment: String,
    pub args: Vec<String>,
}

impl Refusal {
    fn new(code: &'static str, comment: impl Into<String>) -> Self {
        Self { code, comment: comment.into(), args: Vec::new() }
    }

    fn with_args(mut self, args: &[String]) -> Self {
        self.args = args.to_vec();
        self
    }

    pub fn not_allowed() -> Self {
        Self::new("e.p.msg.not-allowed", "This service doesn't take files from you.")
    }

    pub fn not_found() -> Self {
        Self::new("e.p.msg.not-found", "No such file of yours here.")
    }

    fn invalid(what: &str) -> Self {
        Self::new("e.p.msg.invalid", "{1} is missing or malformed.").with_args(&[what.to_string()])
    }
}

/// What a message comes to: the answer's type (after the PIURI) and body.
pub type Answer = Result<(&'static str, Value), Refusal>;

pub struct Host {
    pub store: Store,
    pub config: ServiceConfig,
    /// Ends in `/`.
    public_url: String,
}

impl Host {
    pub fn new(store: Store, config: ServiceConfig, public_url: &str) -> Self {
        let public_url = if public_url.ends_with('/') { public_url.to_string() } else { format!("{public_url}/") };
        Self { store, config, public_url }
    }

    /// Where a stored file is fetched from.
    pub fn download_url(&self, id: &str) -> String {
        format!("{}f/{id}", self.public_url)
    }

    /// Where an upload's bytes are put.
    pub fn upload_url(&self, id: &str) -> String {
        format!("{}u/{id}", self.public_url)
    }

    /// Answers one message from `sender`, already known to be an uploader here.
    /// `None` for a type this protocol doesn't have.
    pub fn handle(&self, sender: &str, message: &Value, now: i64) -> Option<Answer> {
        let kind = message["type"].as_str()?.strip_prefix(PIURI)?.strip_prefix('/')?;
        let body = &message["body"];
        Some(match kind {
            "upload" => self.upload(sender, body, now),
            "complete" => self.complete(sender, body, now),
            "extend" => self.extend(sender, body, now),
            "delete" => self.delete(sender, body),
            "list" => self.list(sender, now),
            _ => return None,
        })
    }

    /// `sender`'s file `id`, if it has one that is still live.
    fn owned(&self, sender: &str, body: &Value, now: i64) -> Result<FileRecord, Refusal> {
        let id = body["file"].as_str().filter(|id| is_id(id)).ok_or_else(|| Refusal::invalid("file"))?;
        match self.store.file(id).map_err(internal)? {
            Some(file) if file.owner == sender && file.live_at(now) => Ok(file),
            _ => Err(Refusal::not_found()),
        }
    }

    fn keep(&self, asked: Option<i64>) -> i64 {
        asked.filter(|&s| s > 0).unwrap_or(self.config.default_keep_seconds).min(self.config.max_keep_seconds)
    }

    fn granted(&self, file: &FileRecord) -> (&'static str, Value) {
        (
            "upload-granted",
            json!({
                "file": file.id,
                "upload_url": self.upload_url(&file.id),
                "chunk_bytes": self.config.chunk_bytes,
                "offset": self.store.received(&file.id),
                "upload_expires_time": file.upload_expires,
                "expires_time": file.expires,
            }),
        )
    }

    fn upload(&self, sender: &str, body: &Value, now: i64) -> Answer {
        let name = body["name"].as_str().filter(|n| !n.is_empty() && n.len() <= 255 && !n.contains(['/', '\\']) && !n.chars().any(char::is_control)).ok_or_else(|| Refusal::invalid("name"))?;
        let media_type = body["media_type"].as_str().filter(|m| !m.is_empty() && m.len() <= 255 && m.is_ascii()).ok_or_else(|| Refusal::invalid("media_type"))?;
        let byte_count = body["byte_count"].as_u64().ok_or_else(|| Refusal::invalid("byte_count"))?;
        let hash = body["hash"].as_str().filter(|h| hash::digest_of(h).is_some()).ok_or_else(|| Refusal::invalid("hash"))?;
        if body.get("keep_seconds").is_some_and(|k| !k.is_i64() && !k.is_u64()) {
            return Err(Refusal::invalid("keep_seconds"));
        }
        if byte_count > self.config.max_file_bytes {
            return Err(Refusal::new("e.p.msg.too-large", "A file here is at most {1} bytes.").with_args(&[self.config.max_file_bytes.to_string()]));
        }
        // The same file asked for again: the same answer, with where it stands.
        if let Some(existing) = self.store.file_like(sender, hash, byte_count).map_err(internal)? {
            if existing.live_at(now) {
                return Ok(self.granted(&existing));
            }
        }
        let used = self.store.used_by(sender).map_err(internal)?;
        if used + byte_count > self.config.quota_bytes {
            return Err(Refusal::new("e.p.msg.quota", "You keep {1} bytes here of the {2} you may.").with_args(&[used.to_string(), self.config.quota_bytes.to_string()]));
        }
        if self.store.uploads_in_progress(sender).map_err(internal)? >= self.config.max_uploads_in_progress {
            return Err(Refusal::new("e.p.msg.quota", "You have {1} uploads going; finish or delete one first.").with_args(&[self.config.max_uploads_in_progress.to_string()]));
        }
        let keep = self.keep(body["keep_seconds"].as_i64());
        let file = self
            .store
            .create(NewFile { owner: sender, name, media_type, byte_count, hash, created: now, upload_expires: now + self.config.upload_deadline_seconds, expires: now + keep })
            .map_err(internal)?;
        tracing::info!(file = %file.id, owner = %sender, bytes = byte_count, "upload granted");
        Ok(self.granted(&file))
    }

    /// What a stored file is, as `completed` and `listing` say it.
    fn described(&self, file: &FileRecord) -> Value {
        json!({
            "file": file.id,
            "url": self.download_url(&file.id),
            "name": file.name,
            "media_type": file.media_type,
            "byte_count": file.byte_count,
            "hash": file.hash,
            "expires_time": file.expires,
        })
    }

    fn complete(&self, sender: &str, body: &Value, now: i64) -> Answer {
        let file = self.owned(sender, body, now)?;
        if file.state == State::Stored {
            return Ok(("completed", self.described(&file)));
        }
        let received = self.store.received(&file.id);
        if received != file.byte_count {
            return Err(Refusal::new("e.p.msg.incomplete", "{1} of {2} bytes are here.").with_args(&[received.to_string(), file.byte_count.to_string()]));
        }
        let found = hash::of_file(&self.store.path(&file.id)).map_err(|e| internal(e.into()))?;
        if found != file.hash {
            self.store.delete(&file.id).map_err(internal)?;
            tracing::info!(file = %file.id, "upload discarded: the bytes don't hash to what was promised");
            return Err(Refusal::new("e.p.msg.hash-mismatch", "The bytes don't hash to {1}; the upload is discarded.").with_args(std::slice::from_ref(&file.hash)));
        }
        self.store.mark_stored(&file.id).map_err(internal)?;
        tracing::info!(file = %file.id, owner = %sender, bytes = file.byte_count, "stored");
        Ok(("completed", self.described(&FileRecord { state: State::Stored, ..file })))
    }

    fn extend(&self, sender: &str, body: &Value, now: i64) -> Answer {
        let file = self.owned(sender, body, now)?;
        if file.state != State::Stored {
            return Err(Refusal::not_found());
        }
        let keep = body["keep_seconds"].as_i64().filter(|&k| k > 0).ok_or_else(|| Refusal::invalid("keep_seconds"))?;
        let expires = (now + keep.min(self.config.max_keep_seconds)).max(file.expires);
        self.store.set_expires(&file.id, expires).map_err(internal)?;
        Ok(("extended", json!({ "file": file.id, "expires_time": expires })))
    }

    fn delete(&self, sender: &str, body: &Value) -> Answer {
        let id = body["file"].as_str().filter(|id| is_id(id)).ok_or_else(|| Refusal::invalid("file"))?;
        // Already gone is the same answer: the file isn't there.
        if let Some(file) = self.store.file(id).map_err(internal)? {
            if file.owner != sender {
                return Err(Refusal::not_found());
            }
            self.store.delete(id).map_err(internal)?;
            tracing::info!(file = %id, owner = %sender, "deleted");
        }
        Ok(("deleted", json!({ "file": id })))
    }

    fn list(&self, sender: &str, now: i64) -> Answer {
        let files: Vec<Value> = self
            .store
            .files_of(sender)
            .map_err(internal)?
            .iter()
            .filter(|file| file.live_at(now))
            .map(|file| match file.state {
                State::Stored => {
                    let mut entry = self.described(file);
                    entry["state"] = json!("stored");
                    entry
                }
                State::Uploading => json!({
                    "file": file.id, "name": file.name, "media_type": file.media_type, "byte_count": file.byte_count, "hash": file.hash,
                    "state": "uploading", "expires_time": file.expires,
                    "offset": self.store.received(&file.id), "upload_url": self.upload_url(&file.id), "upload_expires_time": file.upload_expires,
                }),
            })
            .collect();
        Ok(("listing", json!({ "quota_bytes": self.config.quota_bytes, "used_bytes": self.store.used_by(sender).map_err(internal)?, "files": files })))
    }

    // --- The HTTP side's rules.

    /// The upload a chunk may go to, if `id` names one still being taken.
    pub fn upload_in_progress(&self, id: &str, now: i64) -> Result<FileRecord, Fetch> {
        if !is_id(id) {
            return Err(Fetch::NotFound);
        }
        match self.store.file(id).map_err(|e| Fetch::Failed(e.to_string()))? {
            Some(file) if file.state == State::Stored && file.live_at(now) => Err(Fetch::Gone),
            Some(file) if file.live_at(now) => Ok(file),
            _ => Err(Fetch::NotFound),
        }
    }

    /// Takes a chunk at `offset`. What the file holds afterwards, or why not.
    pub fn put_chunk(&self, id: &str, offset: u64, bytes: &[u8], now: i64) -> Result<u64, Put> {
        let file = self.upload_in_progress(id, now).map_err(Put::Fetch)?;
        if bytes.len() as u64 > self.config.chunk_bytes || offset + bytes.len() as u64 > file.byte_count {
            return Err(Put::TooLong);
        }
        let held = self.store.received(id);
        if offset != held {
            return Err(Put::WrongOffset(held));
        }
        self.store.append(id, bytes).map_err(|e| Put::Fetch(Fetch::Failed(e.to_string())))
    }

    /// The stored file `id` names, if it is there to fetch.
    pub fn stored(&self, id: &str, now: i64) -> Result<FileRecord, Fetch> {
        if !is_id(id) {
            return Err(Fetch::NotFound);
        }
        match self.store.file(id).map_err(|e| Fetch::Failed(e.to_string()))? {
            Some(file) if file.state == State::Stored && file.live_at(now) => Ok(file),
            _ => Err(Fetch::NotFound),
        }
    }
}

/// Why an HTTP request for a file can't be served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetch {
    NotFound,
    /// Complete: no more may be put.
    Gone,
    Failed(String),
}

/// Why a chunk wasn't taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Put {
    Fetch(Fetch),
    WrongOffset(u64),
    TooLong,
}

fn internal(error: anyhow::Error) -> Refusal {
    tracing::error!(error = %format!("{error:#}"), "the store failed");
    Refusal::new("e.p.msg.internal", "Something went wrong here; try again.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host::new(Store::scratch().unwrap(), ServiceConfig { quota_bytes: 100, max_file_bytes: 60, chunk_bytes: 4, max_uploads_in_progress: 2, ..Default::default() }, "https://files.example/")
    }

    fn msg(kind: &str, body: Value) -> Value {
        json!({ "type": format!("{PIURI}/{kind}"), "body": body })
    }

    const ALICE: &str = "did:example:alice";

    #[test]
    fn a_file_is_granted_put_in_chunks_completed_and_fetched() {
        let host = host();
        let bytes = b"hello, files";
        let hash = hash::of_bytes(bytes);
        let (kind, granted) = host.handle(ALICE, &msg("upload", json!({ "name": "hi.txt", "media_type": "text/plain", "byte_count": bytes.len(), "hash": hash })), 1000).unwrap().unwrap();
        assert_eq!(kind, "upload-granted");
        let id = granted["file"].as_str().unwrap().to_string();
        assert_eq!(granted["upload_url"], json!(format!("https://files.example/u/{id}")));
        assert_eq!((granted["offset"].as_u64(), granted["chunk_bytes"].as_u64()), (Some(0), Some(4)));
        assert_eq!((granted["upload_expires_time"].as_i64(), granted["expires_time"].as_i64()), (Some(1000 + 86400), Some(1000 + 14 * 86400)));

        // Too early to complete; chunks in order, and nowhere else.
        assert_eq!(host.handle(ALICE, &msg("complete", json!({ "file": id })), 1001).unwrap().unwrap_err().code, "e.p.msg.incomplete");
        assert_eq!(host.put_chunk(&id, 0, b"hell", 1001), Ok(4));
        assert_eq!(host.put_chunk(&id, 0, b"hell", 1001), Err(Put::WrongOffset(4)));
        assert_eq!(host.put_chunk(&id, 4, b"o, fi", 1001), Err(Put::TooLong));
        assert_eq!(host.put_chunk(&id, 4, b"o, f", 1001), Ok(8));
        assert_eq!(host.put_chunk(&id, 8, b"iles", 1001), Ok(12));
        assert_eq!(host.put_chunk(&id, 12, b"!", 1001), Err(Put::TooLong));
        // Asked for again, the same grant says where it stands.
        let (_, again) = host.handle(ALICE, &msg("upload", json!({ "name": "hi.txt", "media_type": "text/plain", "byte_count": bytes.len(), "hash": hash })), 1002).unwrap().unwrap();
        assert_eq!((again["file"].as_str(), again["offset"].as_u64()), (Some(id.as_str()), Some(12)));
        assert!(host.stored(&id, 1002).is_err());

        let (kind, completed) = host.handle(ALICE, &msg("complete", json!({ "file": id })), 1003).unwrap().unwrap();
        assert_eq!(kind, "completed");
        assert_eq!(completed["url"], json!(format!("https://files.example/f/{id}")));
        assert_eq!((completed["hash"].as_str(), completed["byte_count"].as_u64()), (Some(hash.as_str()), Some(12)));
        assert_eq!(host.stored(&id, 1003).unwrap().name, "hi.txt");
        assert_eq!(host.put_chunk(&id, 12, b"", 1003), Err(Put::Fetch(Fetch::Gone)));
        // Completing twice is the same answer; and listing says it is stored.
        assert_eq!(host.handle(ALICE, &msg("complete", json!({ "file": id })), 1004).unwrap().unwrap().1["url"], completed["url"]);
        let (_, listing) = host.handle(ALICE, &msg("list", json!({})), 1004).unwrap().unwrap();
        assert_eq!((listing["used_bytes"].as_u64(), listing["quota_bytes"].as_u64()), (Some(12), Some(100)));
        assert_eq!(listing["files"][0]["state"], "stored");

        // Kept longer, then let go.
        let (_, extended) = host.handle(ALICE, &msg("extend", json!({ "file": id, "keep_seconds": 3600 })), 2000).unwrap().unwrap();
        assert_eq!(extended["expires_time"], json!(1000 + 14 * 86400)); // not earlier than it was
        let (_, extended) = host.handle(ALICE, &msg("extend", json!({ "file": id, "keep_seconds": 100 * 86400 })), 2000).unwrap().unwrap();
        assert_eq!(extended["expires_time"], json!(2000 + 90 * 86400)); // at most the host's maximum
        assert_eq!(host.handle("did:example:bob", &msg("delete", json!({ "file": id })), 2001).unwrap().unwrap_err().code, "e.p.msg.not-found");
        assert_eq!(host.handle(ALICE, &msg("delete", json!({ "file": id })), 2001).unwrap().unwrap().0, "deleted");
        assert_eq!(host.handle(ALICE, &msg("delete", json!({ "file": id })), 2001).unwrap().unwrap().0, "deleted");
        assert_eq!(host.stored(&id, 2002), Err(Fetch::NotFound));
        assert_eq!(host.handle(ALICE, &msg("extend", json!({ "file": id, "keep_seconds": 1 })), 2002).unwrap().unwrap_err().code, "e.p.msg.not-found");
    }

    #[test]
    fn what_is_promised_is_checked_and_limits_hold() {
        let host = host();
        let upload = |name: &str, bytes: u64, hash: &str| msg("upload", json!({ "name": name, "media_type": "application/octet-stream", "byte_count": bytes, "hash": hash }));
        let real = hash::of_bytes(b"abc");
        assert_eq!(host.handle(ALICE, &upload("a/b", 3, &real), 1).unwrap().unwrap_err().args, ["name"]);
        assert_eq!(host.handle(ALICE, &upload("a", 3, "nothash"), 1).unwrap().unwrap_err().args, ["hash"]);
        assert_eq!(host.handle(ALICE, &upload("a", 61, &real), 1).unwrap().unwrap_err().code, "e.p.msg.too-large");
        assert!(host.handle(ALICE, &msg("nothing", json!({})), 1).is_none());

        // Wrong bytes: discarded at complete.
        let (_, granted) = host.handle(ALICE, &upload("a", 3, &real), 1).unwrap().unwrap();
        let id = granted["file"].as_str().unwrap().to_string();
        assert_eq!(host.put_chunk(&id, 0, b"abd", 2), Ok(3));
        assert_eq!(host.handle(ALICE, &msg("complete", json!({ "file": id })), 3).unwrap().unwrap_err().code, "e.p.msg.hash-mismatch");
        assert_eq!(host.upload_in_progress(&id, 3), Err(Fetch::NotFound));

        // Quota counts uploads in progress; and only so many may be going.
        let (_, one) = host.handle(ALICE, &upload("one", 50, &hash::of_bytes(b"1")), 10).unwrap().unwrap();
        let (_, two) = host.handle(ALICE, &upload("two", 50, &hash::of_bytes(b"2")), 10).unwrap().unwrap();
        assert_eq!(host.handle(ALICE, &upload("three", 1, &hash::of_bytes(b"3")), 10).unwrap().unwrap_err().code, "e.p.msg.quota");
        assert_eq!(host.handle(ALICE, &msg("delete", json!({ "file": one["file"] })), 11).unwrap().unwrap().0, "deleted");
        let (_, three) = host.handle(ALICE, &upload("three", 1, &hash::of_bytes(b"3")), 11).unwrap().unwrap();
        assert_eq!(host.handle(ALICE, &upload("four", 1, &hash::of_bytes(b"4")), 11).unwrap().unwrap_err().args, ["2"]);
        // Someone else's quota is their own.
        assert_eq!(host.handle("did:example:bob", &upload("mine", 60, &hash::of_bytes(b"5")), 11).unwrap().unwrap().0, "upload-granted");
        // An upload past its deadline is as good as gone.
        assert_eq!(host.upload_in_progress(two["file"].as_str().unwrap(), 11 + 86400), Err(Fetch::NotFound));
        assert_eq!(host.handle(ALICE, &msg("list", json!({})), 11 + 86400).unwrap().unwrap().1["files"].as_array().unwrap().len(), 0);
        let _ = three;
    }
}
