//! `config/default.toml` and friends.

use std::path::{Path, PathBuf};

use serde::Deserialize;
pub use wyvrn_service::{address::DidMethod, mediator::MediatorConfig};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub identity: PathConfig,
    pub store: PathConfig,
    #[serde(default)]
    pub service: ServiceConfig,
    /// A mediator for the service's DIDComm endpoint to be reached through. Packs
    /// are served over HTTPS either way, so it always needs a public web address;
    /// this only moves where messages to it arrive.
    #[serde(default)]
    pub mediator: Option<MediatorConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
    /// Where the service is: its DIDComm endpoint, and the base of every pack's address.
    pub public_url: String,
    #[serde(default)]
    pub did_method: DidMethod,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PathConfig {
    pub path: PathBuf,
}

/// The limits on what is hosted. A sticker is at most `max_sticker_px` on a side;
/// a custom emoji, which is drawn at the size of text, much smaller.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServiceConfig {
    pub admins: Vec<String>,
    pub max_sticker_px: u32,
    pub max_sticker_bytes: usize,
    pub max_emoji_px: u32,
    pub max_emoji_bytes: usize,
    pub max_per_pack: usize,
    /// How many packs one maker may have.
    pub max_packs_per_maker: usize,
    /// How long a pack that was started and never published is kept.
    pub draft_hours: i64,
    /// Names no pack may take: the service's own routes, and names that would
    /// pass for the service itself.
    pub reserved_names: Vec<String>,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            admins: Vec::new(),
            max_sticker_px: 512,
            max_sticker_bytes: 500 * 1024,
            max_emoji_px: 128,
            max_emoji_bytes: 128 * 1024,
            max_per_pack: 120,
            max_packs_per_maker: 50,
            draft_hours: 24,
            reserved_names: ["did", "healthz", "drafts", "api", "admin", "official", "wyvrn", "support", "help", "abuse", "security", "staff", "system"]
                .map(str::to_string)
                .to_vec(),
        }
    }
}

impl Config {
    /// Reads `path`, then lets the environment override what a deployment most
    /// often needs to: `STICKERS_BIND`, `STICKERS_PUBLIC_URL`,
    /// `STICKERS_DID_METHOD`, and `STICKERS_ADMINS` (DIDs separated by commas or
    /// whitespace).
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
        let mut config: Self = toml::from_str(&text)?;
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        if let Some(bind) = env("STICKERS_BIND") {
            config.server.bind = bind;
        }
        if let Some(url) = env("STICKERS_PUBLIC_URL") {
            config.server.public_url = url;
        }
        if let Some(method) = env("STICKERS_DID_METHOD") {
            config.server.did_method = DidMethod::parse(&method, "STICKERS_DID_METHOD")?;
        }
        if let Some(admins) = env("STICKERS_ADMINS") {
            config.service.admins = admins.split(|c: char| c == ',' || c.is_whitespace()).filter(|did| !did.is_empty()).map(str::to_string).collect();
        }
        if !config.server.public_url.ends_with('/') {
            config.server.public_url.push('/');
        }
        Ok(config)
    }
}
