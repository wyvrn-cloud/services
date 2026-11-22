//! A Discord server's history moved into a community, from DiscordChatExporter's
//! JSON (one file per channel or thread, `--media` beside it in `<file>_Files/`).
//!
//! - [`survey`] reads a whole backup and says what is in it and what importing it
//!   would take: messages, attachments (uploaded, or links to elsewhere), GIFs from
//!   Giphy, Klipy and Tenor, the disk space.
//! - [`plan`] matches the channels of the files chosen to the community's own (by
//!   category and name, as a Discord template import made them, or as told), for
//!   the owner to look over; [`write`] writes their posts into the store.
//!
//! Imported posts are written straight into the store, never through posting: no
//! AutoMod, no limits, nothing sent to anyone. Each keeps its Discord time as its
//! `posted_at` and takes the channel's next numbers, so it is read by time among
//! the posts already there (`history-request`'s `before_at`), and its numbers are
//! recorded (`imported_ranges`) so no app takes them for news or a hole to fill.
//! An author is `did:wyvrn:tmp:discord:<id>`: no DID anyone resolves, never a
//! member, to be claimed later by whoever proves the Discord account theirs.
//! Every file is sealed as members' apps seal theirs (AES-256-GCM, the key in the
//! post), so a link alone shows nothing. GIFs from Giphy, Klipy and Tenor stay
//! links to where they are. Running again adds only what isn't there yet.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::Context as _;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use wyvrn_service::params;

use crate::files::Files;
use crate::store::{Channel, Store};

/// What an imported author is: never a member, nobody to send anything to.
pub const AUTHOR_PREFIX: &str = "did:wyvrn:tmp:discord:";
/// An imported post's id: its Discord message's, so a second run finds it.
pub const POST_PREFIX: &str = "discord-";
/// Wyvrn's own largest file by default, for the survey's "over the usual size".
const USUAL_FILE_BYTES: u64 = 25 * 1024 * 1024;
/// Posts written in one transaction.
const CHUNK: usize = 500;

