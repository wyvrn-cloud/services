//! Where each community's attached files are kept: the host's own bucket (a
//! folder per community, `<prefix>/c<id>/`), a bucket of the community owner's
//! own, or this host's disk (wyvrn-files' bucket.rs and store.rs).
//!
//! **A bucket of the owner's own** (asked 2026-11-28): set on the community's
//! owner page "Where files are kept", in the app's community settings. The host
//! writes, reads back and removes a test file before anything changes, and asks
//! whether apps may fetch from it (CORS). What the owner gives is kept sealed in
//! the registry with a key in the host's data folder (`.bucket-secrets.key`,
//! readable by this service's user alone), never logged, and never shown again:
//! the page says only the endpoint, bucket and folder.
//!
//! Files from before stay where they are, and are read there, until moved (the
//! page's "Move files there", or `wyvrn-community-host move-files-to-bucket`).
//! Stopping using one's own bucket keeps it known, files still in it read, until
//! they are moved back and it is forgotten.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex, Weak};

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use serde_json::{json, Value};
use wyvrn_files::bucket::{Bucket, BucketConfig, Downloads};
use wyvrn_service::admin::{button, Page, Pages};

use crate::registry::{OwnBucket, Record};
use crate::tenants::Host;

pub const OWN_BUCKET: &str = "owner-files-bucket";

/// The key own buckets are sealed with: made once, in the host's data folder.
fn sealing_key(dir: &Path) -> anyhow::Result<[u8; 32]> {
    let path = dir.join(".bucket-secrets.key");
    if let Ok(bytes) = std::fs::read(&path) {
        return bytes.try_into().map_err(|_| anyhow::anyhow!("{} isn't a key", path.display()));
    }
    std::fs::create_dir_all(dir)?;
    let key = Aes256Gcm::generate_key(OsRng);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    options.open(&path)?.write_all(&key)?;
    Ok(key.into())
}

/// `config` sealed with the host's key, for the registry.
pub fn seal(dir: &Path, config: &BucketConfig) -> anyhow::Result<String> {
    let key = sealing_key(dir)?;
    let cipher = Aes256Gcm::new(&key.into());
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let plain = serde_json::to_vec(config)?;
    let sealed = cipher.encrypt(&nonce, plain.as_slice()).map_err(|_| anyhow::anyhow!("sealing a bucket's settings failed"))?;
    let mut bytes = nonce.to_vec();
    bytes.extend(sealed);
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// What [`seal`] sealed.
pub fn open(dir: &Path, sealed: &str) -> anyhow::Result<BucketConfig> {
    let key = sealing_key(dir)?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(sealed)?;
    if bytes.len() < 12 {
        anyhow::bail!("a sealed bucket too short to be one");
    }
    let (nonce, sealed) = bytes.split_at(12);
    let plain = Aes256Gcm::new(&key.into()).decrypt(Nonce::from_slice(nonce), sealed).map_err(|_| anyhow::anyhow!("a bucket's settings that don't open with this host's key"))?;
    Ok(serde_json::from_slice(&plain)?)
}

/// Runs `work` to its end on a thread and runtime of its own: for what a page's
/// action (which isn't async) has to ask a bucket.
fn block_on<T: Send>(work: impl std::future::Future<Output = anyhow::Result<T>> + Send) -> anyhow::Result<T> {
    std::thread::scope(|scope| scope.spawn(|| tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(work)).join().map_err(|_| anyhow::anyhow!("asking the bucket panicked"))?)
}

impl Host {
    /// The host's bucket, as it is for `record`: its own folder within it.
    pub fn host_bucket_for(&self, record: &Record) -> Option<BucketConfig> {
        self.config.storage.bucket.as_ref().map(|bucket| bucket.within(&format!("c{}", record.id)))
    }

    /// `record`'s own bucket, opened, and whether it is the one in use.
    pub fn own_bucket_for(&self, record: &Record) -> Option<(BucketConfig, OwnBucket)> {
        let kept = match self.registry.own_bucket(&record.name) {
            Ok(Some(kept)) => kept,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(community = %record.name, %error, "couldn't read a community's own bucket");
                return None;
            }
        };
        match open(&self.config.storage.dir, &kept.sealed) {
            Ok(config) => Some((config, kept)),
            Err(error) => {
                tracing::error!(community = %record.name, %error, "a community's own bucket couldn't be opened");
                None
            }
        }
    }

    /// Where `record`'s files go from now on, and the buckets its older files may
    /// still be in.
    pub fn files_buckets(&self, record: &Record) -> (Option<BucketConfig>, Vec<BucketConfig>) {
        let host = self.host_bucket_for(record);
        match self.own_bucket_for(record) {
            Some((own, kept)) if kept.active => (Some(own), host.into_iter().collect()),
            Some((own, _)) => (host, vec![own]),
            None => (host, Vec::new()),
        }
    }
}

