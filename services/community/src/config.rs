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
    pub community: CommunityConfig,
    /// A mediator for the server to be reached through.
    #[serde(default)]
    pub mediator: Option<MediatorConfig>,
    /// Who the service says it is, to someone looking its DID up: a name, a
    /// description and a picture, each optional.
    #[serde(default)]
    pub profile: wyvrn_service::address::ProfileConfig,
    /// Voice channels (community-voice/1.0): left out, there are none.
    #[serde(default)]
    pub voice: Option<VoiceConfig>,
}

/// Where voice channels' media is forwarded, and how its tokens are signed.
#[derive(Debug, Clone, Deserialize)]
pub struct VoiceConfig {
    /// The transport a voice channel uses unless it or an admin says otherwise:
    /// `moq` or `webrtc`.
    #[serde(default = "default_transport")]
    pub transport: String,
    /// The moq relay's public address, which members connect to (`https://…`).
    #[serde(default)]
    pub relay_url: String,
    /// The SHA-256 of a self-signed relay certificate, hex, for members to pin.
    #[serde(default)]
    pub relay_fingerprint: Option<String>,
    /// The moq-auth signing key (a JWK file) tokens are signed with; the relay is
    /// configured with its public half. Made on first start if missing.
    #[serde(default)]
    pub token_key: Option<PathBuf>,
}

fn default_transport() -> String {
    "moq".to_string()
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
pub struct CommunityConfig {
    pub name: String,
    pub description: String,
    pub admins: Vec<String>,
    pub members: Vec<String>,
    pub members_may_create_channels: bool,
    /// The longest a post may be, in characters.
    pub max_post_length: usize,
    /// How many channels there may be that aren't closed.
    pub max_channels: usize,
    /// Who a `join` from someone who isn't a member lets in, until an admin says
    /// otherwise (`set-admission`): `admin` (nobody), `invite` or `open`.
    pub admission: String,
    /// Whether a member who isn't an admin may make invites, until an admin says
    /// otherwise.
    pub members_may_invite: bool,
    /// As many as may wait to be let in at once.
    pub max_waiting: usize,
}

impl Default for CommunityConfig {
    fn default() -> Self {
        Self {
            name: "A community".to_string(),
            description: String::new(),
            admins: Vec::new(),
            members: Vec::new(),
            members_may_create_channels: false,
            max_post_length: 16_000,
            max_channels: 200,
            admission: "admin".to_string(),
            members_may_invite: false,
            max_waiting: 500,
        }
    }
}

fn dids(list: &str) -> Vec<String> {
    list.split(|c: char| c == ',' || c.is_whitespace()).filter(|did| !did.is_empty()).map(str::to_string).collect()
}

impl Config {
    /// Reads `path`, then lets the environment override what a deployment most
    /// often needs to: `COMMUNITY_BIND`, `COMMUNITY_PUBLIC_URL`,
    /// `COMMUNITY_DID_METHOD`, `COMMUNITY_NAME`, and `COMMUNITY_ADMINS` and
    /// `COMMUNITY_MEMBERS` (DIDs separated by commas or whitespace).
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
        let mut config: Self = toml::from_str(&text)?;
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        if let Some(bind) = env("COMMUNITY_BIND") {
            config.server.bind = bind;
        }
        if let Some(url) = env("COMMUNITY_PUBLIC_URL") {
            config.server.public_url = url;
        }
        if let Some(method) = env("COMMUNITY_DID_METHOD") {
            config.server.did_method = DidMethod::parse(&method, "COMMUNITY_DID_METHOD")?;
        }
        if let Some(name) = env("COMMUNITY_NAME") {
            config.community.name = name;
        }
        if let Some(admins) = env("COMMUNITY_ADMINS") {
            config.community.admins = dids(&admins);
        }
        if let Some(members) = env("COMMUNITY_MEMBERS") {
            config.community.members = dids(&members);
        }
        if !config.server.public_url.ends_with('/') {
            config.server.public_url.push('/');
        }
        Ok(config)
    }
}