// ---------------------------------------------------------------------------
// The export, as DiscordChatExporter writes it (only what is used here).

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Export {
    pub guild: Named,
    pub channel: ExportChannel,
    pub messages: Vec<Message>,
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(default)]
pub struct Named {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct ExportChannel {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// For a thread, the id of the channel it is in.
    pub category_id: Option<String>,
    /// For a thread, the name of the channel it is in.
    pub category: Option<String>,
    pub name: String,
    pub topic: Option<String>,
}

impl ExportChannel {
    pub fn is_thread(&self) -> bool {
        self.kind.contains("Thread")
    }
    pub fn is_voice(&self) -> bool {
        self.kind.contains("Voice")
    }
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Message {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub timestamp: String,
    pub timestamp_edited: Option<String>,
    pub is_pinned: bool,
    pub content: String,
    pub author: User,
    pub attachments: Vec<Attachment>,
    pub embeds: Vec<Embed>,
    pub stickers: Vec<Sticker>,
    pub reactions: Vec<Reaction>,
    pub mentions: Vec<User>,
    pub reference: Option<Reference>,
    pub inline_emojis: Vec<Emoji>,
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct User {
    pub id: String,
    pub name: String,
    pub nickname: Option<String>,
    pub is_bot: bool,
    pub avatar_url: Option<String>,
    pub roles: Vec<Named>,
}

impl User {
    pub fn shown_as(&self) -> &str {
        self.nickname.as_deref().filter(|n| !n.is_empty()).unwrap_or(&self.name)
    }
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Attachment {
    pub id: String,
    pub url: String,
    pub file_name: String,
    pub file_size_bytes: u64,
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Embed {
    pub title: Option<String>,
    pub url: Option<String>,
    pub timestamp: Option<String>,
    pub color: Option<String>,
    pub description: Option<String>,
    pub author: Option<EmbedAuthor>,
    pub thumbnail: Option<Image>,
    pub image: Option<Image>,
    pub images: Vec<Image>,
    pub video: Option<Image>,
    pub footer: Option<Footer>,
    pub fields: Vec<Field>,
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct EmbedAuthor {
    pub name: Option<String>,
    pub url: Option<String>,
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Footer {
    pub text: Option<String>,
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Field {
    pub name: String,
    pub value: String,
    pub is_inline: bool,
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Image {
    /// Where the export keeps it: a file beside it, or a link.
    pub url: String,
    /// Where it was on the web.
    pub canonical_url: Option<String>,
}

impl Image {
    /// Its address on the web, if it has one.
    fn web(&self) -> Option<&str> {
        [self.canonical_url.as_deref(), Some(self.url.as_str())].into_iter().flatten().find(|u| u.starts_with("https://") || u.starts_with("http://"))
    }
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Sticker {
    pub id: String,
    pub name: String,
    pub format: String,
    pub source_url: String,
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Reaction {
    pub emoji: Emoji,
    pub count: u64,
    pub users: Vec<User>,
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Emoji {
    /// Empty (or absent) for a Unicode emoji.
    pub id: Option<String>,
    pub name: String,
    pub is_animated: bool,
    pub image_url: String,
}

impl Emoji {
    fn custom(&self) -> Option<&str> {
        self.id.as_deref().filter(|id| !id.is_empty())
    }
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Reference {
    pub message_id: Option<String>,
    pub channel_id: Option<String>,
    pub guild_id: Option<String>,
}

/// One file of the export, read whole.
pub fn read(path: &Path) -> anyhow::Result<Export> {
    let file = std::fs::File::open(path).with_context(|| format!("{}", path.display()))?;
    serde_json::from_reader(std::io::BufReader::with_capacity(1 << 20, file)).with_context(|| format!("{}: not DiscordChatExporter's JSON", path.display()))
}

/// A file's guild and channel, read from its start alone (they come before its
/// messages), so a backup's channels are known without reading every message.
pub fn header(path: &Path) -> anyhow::Result<(Named, ExportChannel)> {
    let mut start = vec![0u8; 16 * 1024];
    let mut file = std::fs::File::open(path)?;
    let n = file.read(&mut start)?;
    let text = String::from_utf8_lossy(&start[..n]);
    let cut = text.find("\"messages\"").context("no messages in it")?;
    let head = format!("{}}}", text[..cut].trim_end().trim_end_matches(','));
    #[derive(Deserialize)]
    struct Head {
        guild: Named,
        channel: ExportChannel,
    }
    let head: Head = serde_json::from_str(&head).with_context(|| format!("{}: not DiscordChatExporter's JSON", path.display()))?;
    Ok((head.guild, head.channel))
}

/// Every export file in `folder`, by name.
pub fn files_in(folder: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
        .with_context(|| format!("{}", folder.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_file() && path.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    Ok(files)
}

/// A file the export kept beside it, by the path it wrote (as it is, or
/// percent-encoded); `None` for a link, or one that isn't there.
pub fn local(folder: &Path, url: &str) -> Option<PathBuf> {
    if url.is_empty() || url.starts_with("http://") || url.starts_with("https://") {
        return None;
    }
    let plain = folder.join(url);
    if plain.is_file() {
        return Some(plain);
    }
    let decoded = percent_decode(url);
    let decoded = folder.join(decoded);
    decoded.is_file().then_some(decoded)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() && text.is_char_boundary(i + 3) {
            if let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn seconds(stamp: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(stamp).ok().map(|t| t.timestamp())
}

fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(|h| h.trim_start_matches("www.").to_lowercase())
}

/// Which GIF service a link is to, if it is to one.
pub fn gif_provider(url: &str) -> Option<&'static str> {
    let host = host_of(url)?;
    let of = |name: &str| host == name || host.ends_with(&format!(".{name}"));
    if of("giphy.com") || of("gph.is") {
        Some("Giphy")
    } else if of("klipy.com") || of("klipy.co") {
        Some("Klipy")
    } else if of("tenor.com") || of("tenor.co") || of("tenor.googleapis.com") {
        Some("Tenor")
    } else {
        None
    }
}

static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>()\[\]"']+"#).expect("a valid pattern"));

/// The kinds of message that are someone saying something; the rest (joins,
/// boosts, pin notices, "started a thread") are left out.
fn is_said(kind: &str) -> bool {
    matches!(kind, "Default" | "Reply" | "ChatInputCommand" | "ContextMenuCommand" | "")
}

fn media_type_of(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "txt" | "log" => "text/plain",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// The survey.

/// What a backup holds, and what importing it would take.
#[derive(Debug, Default)]
pub struct Survey {
    pub guild: String,
    pub channels: Vec<ChannelCounts>,
    pub total: Counts,
    pub authors: HashSet<String>,
    pub bots: HashSet<String>,
    /// Links (in text or a card) to each GIF service, by message.
    pub gifs: BTreeMap<&'static str, u64>,
    /// Other links, by site.
    pub sites: HashMap<String, u64>,
    pub unreadable: Vec<(String, String)>,
    /// Bytes of each distinct file to import: attachments, avatars.
    attachment_files: HashMap<PathBuf, u64>,
    avatar_files: HashMap<PathBuf, u64>,
    card_files: HashMap<PathBuf, u64>,
    emoji_files: HashMap<PathBuf, u64>,
}

#[derive(Debug, Default, Clone)]
pub struct ChannelCounts {
    pub file: String,
    pub name: String,
    pub category: String,
    pub kind: String,
    pub counts: Counts,
}

#[derive(Debug, Default, Clone)]
pub struct Counts {
    pub messages: u64,
    /// Joins, boosts, pin notices and the like: not imported.
    pub system: u64,
    pub by_bots: u64,
    pub attachments: u64,
    /// Kept in the backup (uploaded to Discord).
    pub attachments_local: u64,
    /// Only a link (expired, or never downloaded).
    pub attachments_linked: u64,
    pub attachment_bytes: u64,
    pub images: u64,
    pub gif_files: u64,
    pub videos: u64,
    pub video_bytes: u64,
    pub over_usual: u64,
    pub over_usual_bytes: u64,
    pub cards: u64,
    pub gif_links: u64,
    pub other_links: u64,
    pub reactions: u64,
    pub reacted: u64,
    pub custom_emoji: u64,
    pub stickers: u64,
    pub replies: u64,
    pub edited: u64,
    pub pinned: u64,
    pub text_bytes: u64,
}

impl Counts {
    fn add(&mut self, other: &Counts) {
        macro_rules! sum { ($($f:ident),*) => { $( self.$f += other.$f; )* } }
        sum!(
            messages, system, by_bots, attachments, attachments_local, attachments_linked, attachment_bytes, images, gif_files, videos, video_bytes, over_usual, over_usual_bytes, cards,
            gif_links, other_links, reactions, reacted, custom_emoji, stickers, replies, edited, pinned, text_bytes
        );
    }
}

fn size_of(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Reads every export in `folder` (or those `only` names, by file or channel id).
pub fn survey(folder: &Path, only: &[String], progress: impl Fn(&str)) -> anyhow::Result<Survey> {
    let mut survey = Survey::default();
    for path in files_in(folder)? {
        let file = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        if !only.is_empty() {
            let Ok((_, channel)) = header(&path) else { continue };
            if !only.iter().any(|o| *o == channel.id || file.contains(o.as_str())) {
                continue;
            }
        }
        progress(&file);
        let export = match read(&path) {
            Ok(export) => export,
            Err(error) => {
                survey.unreadable.push((file, format!("{error:#}")));
                continue;
            }
        };
        survey.guild = export.guild.name.clone();
        let counts = survey.count(folder, &export);
        survey.total.add(&counts);
        let channel = &export.channel;
        survey.channels.push(ChannelCounts {
            file,
            name: channel.name.clone(),
            category: channel.category.clone().unwrap_or_default(),
            kind: channel.kind.clone(),
            counts,
        });
    }
    Ok(survey)
}

impl Survey {
    fn count(&mut self, folder: &Path, export: &Export) -> Counts {
        let mut c = Counts::default();
        for m in &export.messages {
            c.messages += 1;
            if !is_said(&m.kind) {
                c.system += 1;
                continue;
            }
            self.authors.insert(m.author.id.clone());
            if m.author.is_bot {
                c.by_bots += 1;
                self.bots.insert(m.author.id.clone());
            }
            if let Some(path) = m.author.avatar_url.as_deref().and_then(|u| local(folder, u)) {
                let bytes = size_of(&path);
                self.avatar_files.insert(path, bytes);
            }
            c.text_bytes += m.content.len() as u64;
            if m.timestamp_edited.is_some() {
                c.edited += 1;
            }
            if m.is_pinned {
                c.pinned += 1;
            }
            if m.reference.as_ref().and_then(|r| r.message_id.as_ref()).is_some() && m.kind == "Reply" {
                c.replies += 1;
            }
            for a in &m.attachments {
                c.attachments += 1;
                let media_type = media_type_of(&a.file_name);
                match local(folder, &a.url) {
                    Some(path) => {
                        c.attachments_local += 1;
                        let bytes = size_of(&path).max(a.file_size_bytes);
                        c.attachment_bytes += bytes;
                        if bytes > USUAL_FILE_BYTES {
                            c.over_usual += 1;
                            c.over_usual_bytes += bytes;
                        }
                        if media_type.starts_with("video/") {
                            c.videos += 1;
                            c.video_bytes += bytes;
                        }
                        self.attachment_files.insert(path, bytes);
                    }
                    None => c.attachments_linked += 1,
                }
                if media_type == "image/gif" {
                    c.gif_files += 1;
                }
                if media_type.starts_with("image/") {
                    c.images += 1;
                }
            }
            // Each link once a message, whether in its text or a card for it.
            let mut links: Vec<String> = LINK.find_iter(&m.content).map(|l| l.as_str().trim_end_matches(['.', ',', '!', '?', ')', '>']).to_string()).collect();
            for e in &m.embeds {
                c.cards += 1;
                links.extend(e.url.iter().cloned());
                for image in e.thumbnail.iter().chain(e.image.iter()).chain(e.images.iter()).chain(e.video.iter()) {
                    if gif_provider(image.web().unwrap_or_default()).is_none() {
                        if let Some(path) = local(folder, &image.url) {
                            let bytes = size_of(&path);
                            self.card_files.insert(path, bytes);
                        }
                    }
                }
            }
            links.sort();
            links.dedup();
            let mut gif_here: HashSet<&'static str> = HashSet::new();
            for link in &links {
                match gif_provider(link) {
                    Some(provider) => {
                        gif_here.insert(provider);
                    }
                    None => {
                        c.other_links += 1;
                        if let Some(host) = host_of(link) {
                            *self.sites.entry(host).or_default() += 1;
                        }
                    }
                }
            }
            for provider in gif_here {
                c.gif_links += 1;
                *self.gifs.entry(provider).or_default() += 1;
            }
            for r in &m.reactions {
                c.reactions += 1;
                c.reacted += r.count;
                if let Some(path) = r.emoji.custom().and_then(|_| local(folder, &r.emoji.image_url)) {
                    let bytes = size_of(&path);
                    self.emoji_files.insert(path, bytes);
                }
            }
            c.custom_emoji += m.inline_emojis.iter().filter(|e| e.custom().is_some()).count() as u64;
            for e in m.inline_emojis.iter().filter(|e| e.custom().is_some()) {
                if let Some(path) = local(folder, &e.image_url) {
                    let bytes = size_of(&path);
                    self.emoji_files.insert(path, bytes);
                }
            }
            c.stickers += m.stickers.len() as u64;
        }
        c
    }

    /// The survey as Markdown: counts only, none of what anyone said.
    pub fn markdown(&self) -> String {
        let t = &self.total;
        let distinct = |files: &HashMap<PathBuf, u64>| (files.len() as u64, files.values().sum::<u64>());
        let (attachment_n, attachment_bytes) = distinct(&self.attachment_files);
        let (avatar_n, avatar_bytes) = distinct(&self.avatar_files);
        let (card_n, card_bytes) = distinct(&self.card_files);
        let (emoji_n, emoji_bytes) = distinct(&self.emoji_files);
        let posts = t.messages - t.system;
        // A post's row, its state, its extra (files, cards, the Discord author)
        // and indexes: about 600 bytes besides its text, and about 80 a reaction.
        let store = t.text_bytes + posts * 600 + t.reacted * 80 + t.attachments * 350 + t.cards * 400;
        let mut out = String::new();
        out.push_str(&format!("# {}: what the backup holds\n\n", if self.guild.is_empty() { "Discord backup" } else { &self.guild }));
        out.push_str("Counts only: nothing anyone wrote is copied here.\n\n");
        out.push_str("## Messages\n\n| | |\n|---|---|\n");
        out.push_str(&format!("| Files (channels and threads) | {} |\n", self.channels.len()));
        out.push_str(&format!("| Messages | {} |\n", num(t.messages)));
        out.push_str(&format!("| Of which system messages (joins, boosts, pin notices: not imported) | {} |\n", num(t.system)));
        out.push_str(&format!("| Posts to import | {} |\n", num(posts)));
        out.push_str(&format!("| By bots | {} |\n", num(t.by_bots)));
        out.push_str(&format!("| Authors | {} ({} bots) |\n", num(self.authors.len() as u64), num(self.bots.len() as u64)));
        out.push_str(&format!("| Replies / edited / pinned | {} / {} / {} |\n", num(t.replies), num(t.edited), num(t.pinned)));
        out.push_str(&format!("| Reactions | {} on posts, {} people's in all |\n", num(t.reactions), num(t.reacted)));
        out.push_str(&format!("| Custom emoji in text / stickers | {} / {} |\n\n", num(t.custom_emoji), num(t.stickers)));
        out.push_str("## Attachments and links\n\n| | |\n|---|---|\n");
        out.push_str(&format!("| Attachments | {} |\n", num(t.attachments)));
        out.push_str(&format!("| Uploaded to Discord (in the backup) | {} ({} distinct files, {}) |\n", num(t.attachments_local), num(attachment_n), bytes(attachment_bytes)));
        out.push_str(&format!("| Only a link (not in the backup) | {} |\n", num(t.attachments_linked)));
        out.push_str(&format!("| Pictures (of which GIF files) | {} ({}) |\n", num(t.images), num(t.gif_files)));
        out.push_str(&format!("| Videos | {} ({}) |\n", num(t.videos), bytes(t.video_bytes)));
        out.push_str(&format!("| Over 25 MiB (Wyvrn's usual largest) | {} ({}) |\n", num(t.over_usual), bytes(t.over_usual_bytes)));
        out.push_str(&format!("| Link cards (embeds) | {} |\n", num(t.cards)));
        let gifs: Vec<String> = self.gifs.iter().map(|(p, n)| format!("{p} {}", num(*n))).collect();
        out.push_str(&format!("| Messages with a GIF from Giphy, Klipy or Tenor (kept as links) | {} ({}) |\n", num(t.gif_links), if gifs.is_empty() { "none".into() } else { gifs.join(", ") }));
        out.push_str(&format!("| Other links | {} |\n\n", num(t.other_links)));
        let mut sites: Vec<(&String, &u64)> = self.sites.iter().collect();
        sites.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        if !sites.is_empty() {
            out.push_str("Most linked sites: ");
            out.push_str(&sites.iter().take(12).map(|(s, n)| format!("{s} {}", num(**n))).collect::<Vec<_>>().join(", "));
            out.push_str("\n\n");
        }
        out.push_str("## Disk space, imported\n\n| | |\n|---|---|\n");
        out.push_str(&format!("| Attachments (sealing adds nothing) | {} |\n", bytes(attachment_bytes)));
        out.push_str(&format!("| Without files over 25 MiB | {} |\n", bytes(attachment_bytes.saturating_sub(t.over_usual_bytes))));
        out.push_str(&format!("| Without videos | {} |\n", bytes(attachment_bytes.saturating_sub(t.video_bytes))));
        out.push_str(&format!("| Authors' pictures | {} ({} files) |\n", bytes(avatar_bytes), num(avatar_n)));
        out.push_str(&format!("| The database (posts, reactions, cards), roughly | {} to {} |\n", bytes(store), bytes(store * 2)));
        out.push_str(&format!("| **In all, with media** | **about {}** |\n", bytes(attachment_bytes + avatar_bytes + store * 2)));
        out.push_str(&format!("| In all, text only | about {} |\n\n", bytes(avatar_bytes + store * 2)));
        out.push_str(&format!(
            "Not copied, and not counted above: link cards' pictures ({} files, {} in the backup: a card shows its picture from where it is on the web), and custom emoji pictures ({} files, {}: the server's own become its packs with `import-expressions`, others stay `:name:`).\n\n",
            num(card_n),
            bytes(card_bytes),
            num(emoji_n),
            bytes(emoji_bytes)
        ));
        out.push_str("## By channel\n\n| Channel | Category / in | Kind | Messages | Attachments | Size | GIF links |\n|---|---|---|---|---|---|---|\n");
        let mut channels = self.channels.clone();
        channels.sort_by(|a, b| b.counts.messages.cmp(&a.counts.messages));
        for ch in &channels {
            let c = &ch.counts;
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                ch.name.replace('|', "/"),
                ch.category.replace('|', "/"),
                ch.kind,
                num(c.messages),
                num(c.attachments),
                bytes(c.attachment_bytes),
                num(c.gif_links)
            ));
        }
        if !self.unreadable.is_empty() {
            out.push_str("\n## Not read\n\n");
            for (file, why) in &self.unreadable {
                out.push_str(&format!("- {file}: {why}\n"));
            }
        }
        out
    }
}

fn num(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(d);
    }
    out
}

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ---------------------------------------------------------------------------
// Matching channels, and the plan.

/// Where one export goes.
#[derive(Debug, Clone)]
pub struct Target {
    pub file: PathBuf,
    pub source: ExportChannel,
    /// The channel here, or why there is none.
    pub channel: Result<Channel, String>,
    /// How it was found: "as told", "by category and name", "by name".
    pub how: &'static str,
}

/// A channel's name and its category's, as compared.
pub fn name_key(name: &str) -> String {
    key(name)
}

fn key(name: &str) -> String {
    crate::discord::text_name(name)
}

/// Matches each export to a channel here: `told` first (Discord channel id ->
/// channel id here), then by category and name, then by name where only one has it.
/// A thread goes where the channel it is in went (that channel's file among
/// `all`), or, in a forum (which has no file of its own), to the forum here of its
/// name. A voice channel's text chat goes to the voice channel here of its name.
pub fn match_channels(store: &Store, files: &[PathBuf], all: &[PathBuf], told: &HashMap<String, String>) -> anyhow::Result<Vec<Target>> {
    let channels = store.channels()?;
    let categories: HashMap<String, String> = store.categories()?.into_iter().map(|c| (c.id, key(&c.name))).collect();
    let category_of = |c: &Channel| c.category.as_ref().and_then(|id| categories.get(id)).cloned().unwrap_or_default();
    let headers: HashMap<String, ExportChannel> = all.iter().filter_map(|f| header(f).ok()).map(|(_, c)| (c.id.clone(), c)).collect();
    let find = |source: &ExportChannel| -> (Result<Channel, String>, &'static str) {
        if let Some(local) = told.get(&source.id) {
            return match channels.iter().find(|c| c.id == *local || key(&c.name) == key(local)) {
                Some(channel) => (Ok(channel.clone()), "as told"),
                None => (Err(format!("there is no channel {local} here")), "as told"),
            };
        }
        let name = key(&source.name);
        let category = source.category.as_deref().map(key).unwrap_or_default();
        let voice = source.is_voice();
        let kind = |c: &&Channel| (c.voice.is_some() == voice) && !c.calendar;
        let both: Vec<&Channel> = channels.iter().filter(kind).filter(|c| key(&c.name) == name && category_of(c) == category).collect();
        let named: Vec<&Channel> = channels.iter().filter(kind).filter(|c| key(&c.name) == name).collect();
        match (both.as_slice(), named.as_slice()) {
            ([one], _) => (Ok((*one).clone()), "by category and name"),
            ([], [one]) => (Ok((*one).clone()), "by name"),
            ([], []) => (Err(format!("no {}channel here has its name", if voice { "voice " } else { "" })), ""),
            _ => (Err("several channels here have its name: say which".to_string()), ""),
        }
    };
    let mut targets = Vec::new();
    for file in files {
        let (_, source) = header(file)?;
        let found = if source.is_thread() {
            let parent = source.category_id.as_ref().and_then(|id| headers.get(id));
            match parent {
                Some(parent) => {
                    let (channel, how) = find(parent);
                    (channel.map_err(|why| format!("the channel it is in, #{}: {why}", parent.name)), how)
                }
                None => {
                    let forum = key(source.category.as_deref().unwrap_or_default());
                    let forums: Vec<&Channel> = channels.iter().filter(|c| c.forum.is_some() && key(&c.name) == forum).collect();
                    match forums.as_slice() {
                        [one] => (Ok((*one).clone()), "a post in the forum of its name"),
                        [] => (Err(format!("no forum here is called {}", source.category.as_deref().unwrap_or_default())), ""),
                        _ => (Err("several forums here have its name".to_string()), ""),
                    }
                }
            }
        } else {
            find(&source)
        };
        targets.push(Target { file: file.clone(), source, channel: found.0, how: found.1 });
    }
    // A channel before its threads: a thread's first post may be the channel's.
    targets.sort_by_key(|t| t.source.is_thread());
    Ok(targets)
}

/// What importing a file would do.
#[derive(Debug, Default, Clone)]
pub struct Planned {
    pub posts: usize,
    pub already: usize,
    pub left_out: usize,
    pub files: usize,
    pub file_bytes: u64,
    pub linked: usize,
    pub newest_here: Option<i64>,
    pub interleaved: usize,
}

pub fn plan_one(store: &Store, folder: &Path, export: &Export, channel: &Channel, media: bool) -> anyhow::Result<Planned> {
    let have = imported_here(store, &channel.id)?;
    let newest_here: Option<i64> = store.db.lock().query_value("SELECT MAX(posted_at) FROM posts WHERE channel = ?1 AND id NOT LIKE 'discord-%'", params![channel.id])?;
    let mut planned = Planned { newest_here, ..Default::default() };
    let oldest_here: Option<i64> = store.db.lock().query_value("SELECT MIN(posted_at) FROM posts WHERE channel = ?1 AND id NOT LIKE 'discord-%'", params![channel.id])?;
    for m in &export.messages {
        if !is_said(&m.kind) {
            planned.left_out += 1;
            continue;
        }
        if have.contains(&format!("{POST_PREFIX}{}", m.id)) {
            planned.already += 1;
            continue;
        }
        planned.posts += 1;
        if let (Some(oldest), Some(at)) = (oldest_here, seconds(&m.timestamp)) {
            if at >= oldest {
                planned.interleaved += 1;
            }
        }
        for a in &m.attachments {
            match local(folder, &a.url) {
                Some(path) if media => {
                    planned.files += 1;
                    planned.file_bytes += size_of(&path);
                }
                Some(_) => {}
                None => planned.linked += 1,
            }
        }
    }
    Ok(planned)
}

fn imported_here(store: &Store, channel: &str) -> anyhow::Result<HashSet<String>> {
    Ok(store.db.lock().query("SELECT id FROM posts WHERE channel = ?1 AND id LIKE 'discord-%'", params![channel], |row| row.get::<String>(0))?.into_iter().collect())
}

// ---------------------------------------------------------------------------
// Writing.

/// What the import needs besides the store.
pub struct Context<'a> {
    pub folder: &'a Path,
    /// Where files go; `None` to import text only.
    pub files: Option<&'a Files>,
    /// Discord channel id -> its name, for `<#id>`.
    pub channel_names: HashMap<String, String>,
    /// Discord role id -> the role here, by name: `(local id, name)`.
    pub roles: HashMap<String, (String, String)>,
    /// The server's own emoji and stickers as the community's packs
    /// (`import-expressions`' `imported.json`): Discord id -> `(shortcode, url)`.
    pub expressions: HashMap<String, (String, String)>,
    pub now: i64,
}

/// What was written.
#[derive(Debug, Default)]
pub struct Written {
    pub posts: usize,
    pub already: usize,
    pub left_out: usize,
    pub files: usize,
    pub file_bytes: u64,
    pub files_failed: Vec<String>,
    pub reactions: usize,
    pub reactions_left_out: usize,
}

/// Discord role ids (as the export's authors carry them) matched to the roles here by name.
pub fn roles_by_name(store: &Store, export: &Export) -> anyhow::Result<HashMap<String, (String, String)>> {
    let here: HashMap<String, (String, String)> = store.roles()?.into_iter().map(|r| (r.name.to_lowercase(), (r.id, r.name))).collect();
    let mut roles = HashMap::new();
    for m in &export.messages {
        for role in m.author.roles.iter().chain(m.mentions.iter().flat_map(|u| u.roles.iter())) {
            if let Some(found) = here.get(&role.name.to_lowercase()) {
                roles.insert(role.id.clone(), found.clone());
            }
        }
    }
    Ok(roles)
}

/// The packs `import-expressions` made: its `imported.json`, the stickers'
/// service at `packs_url` (the community's `s/`).
pub fn expressions_from(imported: &Value, packs_url: &str) -> HashMap<String, (String, String)> {
    let base = if packs_url.ends_with('/') { packs_url.to_string() } else { format!("{packs_url}/") };
    imported
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(id, item)| {
            let shortcode = item["shortcode"].as_str()?;
            let pack = item["pack"].as_str()?;
            let file = item["file"].as_str()?;
            Some((id.clone(), (shortcode.to_string(), format!("{base}{pack}/{file}"))))
        })
        .collect()
}

static USER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<@!?(\d+)>").expect("a valid pattern"));
static ROLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<@&(\d+)>").expect("a valid pattern"));
static CHANNEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<#(\d+)>").expect("a valid pattern"));
static EMOJI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<a?:(\w{1,64}):(\d+)>").expect("a valid pattern"));
static TIME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<t:(-?\d{1,13})(?::[tTdDfFR])?>").expect("a valid pattern"));

/// A message's text as it reads here: people, roles and channels by name, the
/// server's own emoji as its packs' (others as `:name:`), times written out. And
/// who it calls on, as `mentions` (nobody is told: it is history).
pub fn rewrite(m: &Message, ctx: &Context) -> (String, Vec<Value>) {
    let mut mentions = Vec::new();
    let names: HashMap<&str, &User> = m.mentions.iter().map(|u| (u.id.as_str(), u)).collect();
    let text = USER.replace_all(&m.content, |c: &regex::Captures| match names.get(&c[1]) {
        Some(user) => {
            mentions.push(json!({ "who": format!("{AUTHOR_PREFIX}{}", user.id), "name": user.shown_as() }));
            format!("@{}", user.shown_as())
        }
        None => "@someone".to_string(),
    });
    let text = ROLE.replace_all(&text, |c: &regex::Captures| match ctx.roles.get(&c[1]) {
        Some((id, name)) => {
            mentions.push(json!({ "who": format!("role:{id}"), "name": name }));
            format!("@{name}")
        }
        None => "@a-role".to_string(),
    });
    let text = CHANNEL.replace_all(&text, |c: &regex::Captures| match ctx.channel_names.get(&c[1]) {
        Some(name) => format!("#{name}"),
        None => "#a-channel".to_string(),
    });
    let text = EMOJI.replace_all(&text, |c: &regex::Captures| match ctx.expressions.get(&c[2]) {
        Some((shortcode, url)) => format!("![:{shortcode}:]({url})"),
        None => format!(":{}:", &c[1]),
    });
    let text = TIME.replace_all(&text, |c: &regex::Captures| {
        c[1].parse::<i64>().ok().and_then(|s| chrono::DateTime::from_timestamp(s, 0)).map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_else(|| c[0].to_string())
    });
    (text.into_owned(), mentions)
}

/// A file sealed as wyvrn-chat seals one: its ciphertext, and `ciphering`.
pub fn seal(plain: &[u8]) -> (Vec<u8>, Value) {
    use aes_gcm::aead::{AeadCore, AeadInPlace, KeyInit, OsRng};
    let key = aes_gcm::Aes256Gcm::generate_key(OsRng);
    let nonce = aes_gcm::Aes256Gcm::generate_nonce(OsRng);
    let cipher = aes_gcm::Aes256Gcm::new(&key);
    let mut buffer = plain.to_vec();
    let tag = cipher.encrypt_in_place_detached(&nonce, b"", &mut buffer).expect("AES-GCM takes any length a file has");
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    (buffer, json!({ "algorithm": "aes-256-gcm", "parameters": { "key": hex(&key), "iv": hex(&nonce), "tag": hex(&tag) } }))
}

/// Keeps a file sealed at the community's host: its attachment (chat-message/1.0's,
/// as a member's app sends one) and its media item.
fn keep_file(files: &Files, owner: &str, path: &Path, name: &str, now: i64) -> anyhow::Result<(Value, Value)> {
    let plain = std::fs::read(path).with_context(|| format!("{}", path.display()))?;
    let media_type = media_type_of(name);
    let (sealed, ciphering) = seal(&plain);
    let (id, url, hash) = files.keep_any(owner, name, media_type, &sealed, now)?;
    let attachment = json!({ "id": id, "media_type": media_type, "filename": name, "byte_count": sealed.len(), "data": { "links": [url], "hash": hash } });
    let item = json!({ "attachment": id, "ciphering": ciphering });
    Ok((attachment, item))
}

/// A card as the app draws a webhook's (Discord's embed object): pictures by their
/// address on the web.
fn card(e: &Embed) -> Option<Value> {
    let mut card = serde_json::Map::new();
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            card.insert(key.to_string(), value);
        }
    };
    put("title", e.title.clone().filter(|t| !t.is_empty()).map(Value::from));
    put("description", e.description.clone().filter(|t| !t.is_empty()).map(Value::from));
    put("url", e.url.clone().filter(|u| u.starts_with("http")).map(Value::from));
    put("timestamp", e.timestamp.clone().map(Value::from));
    put("color", e.color.as_deref().and_then(|c| i64::from_str_radix(c.trim_start_matches('#'), 16).ok()).map(Value::from));
    put("author", e.author.as_ref().and_then(|a| a.name.clone()).map(|name| json!({ "name": name, "url": e.author.as_ref().and_then(|a| a.url.clone()) })));
    put("footer", e.footer.as_ref().and_then(|f| f.text.clone()).map(|text| json!({ "text": text })));
    let picture = e.image.iter().chain(e.images.iter()).find_map(Image::web).map(|u| json!({ "url": u }));
    put("image", picture);
    put("thumbnail", e.thumbnail.as_ref().and_then(Image::web).map(|u| json!({ "url": u })));
    if !e.fields.is_empty() {
        put("fields", Some(json!(e.fields.iter().map(|f| json!({ "name": f.name, "value": f.value, "inline": f.is_inline })).collect::<Vec<_>>())));
    }
    (!card.is_empty()).then(|| Value::Object(card))
}

/// Writes `export`'s posts into `channel`, oldest first, those not there yet.
pub fn write(store: &Store, export: &Export, channel: &Channel, ctx: &Context, progress: impl Fn(usize, usize)) -> anyhow::Result<Written> {
    let mut written = Written::default();
    let have = imported_here(store, &channel.id)?;
    // A thread: its posts are in the thread that grew from the post of its id (in a
    // forum, its own first post; else the channel's post it was started from). One
    // with neither gets a post to grow from, saying what it was called.
    let thread_id = export.channel.is_thread().then(|| export.channel.id.clone());
    let root = thread_id.as_ref().map(|id| format!("{POST_PREFIX}{id}"));
    let forum = channel.forum.is_some();
    let stub = match (&thread_id, &root) {
        (Some(id), Some(root)) if !export.messages.iter().any(|m| m.id == *id) && store.post_by_id(root)?.is_none() => export.messages.iter().find(|m| is_said(&m.kind)).map(|first| Message {
            id: id.clone(),
            kind: "Default".into(),
            timestamp: first.timestamp.clone(),
            content: format!("🧵 {}", export.channel.name),
            author: first.author.clone(),
            ..Default::default()
        }),
        _ => None,
    };
    let mut messages: Vec<&Message> = export.messages.iter().chain(stub.iter()).collect();
    messages.sort_by(|a, b| seconds(&a.timestamp).cmp(&seconds(&b.timestamp)).then(a.id.len().cmp(&b.id.len())).then(a.id.cmp(&b.id)));
    let todo: Vec<&Message> = messages
        .into_iter()
        .filter(|m| {
            if !is_said(&m.kind) {
                written.left_out += 1;
                return false;
            }
            if have.contains(&format!("{POST_PREFIX}{}", m.id)) {
                written.already += 1;
                return false;
            }
            true
        })
        .collect();
    let owner = format!("import:discord:{}", export.guild.id);
    // Each author's picture, kept once.
    let mut avatars: HashMap<String, Option<Value>> = HashMap::new();
    let total = todo.len();
    for (n, chunk) in todo.chunks(CHUNK).enumerate() {
        // Files first, outside the transaction: they are kept whether or not it commits,
        // and a second run keeps them again (a file nobody's post names is only space).
        let mut prepared = Vec::with_capacity(chunk.len());
        for m in chunk {
            let mut extra = serde_json::Map::new();
            let mut media = Vec::new();
            let mut attachments = Vec::new();
            let mut notes = Vec::new();
            for a in &m.attachments {
                match (ctx.files, local(ctx.folder, &a.url)) {
                    (Some(files), Some(path)) => match keep_file(files, &owner, &path, &a.file_name, ctx.now) {
                        Ok((attachment, item)) => {
                            written.files += 1;
                            written.file_bytes += attachment["byte_count"].as_u64().unwrap_or(0);
                            attachments.push(attachment);
                            media.push(item);
                        }
                        Err(error) => {
                            written.files_failed.push(format!("{}: {error:#}", a.file_name));
                            notes.push(format!("📎 {} (not brought over)", a.file_name));
                        }
                    },
                    (None, Some(_)) => notes.push(format!("📎 {} (not brought over)", a.file_name)),
                    (_, None) => notes.push(if a.url.starts_with("http") { format!("📎 [{}]({})", a.file_name, a.url) } else { format!("📎 {} (not in the backup)", a.file_name) }),
                }
            }
            if !media.is_empty() {
                extra.insert("media".into(), json!(media));
                extra.insert("attachments".into(), json!(attachments));
            }
            let cards: Vec<Value> = m.embeds.iter().filter_map(card).take(10).collect();
            if !cards.is_empty() {
                extra.insert("embeds".into(), json!(cards));
            }
            let avatar = match (ctx.files, m.author.avatar_url.as_deref().and_then(|u| local(ctx.folder, u))) {
                (Some(files), Some(path)) => avatars
                    .entry(m.author.id.clone())
                    .or_insert_with(|| {
                        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("avatar").to_string();
                        keep_file(files, &owner, &path, &name, ctx.now).ok().map(|(attachment, item)| {
                            json!({ "links": attachment["data"]["links"], "hash": attachment["data"]["hash"], "media_type": attachment["media_type"], "byte_count": attachment["byte_count"], "ciphering": item["ciphering"] })
                        })
                    })
                    .clone(),
                _ => None,
            };
            let mut author = json!({ "id": m.author.id, "name": m.author.name });
            if m.author.is_bot {
                author["bot"] = json!(true);
            }
            if let Some(avatar) = avatar {
                author["avatar"] = avatar;
            }
            extra.insert("imported".into(), json!({ "from": "discord", "id": m.id, "channel": export.channel.id, "author": author }));
            if forum && thread_id.as_deref() == Some(m.id.as_str()) {
                extra.insert("title".into(), json!(export.channel.name.chars().take(100).collect::<String>()));
            }
            let (mut content, mentions) = rewrite(m, ctx);
            for sticker in &m.stickers {
                let shown = match ctx.expressions.get(&sticker.id) {
                    Some((shortcode, url)) => format!("![:{shortcode}:]({url})"),
                    None => format!("[sticker: {}]", sticker.name),
                };
                content = if content.is_empty() { shown } else { format!("{content}\n{shown}") };
            }
            for note in notes {
                content = if content.is_empty() { note } else { format!("{content}\n{note}") };
            }
            prepared.push((m, content, mentions, Value::Object(extra)));
        }
        store.db.grouped(|| -> anyhow::Result<()> {
            let mut db = store.db.lock();
            let mut seq: i64 = db.query_value("SELECT COALESCE(MAX(seq), 0) FROM posts WHERE channel = ?1", params![channel.id])?;
            let first = seq + 1;
            for (m, content, mentions, extra) in &prepared {
                seq += 1;
                let id = format!("{POST_PREFIX}{}", m.id);
                let at = seconds(&m.timestamp).unwrap_or(ctx.now);
                db.execute(
                    "INSERT INTO posts (channel, seq, id, content, posted_at, author_did, author_name) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![channel.id, seq, id, content, at, format!("{AUTHOR_PREFIX}{}", m.author.id), m.author.shown_as()],
                )?;
                // The post it answers, where it is here (imported, or by now).
                let reply: Option<(String, String, String)> = match m.reference.as_ref().and_then(|r| r.message_id.as_ref()).filter(|_| m.kind == "Reply") {
                    Some(to) => db.query_row("SELECT id, author_did, content FROM posts WHERE id = ?1", params![format!("{POST_PREFIX}{to}")], |row| {
                        Ok((row.get::<String>(0)?, row.get::<String>(1)?, row.get::<String>(2)?))
                    })?,
                    None => None,
                };
                let (reply_to, reply_author, snippet) = match &reply {
                    Some((id, author, said)) => (Some(id.clone()), Some(author.clone()), Some(said.chars().take(100).collect::<String>())),
                    None => (None, None, None),
                };
                let mentions = (!mentions.is_empty()).then(|| Value::from(mentions.clone()).to_string());
                let edited = m.timestamp_edited.as_deref().and_then(seconds);
                let in_thread = root.as_ref().filter(|root| **root != id).cloned();
                // rev 0: written before changes were counted, so nobody is sent it as one.
                db.execute(
                    "INSERT INTO post_state (post, rev, reply_to, reply_author, reply_snippet, thread, mentions, edited_at, pinned) VALUES (?1, 0, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![id, reply_to, reply_author, snippet, in_thread, mentions, edited, m.is_pinned],
                )?;
                db.execute("INSERT INTO post_extra (post, extra) VALUES (?1, ?2)", params![id, extra.to_string()])?;
                for r in &m.reactions {
                    let emoji = match r.emoji.custom() {
                        None => Some(r.emoji.name.clone()),
                        Some(custom) => ctx.expressions.get(custom).map(|(shortcode, url)| format!("![:{shortcode}:]({url})")),
                    };
                    let Some(emoji) = emoji.filter(|e| crate::community::is_emoji(e)) else {
                        written.reactions_left_out += 1;
                        continue;
                    };
                    for user in &r.users {
                        db.execute("INSERT OR IGNORE INTO reactions (post, did, emoji, at) VALUES (?1, ?2, ?3, ?4)", params![id, format!("{AUTHOR_PREFIX}{}", user.id), emoji.clone(), at])?;
                        written.reactions += 1;
                    }
                }
                written.posts += 1;
            }
            // A private thread stays one: read by whoever manages threads here (who
            // was in it isn't in the export), until people are added to it.
            if let (Some(root), true) = (&root, export.channel.kind.contains("Private")) {
                db.execute("INSERT OR IGNORE INTO private_threads (thread) VALUES (?1)", params![root])?;
            }
            // The thread, once the post it grows from is here (not touched: it is history).
            if let Some(root) = &root {
                db.execute(
                    "INSERT OR IGNORE INTO threads (id, channel, name, created_by, created_at) SELECT id, channel, ?2, author_did, posted_at FROM posts WHERE id = ?1",
                    params![root, export.channel.name.chars().take(100).collect::<String>()],
                )?;
            }
            if seq >= first {
                db.execute("INSERT INTO imported_ranges (channel, from_seq, to_seq, at) VALUES (?1, ?2, ?3, ?4)", params![channel.id, first, seq, ctx.now])?;
            }
            Ok(())
        })?;
        progress((n * CHUNK + chunk.len()).min(total), total);
    }
    store.rebuild_counts()?;
    Ok(written)
}

/// Discord channel ids to names, for `<#id>`, from every export's header.
pub fn channel_names(files: &[PathBuf]) -> HashMap<String, String> {
    files.iter().filter_map(|f| header(f).ok()).map(|(_, c)| (c.id, c.name)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, at: &str, content: &str) -> Message {
        Message {
            id: id.into(),
            kind: "Default".into(),
            timestamp: at.into(),
            content: content.into(),
            author: User { id: "111".into(), name: "ann".into(), nickname: Some("Ann".into()), ..Default::default() },
            ..Default::default()
        }
    }

    fn ctx(folder: &Path) -> Context<'_> {
        Context {
            folder,
            files: None,
            channel_names: HashMap::from([("900".to_string(), "fanart".to_string())]),
            roles: HashMap::from([("700".to_string(), ("r1".to_string(), "Artists".to_string()))]),
            expressions: HashMap::from([("555".to_string(), ("frostyhi".to_string(), "https://c.example/c/f/s/frosty-emoji/a.png".to_string()))]),
            now: 2_000_000_000,
        }
    }

    #[test]
    fn discord_markup_reads_as_it_does_here() {
        let dir = std::env::temp_dir();
        let mut m = message("1", "2020-01-01T00:00:00+00:00", "hi <@222> and <@!333>, <@&700> <#900> <:frostyhi:555> <a:other:666> <t:0:R>");
        m.mentions = vec![User { id: "222".into(), name: "bob".into(), nickname: Some("Bobby".into()), ..Default::default() }];
        let (text, mentions) = rewrite(&m, &ctx(&dir));
        assert_eq!(text, "hi @Bobby and @someone, @Artists #fanart ![:frostyhi:](https://c.example/c/f/s/frosty-emoji/a.png) :other: 1970-01-01 00:00 UTC");
        assert_eq!(mentions, vec![json!({ "who": "did:wyvrn:tmp:discord:222", "name": "Bobby" }), json!({ "who": "role:r1", "name": "Artists" })]);
    }

    #[test]
    fn gif_services_are_known_by_their_sites() {
        assert_eq!(gif_provider("https://media.giphy.com/media/x/giphy.gif"), Some("Giphy"));
        assert_eq!(gif_provider("https://tenor.com/view/cat-123"), Some("Tenor"));
        assert_eq!(gif_provider("https://static.klipy.com/a.gif"), Some("Klipy"));
        assert_eq!(gif_provider("https://example.com/giphy.com"), None);
    }

    #[test]
    fn a_file_is_sealed_as_the_app_seals_one() {
        let (sealed, ciphering) = seal(b"a picture");
        assert_ne!(sealed, b"a picture");
        assert_eq!(crate::files::open_sealed(&sealed, &ciphering).as_deref(), Some(&b"a picture"[..]));
    }

    #[test]
    fn history_is_read_by_time_among_the_posts_there_and_never_sent_as_new() {
        let store = Store::in_memory().unwrap();
        store.add_channel("fan", "fanart", "", "did:example:owner", 100).unwrap();
        store.admit("did:example:bea", crate::store::Role::Member, 100).unwrap();
        let bea = store.member("did:example:bea").unwrap().unwrap();
        // Already here: a post at 1,700,000,000.
        store.add_post("fan", "live-1", "hello from wyvrn", &bea, None, None, 1_700_000_000).unwrap();
        let mut first = message("10", "2020-01-01T00:00:00+00:00", "art!");
        first.reactions = vec![Reaction {
            emoji: Emoji { name: "❤️".into(), ..Default::default() },
            count: 2,
            users: vec![User { id: "222".into(), ..Default::default() }, User { id: "333".into(), ..Default::default() }],
        }];
        first.is_pinned = true;
        let mut reply = message("11", "2020-01-02T00:00:00+00:00", "lovely");
        reply.kind = "Reply".into();
        reply.reference = Some(Reference { message_id: Some("10".into()), ..Default::default() });
        let later = message("12", "2024-01-01T00:00:00+00:00", "after wyvrn's first post"); // 1,704,067,200
        let mut joined = message("13", "2020-01-01T00:00:01+00:00", "");
        joined.kind = "GuildMemberJoin".into();
        let export = Export { guild: Named { id: "1".into(), name: "G".into() }, channel: ExportChannel { id: "900".into(), name: "fanart".into(), ..Default::default() }, messages: vec![later, joined, reply, first] };
        let channel = store.channel("fan").unwrap().unwrap();
        let dir = std::env::temp_dir();
        let written = write(&store, &export, &channel, &ctx(&dir), |_, _| {}).unwrap();
        assert_eq!((written.posts, written.left_out, written.reactions), (3, 1, 2));

        // By time, newest first: the imported 2024 post before the 2023 one here.
        let ids = |posts: Vec<crate::store::Post>| posts.into_iter().map(|p| p.id).collect::<Vec<_>>();
        assert_eq!(ids(store.channel_posts_by_time("fan", None, None, 10).unwrap()), ["discord-12", "live-1", "discord-11", "discord-10"]);
        let older = store.channel_posts_by_time("fan", None, Some((1_700_000_000, 1)), 10).unwrap();
        assert_eq!(ids(older), ["discord-11", "discord-10"]);
        // Numbers after the one already here, recorded as imported.
        assert_eq!(store.imported_ranges().unwrap()["fan"], vec![(2, 4)]);
        // Nothing of it is sent out as new.
        assert_eq!(ids(store.posts_after(0, 10).unwrap()), ["live-1"]);
        let art = store.post_by_id("discord-10").unwrap().unwrap();
        assert!(art.pinned);
        assert_eq!(art.rev, 0);
        assert_eq!(art.author_did, "did:wyvrn:tmp:discord:111");
        assert_eq!(art.reactions, vec![("❤️".to_string(), vec!["did:wyvrn:tmp:discord:222".to_string(), "did:wyvrn:tmp:discord:333".to_string()])]);
        assert_eq!(art.extra.as_ref().unwrap()["imported"]["id"], "10");
        let answer = store.post_by_id("discord-11").unwrap().unwrap();
        assert_eq!(answer.reply_to.as_deref(), Some("discord-10"));
        // Again: nothing new.
        let again = write(&store, &export, &channel, &ctx(&dir), |_, _| {}).unwrap();
        assert_eq!((again.posts, again.already), (0, 3));
    }

    #[test]
    fn threads_and_forum_posts_grow_from_their_first_posts() {
        let store = Store::in_memory().unwrap();
        store.add_channel("art", "fanart", "", "did:example:owner", 100).unwrap();
        store.add_channel("games", "games-general", "", "did:example:owner", 100).unwrap();
        store.set_forum("games", &json!([])).unwrap();
        let dir = std::env::temp_dir();
        let export = |id: &str, kind: &str, name: &str, parent: Option<&str>, messages: Vec<Message>| Export {
            guild: Named { id: "1".into(), name: "G".into() },
            channel: ExportChannel { id: id.into(), kind: kind.into(), name: name.into(), category_id: parent.map(Into::into), ..Default::default() },
            messages,
        };
        let art = store.channel("art").unwrap().unwrap();
        let games = store.channel("games").unwrap().unwrap();
        // A thread started from a channel's post: that post first, then the thread.
        let parent = export("900", "GuildTextChat", "fanart", None, vec![message("50", "2021-01-01T00:00:00+00:00", "a drawing")]);
        write(&store, &parent, &art, &ctx(&dir), |_, _| {}).unwrap();
        let thread = export("50", "GuildPublicThread", "drawing talk", Some("900"), vec![message("51", "2021-01-01T01:00:00+00:00", "nice lines")]);
        write(&store, &thread, &art, &ctx(&dir), |_, _| {}).unwrap();
        let started = store.thread("discord-50").unwrap().unwrap();
        assert_eq!((started.channel.as_str(), started.name.as_deref(), started.count), ("art", Some("drawing talk"), 1));
        assert_eq!(store.post_by_id("discord-51").unwrap().unwrap().thread.as_deref(), Some("discord-50"));
        // The channel reads without the thread's posts.
        assert_eq!(store.channel_posts_by_time("art", None, None, 10).unwrap().len(), 1);
        // A forum post: its own first post, titled with the thread's name.
        let post = export("60", "GuildPublicThread", "Forum Rules", Some("700"), vec![message("60", "2022-01-01T00:00:00+00:00", "be nice"), message("61", "2022-01-02T00:00:00+00:00", "ok")]);
        write(&store, &post, &games, &ctx(&dir), |_, _| {}).unwrap();
        let root = store.post_by_id("discord-60").unwrap().unwrap();
        assert_eq!((root.thread.as_deref(), root.extra.as_ref().unwrap()["title"].as_str()), (None, Some("Forum Rules")));
        assert_eq!(store.thread("discord-60").unwrap().unwrap().count, 1);
        // A thread whose first post isn't anywhere: one to grow from, named for it.
        let lost = export("70", "GuildPublicThread", "lost start", Some("900"), vec![message("71", "2023-01-01T00:00:00+00:00", "where did it go")]);
        write(&store, &lost, &art, &ctx(&dir), |_, _| {}).unwrap();
        assert_eq!(store.post_by_id("discord-70").unwrap().unwrap().content, "🧵 lost start");
        assert_eq!(store.thread("discord-70").unwrap().unwrap().count, 1);
        // A private one stays private.
        let hidden = export("80", "GuildPrivateThread", "mods only", Some("900"), vec![message("81", "2023-02-01T00:00:00+00:00", "between us")]);
        write(&store, &hidden, &art, &ctx(&dir), |_, _| {}).unwrap();
        assert!(store.is_private_thread("discord-80").unwrap());
        // Nothing of it was counted as a change.
        assert_eq!(store.latest_rev().unwrap(), 0);
    }

    #[test]
    fn a_header_is_read_without_the_messages() {
        let dir = std::env::temp_dir().join(format!("dce-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("g - c [9].json");
        std::fs::write(&path, r#"{"guild":{"id":"1","name":"G","iconUrl":"x"},"channel":{"id":"9","type":"GuildTextChat","categoryId":"8","category":"Art","name":"fanart","topic":null},"dateRange":{"after":null,"before":null},"exportedAt":"2026","messages":[{"id":"5"}],"messageCount":1}"#).unwrap();
        let (guild, channel) = header(&path).unwrap();
        assert_eq!((guild.name.as_str(), channel.name.as_str(), channel.category.as_deref()), ("G", "fanart", Some("Art")));
        assert_eq!(read(&path).unwrap().messages.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
