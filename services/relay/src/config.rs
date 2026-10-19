//! `config/default.toml` and friends.

use std::path::{Path, PathBuf};

use serde::Deserialize;
pub use wyvrn_service::address::DidMethod;

use crate::relay::Carries;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub identity: PathConfig,
    pub store: PathConfig,
    #[serde(default)]
    pub relay: RelayConfig,
    /// Who the relay says it is, to someone looking its DID up.
    #[serde(default)]
    pub profile: wyvrn_service::address::ProfileConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
    pub public_url: String,
    #[serde(default)]
    pub did_method: DidMethod,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PathConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RelayConfig {
    pub carries: Carries,
    pub open: bool,
    pub clients: Vec<String>,
    pub allow: Vec<String>,
    pub max_bytes: usize,
    pub timeout_seconds: u64,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self { carries: Carries::All, open: true, clients: Vec::new(), allow: Vec::new(), max_bytes: 16 * 1024 * 1024, timeout_seconds: 30 }
    }
}

impl Config {
    /// Reads `path`, then lets the environment override what a deployment most
    /// often needs to: `RELAY_BIND`, `RELAY_PUBLIC_URL`, `RELAY_DID_METHOD`,
    /// `RELAY_CARRIES` (`sends` or `all`), `RELAY_OPEN` (`true` or `false`),
    /// `RELAY_CLIENTS` (DIDs) and `RELAY_ALLOW` (hosts), separated by commas or
    /// whitespace, and `RELAY_MAX_BYTES`.
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
        let mut config: Self = toml::from_str(&text)?;
        config.apply_env()?;
        Ok(config)
    }

    pub fn apply_env(&mut self) -> anyhow::Result<()> {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        if let Some(bind) = env("RELAY_BIND") {
            self.server.bind = bind;
        }
        if let Some(url) = env("RELAY_PUBLIC_URL") {
            self.server.public_url = url;
        }
        if let Some(method) = env("RELAY_DID_METHOD") {
            self.server.did_method = DidMethod::parse(&method, "RELAY_DID_METHOD")?;
        }
        if let Some(carries) = env("RELAY_CARRIES") {
            self.relay.carries = match carries.trim() {
                "sends" => Carries::Sends,
                "all" => Carries::All,
                other => anyhow::bail!("RELAY_CARRIES is {other:?}: say sends or all"),
            };
        }
        if let Some(open) = env("RELAY_OPEN") {
            self.relay.open = match open.trim() {
                "true" | "1" | "yes" => true,
                "false" | "0" | "no" => false,
                other => anyhow::bail!("RELAY_OPEN is {other:?}: say true or false"),
            };
        }
        let list = |text: String| text.split(|c: char| c == ',' || c.is_whitespace()).filter(|one| !one.is_empty()).map(str::to_string).collect();
        if let Some(clients) = env("RELAY_CLIENTS") {
            self.relay.clients = list(clients);
        }
        if let Some(allow) = env("RELAY_ALLOW") {
            self.relay.allow = list(allow);
        }
        if let Some(max) = env("RELAY_MAX_BYTES") {
            self.relay.max_bytes = max.trim().parse().map_err(|e| anyhow::anyhow!("RELAY_MAX_BYTES is {max:?}: {e}"))?;
        }
        if !self.server.public_url.ends_with('/') {
            self.server.public_url.push('/');
        }
        Ok(())
    }
}
