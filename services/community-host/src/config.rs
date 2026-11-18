//! `config/default.toml` and friends.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use wyvrn_community::config::VoiceConfig;
pub use wyvrn_service::address::DidMethod;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    /// The host's own keys: its DID, which its admins write to.
    pub identity: PathConfig,
    /// The host's own store: which communities it keeps, and where.
    pub store: PathConfig,
    #[serde(default)]
    pub host: HostConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    /// What each community may hold, unless an admin sets otherwise for it.
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub loading: LoadingConfig,
    /// Voice channels, for every community: the one relay they share.
    #[serde(default)]
    pub voice: Option<VoiceConfig>,
    /// Every community on the web, each at its own path (`<public_url>c/<name>/`):
    /// `public_url` is ignored here.
    #[serde(default)]
    pub web: wyvrn_community::config::WebConfig,
    /// Each community's owners beside the one it was made for, by its name: all
    /// equal, and admins too. Set here only, never from the app:
    ///
    /// ```toml
    /// [owners]
    /// saikous = ["did:peer:2...", "did:peer:2..."]
    /// ```
    #[serde(default)]
    pub owners: std::collections::BTreeMap<String, Vec<String>>,
    /// Who may make a community here from their app (creating.rs), until a host
    /// admin says otherwise on the admin page.
    #[serde(default)]
    pub creating: CreatingConfig,
    /// Each community backed up on a schedule, sealed with a key (backups.rs).
    #[serde(default)]
    pub backups: BackupsConfig,
}

/// Backups: where (`dir`, empty for `<storage.dir>/.backups`), how often, how many
/// kept, and the key they are sealed with (`key`, a file of 64 hex digits, or
/// `COMMUNITY_HOST_BACKUP_KEY`); none, no backups.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BackupsConfig {
    pub dir: PathBuf,
    pub every_hours: i64,
    pub keep: usize,
    pub key: PathBuf,
}

impl Default for BackupsConfig {
    fn default() -> Self {
        Self { dir: PathBuf::new(), every_hours: 24, keep: 7, key: PathBuf::new() }
    }
}

impl BackupsConfig {
    pub fn dir_or(&self, storage: &Path) -> PathBuf {
        if self.dir.as_os_str().is_empty() { storage.join(".backups") } else { self.dir.clone() }
    }
}

/// Making a community from the app: `closed` (host admins only), `codes` (with a
/// code a host admin hands out) or `anyone`; as many as one person may have made
/// here, and how long they wait between two.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CreatingConfig {
    pub mode: String,
    pub per_owner: i64,
    pub every_seconds: i64,
}

impl Default for CreatingConfig {
    fn default() -> Self {
        Self { mode: "closed".into(), per_owner: 3, every_seconds: 600 }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
    /// Where the host is reached. A community is at `<public_url>c/<name>/`, and
    /// is the `did:web` there.
    pub public_url: String,
    /// `web` (the default): the host and each community are the `did:web` at their
    /// address, which has to be HTTPS. `peer`, for a host without it (development,
    /// tests): each a `did:peer:4` naming its address.
    #[serde(default = "web")]
    pub did_method: DidMethod,
}

fn web() -> DidMethod {
    DidMethod::Web
}

#[derive(Debug, Clone, Deserialize)]
pub struct PathConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HostConfig {
    /// Who may make, change and remove communities here: DIDs, or public addresses.
    pub admins: Vec<String>,
    /// The domain of the DID host each community holds its public address at
    /// (`community:<name>`); empty for none.
    pub did_host: String,
}

/// Where a community's stores go: what [`crate::registry::Storage`] names.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// What a new community is given unless said otherwise: `sqlite`, `mysql-db`
    /// or `mysql-shared`.
    pub default: String,
    /// Each community's folder: its SQLite files and keys (for `sqlite`), and the
    /// files its members attach (for every kind).
    pub dir: PathBuf,
    /// The MySQL server, without a database: `mysql://user:password@host:3306`.
    pub mysql: String,
    /// What a `mysql-db` community's database is called: this, then its number.
    pub database_prefix: String,
    /// The database `mysql-shared` communities share, each in tables of its own.
    pub shared_database: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self { default: "sqlite".into(), dir: "communities".into(), mysql: String::new(), database_prefix: "wyvrn_c".into(), shared_database: "wyvrn_communities".into() }
    }
}

