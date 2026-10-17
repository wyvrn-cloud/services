//! `config/default.toml` and friends.

use std::path::{Path, PathBuf};

use serde::Deserialize;
pub use wyvrn_service::{address::DidMethod, mediator::MediatorConfig};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub identity: PathConfig,
    pub store: StoreConfig,
    #[serde(default)]
    pub service: ServiceConfig,
    /// A mediator for the service's DIDComm endpoint to be reached through. Files
    /// are put and fetched over HTTPS either way, so it always needs a public web
    /// address; this only moves where messages to it arrive.
    #[serde(default)]
    pub mediator: Option<MediatorConfig>,
    /// Who the service says it is, to someone looking its DID up.
    #[serde(default)]
    pub profile: wyvrn_service::address::ProfileConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
    /// Where the service is: its DIDComm endpoint, and the start of every file's address.
    pub public_url: String,
    #[serde(default)]
    pub did_method: DidMethod,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PathConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StoreConfig {
    /// The records (SQLite), unless the environment names a database.
    pub path: PathBuf,
    /// The bytes.
    pub dir: PathBuf,
}

/// Who may upload, and the limits on what is held.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServiceConfig {
    pub uploaders: Vec<String>,
    pub quota_bytes: u64,
    pub max_file_bytes: u64,
    pub chunk_bytes: u64,
    pub default_keep_seconds: i64,
    pub max_keep_seconds: i64,
    pub upload_deadline_seconds: i64,
    pub max_uploads_in_progress: usize,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            uploaders: Vec::new(),
            quota_bytes: 10 * 1024 * 1024 * 1024,
            max_file_bytes: 4 * 1024 * 1024 * 1024,
            chunk_bytes: 64 * 1024 * 1024,
            default_keep_seconds: 14 * 24 * 60 * 60,
            max_keep_seconds: 90 * 24 * 60 * 60,
            upload_deadline_seconds: 24 * 60 * 60,
            max_uploads_in_progress: 16,
        }
    }
}

impl Config {
    /// Reads `path`, then lets the environment override what a deployment most
    /// often needs to: `FILES_BIND`, `FILES_PUBLIC_URL`, `FILES_DID_METHOD`,
    /// `FILES_DIR`, `FILES_UPLOADERS` (DIDs separated by commas or whitespace),
    /// `FILES_QUOTA_BYTES` and `FILES_MAX_FILE_BYTES`.
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
        let mut config: Self = toml::from_str(&text)?;
        config.apply_env()?;
        Ok(config)
    }

    pub fn apply_env(&mut self) -> anyhow::Result<()> {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        if let Some(bind) = env("FILES_BIND") {
            self.server.bind = bind;
        }
        if let Some(url) = env("FILES_PUBLIC_URL") {
            self.server.public_url = url;
        }
        if let Some(method) = env("FILES_DID_METHOD") {
            self.server.did_method = DidMethod::parse(&method, "FILES_DID_METHOD")?;
        }
        if let Some(dir) = env("FILES_DIR") {
            self.store.dir = dir.into();
        }
        if let Some(uploaders) = env("FILES_UPLOADERS") {
            self.service.uploaders = uploaders.split(|c: char| c == ',' || c.is_whitespace()).filter(|did| !did.is_empty()).map(str::to_string).collect();
        }
        for (name, setting) in [("FILES_QUOTA_BYTES", &mut self.service.quota_bytes), ("FILES_MAX_FILE_BYTES", &mut self.service.max_file_bytes)] {
            if let Some(value) = env(name) {
                *setting = value.trim().parse().map_err(|e| anyhow::anyhow!("{name} is {value:?}: {e}"))?;
            }
        }
        if !self.server.public_url.ends_with('/') {
            self.server.public_url.push('/');
        }
        Ok(())
    }
}
