//! Files kept in an S3-compatible bucket (Linode Object Storage, MinIO, AWS S3)
//! instead of on the service's disk.
//!
//! What goes where (asked 2026-11-28):
//!
//! - **Uploads still land on disk**, a chunk at a time as files/1.0 puts them,
//!   and are checked there (`complete` hashes the whole file). The apps are
//!   unchanged, old versions too. A file once stored is then moved to the bucket
//!   by the store's mover (`Store::settle`), and its bytes on disk removed. So
//!   a file crosses the service's machine once on the way in.
//! - **Downloads** are a `302` to a link signed for a few minutes
//!   (`Downloads::Redirect`, the default): the bytes come straight from the
//!   bucket, never through the service. Object storage charges for what leaves it
//!   to the service's machine as well (Linode, within one data centre too), so
//!   passing them on would bill each byte twice. `Downloads::Proxy` passes them on
//!   instead, Range and all, for a bucket that mustn't be seen.
//! - A redirect is followed by the page's own `fetch` in the apps (and the web
//!   build), which is across origins: **the bucket must answer CORS** for it
//!   (`GET`, `HEAD`, the `Range` header, exposing `Content-Range`, `ETag` and
//!   `Content-Length`). `check` says whether it does; MinIO answers every origin
//!   on its own, Linode's buckets are set in its Cloud Manager or with `s3cmd
//!   setcors`.
//!
//! The signing is rusty-s3's (SigV4, query-signed links); reqwest sends.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use rusty_s3::{Bucket as S3Bucket, Credentials, S3Action, UrlStyle};
use serde::Deserialize;

/// How a file in the bucket is given to whoever asks for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Downloads {
    /// A `302` to a link signed for `link_seconds`: the bytes skip the service.
    #[default]
    Redirect,
    /// The service fetches from the bucket and passes it on.
    Proxy,
}

/// A bucket, as configured (`[bucket]`, or a community's own). The key and secret
/// are never in a config file that is checked in: they come from the
/// environment, or a file beside the service's data (`key_file`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BucketConfig {
    /// `https://us-east-1.linodeobjects.com`, `http://127.0.0.1:9000`, ...
    pub endpoint: String,
    /// The bucket's name.
    pub name: String,
    /// The region it is signed for. Linode's is the cluster's (`us-east-1`,
    /// `nl-ams-1`, ...); MinIO takes anything.
    #[serde(default = "default_region")]
    pub region: String,
    /// What every key starts with: a folder of the bucket for this service (or,
    /// on a community host, for one community).
    #[serde(default)]
    pub prefix: String,
    /// `https://endpoint/bucket/key` rather than `https://bucket.endpoint/key`.
    /// Both work on Linode; MinIO needs this.
    #[serde(default = "yes")]
    pub path_style: bool,
    #[serde(default)]
    pub downloads: Downloads,
    /// How long a download link is good for.
    #[serde(default = "default_link_seconds")]
    pub link_seconds: u64,
    /// The access key and secret: set from the environment (`key_env` and
    /// `secret_env`, or the service's defaults) by [`BucketConfig::with_secrets`].
    /// Never written in a config file that is checked in.
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub secret: String,
    /// The environment variables holding the key and secret.
    #[serde(default)]
    pub key_env: Option<String>,
    #[serde(default)]
    pub secret_env: Option<String>,
}

impl BucketConfig {
    /// The key and secret filled in from the environment: `key_env`/`secret_env`
    /// if set, else `key_var`/`secret_var`. What is already set stays.
    pub fn with_secrets(mut self, key_var: &str, secret_var: &str) -> anyhow::Result<Self> {
        let read = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        if self.key.is_empty() {
            let name = self.key_env.clone().unwrap_or_else(|| key_var.to_string());
            self.key = read(&name).ok_or_else(|| anyhow!("the bucket {} has no key: set {name}", self.name))?;
        }
        if self.secret.is_empty() {
            let name = self.secret_env.clone().unwrap_or_else(|| secret_var.to_string());
            self.secret = read(&name).ok_or_else(|| anyhow!("the bucket {} has no secret: set {name}", self.name))?;
        }
        Ok(self)
    }