/// How a move of a community's files is going, for its page to say.
static MOVING: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);

fn moving(name: &str) -> Option<String> {
    MOVING.lock().unwrap_or_else(|p| p.into_inner()).get(name).cloned()
}

fn set_moving(name: &str, said: Option<String>) {
    let mut moving = MOVING.lock().unwrap_or_else(|p| p.into_inner());
    match said {
        Some(said) => moving.insert(name.to_string(), said),
        None => moving.remove(name),
    };
}

/// The owner's page in one community: where its files are kept.
pub struct OwnBucketPage {
    pub host: Weak<Host>,
    pub community: String,
}

/// A text field of the form, trimmed.
fn field<'a>(input: &'a Value, name: &str) -> &'a str {
    input[name].as_str().map_or("", str::trim)
}

impl OwnBucketPage {
    /// How many files, and bytes, are where, as the open community counts them.
    /// Read from the store itself when the community isn't open (it is closed
    /// on every change here): an empty answer would let a bucket with files in
    /// it be forgotten.
    fn places(&self, host: &Host) -> anyhow::Result<Vec<(String, u64, u64)>> {
        if let Some(files) = host.loaded(&self.community).and_then(|tenant| tenant.state.community.files.as_ref().map(|files| files.host.store.places())) {
            return files;
        }
        files_of(host, &self.community)?.host.store.places()
    }

    /// The tag a bucket's files carry.
    fn tag_of(config: &BucketConfig) -> Option<String> {
        Bucket::new(config).ok().map(|bucket| bucket.tag().to_string())
    }
}

impl Pages for OwnBucketPage {
    fn pages(&self) -> Vec<Page> {
        let Some(host) = self.host.upgrade() else { return Vec::new() };
        if !host.config.storage.own_buckets {
            return Vec::new();
        }
        vec![Page {
            id: OWN_BUCKET,
            title: "Where files are kept",
            summary: "Keep the files members attach in a storage bucket of your own (S3, Linode Object Storage, MinIO).",
            events: &["save", "check", "move", "stop", "forget"],
        }]
    }

