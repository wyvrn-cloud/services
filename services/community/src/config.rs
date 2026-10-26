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
    /// Files members attach to posts, kept here (files/1.0's host): left out, none.
    #[serde(default)]
    pub files: Option<FilesConfig>,
    /// An AI assistant answering in every channel (wyvrn-claude; assistant.rs).
    /// The community server itself leaves it out.
    #[serde(default)]
    pub assistant: Option<crate::assistant::AssistantConfig>,
}

/// Files members attach (community/1.0's "Files, stickers…"): where they are kept,
/// and how much. Their records are in `dir/files.sqlite`, unless the environment
/// names a database for them (`COMMUNITY_FILES_DATABASE_URL`; with only
/// `DATABASE_URL` set they share the community's).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FilesConfig {
    pub dir: std::path::PathBuf,
    /// The largest file a member may attach.
    pub max_file_bytes: u64,
    /// The most one member may keep here in all.
    pub quota_bytes: u64,
    /// The most kept of everyone's; 0 for no limit but the disk.
    pub total_bytes: u64,
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self { dir: "files".into(), max_file_bytes: 25 * 1024 * 1024, quota_bytes: 1024 * 1024 * 1024, total_bytes: 0 }
    }
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
    /// As many posts as one member makes in a minute, in all channels; 0 for no
    /// limit. Whoever holds `administrator` isn't held to it.
    pub max_posts_per_minute: usize,
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
            max_posts_per_minute: 30,
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
    /// `COMMUNITY_MEMBERS` (DIDs separated by commas or whitespace), and for voice
    /// channels `COMMUNITY_VOICE_RELAY_URL`, `COMMUNITY_VOICE_RELAY_FINGERPRINT` and
    /// `COMMUNITY_VOICE_TRANSPORT`.
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
        // Voice channels, turned on by naming the relay members connect to.
        if let Some(relay_url) = env("COMMUNITY_VOICE_RELAY_URL") {
            let voice = config.voice.get_or_insert_with(|| VoiceConfig { transport: default_transport(), relay_url: String::new(), relay_fingerprint: None });
            voice.relay_url = relay_url;
            if let Some(fingerprint) = env("COMMUNITY_VOICE_RELAY_FINGERPRINT") {
                voice.relay_fingerprint = Some(fingerprint);
            }
            if let Some(transport) = env("COMMUNITY_VOICE_TRANSPORT") {
                voice.transport = transport;
            }
        }
        if !config.server.public_url.ends_with('/') {
            config.server.public_url.push('/');
        }
        Ok(config)
    }
}