    /// A bucket wholly from the environment (`<PREFIX>_BUCKET_ENDPOINT`,
    /// `_NAME`, `_REGION`, `_PREFIX`, `_DOWNLOADS`), over what the file said.
    pub fn from_env(prefix: &str, said: Option<Self>) -> anyhow::Result<Option<Self>> {
        let read = |name: &str| std::env::var(format!("{prefix}_BUCKET_{name}")).ok().filter(|v| !v.trim().is_empty());
        let mut config = match (said, read("ENDPOINT"), read("NAME")) {
            (Some(config), _, _) => config,
            (None, Some(endpoint), Some(name)) => BucketConfig {
                endpoint,
                name,
                region: default_region(),
                prefix: String::new(),
                path_style: true,
                downloads: Downloads::default(),
                link_seconds: default_link_seconds(),
                key: String::new(),
                secret: String::new(),
                key_env: None,
                secret_env: None,
            },
            _ => return Ok(None),
        };
        if let Some(endpoint) = read("ENDPOINT") {
            config.endpoint = endpoint;
        }
        if let Some(name) = read("NAME") {
            config.name = name;
        }
        if let Some(region) = read("REGION") {
            config.region = region;
        }
        if let Some(folder) = read("PREFIX") {
            config.prefix = folder;
        }
        if let Some(downloads) = read("DOWNLOADS") {
            config.downloads = match downloads.trim() {
                "redirect" => Downloads::Redirect,
                "proxy" => Downloads::Proxy,
                other => bail!("{prefix}_BUCKET_DOWNLOADS is {other:?}: say redirect or proxy"),
            };
        }
        Ok(Some(config.with_secrets(&format!("{prefix}_BUCKET_KEY"), &format!("{prefix}_BUCKET_SECRET"))?))
    }
}

fn default_region() -> String {
    "us-east-1".into()
}

fn yes() -> bool {
    true
}

fn default_link_seconds() -> u64 {
    300
}

/// How big each part of a multipart upload is: S3 wants 5 MiB at least (but the
/// last), and at most 10,000 parts.
const PART_BYTES: u64 = 16 * 1024 * 1024;
/// A file up to this size goes up in one `PUT`.
const ONE_PUT_BYTES: u64 = 64 * 1024 * 1024;
/// How long a signed request the service sends itself is good for.
const SIGNED_FOR: Duration = Duration::from_secs(600);

/// A bucket the store keeps files in.
pub struct Bucket {
    s3: S3Bucket,
    credentials: Credentials,
    prefix: String,
    downloads: Downloads,
    link_for: Duration,
    tag: String,
    client: reqwest::Client,
}

impl std::fmt::Debug for Bucket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key or secret.
        f.debug_struct("Bucket").field("bucket", &self.s3.name()).field("prefix", &self.prefix).field("tag", &self.tag).finish()
    }
}

/// What `check` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// Whether a browser's cross-origin `fetch` of a download link is answered
    /// (a preflight for a `Range` request, from `origin`).
    pub cors: bool,
    pub origin: String,
}

impl Bucket {
    pub fn new(config: &BucketConfig) -> anyhow::Result<Self> {
        if config.key.is_empty() || config.secret.is_empty() {
            bail!("the bucket {} has no key and secret", config.name);
        }
        let endpoint: url::Url = config.endpoint.parse().with_context(|| format!("the bucket's endpoint {:?}", config.endpoint))?;
        let style = if config.path_style { UrlStyle::Path } else { UrlStyle::VirtualHost };
        let s3 = S3Bucket::new(endpoint, style, config.name.clone(), config.region.clone()).map_err(|e| anyhow!("the bucket {}: {e}", config.name))?;
        let prefix = normal_prefix(&config.prefix);
        Ok(Self {
            tag: tag_of(&config.endpoint, &config.name, &prefix),
            s3,
            credentials: Credentials::new(config.key.clone(), config.secret.clone()),
            prefix,
            downloads: config.downloads,
            link_for: Duration::from_secs(config.link_seconds.clamp(30, 7 * 24 * 3600)),
            client: reqwest::Client::builder().connect_timeout(Duration::from_secs(20)).build()?,
        })
    }

