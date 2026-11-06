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
    /// The community's own sticker and emoji packs (stickers/1.0's service): left
    /// out, none.
    #[serde(default)]
    pub stickers: Option<StickersConfig>,
    /// Files members attach to posts, kept here (files/1.0's host): left out, none.
    #[serde(default)]
    pub files: Option<FilesConfig>,
    /// An AI assistant answering in every channel (wyvrn-claude; assistant.rs).
    /// The community server itself leaves it out.
    #[serde(default)]
    pub assistant: Option<crate::assistant::AssistantConfig>,
    /// The web view (web/): the community's page and the channels its admins make
    /// public, at `public_url` (the server's own address unless set).
    #[serde(default)]
    pub web: WebConfig,
}

/// The web view (community/1.0's "On the web").
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// Whether the server shows anything on the web at all. Whether a community's
    /// page is on is then its admins' (`set-web`).
    pub enabled: bool,
    /// Where the web view is: any origin and path, ending in `/`. The server's
    /// own `public_url` when left out. Every absolute address in its pages, and
    /// the identity provider's issuer, is made from this and never from a request.
    pub public_url: Option<String>,
    /// The reverse proxies in front of it, as addresses or ranges (`10.0.0.0/8`,
    /// `::1`): only a request from one of these is believed about whom it is for
    /// (`X-Forwarded-For`), which rate limits go by.
    pub trusted_proxies: Vec<String>,
    /// Whether pages send `Strict-Transport-Security`.
    pub hsts: bool,
    /// The issuers of website-sign-in/1.0's credential the web view trusts.
    pub sign_in_issuers: Vec<String>,
    /// How many requests one address may make of the web view in a minute.
    pub requests_per_minute: u32,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self { enabled: true, public_url: None, trusted_proxies: Vec::new(), hsts: false, sign_in_issuers: vec!["did:web:verify.wyvrn.app".to_string()], requests_per_minute: 300 }
    }
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

/// The community's own packs: where they are kept (`COMMUNITY_STICKERS_DATABASE_URL`
/// names a database instead).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StickersConfig {
    pub path: std::path::PathBuf,
}

impl Default for StickersConfig {
    fn default() -> Self {
        Self { path: "stickers.sqlite".into() }
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
    /// Its owners, all equal (`COMMUNITY_OWNERS`): every permission, and nobody
    /// may take them out, ban them or time them out. Each is an admin too. Set
    /// here only, never from the app; none named, the owner is the one handed it
    /// (`Community::owners`).
    pub owners: Vec<String>,
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
    /// As many members as there may be, those who left included; 0 for no limit.
    /// Past it nobody more is let in (`e.p.req.community-full`).
    pub max_members: usize,
}

impl CommunityConfig {
    /// Each owner is an admin too, as named in `admins`.
    pub fn owners_are_admins(&mut self) {
        for owner in &self.owners {
            if !self.admins.contains(owner) {
                self.admins.push(owner.clone());
            }
        }
    }
}

impl Default for CommunityConfig {
    fn default() -> Self {
        Self {
            name: "A community".to_string(),
            description: String::new(),
            admins: Vec::new(),
            owners: Vec::new(),
            members: Vec::new(),
            members_may_create_channels: false,
            max_post_length: 16_000,
            max_channels: 200,
            admission: "admin".to_string(),
            members_may_invite: false,
            max_waiting: 500,
            max_posts_per_minute: 30,
            max_members: 0,
        }
    }
}

fn dids(list: &str) -> Vec<String> {
    list.split(|c: char| c == ',' || c.is_whitespace()).filter(|did| !did.is_empty()).map(str::to_string).collect()
}

impl Config {
    /// Reads `path`, then lets the environment override what a deployment most
    /// often needs to: `COMMUNITY_BIND`, `COMMUNITY_PUBLIC_URL`,
    /// `COMMUNITY_DID_METHOD`, `COMMUNITY_NAME`, and `COMMUNITY_ADMINS`,
    /// `COMMUNITY_OWNERS` and `COMMUNITY_MEMBERS` (DIDs separated by commas or whitespace), and for voice
    /// channels `COMMUNITY_VOICE_RELAY_URL`, `COMMUNITY_VOICE_RELAY_FINGERPRINT` and
    /// `COMMUNITY_VOICE_TRANSPORT`; for the files members attach `COMMUNITY_FILES_DIR`
    /// and `COMMUNITY_FILES_MAX_BYTES`, for the community's own packs
    /// `COMMUNITY_STICKERS_PATH`, and for the web view `COMMUNITY_WEB` (`off`),
    /// `COMMUNITY_WEB_URL` and `COMMUNITY_WEB_TRUSTED_PROXIES`.
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
        if let Some(owners) = env("COMMUNITY_OWNERS") {
            config.community.owners = dids(&owners);
        }
        config.community.owners_are_admins();
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
        // Files members attach, turned on by naming where they are kept.
        if let Some(dir) = env("COMMUNITY_FILES_DIR") {
            config.files.get_or_insert_with(FilesConfig::default).dir = dir.into();
        }
        if let (Some(files), Some(most)) = (config.files.as_mut(), env("COMMUNITY_FILES_MAX_BYTES")) {
            files.max_file_bytes = most.trim().parse().map_err(|e| anyhow::anyhow!("COMMUNITY_FILES_MAX_BYTES is {most:?}: {e}"))?;
        }
        // The community's own packs, likewise.
        if let Some(path) = env("COMMUNITY_STICKERS_PATH") {
            config.stickers.get_or_insert_with(StickersConfig::default).path = path.into();
        }
        // The web view: COMMUNITY_WEB=off turns it off; COMMUNITY_WEB_URL says where it is.
        if let Some(web) = env("COMMUNITY_WEB") {
            config.web.enabled = !matches!(web.trim().to_ascii_lowercase().as_str(), "off" | "false" | "0" | "no");
        }
        if let Some(url) = env("COMMUNITY_WEB_URL") {
            config.web.public_url = Some(url);
        }
        if let Some(proxies) = env("COMMUNITY_WEB_TRUSTED_PROXIES") {
            config.web.trusted_proxies = dids(&proxies);
        }
        if !config.server.public_url.ends_with('/') {
            config.server.public_url.push('/');
        }
        Ok(config)
    }
}