/// What one community may hold. Each refused where it is met, as community/1.0's
/// "Hosted communities, and moving" says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Limits {
    /// Members, those who left included.
    pub members: usize,
    /// Channels that aren't closed.
    pub channels: usize,
    /// The largest file a member may attach.
    pub file_bytes: u64,
    /// The most one member may keep.
    pub files_quota: u64,
    /// The most kept of everyone's.
    pub files_total: u64,
    /// Roles (at most 250).
    pub roles: usize,
    /// Posts a minute, from everyone; 0 for no limit.
    pub posts_per_minute: usize,
    /// What its store may hold, in bytes (as measured every few minutes); 0 for no
    /// limit. Files have limits of their own.
    pub store_bytes: u64,
    /// The community's own sticker packs, and stickers in each.
    pub sticker_packs: usize,
    pub stickers_per_pack: usize,
    /// Its own custom emoji packs, and emoji in each.
    pub emoji_packs: usize,
    pub emoji_per_pack: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { members: 1000, channels: 500, file_bytes: 25 * 1024 * 1024, files_quota: 1024 * 1024 * 1024, files_total: 5 * 1024 * 1024 * 1024, roles: 250, posts_per_minute: 0, store_bytes: 0, sticker_packs: 1, stickers_per_pack: 25, emoji_packs: 1, emoji_per_pack: 75 }
    }
}

/// How many communities are kept open, and for how long after the last request.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LoadingConfig {
    pub max_loaded: usize,
    pub idle_seconds: i64,
}

impl Default for LoadingConfig {
    fn default() -> Self {
        Self { max_loaded: 200, idle_seconds: 600 }
    }
}

fn dids(list: &str) -> Vec<String> {
    list.split(|c: char| c == ',' || c.is_whitespace()).filter(|did| !did.is_empty()).map(str::to_string).collect()
}

impl Config {
    /// Reads `path`, then lets the environment override what a deployment most
    /// often needs to: `COMMUNITY_HOST_BIND`, `COMMUNITY_HOST_PUBLIC_URL`,
    /// `COMMUNITY_HOST_ADMINS`, `COMMUNITY_HOST_DID_HOST`, `COMMUNITY_HOST_STORAGE`
    /// (the default kind), `COMMUNITY_HOST_DIR`, `COMMUNITY_HOST_MYSQL_URL`, and
    /// `COMMUNITY_HOST_VOICE_RELAY_URL` with `COMMUNITY_HOST_VOICE_RELAY_FINGERPRINT`.
    /// The host's own store is `COMMUNITY_HOST_DATABASE_URL` if set ([`crate::registry`]).
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
        let mut config: Self = toml::from_str(&text)?;
        let env = |name: &str| std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        if let Some(bind) = env("COMMUNITY_HOST_BIND") {
            config.server.bind = bind;
        }
        if let Some(url) = env("COMMUNITY_HOST_PUBLIC_URL") {
            config.server.public_url = url;
        }
        if let Some(method) = env("COMMUNITY_HOST_DID_METHOD") {
            config.server.did_method = DidMethod::parse(&method, "COMMUNITY_HOST_DID_METHOD")?;
        }
        if let Some(admins) = env("COMMUNITY_HOST_ADMINS") {
            config.host.admins = dids(&admins);
        }
        if let Some(did_host) = env("COMMUNITY_HOST_DID_HOST") {
            config.host.did_host = did_host;
        }
        if let Some(kind) = env("COMMUNITY_HOST_STORAGE") {
            config.storage.default = kind;
        }
        if let Some(dir) = env("COMMUNITY_HOST_DIR") {
            config.storage.dir = dir.into();
        }
        if let Some(mysql) = env("COMMUNITY_HOST_MYSQL_URL") {
            config.storage.mysql = mysql;
        }
        if let Some(relay_url) = env("COMMUNITY_HOST_VOICE_RELAY_URL") {
            let voice = config.voice.get_or_insert_with(|| VoiceConfig { transport: "moq".into(), relay_url: String::new(), relay_fingerprint: None });
            voice.relay_url = relay_url;
            if let Some(fingerprint) = env("COMMUNITY_HOST_VOICE_RELAY_FINGERPRINT") {
                voice.relay_fingerprint = Some(fingerprint);
            }
        }
        config.settle();
        Ok(config)
    }

    /// What every reading of a configuration does after it: the public URL ends
    /// with `/`.
    pub fn settle(&mut self) {
        if !self.server.public_url.ends_with('/') {
            self.server.public_url.push('/');
        }
    }
}