    /// The same bucket with an HTTP client of its own: for a runtime other than
    /// the one this bucket's connections were made on (a pooled connection is
    /// driven by the runtime that made it, which may be the one waiting).
    pub fn detached(&self) -> anyhow::Result<Self> {
        Ok(Self {
            s3: self.s3.clone(),
            credentials: self.credentials.clone(),
            prefix: self.prefix.clone(),
            downloads: self.downloads,
            link_for: self.link_for,
            tag: self.tag.clone(),
            client: reqwest::Client::builder().connect_timeout(Duration::from_secs(20)).build()?,
        })
    }

    /// What a file's record says it is in: the same bucket and folder always give
    /// the same tag, so a record still finds its bytes once the service is
    /// started again, and tells them apart from another bucket's.
    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn downloads(&self) -> Downloads {
        self.downloads
    }

    pub fn link_for(&self) -> Duration {
        self.link_for
    }

    /// The bucket's key for a file (or anything) of this service's.
    pub fn key(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }

    /// A link to `key` good for a while, which says it is `media_type` and is to
    /// be saved as `filename` (S3's `response-content-*`): what a download is
    /// sent to.
    pub fn link(&self, key: &str, media_type: &str, filename: &str) -> url::Url {
        let mut action = self.s3.get_object(Some(&self.credentials), key);
        action.query_mut().insert("response-content-type", media_type.to_string());
        action.query_mut().insert("response-content-disposition", attachment(filename));
        action.sign(self.link_for)
    }

    /// Puts the file at `path` (`size` bytes) at `key`.
    pub async fn put_file(&self, key: &str, path: &Path, size: u64) -> anyhow::Result<()> {
        if size <= ONE_PUT_BYTES {
            let bytes = tokio::fs::read(path).await?;
            return self.put_bytes(key, bytes).await;
        }
        let create = self.s3.create_multipart_upload(Some(&self.credentials), key).sign(SIGNED_FOR);
        let answer = self.client.post(create).send().await?;
        let text = ok(answer, "starting an upload").await?.text().await?;
        let started = rusty_s3::actions::CreateMultipartUpload::parse_response(&text).map_err(|e| anyhow!("starting an upload: {e}"))?;
        let upload = started.upload_id().to_string();
        match self.put_parts(key, path, size, &upload).await {
            Ok(()) => Ok(()),
            Err(error) => {
                let abort = self.s3.abort_multipart_upload(Some(&self.credentials), key, &upload).sign(SIGNED_FOR);
                let _ = self.client.delete(abort).send().await;
                Err(error)
            }
        }
    }

    async fn put_parts(&self, key: &str, path: &Path, size: u64, upload: &str) -> anyhow::Result<()> {
        use tokio::io::AsyncReadExt;
        let mut file = tokio::fs::File::open(path).await?;
        let mut etags = Vec::new();
        let mut sent = 0u64;
        let mut number: u16 = 1;
        while sent < size {
            let length = PART_BYTES.min(size - sent) as usize;
            let mut part = vec![0u8; length];
            file.read_exact(&mut part).await?;
            let url = self.s3.upload_part(Some(&self.credentials), key, number, upload).sign(SIGNED_FOR);
            let answer = ok(self.client.put(url).body(part).send().await?, "putting a part").await?;
            let etag = answer.headers().get("etag").and_then(|v| v.to_str().ok()).ok_or_else(|| anyhow!("a part put with no ETag"))?;
            etags.push(etag.to_string());
            sent += length as u64;
            number = number.checked_add(1).ok_or_else(|| anyhow!("too many parts"))?;
        }
        let complete = self.s3.complete_multipart_upload(Some(&self.credentials), key, upload, etags.iter().map(String::as_str));
        let url = complete.sign(SIGNED_FOR);
        let body = complete.body();
        let text = ok(self.client.post(url).body(body).send().await?, "completing an upload").await?.text().await?;
        // S3 can say 200 and still have failed: the body says.
        if text.contains("<Error>") {
            bail!("completing an upload: {text}");
        }
        Ok(())
    }

    /// Puts `bytes` at `key` in one go.
    pub async fn put_bytes(&self, key: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
        let url = self.s3.put_object(Some(&self.credentials), key).sign(SIGNED_FOR);
        ok(self.client.put(url).body(bytes).send().await?, "putting a file").await?;
        Ok(())
    }