    fn view(&self, _page: &str, _owner: &str, _now: i64) -> anyhow::Result<Vec<Value>> {
        let Some(host) = self.host.upgrade() else { return Ok(vec![]) };
        let Some(record) = host.registry.get(&self.community)? else { return Ok(vec![]) };
        let mut items = vec![json!({ "type": "text", "text": "The files members attach to posts can be kept in a storage bucket of your own: Amazon S3, Linode Object Storage, MinIO, or anything else that speaks S3. Members' apps don't change. Files are encrypted in members' apps before they are uploaded, so the bucket only ever holds what it can't read." })];
        let places = self.places(&host)?;
        let count_in = |tag: &str| places.iter().filter(|(place, ..)| place == &format!("bucket:{tag}")).map(|(_, count, _)| *count).sum::<u64>();
        let elsewhere = |tag: Option<&str>| places.iter().filter(|(place, ..)| tag.is_none_or(|tag| place != &format!("bucket:{tag}"))).map(|(_, count, _)| *count).sum::<u64>();
        if let Some(said) = moving(&self.community) {
            items.push(json!({ "type": "information", "text": said }));
        }
        match host.own_bucket_for(&record) {
            Some((own, kept)) if kept.active => {
                items.push(json!({ "type": "information", "text": format!("Files go to your bucket {} at {}{}.", own.name, own.endpoint, if own.prefix.is_empty() { String::new() } else { format!(", in {}", own.prefix) }) }));
                if !kept.cors {
                    items.push(json!({ "type": "warning", "text": "Your bucket doesn't let apps fetch from it (CORS): members won't be able to open files sent there. Allow GET and HEAD from any origin, with the Range header, exposing Content-Range, Content-Length and ETag, then press Check again." }));
                }
                let tag = Self::tag_of(&own);
                if let Some(tag) = &tag {
                    items.push(json!({ "type": "text", "text": format!("{} files are in it.", count_in(tag)) }));
                }
                let left = elsewhere(tag.as_deref());
                if left > 0 {
                    items.push(json!({ "type": "text", "text": format!("{left} files from before are still kept by this host: move them to your bucket, or leave them where they are (they are still read there).") }));
                    items.push(button("Move files there", "move", json!({})));
                }
                items.push(button("Check again", "check", json!({})));
                items.push(button("Stop using it", "stop", json!({})));
            }
            Some((own, _)) => {
                let tag = Self::tag_of(&own);
                let there = tag.as_deref().map_or(0, count_in);
                items.push(json!({ "type": "information", "text": format!("Files go to this host's storage again. {there} are still in your bucket {}, and are read from it.", own.name) }));
                if there > 0 {
                    items.push(button("Move them back", "move", json!({})));
                } else {
                    items.push(button("Forget my bucket", "forget", json!({})));
                }
            }
            None => {
                items.push(json!({ "type": "text", "text": if host.config.storage.bucket.is_some() { "Files are kept in this host's storage now." } else { "Files are kept on this host's disk now." } }));
                items.push(json!({ "type": "text", "text": "Make a bucket and an access key for it that may read, write, list and delete. For downloads, the bucket must allow CORS: GET and HEAD from any origin, with the Range header, exposing Content-Range, Content-Length and ETag." }));
                items.push(json!({ "type": "form", "children": [
                    { "type": "text-input", "form-id": "endpoint", "label": "Endpoint (https://us-east-1.linodeobjects.com)", "value": "", "required": true },
                    { "type": "text-input", "form-id": "name", "label": "Bucket", "value": "", "required": true },
                    { "type": "text-input", "form-id": "region", "label": "Region", "value": "us-east-1" },
                    { "type": "text-input", "form-id": "prefix", "label": "Folder in the bucket", "value": "wyvrn" },
                    { "type": "text-input", "form-id": "key", "label": "Access key", "value": "", "required": true },
                    { "type": "text-input", "form-id": "secret", "label": "Secret key", "value": "", "required": true, "format": "secret" },
                    { "type": "check-box", "form-id": "proxy", "label": "Pass downloads through this host (only if the bucket must not be seen: every download then crosses the host)", "value": false },
                    button("Use this bucket", "save", json!({})),
                ] }));
            }
        }
        Ok(items)
    }

