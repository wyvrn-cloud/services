//! `config/default.toml` and friends.

use std::path::{Path, PathBuf};

use serde::Deserialize;
pub use wyvrn_service::{address::DidMethod, mediator::MediatorConfig};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub identity: PathConfig,
    pub store: PathConfig,
    pub host: HostConfig,
    /// A mediator for the host's own DIDComm endpoint to be reached through. The
    /// DIDs it hosts are served over HTTPS either way, so the host itself always
    /// needs a public web address; this only moves where messages to it arrive.
    #[serde(default)]
    pub mediator: Option<MediatorConfig>,
    /// Who the service says it is, to someone looking its DID up: a name, a
    /// description and a picture, each optional.
    #[serde(default)]
    pub profile: wyvrn_service::address::ProfileConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
    /// Where the host's own DIDComm endpoint is.
    pub public_url: String,
    #[serde(default)]
    pub did_method: DidMethod,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PathConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HostConfig {
    /// The host as it appears in the DIDs it hosts: `id.example`, or with a port
    /// `id.example%3A8443`. `did:webvh` wants a domain name here, not an address.
    pub domain: String,
    /// How long a name is held for whoever asked for it, to publish under.
    #[serde(default = "default_hold_seconds")]
    pub hold_seconds: i64,
    /// How many labels a name may have: 2 allows `official:news`.
    #[serde(default = "default_max_labels")]
    pub max_labels: usize,
    /// Whether a DID's first entry has to say it may move to another host.
    #[serde(default = "yes")]
    pub require_portable: bool,
    #[serde(default = "default_max_log_bytes")]
    pub max_log_bytes: usize,
    #[serde(default = "default_max_whois_bytes")]
    pub max_whois_bytes: usize,
    /// The DIDs naming decisions are taken from.
    #[serde(default)]
    pub admins: Vec<String>,
    /// Names nobody may claim unless an admin assigns them.
    #[serde(default = "default_reserved_names")]
    pub reserved_names: Vec<String>,
    /// Leading labels under which every name is reserved.
    #[serde(default = "default_reserved_namespaces")]
    pub reserved_namespaces: Vec<String>,
    /// The namespaces new names may be claimed in (did-hosting/1.0, "Namespaces by
    /// kind"): `user` for people's apps, `community` for community servers. Empty,
    /// a name may be anything. Names already held are left where they are.
    #[serde(default)]
    pub open_namespaces: Vec<String>,
    /// Namespaces held for one kind of agent: the protocol it must disclose, and
    /// in what role, before a name there is held for it.
    #[serde(default = "default_kinds")]
    pub kinds: std::collections::BTreeMap<String, Kind>,
    /// How long without hearing from a holder before it is asked whether it is
    /// still there, and how long it then has to answer.
    #[serde(default = "default_ask_after_days")]
    pub ask_after_days: i64,
    #[serde(default = "default_answer_within_days")]
    pub answer_within_days: i64,
}

/// What an agent must disclose (discover-features/2.0) to claim a name in a namespace.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Kind {
    pub protocol: String,
    pub role: String,
}

fn default_kinds() -> std::collections::BTreeMap<String, Kind> {
    [("community".to_string(), Kind { protocol: "https://wyvrn.app/community/1.0".to_string(), role: "server".to_string() })].into()
}

fn default_hold_seconds() -> i64 {
    600
}
fn default_max_labels() -> usize {
    2
}
fn yes() -> bool {
    true
}
fn default_max_log_bytes() -> usize {
    256 * 1024
}
fn default_max_whois_bytes() -> usize {
    128 * 1024
}
fn default_ask_after_days() -> i64 {
    365
}
fn default_answer_within_days() -> i64 {
    7
}
/// The host's own routes, and names that would pass for the host itself.
fn default_reserved_names() -> Vec<String> {
    ["did", "healthz", "api", "admin", "administrator", "root", "support", "help", "security", "abuse", "wyvrn", "staff", "system", "official"]
        .map(str::to_string)
        .to_vec()
}
fn default_reserved_namespaces() -> Vec<String> {
    vec!["official".to_string()]
}

impl Config {
    /// Reads `path`, then lets the environment override what a deployment most
    /// often needs to: `DID_HOST_BIND`, `DID_HOST_PUBLIC_URL`,
    /// `DID_HOST_DID_METHOD`, `DID_HOST_DOMAIN`, `DID_HOST_ADMINS` (DIDs
    /// separated by commas or whitespace) and `DID_HOST_OPEN_NAMESPACES` (labels,
    /// likewise; `*` for any name).
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
        let mut config: Self = toml::from_str(&text)?;
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        if let Some(bind) = env("DID_HOST_BIND") {
            config.server.bind = bind;
        }
        if let Some(url) = env("DID_HOST_PUBLIC_URL") {
            config.server.public_url = url;
        }
        if let Some(method) = env("DID_HOST_DID_METHOD") {
            config.server.did_method = DidMethod::parse(&method, "DID_HOST_DID_METHOD")?;
        }
        if let Some(domain) = env("DID_HOST_DOMAIN") {
            config.host.domain = domain;
        }
        if let Some(open) = env("DID_HOST_OPEN_NAMESPACES") {
            config.host.open_namespaces = open.split(|c: char| c == ',' || c.is_whitespace()).filter(|label| !label.is_empty() && *label != "*").map(str::to_string).collect();
        }
        if let Some(admins) = env("DID_HOST_ADMINS") {
            config.host.admins = admins.split(|c: char| c == ',' || c.is_whitespace()).filter(|did| !did.is_empty()).map(str::to_string).collect();
        }
        Ok(config)
    }
}