    /// How many bytes are at `key`, if anything is.
    pub async fn size(&self, key: &str) -> anyhow::Result<Option<u64>> {
        let url = self.s3.head_object(Some(&self.credentials), key).sign(SIGNED_FOR);
        let answer = self.client.head(url).send().await?;
        if answer.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let answer = ok(answer, "asking about a file").await?;
        Ok(answer.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()))
    }

    /// The whole of what is at `key`.
    pub async fn get(&self, key: &str) -> anyhow::Result<Vec<u8>> {
        Ok(ok(self.fetch(key, None).await?, "fetching a file").await?.bytes().await?.to_vec())
    }

    /// `GET` of `key`, with a `Range` passed on if one is given: the answer as the
    /// bucket gave it, to be passed on.
    pub async fn fetch(&self, key: &str, range: Option<&str>) -> anyhow::Result<reqwest::Response> {
        let url = self.s3.get_object(Some(&self.credentials), key).sign(SIGNED_FOR);
        let mut request = self.client.get(url);
        if let Some(range) = range {
            request = request.header("range", range);
        }
        Ok(request.send().await?)
    }

    /// The SHA-256 of what is at `key`, read through once (not held whole), and its size.
    pub async fn hash(&self, key: &str) -> anyhow::Result<(Vec<u8>, u64)> {
        use futures_util::StreamExt;
        use sha2::{Digest, Sha256};
        let answer = ok(self.fetch(key, None).await?, "reading a file back").await?;
        let mut stream = answer.bytes_stream();
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            size += chunk.len() as u64;
            hasher.update(&chunk);
        }
        Ok((hasher.finalize().to_vec(), size))
    }

    /// Removes what is at `key`; nothing there is no error.
    pub async fn delete(&self, key: &str) -> anyhow::Result<()> {
        let url = self.s3.delete_object(Some(&self.credentials), key).sign(SIGNED_FOR);
        let answer = self.client.delete(url).send().await?;
        if answer.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        ok(answer, "removing a file").await?;
        Ok(())
    }

    /// Every key under this bucket's prefix (and `more`), with its size.
    pub async fn list(&self, more: &str) -> anyhow::Result<Vec<(String, u64)>> {
        let mut found = Vec::new();
        let mut next: Option<String> = None;
        let prefix = self.key(more);
        loop {
            let mut action = self.s3.list_objects_v2(Some(&self.credentials));
            action.with_prefix(prefix.as_str());
            if let Some(token) = &next {
                action.with_continuation_token(token.as_str());
            }
            let text = ok(self.client.get(action.sign(SIGNED_FOR)).send().await?, "listing files").await?.text().await?;
            let listed = rusty_s3::actions::ListObjectsV2::parse_response(&text).map_err(|e| anyhow!("listing files: {e}"))?;
            found.extend(listed.contents.into_iter().map(|c| (c.key, c.size.unwrap_or(0))));
            match listed.next_continuation_token {
                Some(token) => next = Some(token),
                None => return Ok(found),
            }
        }
    }

    /// Removes everything under this bucket's prefix: a community gone for good.
    pub async fn delete_all(&self) -> anyhow::Result<usize> {
        let keys = self.list("").await?;
        for (key, _) in &keys {
            self.delete(key).await?;
        }
        Ok(keys.len())
    }

    /// Writes, reads back and removes a test file, and asks the way a browser
    /// would whether a download link may be fetched from `origin`. An error if the
    /// bucket can't be written to or read from; `cors` says the rest.
    pub async fn check(&self, origin: &str) -> anyhow::Result<Checked> {
        let key = self.key(&format!(".wyvrn-check-{}", uuid::Uuid::new_v4()));
        let said = b"wyvrn: can this bucket keep files?".to_vec();
        self.put_bytes(&key, said.clone()).await.context("writing a test file")?;
        let read = self.get(&key).await.context("reading the test file back");
        let link = self.link(&key, "application/octet-stream", "check");
        let preflight = self
            .client
            .request(reqwest::Method::OPTIONS, link)
            .header("origin", origin)
            .header("access-control-request-method", "GET")
            .header("access-control-request-headers", "range")
            .send()
            .await;
        self.delete(&key).await.context("removing the test file")?;
        if read? != said {
            bail!("the test file came back changed");
        }
        let cors = match preflight {
            Ok(answer) => {
                let allowed = answer.headers().get("access-control-allow-origin").and_then(|v| v.to_str().ok()).unwrap_or("");
                answer.status().is_success() && (allowed == "*" || allowed == origin)
            }
            Err(_) => false,
        };
        Ok(Checked { cors, origin: origin.to_string() })
    }
}