    fn act(&self, _page: &str, event: &str, input: &Value, owner: &str, now: i64) -> Result<Option<String>, String> {
        let host = self.host.upgrade().ok_or("The host is stopping.")?;
        if !host.config.storage.own_buckets {
            return Err("This host doesn't take buckets of communities' own.".into());
        }
        let record = host.registry.get(&self.community).map_err(|e| e.to_string())?.ok_or("There is no such community.")?;
        let failed = |error: anyhow::Error| {
            tracing::warn!(community = %self.community, %error, "a community's own bucket");
            error.to_string()
        };
        if moving(&self.community).is_some_and(|said| !said.starts_with("Moved")) && event != "check" {
            return Err("Files are being moved; wait until that is done.".into());
        }
        match event {
            "save" => {
                if host.registry.own_bucket(&self.community).map_err(|e| e.to_string())?.is_some() {
                    return Err("Move the files out of the bucket you had before, and forget it, first.".into());
                }
                let config = BucketConfig {
                    endpoint: field(input, "endpoint").to_string(),
                    name: field(input, "name").to_string(),
                    region: Some(field(input, "region")).filter(|r| !r.is_empty()).unwrap_or("us-east-1").to_string(),
                    prefix: field(input, "prefix").to_string(),
                    path_style: true,
                    downloads: if input["proxy"].as_bool() == Some(true) { Downloads::Proxy } else { Downloads::Redirect },
                    link_seconds: 300,
                    key: field(input, "key").to_string(),
                    secret: field(input, "secret").to_string(),
                    key_env: None,
                    secret_env: None,
                };
                if config.endpoint.is_empty() || config.name.is_empty() || config.key.is_empty() || config.secret.is_empty() {
                    return Err("Say the endpoint, the bucket, and the access key and secret.".into());
                }
                if !config.endpoint.starts_with("https://") && !host.config.server.public_url.starts_with("http://") {
                    return Err("The endpoint has to be https://.".into());
                }
                let bucket = Bucket::new(&config).map_err(failed)?;
                let checked = block_on(bucket.check("http://tauri.localhost")).map_err(|e| format!("The bucket couldn't be used: {e}"))?;
                let sealed = seal(&host.config.storage.dir, &config).map_err(failed)?;
                host.registry.set_own_bucket(&self.community, &OwnBucket { sealed, active: true, cors: checked.cors, set_at: now }).map_err(|e| e.to_string())?;
                tracing::info!(community = %self.community, %owner, bucket = %config.name, cors = checked.cors, "a community's files go to its own bucket");
                host.unload(&self.community);
                Ok(Some(if checked.cors {
                    "Files go to your bucket from now on. Files from before stay where they are until you move them.".into()
                } else {
                    "Files go to your bucket from now on, but it doesn't let apps fetch from it yet (CORS): set that, then press Check again.".into()
                }))
            }
            "check" => {
                let (own, mut kept) = host.own_bucket_for(&record).ok_or("There is no bucket of your own to check.")?;
                let checked = block_on(Bucket::new(&own).map_err(failed)?.check("http://tauri.localhost")).map_err(|e| format!("The bucket couldn't be used: {e}"))?;
                kept.cors = checked.cors;
                host.registry.set_own_bucket(&self.community, &kept).map_err(|e| e.to_string())?;
                Ok(Some(if checked.cors { "It works, and apps may fetch from it.".into() } else { "It works, but apps may not fetch from it (CORS).".into() }))
            }
            "move" => {
                let runtime = tokio::runtime::Handle::try_current().map_err(|_| "Moving can't be started here.")?;
                set_moving(&self.community, Some("Moving files…".into()));
                let (weak, name) = (self.host.clone(), self.community.clone());
                runtime.spawn(async move {
                    let said = match move_files(&weak, &name).await {
                        Ok(moved) => format!(
                            "Moved {} files.{}",
                            moved.moved,
                            if moved.mismatched.is_empty() && moved.unreadable.is_empty() { String::new() } else { format!(" {} couldn't be moved and were left where they are.", moved.mismatched.len() + moved.unreadable.len()) }
                        ),
                        Err(error) => {
                            tracing::warn!(community = %name, %error, "moving a community's files failed");
                            format!("Moving files stopped: {error}. Press it again to carry on.")
                        }
                    };
                    set_moving(&name, Some(said));
                });
                Ok(Some("Moving files. This page says how it went.".into()))
            }
            "stop" => {
                let (_, mut kept) = host.own_bucket_for(&record).ok_or("There is no bucket of your own.")?;
                kept.active = false;
                host.registry.set_own_bucket(&self.community, &kept).map_err(|e| e.to_string())?;
                host.unload(&self.community);
                set_moving(&self.community, None);
                Ok(Some("Files go to this host's storage again. Those in your bucket are still read from it until you move them back.".into()))
            }
            "forget" => {
                let (own, kept) = host.own_bucket_for(&record).ok_or("There is no bucket of your own.")?;
                if kept.active {
                    return Err("Stop using it first.".into());
                }
                let tag = Self::tag_of(&own).unwrap_or_default();
                if self.places(&host).map_err(failed)?.iter().any(|(place, count, _)| *count > 0 && place == &format!("bucket:{tag}")) {
                    return Err("Files are still in it: move them back first.".into());
                }
                host.registry.forget_own_bucket(&self.community).map_err(|e| e.to_string())?;
                host.unload(&self.community);
                set_moving(&self.community, None);
                Ok(Some("Your bucket is forgotten.".into()))
            }
            _ => Err("Not something this page does.".into()),
        }
    }
}

