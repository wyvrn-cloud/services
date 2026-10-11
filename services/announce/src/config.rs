//! `config/default.toml` and friends.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub identity: IdentityConfig,
    pub store: StoreConfig,
    #[serde(default)]
    pub channel: ChannelConfig,
    /// A mediator to be reached through, for a channel with no address of its own
    /// that the people it serves can get to. Left out, the channel is reached
    /// directly at `server.public_url`.
    #[serde(default)]
    pub mediator: Option<MediatorConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MediatorConfig {
    /// The mediator's DID, e.g. `did:web:mediator.wyvrn.app`.
    pub did: String,
    /// How often the channel collects what the mediator holds for it.
    #[serde(default = "default_poll_seconds")]
    pub poll_seconds: u64,
}

fn default_poll_seconds() -> u64 {
    2
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
    pub public_url: String,
    #[serde(default)]
    pub did_method: DidMethod,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DidMethod {
    #[default]
    Peer,
    Web,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IdentityConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StoreConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelConfig {
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub picture: Option<PathBuf>,
    #[serde(default)]
    pub show_authors: bool,
    #[serde(default)]
    pub admins: Vec<String>,
    #[serde(default = "default_max_post_length")]
    pub max_post_length: usize,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            name: default_name(),
            description: String::new(),
            picture: None,
            show_authors: false,
            admins: Vec::new(),
            max_post_length: default_max_post_length(),
        }
    }
}

fn default_name() -> String {
    "Announcements".to_string()
}

fn default_max_post_length() -> usize {
    16_000
}

impl Config {
    /// Reads `path`, then lets the environment override what a deployment most often
    /// needs to: `ANNOUNCE_BIND`, `ANNOUNCE_PUBLIC_URL`, `ANNOUNCE_DID_METHOD`,
    /// `ANNOUNCE_NAME`, `ANNOUNCE_DESCRIPTION`, and `ANNOUNCE_ADMINS` (DIDs separated
    /// by commas or whitespace).
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
        let mut config: Self = toml::from_str(&text)?;
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        if let Some(bind) = env("ANNOUNCE_BIND") {
            config.server.bind = bind;
        }
        if let Some(url) = env("ANNOUNCE_PUBLIC_URL") {
            config.server.public_url = url;
        }
        if let Some(method) = env("ANNOUNCE_DID_METHOD") {
            config.server.did_method = match method.to_lowercase().as_str() {
                "peer" => DidMethod::Peer,
                "web" => DidMethod::Web,
                other => anyhow::bail!("ANNOUNCE_DID_METHOD is {other:?}; expected \"peer\" or \"web\""),
            };
        }
        if let Some(name) = env("ANNOUNCE_NAME") {
            config.channel.name = name;
        }
        if let Some(description) = env("ANNOUNCE_DESCRIPTION") {
            config.channel.description = description;
        }
        if let Some(admins) = env("ANNOUNCE_ADMINS") {
            config.channel.admins = admins
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|did| !did.is_empty())
                .map(str::to_string)
                .collect();
        }
        Ok(config)
    }
}