/// `Content-Disposition: attachment` for `filename`, quoted safely and with its
/// UTF-8 form beside it.
pub fn attachment(filename: &str) -> String {
    let plain: String = filename.chars().map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' }).collect();
    let encoded: String = filename
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    format!("attachment; filename=\"{plain}\"; filename*=UTF-8''{encoded}")
}

/// A prefix as a folder: no leading `/`, one trailing one (or none at all).
fn normal_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim_matches('/');
    if trimmed.is_empty() { String::new() } else { format!("{trimmed}/") }
}

fn tag_of(endpoint: &str, name: &str, prefix: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("{}|{name}|{prefix}", endpoint.trim_end_matches('/')).as_bytes());
    digest[..6].iter().map(|b| format!("{b:02x}")).collect()
}

/// The answer, if it says it worked; its words otherwise.
async fn ok(answer: reqwest::Response, doing: &str) -> anyhow::Result<reqwest::Response> {
    if answer.status().is_success() {
        return Ok(answer);
    }
    let status = answer.status();
    let text = answer.text().await.unwrap_or_default();
    // An S3 error's body names the bucket and key, never a credential.
    bail!("{doing}: {status} {}", text.chars().take(300).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> BucketConfig {
        BucketConfig {
            endpoint: "https://us-east-1.linodeobjects.com".into(),
            name: "wyvrn-files".into(),
            region: "us-east-1".into(),
            prefix: "/communities/c7/".into(),
            path_style: true,
            downloads: Downloads::Redirect,
            link_seconds: 300,
            key: "KEY".into(),
            secret: "SECRET".into(),
            key_env: None,
            secret_env: None,
        }
    }

    #[test]
    fn a_bucket_names_its_files_under_its_folder_and_is_tagged_by_where_that_is() {
        let bucket = Bucket::new(&config()).unwrap();
        assert_eq!(bucket.key("abc"), "communities/c7/abc");
        assert_eq!(bucket.tag().len(), 12);
        assert_eq!(Bucket::new(&config()).unwrap().tag(), bucket.tag());
        let other = Bucket::new(&BucketConfig { prefix: "communities/c8".into(), ..config() }).unwrap();
        assert_ne!(other.tag(), bucket.tag());
        assert_eq!(Bucket::new(&BucketConfig { prefix: String::new(), ..config() }).unwrap().key("abc"), "abc");
        // Neither the key nor the secret is ever printed.
        let shown = format!("{bucket:?}");
        assert!(!shown.contains("KEY") && !shown.contains("SECRET"), "{shown}");
    }

    #[test]
    fn a_download_link_is_signed_for_a_while_and_says_how_to_save_it() {
        let bucket = Bucket::new(&config()).unwrap();
        let link = bucket.link("communities/c7/abc", "image/png", "a picture.png").to_string();
        assert!(link.starts_with("https://us-east-1.linodeobjects.com/wyvrn-files/communities/c7/abc?"), "{link}");
        assert!(link.contains("X-Amz-Expires=300"), "{link}");
        assert!(link.contains("X-Amz-Signature="), "{link}");
        assert!(link.contains("response-content-type=image%2Fpng"), "{link}");
        assert!(link.contains("response-content-disposition=attachment"), "{link}");
        assert!(!link.contains("SECRET"), "{link}");
    }

    #[test]
    fn a_bucket_needs_its_key_and_secret() {
        assert!(Bucket::new(&BucketConfig { secret: String::new(), ..config() }).is_err());
    }

    #[test]
    fn a_file_name_is_given_safely() {
        assert_eq!(attachment("a.png"), "attachment; filename=\"a.png\"; filename*=UTF-8''a.png");
        assert_eq!(attachment("say \"hi\"\n.txt"), "attachment; filename=\"say _hi__.txt\"; filename*=UTF-8''say%20%22hi%22%0A.txt");
        assert_eq!(attachment("写真.jpg"), "attachment; filename=\"__.jpg\"; filename*=UTF-8''%E5%86%99%E7%9C%9F.jpg");
    }
}