/// The community `name`'s attached files, opened apart from the running host:
/// what the bucket commands work on. Safe while the host runs (each move
/// changes a record only from what it was).
pub fn files_of(host: &Host, name: &str) -> anyhow::Result<wyvrn_community::files::Files> {
    let record = host.registry.get(name)?.ok_or_else(|| anyhow::anyhow!("there is no community called {name}"))?;
    let stores = host.stores(&record, record.storage)?;
    let settings = host.settings(&record);
    let (Some(config), Some(location)) = (&settings.files, &stores.files) else { anyhow::bail!("{name} keeps no files") };
    wyvrn_community::files::Files::open(config, location, &settings.server.public_url)
}

/// Moves the community `name`'s files into the bucket they go to now.
pub async fn move_files(host: &Weak<Host>, name: &str) -> anyhow::Result<wyvrn_files::store::Moved> {
    let host = host.upgrade().ok_or_else(|| anyhow::anyhow!("the host is stopping"))?;
    let tenant = host.tenant(name).await?.ok_or_else(|| anyhow::anyhow!("{name} isn't open here"))?;
    let files = tenant.state.community.files.as_ref().ok_or_else(|| anyhow::anyhow!("{name} keeps no files"))?;
    let mut done = 0u64;
    let moved = files
        .host
        .store
        .move_to_bucket(|_| {
            done += 1;
            if done.is_multiple_of(20) {
                set_moving(name, Some(format!("Moving files… {done} so far.")));
            }
        })
        .await?;
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> BucketConfig {
        BucketConfig {
            endpoint: "https://us-east-1.linodeobjects.com".into(),
            name: "theirs".into(),
            region: "us-east-1".into(),
            prefix: "wyvrn".into(),
            path_style: true,
            downloads: Downloads::Redirect,
            link_seconds: 300,
            key: "THE-KEY".into(),
            secret: "THE-SECRET".into(),
            key_env: None,
            secret_env: None,
        }
    }

    #[test]
    fn an_owners_bucket_is_kept_sealed_with_the_hosts_key_and_opens_with_it_alone() {
        let dir = std::env::temp_dir().join(format!("wyvrn-host-buckets-{}", uuid::Uuid::new_v4()));
        let sealed = seal(&dir, &config()).unwrap();
        assert!(!sealed.contains("THE-SECRET") && !sealed.contains("THE-KEY"));
        assert_eq!(open(&dir, &sealed).unwrap(), config());
        // The key is made once, readable by this service's user alone.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join(".bucket-secrets.key")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(open(&dir, &seal(&dir, &config()).unwrap()).unwrap(), config());
        // Another host's key doesn't open it, nor does a changed seal.
        let other = std::env::temp_dir().join(format!("wyvrn-host-buckets-{}", uuid::Uuid::new_v4()));
        assert!(open(&other, &sealed).is_err());
        let mut bytes = base64::engine::general_purpose::STANDARD.decode(&sealed).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        assert!(open(&dir, &base64::engine::general_purpose::STANDARD.encode(bytes)).is_err());
        // Its settings say nothing of the secret when printed.
        assert!(!format!("{:?}", config()).contains("THE-SECRET"));
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn a_communitys_folder_is_its_own_within_the_hosts_bucket() {
        assert_eq!(config().within("c7").prefix, "wyvrn/c7");
        assert_eq!(BucketConfig { prefix: String::new(), ..config() }.within("c7").prefix, "c7");
        assert_eq!(BucketConfig { prefix: "/files/".into(), ..config() }.within("/c7/").prefix, "files/c7");
    }
}
