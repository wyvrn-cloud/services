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
    /// Its size in pixels, as Discord gave it (an embed's pictures have them).
    pub width: Option<u64>,
    pub height: Option<u64>,
}

impl Image {
    /// As an embed's picture is sent: its address on the web, with its width and
    /// height where they are given, so an app keeps its room before it loads.
    fn card(&self) -> Option<Value> {
        let mut picture = json!({ "url": self.web()? });
        let side = |n: Option<u64>| n.filter(|n| (1..=100_000).contains(n));
        if let (Some(width), Some(height)) = (side(self.width), side(self.height)) {
            picture["width"] = width.into();
            picture["height"] = height.into();
        }
        Some(picture)
    }

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
///
/// Only ever a file inside `folder`: an export names its files relative to itself,
/// and one naming an absolute path, `..`, or a link out of the folder (another
/// community's keys, `/proc`) is given nothing (security review WYV-19).
pub fn local(folder: &Path, url: &str) -> Option<PathBuf> {
    if url.is_empty() || url.starts_with("http://") || url.starts_with("https://") {
        return None;
    }
    inside(folder, url).or_else(|| inside(folder, &percent_decode(url)))
}

/// `relative` under `folder`, if it is a plain relative path to a file there:
/// no root, prefix or `..`, and, links resolved, still inside the folder.
fn inside(folder: &Path, relative: &str) -> Option<PathBuf> {
    use std::path::Component;
    let path = Path::new(relative);
    if !path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
        return None;
    }
    let joined = folder.join(path);
    if std::fs::symlink_metadata(&joined).ok()?.file_type().is_symlink() {
        return None;
    }
    let (real, base) = (joined.canonicalize().ok()?, folder.canonicalize().ok()?);
    (real.starts_with(&base) && real.is_file()).then_some(joined)
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
    /// Discord user id -> what they are shown as, for `<@id>` where the message
    /// doesn't say whom it calls on (in an embed): [`users_in`].
    pub users: HashMap<String, String>,
    pub now: i64,
}

/// Everyone `export` says something of (its authors, whom they call on, who
/// reacted): Discord user id -> what they are shown as.
pub fn users_in(export: &Export) -> HashMap<String, String> {
    let mut users = HashMap::new();
    for m in &export.messages {
        for user in std::iter::once(&m.author).chain(m.mentions.iter()).chain(m.reactions.iter().flat_map(|r| r.users.iter())) {
            users.entry(user.id.clone()).or_insert_with(|| user.shown_as().to_string());
        }
    }
    users
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

/// Discord role ids matched to the roles here: as the import of the server's roles
/// mapped them (`import-discord-roles`: `discord_mapped`), else by the name the
/// role had there, else by the names the export's authors' roles carry
/// ([`roles_by_name`]). For `<@&id>`, which names a role by its id alone.
pub fn roles_for(store: &Store, export: &Export) -> anyhow::Result<HashMap<String, (String, String)>> {
    let roles = store.roles()?;
    let by_id: HashMap<String, String> = roles.iter().map(|r| (r.id.clone(), r.name.clone())).collect();
    let by_name: HashMap<String, (String, String)> = roles.into_iter().map(|r| (r.name.to_lowercase(), (r.id, r.name))).collect();
    let mut found = roles_by_name(store, export)?;
    for mapped in store.discord_mapped("role")? {
        let here = match mapped.local.as_ref().and_then(|local| by_id.get(local).map(|name| (local.clone(), name.clone()))) {
            Some(here) => Some(here),
            None => by_name.get(&mapped.name.to_lowercase()).cloned(),
        };
        if let Some(here) = here {
            found.insert(mapped.discord_id, here);
        }
    }
    Ok(found)
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
static EVERYONE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|[^\w`])@(everyone|here)\b").expect("a valid pattern"));

/// A message's text as it reads here: people, roles and channels by name, the
/// server's own emoji as its packs' (others as `:name:`). And who it calls on, as
/// `mentions` (nobody is told: it is history). A time (`<t:1767225600:F>`) is
/// left as written: the app and the web view show it in the reader's zone.
pub fn rewrite(m: &Message, ctx: &Context) -> (String, Vec<Value>) {
    let mut mentions = Vec::new();
    let text = rewrite_text(&m.content, m, ctx, &mut mentions);
    (text, mentions)
}

/// `text`, of `m` (its content, or its embeds' words), as [`rewrite`] has it: who
/// it calls on added to `mentions`, each once.
fn rewrite_text(text: &str, m: &Message, ctx: &Context, mentions: &mut Vec<Value>) -> String {
    let mut mention = |who: String, name: &str| {
        if !mentions.iter().any(|known| known["who"] == who.as_str()) {
            mentions.push(json!({ "who": who, "name": name }));
        }
    };
    // `@everyone` and `@here` call on everyone who reads it: here, both are everyone.
    if EVERYONE.is_match(text) {
        mention(crate::community::EVERYONE.to_string(), "everyone");
    }
    let text = USER.replace_all(text, |c: &regex::Captures| match m.mentions.iter().find(|u| u.id == c[1]).map(|u| u.shown_as()).or_else(|| ctx.users.get(&c[1]).map(String::as_str)) {
        Some(name) => {
            mention(format!("{AUTHOR_PREFIX}{}", &c[1]), name);
            format!("@{name}")
        }
        None => "@someone".to_string(),
    });
    let text = ROLE.replace_all(&text, |c: &regex::Captures| match ctx.roles.get(&c[1]) {
        Some((id, name)) => {
            mention(format!("role:{id}"), name);
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
    text.into_owned()
}

/// `m`'s embeds as cards (ten at most), their words rewritten as its text is, and
/// who they call on added to `mentions`.
pub fn cards(m: &Message, ctx: &Context, mentions: &mut Vec<Value>) -> Vec<Value> {
    m.embeds.iter().filter_map(|e| card(e, &mut |text| rewrite_text(text, m, ctx, mentions))).take(10).collect()
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
/// as a member's app sends one) and its media item: a picture's with its width and
/// height, where they can be read of it (and without them where they can't).
fn keep_file(files: &Files, owner: &str, path: &Path, name: &str, now: i64) -> anyhow::Result<(Value, Value)> {
    let plain = std::fs::read(path).with_context(|| format!("{}", path.display()))?;
    // (By what it is, where it can be told: an embed's picture is kept without its
    // name's ending.)
    let media_type = sniffed(&plain).unwrap_or_else(|| media_type_of(name));
    let (sealed, ciphering) = seal(&plain);
    let (id, url, hash) = files.keep_any(owner, name, media_type, &sealed, now)?;
    let attachment = json!({ "id": id, "media_type": media_type, "filename": name, "byte_count": sealed.len(), "data": { "links": [url], "hash": hash } });
    let mut item = json!({ "attachment": id, "ciphering": ciphering });
    crate::picture_sizes::put(&mut item, media_type, &plain);
    Ok((attachment, item))
}

/// A picture's media type, read from its first bytes.
fn sniffed(bytes: &[u8]) -> Option<&'static str> {
    Some(match image::guess_format(bytes).ok()? {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        image::ImageFormat::Avif => "image/avif",
        image::ImageFormat::Bmp => "image/bmp",
        _ => return None,
    })
}

/// Whether `url` is Discord's own (its CDN, its media proxy, its proxy of other
/// sites' pictures): a link that stops working, signed for a while only.
pub fn expires(url: &str) -> bool {
    host_of(url).is_some_and(|host| ["discordapp.com", "discordapp.net", "discord.com"].iter().any(|own| host == *own || host.ends_with(&format!(".{own}"))))
}

/// The file name a link ends in (without its query), percent-decoded.
fn file_name_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let last = parsed.path_segments()?.next_back().filter(|s| !s.is_empty())?;
    Some(percent_decode(last))
}

/// `url` as compared: without its query or fragment.
fn bare(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// A file kept for a post: `(attachment, media item)`, as [`keep_file`] gives them.
pub type Kept = (Value, Value);

/// What a post brings over of its files, given the files it already has (by
/// where each came from: `imported.files`, see [`prepare`]); how a file not yet
/// kept is kept is the caller's (`upload`), as is what to do with none.
pub struct PostFiles<'a> {
    pub upload: &'a mut dyn FnMut(&Path, &str) -> anyhow::Result<Kept>,
}

/// A post of `m` as it is written here: its text, whom it calls on and its
/// `extra` (`media` and `attachments`, `embeds`, `imported`). `files`, where
/// files are brought over: each attachment, and each picture of an embed that is
/// in the export's files and whose link is Discord's (it stops working), kept as
/// the post's own file (Discord shows a picture's link as the picture; so here:
/// the link said in the text goes, and a card that was the picture alone with
/// it). `had`: the files the post has already, by where they came from
/// (`a:<attachment id>`, `e:<link>`), kept as they are, not again. Without files,
/// each is said in the text as a note. `written` counts what was kept, or not.
pub fn prepare(m: &Message, ctx: &Context, mut files: Option<PostFiles<'_>>, had: &HashMap<String, Kept>, written: &mut Written) -> (String, Vec<Value>, serde_json::Map<String, Value>, Vec<String>) {
    let mut got = Gathered::default();
    let mut notes = Vec::new();
    for a in &m.attachments {
        match local(ctx.folder, &a.url) {
            Some(path) => {
                if !got.keep(format!("a:{}", a.id), &path, &a.file_name, &mut files, had, written) {
                    notes.push(format!("📎 {} (not brought over)", a.file_name));
                }
            }
            None => notes.push(if a.url.starts_with("http") { format!("📎 [{}]({})", a.file_name, a.url) } else { format!("📎 {} (not in the backup)", a.file_name) }),
        }
    }
    let (mut content, mut mentions) = rewrite(m, ctx);
    // An embed's pictures whose links stop working, kept as the post's files; what
    // is left of it a card, where anything is.
    let mut moved: Vec<String> = Vec::new();
    let mut embeds = m.embeds.clone();
    for e in embeds.iter_mut() {
        let link = e.url.clone().filter(|u| expires(u));
        let mut take = |image: &Image, got: &mut Gathered, files: &mut Option<PostFiles<'_>>, written: &mut Written| -> bool {
            let web = image.web().map(str::to_string);
            if web.as_deref().is_some_and(|w| !expires(w)) && link.is_none() {
                return false;
            }
            let Some(path) = local(ctx.folder, &image.url) else { return false };
            let name = [link.as_deref(), web.as_deref()].into_iter().flatten().find_map(file_name_of).unwrap_or_else(|| "picture".to_string());
            let source = format!("e:{}", bare(web.as_deref().or(link.as_deref()).unwrap_or(&image.url)));
            let kept = got.sources.contains(&source) || got.keep(source, &path, &name, files, had, written);
            if kept {
                moved.extend(web);
            }
            kept
        };
        let mut taken = false;
        for picture in [&mut e.image, &mut e.thumbnail] {
            if picture.as_ref().is_some_and(|image| take(image, &mut got, &mut files, written)) {
                *picture = None;
                taken = true;
            }
        }
        let mut images = std::mem::take(&mut e.images);
        images.retain(|image| {
            let kept = take(image, &mut got, &mut files, written);
            taken |= kept;
            !kept
        });
        e.images = images;
        if taken {
            moved.extend(link);
        }
    }
    let cards: Vec<Value> = embeds
        .iter()
        .filter(|e| {
            // A card that was a picture's link and nothing more is the picture now.
            let words = [&e.title, &e.description].iter().any(|t| t.as_deref().is_some_and(|t| !t.is_empty())) || !e.fields.is_empty() || e.author.is_some() || e.footer.is_some();
            let pictured = e.image.is_some() || e.thumbnail.is_some() || !e.images.is_empty() || e.video.is_some();
            words || pictured || !e.url.as_deref().is_some_and(|u| moved.iter().any(|l| bare(l) == bare(u)))
        })
        .filter_map(|e| card(e, &mut |text| rewrite_text(text, m, ctx, &mut mentions)))
        .take(10)
        .collect();
    // The links of what is kept now, out of the text.
    if !moved.is_empty() {
        let kept: Vec<&str> = moved.iter().map(|l| bare(l)).collect();
        let names: Vec<&str> = got.attachments.iter().filter_map(|a| a["filename"].as_str()).collect();
        let without = LINK.replace_all(&content, |c: &regex::Captures| {
            let link = c[0].trim_end_matches(['.', ',', '!', '?', ')', '>']);
            if expires(link) && (kept.contains(&bare(link)) || file_name_of(link).is_some_and(|name| names.contains(&name.as_str()))) {
                c[0][link.len()..].to_string()
            } else {
                c[0].to_string()
            }
        });
        content = without.lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim().to_string();
    }
    let mut extra = serde_json::Map::new();
    if !got.media.is_empty() {
        extra.insert("media".into(), json!(got.media));
        extra.insert("attachments".into(), json!(got.attachments));
    }
    if !cards.is_empty() {
        extra.insert("embeds".into(), json!(cards));
    }
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
    (content, mentions, extra, got.sources)
}

/// A post's files as [`prepare`] gathers them: each with where it came from.
#[derive(Default)]
struct Gathered {
    media: Vec<Value>,
    attachments: Vec<Value>,
    sources: Vec<String>,
}

impl Gathered {
    /// The file at `path` from `source`: the post's already (`had`), or kept now.
    /// Whether it is the post's.
    fn keep(&mut self, source: String, path: &Path, name: &str, files: &mut Option<PostFiles<'_>>, had: &HashMap<String, Kept>, written: &mut Written) -> bool {
        let kept = match had.get(&source) {
            Some(kept) => kept.clone(),
            None => {
                let Some(files) = files.as_mut() else { return false };
                match (files.upload)(path, name) {
                    Ok(kept) => {
                        written.files += 1;
                        written.file_bytes += kept.0["byte_count"].as_u64().unwrap_or(0);
                        kept
                    }
                    Err(error) => {
                        written.files_failed.push(format!("{name}: {error:#}"));
                        return false;
                    }
                }
            }
        };
        self.attachments.push(kept.0);
        self.media.push(kept.1);
        self.sources.push(source);
        true
    }
}

/// A card as the app draws a webhook's (Discord's embed object): pictures by their
/// address on the web; its title, description and fields' words through
/// `rewrite` (Discord's markup in them is Markdown, as a message's is).
fn card(e: &Embed, rewrite: &mut dyn FnMut(&str) -> String) -> Option<Value> {
    let mut card = serde_json::Map::new();
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            card.insert(key.to_string(), value);
        }
    };
    let mut words = |text: &Option<String>| text.as_deref().filter(|t| !t.is_empty()).map(|t| Value::from(rewrite(t)));
    let title = words(&e.title);
    let description = words(&e.description);
    let fields: Vec<Value> = e.fields.iter().map(|f| json!({ "name": rewrite(&f.name), "value": rewrite(&f.value), "inline": f.is_inline })).collect();
    put("title", title);
    put("description", description);
    put("url", e.url.clone().filter(|u| u.starts_with("http")).map(Value::from));
    put("timestamp", e.timestamp.clone().map(Value::from));
    put("color", e.color.as_deref().and_then(|c| i64::from_str_radix(c.trim_start_matches('#'), 16).ok()).map(Value::from));
    put("author", e.author.as_ref().and_then(|a| a.name.clone()).map(|name| json!({ "name": name, "url": e.author.as_ref().and_then(|a| a.url.clone()) })));
    put("footer", e.footer.as_ref().and_then(|f| f.text.clone()).map(|text| json!({ "text": text })));
    put("image", e.image.iter().chain(e.images.iter()).find_map(Image::card));
    put("thumbnail", e.thumbnail.as_ref().and_then(Image::card));
    if !fields.is_empty() {
        put("fields", Some(json!(fields)));
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
            let mut upload = |path: &Path, name: &str| keep_file(ctx.files.expect("only asked with files"), &owner, path, name, ctx.now);
            let files = ctx.files.map(|_| PostFiles { upload: &mut upload });
            let (content, mentions, mut extra, sources) = prepare(m, ctx, files, &HashMap::new(), &mut written);
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
            let mut imported = json!({ "from": "discord", "id": m.id, "channel": export.channel.id, "author": author, "at": ctx.now });
            if !sources.is_empty() {
                imported["files"] = json!(sources);
            }
            extra.insert("imported".into(), imported);
            if forum && thread_id.as_deref() == Some(m.id.as_str()) {
                extra.insert("title".into(), json!(export.channel.name.chars().take(100).collect::<String>()));
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

/// What [`refresh`] did.
#[derive(Debug, Default)]
pub struct Refreshed {
    /// Posts written again.
    pub posts: usize,
    /// Posts here already as the export has them.
    pub same: usize,
    /// The export's posts that aren't here (never imported, or taken back).
    pub missing: usize,
    /// Of `posts`: those whose text or whom they call on changed.
    pub texts: usize,
    /// Posts changed here since (edited by whoever claimed them): their text and
    /// whom they call on are left as they are.
    pub edited_here: usize,
    /// Files kept for them now (or, in a dry run, that would be), and their bytes.
    pub files: usize,
    pub file_bytes: u64,
    pub files_failed: Vec<String>,
}

/// What [`refresh`] writes again of a post already imported.
#[derive(Debug, Default, Clone, Copy)]
pub struct Refresh {
    /// Its cards (`embeds`).
    pub embeds: bool,
    /// Its files: each attachment and Discord-linked embed picture in the export's
    /// files kept as its own (`ctx.files`), those it has already kept as they are;
    /// its text without the links of what is kept now.
    pub files: bool,
    /// Its text as it reads now (people, roles and channels by name) and whom it
    /// calls on (`mentions`: people, roles, everyone).
    pub mentions: bool,
}

/// The embeds of `export`'s posts already imported into `channel`, written again
/// from it as [`write`] would write them now ([`refresh`] with embeds alone).
pub fn refresh_embeds(store: &Store, export: &Export, channel: &Channel, ctx: &Context, dry_run: bool) -> anyhow::Result<Refreshed> {
    refresh(store, export, channel, ctx, Refresh { embeds: true, ..Default::default() }, dry_run)
}

/// `export`'s posts already imported into `channel`, written again from it as
/// [`write`] would write them now, in what `what` says, and each post that changed
/// marked changed (as an edit is), so members' apps take it up. Its reactions,
/// author and place stay as they are; a post edited here since keeps its text.
/// Safe to run again: a file kept once is the post's, and isn't kept again.
/// `dry_run`: only counted (files measured, not kept).
pub fn refresh(store: &Store, export: &Export, channel: &Channel, ctx: &Context, what: Refresh, dry_run: bool) -> anyhow::Result<Refreshed> {
    let mut refreshed = Refreshed::default();
    let have = imported_here(store, &channel.id)?;
    let owner = format!("import:discord:{}", export.guild.id);
    for m in export.messages.iter().filter(|m| is_said(&m.kind)) {
        let id = format!("{POST_PREFIX}{}", m.id);
        let extra: Option<String> = match have.contains(&id) {
            true => store.db.lock().query_row("SELECT extra FROM post_extra WHERE post = ?1", params![id], |row| row.get(0))?,
            false => None,
        };
        // (A post taken back has no extra left: it stays gone.)
        let Some(extra) = extra.and_then(|text| serde_json::from_str::<Value>(&text).ok()).filter(Value::is_object) else {
            refreshed.missing += 1;
            continue;
        };
        let Some((content, kept_mentions, edited_at)) = store.db.lock().query_row(
            "SELECT posts.content, post_state.mentions, post_state.edited_at FROM posts JOIN post_state ON post_state.post = posts.id WHERE posts.id = ?1",
            params![id],
            |row| Ok((row.get::<String>(0)?, row.get::<Option<String>>(1)?, row.get::<Option<i64>>(2)?)),
        )?
        else {
            refreshed.missing += 1;
            continue;
        };
        let had = files_had(m, &extra);
        let mut written = Written::default();
        let mut upload = |path: &Path, name: &str| -> anyhow::Result<Kept> {
            match (dry_run, ctx.files) {
                (false, Some(files)) => keep_file(files, &owner, path, name, ctx.now),
                // Counted as it would be kept.
                _ => {
                    let bytes = std::fs::metadata(path).with_context(|| format!("{}", path.display()))?.len();
                    Ok((json!({ "id": format!("dry-run-{name}"), "filename": name, "byte_count": bytes }), json!({ "attachment": format!("dry-run-{name}") })))
                }
            }
        };
        let files = (what.files && ctx.files.is_some()).then(|| PostFiles { upload: &mut upload });
        let (new_content, new_mentions, prepared, sources) = prepare(m, ctx, files, &had, &mut written);
        refreshed.files += written.files;
        refreshed.file_bytes += written.file_bytes;
        refreshed.files_failed.extend(written.files_failed);
        let mut new_extra = extra.clone();
        // When it was brought in, for a post brought in before that was said.
        if new_extra["imported"].is_object() && new_extra["imported"].get("at").is_none() {
            let at: Option<i64> = store.db.lock().query_value(
                "SELECT MIN(imported_ranges.at) FROM imported_ranges JOIN posts ON posts.channel = imported_ranges.channel AND posts.seq BETWEEN imported_ranges.from_seq AND imported_ranges.to_seq WHERE posts.id = ?1",
                params![id],
            )?;
            if let Some(at) = at {
                new_extra["imported"]["at"] = json!(at);
            }
        }
        if what.embeds || what.files {
            match prepared.get("embeds") {
                Some(cards) => new_extra["embeds"] = cards.clone(),
                None => {
                    new_extra.as_object_mut().map(|e| e.remove("embeds"));
                }
            }
        }
        if what.files && ctx.files.is_some() {
            for key in ["media", "attachments"] {
                match prepared.get(key) {
                    Some(value) => new_extra[key] = value.clone(),
                    None => {
                        new_extra.as_object_mut().map(|e| e.remove(key));
                    }
                }
            }
            if !sources.is_empty() {
                new_extra["imported"]["files"] = json!(sources);
            }
        }
        // Its text, unless it was edited here since (by whoever claimed it).
        let edited_here = edited_at != m.timestamp_edited.as_deref().and_then(seconds);
        let rewrite_text = (what.mentions || (what.files && ctx.files.is_some())) && !edited_here;
        if (what.mentions || what.files) && edited_here {
            refreshed.edited_here += 1;
        }
        let new_content = if rewrite_text { new_content } else { content.clone() };
        // Whom it calls on: a Discord account claimed since is its member's.
        let mut mentions = Vec::new();
        for mut mention in new_mentions {
            if let Some(who) = mention["who"].as_str().filter(|who| who.starts_with(AUTHOR_PREFIX)) {
                mention["who"] = json!(store.current_did(who)?);
            }
            mentions.push(mention);
        }
        let new_mentions = (!mentions.is_empty()).then(|| Value::from(mentions).to_string());
        let mentions_changed = what.mentions && !edited_here && new_mentions != kept_mentions;
        let text_changed = new_content != content;
        if new_extra == extra && !text_changed && !mentions_changed {
            refreshed.same += 1;
            continue;
        }
        refreshed.posts += 1;
        if text_changed || mentions_changed {
            refreshed.texts += 1;
        }
        if dry_run {
            continue;
        }
        store.db.grouped(|| -> anyhow::Result<()> {
            if new_extra != extra {
                store.set_post_extra(&id, &new_extra)?;
            }
            let mut db = store.db.lock();
            if text_changed {
                db.execute("UPDATE posts SET content = ?2 WHERE id = ?1", params![id, new_content])?;
            }
            if mentions_changed {
                db.execute("UPDATE post_state SET mentions = ?2 WHERE post = ?1", params![id, new_mentions])?;
            }
            drop(db);
            store.touch_post(&id)?;
            Ok(())
        })?;
    }
    Ok(refreshed)
}

/// The files post `m` has kept already, by where each came from ([`prepare`]'s
/// `had`): as `imported.files` says, or, for a post imported before it said,
/// each attachment by its file name.
fn files_had(m: &Message, extra: &Value) -> HashMap<String, Kept> {
    let attachments = extra["attachments"].as_array().cloned().unwrap_or_default();
    let media = extra["media"].as_array().cloned().unwrap_or_default();
    let mut had = HashMap::new();
    if let Some(sources) = extra["imported"]["files"].as_array() {
        for (n, source) in sources.iter().enumerate() {
            if let (Some(source), Some(attachment), Some(item)) = (source.as_str(), attachments.get(n), media.get(n)) {
                had.insert(source.to_string(), (attachment.clone(), item.clone()));
            }
        }
        return had;
    }
    let mut used = vec![false; attachments.len()];
    for a in &m.attachments {
        let found = attachments.iter().enumerate().find(|(n, kept)| !used[*n] && kept["filename"] == a.file_name.as_str());
        if let Some((n, kept)) = found {
            // (Its media item names it by id.)
            if let Some(item) = media.iter().find(|item| item["attachment"] == kept["id"]) {
                used[n] = true;
                had.insert(format!("a:{}", a.id), (kept.clone(), item.clone()));
            }
        }
    }
    had
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
            users: HashMap::from([("444".to_string(), "Dee".to_string())]),
            now: 2_000_000_000,
        }
    }

    #[test]
    fn discord_markup_reads_as_it_does_here() {
        let dir = std::env::temp_dir();
        let mut m = message("1", "2020-01-01T00:00:00+00:00", "hi <@222> and <@!333>, <@&700> <#900> <:frostyhi:555> <a:other:666> <t:0:R>");
        m.mentions = vec![User { id: "222".into(), name: "bob".into(), nickname: Some("Bobby".into()), ..Default::default() }];
        let (text, mentions) = rewrite(&m, &ctx(&dir));
        // (A time is left as written: the app shows it in the reader's zone.)
        assert_eq!(text, "hi @Bobby and @someone, @Artists #fanart ![:frostyhi:](https://c.example/c/f/s/frosty-emoji/a.png) :other: <t:0:R>");
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
    fn a_picture_brought_in_carries_its_size_and_nothing_else_does() {
        let files = Files::scratch(&crate::config::FilesConfig::default(), "https://c.example/").unwrap();
        let dir = files.host.store.path("");
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::new(21, 13)).write_to(&mut png, image::ImageFormat::Png).unwrap();
        std::fs::write(dir.join("in.png"), png.into_inner()).unwrap();
        std::fs::write(dir.join("in.jpg"), b"not a jpeg at all").unwrap();
        std::fs::write(dir.join("in.txt"), b"words").unwrap();
        let (_, item) = keep_file(&files, "import:discord:1", &dir.join("in.png"), "art.png", 1).unwrap();
        assert_eq!((item["width"].clone(), item["height"].clone()), (json!(21), json!(13)));
        for (path, name) in [("in.jpg", "broken.jpg"), ("in.txt", "notes.txt")] {
            let (attachment, item) = keep_file(&files, "import:discord:1", &dir.join(path), name, 1).unwrap();
            assert!(item.get("width").is_none() && item.get("height").is_none(), "{name}");
            assert!(attachment["id"].is_string());
        }
        let _ = std::fs::remove_dir_all(dir);
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
        assert_eq!(art.reactions, vec![crate::store::Reacted { emoji: "❤️".to_string(), count: 2, by: vec!["did:wyvrn:tmp:discord:222".to_string(), "did:wyvrn:tmp:discord:333".to_string()] }]);
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

    /// A bot's post of rules: no text, its words in embeds, in Discord's markup.
    fn rules(id: &str) -> Message {
        let mut m = message(id, "2022-12-22T00:00:00+00:00", "");
        m.author.is_bot = true;
        m.embeds = vec![
            Embed { title: Some("**Rules:**".into()), fields: vec![Field { name: "**__1. Be kind__**".into(), value: "Ask <@&700> in <#900> <:frostyhi:555>".into(), is_inline: false }], ..Default::default() },
            Embed { description: Some("__**you agree**__ <@&700>, <@&701> <t:1798750800:F>".into()), color: Some("#2ECC71".into()), ..Default::default() },
        ];
        m
    }

    #[test]
    fn an_embeds_words_read_as_a_messages_text_does() {
        let store = Store::in_memory().unwrap();
        store.add_channel("rules", "rules", "", "did:example:owner", 100).unwrap();
        let export = Export { guild: Named { id: "1".into(), name: "G".into() }, channel: ExportChannel { id: "901".into(), name: "rules".into(), ..Default::default() }, messages: vec![rules("20")] };
        let channel = store.channel("rules").unwrap().unwrap();
        let dir = std::env::temp_dir();
        write(&store, &export, &channel, &ctx(&dir), |_, _| {}).unwrap();
        let post = store.post_by_id("discord-20").unwrap().unwrap();
        let embeds = &post.extra.as_ref().unwrap()["embeds"];
        // Markdown stays Markdown; Discord's mentions, channels and emoji read as here.
        assert_eq!(embeds[0]["title"], "**Rules:**");
        assert_eq!(embeds[0]["fields"][0], json!({ "name": "**__1. Be kind__**", "value": "Ask @Artists in #fanart ![:frostyhi:](https://c.example/c/f/s/frosty-emoji/a.png)", "inline": false }));
        assert_eq!(embeds[1]["description"], "__**you agree**__ @Artists, @a-role <t:1798750800:F>");
        assert_eq!(embeds[1]["color"], 0x2ecc71);
        // The role they call on is the post's mention, once.
        assert_eq!(post.mentions.iter().map(|m| m.who.as_str()).collect::<Vec<_>>(), ["role:r1"]);
    }

    #[test]
    fn an_embeds_pictures_carry_their_width_and_height_from_the_export() {
        // As DiscordChatExporter writes an embed's pictures: the file beside it, and where it was.
        let export: Embed = serde_json::from_value(json!({
            "title": "A post",
            "image": { "url": "media/a.png", "canonicalUrl": "https://cdn.example/a.png", "width": 1200, "height": 630 },
            "thumbnail": { "url": "https://cdn.example/t.png", "width": 80, "height": 0 },
        }))
        .unwrap();
        let card = card(&export, &mut |t| t.to_string()).unwrap();
        assert_eq!(card["image"], json!({ "url": "https://cdn.example/a.png", "width": 1200, "height": 630 }));
        // A size that isn't one is left out, and the picture kept.
        assert_eq!(card["thumbnail"], json!({ "url": "https://cdn.example/t.png" }));
    }

    #[test]
    fn embeds_imported_before_are_written_again_and_nothing_else() {
        let store = Store::in_memory().unwrap();
        store.add_channel("rules", "rules", "", "did:example:owner", 100).unwrap();
        let channel = store.channel("rules").unwrap().unwrap();
        let dir = std::env::temp_dir();
        let mut old = rules("20");
        old.embeds[0].fields[0].value = "first words".into();
        let mut plain = message("21", "2022-12-23T00:00:00+00:00", "hello");
        plain.embeds = vec![];
        let export = |messages| Export { guild: Named { id: "1".into(), name: "G".into() }, channel: ExportChannel { id: "901".into(), name: "rules".into(), ..Default::default() }, messages };
        write(&store, &export(vec![old, plain.clone()]), &channel, &ctx(&dir), |_, _| {}).unwrap();
        // As an earlier import wrote it: the embed's words verbatim.
        let mut extra = store.post_by_id("discord-20").unwrap().unwrap().extra.unwrap();
        extra["embeds"][0]["fields"][0]["value"] = json!("Ask <@&700>");
        store.set_post_extra("discord-20", &extra).unwrap();
        assert_eq!(store.latest_rev().unwrap(), 0);
        // A dry run only counts.
        let counted = refresh_embeds(&store, &export(vec![rules("20")]), &channel, &ctx(&dir), true).unwrap();
        assert_eq!((counted.posts, store.latest_rev().unwrap()), (1, 0));

        let refreshed = refresh_embeds(&store, &export(vec![rules("20"), plain, message("22", "2022-12-24T00:00:00+00:00", "never imported")]), &channel, &ctx(&dir), false).unwrap();
        assert_eq!((refreshed.posts, refreshed.same, refreshed.missing), (1, 1, 1));
        let post = store.post_by_id("discord-20").unwrap().unwrap();
        let extra = post.extra.unwrap();
        assert_eq!(extra["embeds"][0]["fields"][0]["value"], "Ask @Artists in #fanart ![:frostyhi:](https://c.example/c/f/s/frosty-emoji/a.png)");
        assert_eq!(extra["imported"]["id"], "20");
        // Sent as changed, as an edit is; nothing else of it, nor of the others.
        assert!(post.rev > 0);
        assert_eq!((post.content.as_str(), post.edited_at), ("", None));
        assert_eq!(store.post_by_id("discord-21").unwrap().unwrap().rev, 0);
        assert!(store.post_by_id("discord-22").unwrap().is_none());
        // Again: nothing to change.
        let again = refresh_embeds(&store, &export(vec![rules("20")]), &channel, &ctx(&dir), false).unwrap();
        assert_eq!((again.posts, again.same), (0, 1));
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

    #[test]
    fn a_role_everyone_and_someone_named_only_in_an_embed_are_called_on() {
        let dir = std::env::temp_dir();
        let mut m = message("1", "2020-01-01T00:00:00+00:00", "@everyone read the rules");
        m.embeds = vec![Embed { description: Some("ask <@&700> or <@444>".into()), ..Default::default() }];
        let ctx = ctx(&dir);
        let (text, mut mentions) = rewrite(&m, &ctx);
        assert_eq!(text, "@everyone read the rules");
        let cards = cards(&m, &ctx, &mut mentions);
        assert_eq!(cards[0]["description"], "ask @Artists or @Dee");
        assert_eq!(
            mentions,
            vec![json!({ "who": "everyone", "name": "everyone" }), json!({ "who": "did:wyvrn:tmp:discord:444", "name": "Dee" }), json!({ "who": "role:r1", "name": "Artists" })]
        );
        // Not in a word, nor code.
        assert!(rewrite(&message("2", "2020-01-01T00:00:00+00:00", "mail me@everyone.example"), &ctx).1.is_empty());
    }

    #[test]
    fn a_role_is_found_by_the_import_of_the_servers_roles_then_by_its_name_there() {
        let store = Store::in_memory().unwrap();
        let role = |id: &str, name: &str| {
            store.save_role(&crate::store::RoleDef { id: id.into(), name: name.into(), color: None, icon: None, position: 1, permissions: Vec::new(), mentionable: true, hoist: false, self_assignable: false }).unwrap();
            id.to_string()
        };
        let (mods, artists) = (role("m1", "Discord Mods"), role("a1", "Artists"));
        {
            let mut db = store.db.lock();
            db.execute("INSERT INTO discord_mapped (kind, discord_id, local, name) VALUES ('role', '464', ?1, 'Mods (old name)')", params![mods]).unwrap();
            db.execute("INSERT INTO discord_mapped (kind, discord_id, local, name) VALUES ('role', '465', 'gone', 'Artists')", params![]).unwrap();
        }
        let roles = roles_for(&store, &Export::default()).unwrap();
        assert_eq!(roles.get("464"), Some(&(mods, "Discord Mods".to_string())));
        assert_eq!(roles.get("465"), Some(&(artists, "Artists".to_string())));
    }

    /// A PNG of `width` by `height`, its bytes.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::new(width, height)).write_to(&mut png, image::ImageFormat::Png).unwrap();
        png.into_inner()
    }

    /// A post whose text is a Discord CDN link, carded as Discord cards one (its
    /// picture in the export's files, without its name's ending), an attachment
    /// beside it, and a YouTube card (its picture on the web for good).
    fn post_with_discord_links(folder: &Path) -> Message {
        std::fs::create_dir_all(folder.join("ch.json_Files")).unwrap();
        std::fs::write(folder.join("ch.json_Files/Sketch_2-eb27"), png(8, 6)).unwrap();
        std::fs::write(folder.join("ch.json_Files/art-1.png"), png(4, 4)).unwrap();
        std::fs::write(folder.join("ch.json_Files/yt-1.jpg"), png(2, 2)).unwrap();
        let cdn = "https://cdn.discordapp.com/attachments/1/2/Sketch_2.png";
        let mut m = message("50", "2020-01-01T00:00:00+00:00", &format!("{cdn} (the bigger one)"));
        m.attachments = vec![Attachment { id: "77".into(), url: "ch.json_Files/art-1.png".into(), file_name: "art.png".into(), file_size_bytes: 10 }];
        m.embeds = vec![
            Embed {
                title: Some(String::new()),
                url: Some(cdn.into()),
                thumbnail: Some(Image { url: "ch.json_Files/Sketch_2-eb27".into(), canonical_url: Some(format!("{cdn}?ex=1")), width: Some(8), height: Some(6) }),
                ..Default::default()
            },
            Embed {
                title: Some("A video".into()),
                url: Some("https://www.youtube.com/watch?v=x".into()),
                thumbnail: Some(Image { url: "ch.json_Files/yt-1.jpg".into(), canonical_url: Some("https://i.ytimg.com/vi/x/hq.jpg".into()), width: Some(2), height: Some(2) }),
                ..Default::default()
            },
        ];
        m
    }

    #[test]
    fn a_discord_link_to_a_picture_in_the_backup_is_brought_over_as_the_posts_file() {
        let files = Files::scratch(&crate::config::FilesConfig::default(), "https://c.example/").unwrap();
        let folder = files.host.store.path("").join("export");
        let store = Store::in_memory().unwrap();
        store.add_channel("fan", "fanart", "", "did:example:owner", 100).unwrap();
        let channel = store.channel("fan").unwrap().unwrap();
        let export = Export { messages: vec![post_with_discord_links(&folder)], ..Default::default() };
        let mut setting = ctx(&folder);
        setting.files = Some(&files);
        let written = write(&store, &export, &channel, &setting, |_, _| {}).unwrap();
        assert_eq!(written.files, 2, "{:?}", written.files_failed);
        let post = store.post_by_id("discord-50").unwrap().unwrap();
        assert_eq!(post.content, "(the bigger one)");
        let extra = post.extra.unwrap();
        let names: Vec<&str> = extra["attachments"].as_array().unwrap().iter().map(|a| a["filename"].as_str().unwrap()).collect();
        assert_eq!(names, ["art.png", "Sketch_2.png"]);
        assert_eq!(extra["attachments"][1]["media_type"], "image/png");
        assert_eq!((extra["media"][1]["width"].clone(), extra["media"][1]["height"].clone()), (json!(8), json!(6)));
        // The picture's card is the picture now; the video's stays, its picture on the web.
        let cards = extra["embeds"].as_array().unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["thumbnail"]["url"], "https://i.ytimg.com/vi/x/hq.jpg");
        assert_eq!(extra["imported"]["files"], json!(["a:77", "e:https://cdn.discordapp.com/attachments/1/2/Sketch_2.png"]));
        assert_eq!(extra["imported"]["at"], 2_000_000_000);
        let _ = std::fs::remove_dir_all(files.host.store.path(""));
    }

    #[test]
    fn posts_brought_in_as_text_take_up_their_files_and_mentions_once_and_one_edited_here_keeps_its_text() {
        let files = Files::scratch(&crate::config::FilesConfig::default(), "https://c.example/").unwrap();
        let folder = files.host.store.path("").join("export");
        let store = Store::in_memory().unwrap();
        store.add_channel("fan", "fanart", "", "did:example:owner", 100).unwrap();
        let channel = store.channel("fan").unwrap().unwrap();
        let mut rules = message("51", "2020-01-02T00:00:00+00:00", "ask <@&464>");
        rules.embeds = vec![Embed { description: Some("@here".into()), ..Default::default() }];
        let edited = message("52", "2020-01-03T00:00:00+00:00", "ask <@&464> too");
        let export = Export { messages: vec![post_with_discord_links(&folder), rules, edited], ..Default::default() };
        // Brought in before: text only, and the role unknown.
        let mut before = ctx(&folder);
        before.roles = HashMap::new();
        write(&store, &export, &channel, &before, |_, _| {}).unwrap();
        let post = store.post_by_id("discord-50").unwrap().unwrap();
        assert!(post.content.contains("📎 art.png (not brought over)") && post.content.contains("cdn.discordapp.com"), "{}", post.content);
        assert_eq!(store.post_by_id("discord-51").unwrap().unwrap().content, "ask @a-role");
        // As if it were from before `imported.at` and `imported.files` were said.
        for id in ["discord-50", "discord-51", "discord-52"] {
            let mut extra = store.post_by_id(id).unwrap().unwrap().extra.unwrap();
            extra["imported"].as_object_mut().unwrap().remove("at");
            store.set_post_extra(id, &extra).unwrap();
        }
        store.edit_post("discord-52", "my own words", &[], 2_100_000_000).unwrap();

        let mut now = ctx(&folder);
        now.roles = HashMap::from([("464".to_string(), ("m1".to_string(), "Discord Mods".to_string()))]);
        now.files = Some(&files);
        let what = Refresh { embeds: true, files: true, mentions: true };
        let counted = refresh(&store, &export, &channel, &now, what, true).unwrap();
        assert_eq!((counted.posts, counted.files), (3, 2), "{counted:?}");
        assert!(store.post_by_id("discord-50").unwrap().unwrap().extra.unwrap().get("media").is_none(), "a dry run changes nothing");
        let done = refresh(&store, &export, &channel, &now, what, false).unwrap();
        assert_eq!((done.posts, done.files, done.edited_here), (3, 2, 1), "{done:?}");
        let post = store.post_by_id("discord-50").unwrap().unwrap();
        assert_eq!(post.content, "(the bigger one)");
        assert_eq!(post.extra.as_ref().unwrap()["media"].as_array().unwrap().len(), 2);
        assert!(post.rev > 0, "sent as changed");
        let rules = store.post_by_id("discord-51").unwrap().unwrap();
        assert_eq!(rules.content, "ask @Discord Mods");
        let who: Vec<&str> = rules.mentions.iter().map(|m| m.who.as_str()).collect();
        assert_eq!(who, ["role:m1", "everyone"]);
        assert_eq!(rules.extra.unwrap()["imported"]["at"], 2_000_000_000);
        assert_eq!(store.post_by_id("discord-52").unwrap().unwrap().content, "my own words");
        // Again: nothing more to do, nothing kept twice.
        let again = refresh(&store, &export, &channel, &now, what, false).unwrap();
        assert_eq!((again.posts, again.files, again.same), (0, 0, 3), "{again:?}");
        let _ = std::fs::remove_dir_all(files.host.store.path(""));
    }

    /// A dry run against a real export, counts only: `WYVRN_DISCORD_EXPORT` names
    /// one of its files. Brought in as text, then what `--refresh-files
    /// --refresh-mentions` would do, said. (`cargo test -- --ignored --nocapture`)
    #[test]
    #[ignore]
    fn dry_run_of_a_refresh_against_a_real_export() {
        let Ok(path) = std::env::var("WYVRN_DISCORD_EXPORT") else { return };
        let path = PathBuf::from(path);
        let export = read(&path).unwrap();
        let folder = path.parent().unwrap();
        let store = Store::in_memory().unwrap();
        store.add_channel("c", &export.channel.name, "", "did:example:owner", 100).unwrap();
        let channel = store.channel("c").unwrap().unwrap();
        let mut setting = Context { folder, files: None, channel_names: HashMap::new(), roles: HashMap::new(), expressions: HashMap::new(), users: users_in(&export), now: 1 };
        let written = write(&store, &export, &channel, &setting, |_, _| {}).unwrap();
        let files = Files::scratch(&crate::config::FilesConfig::default(), "https://c.example/").unwrap();
        setting.files = Some(&files);
        let what = Refresh { embeds: true, files: true, mentions: true };
        let counted = refresh(&store, &export, &channel, &setting, what, true).unwrap();
        let cdn_in_text = store.db.lock().query_value::<i64>("SELECT COUNT(*) FROM posts WHERE content LIKE '%discordapp.%/attachments/%'", params![]).unwrap();
        println!(
            "{} posts brought in as text ({cdn_in_text} with a Discord attachment link in their text); a refresh would write {} again ({} their text or mentions), keeping {} files ({}), {} failed",
            written.posts,
            counted.posts,
            counted.texts,
            counted.files,
            bytes(counted.file_bytes),
            counted.files_failed.len()
        );
        let _ = std::fs::remove_dir_all(files.host.store.path(""));
    }
}

#[cfg(test)]
mod confined {
    use super::local;

    #[test]
    fn an_exports_files_are_only_ever_inside_its_folder() {
        let root = std::env::temp_dir().join(format!("discord-confined-{}", std::process::id()));
        let folder = root.join("export");
        std::fs::create_dir_all(folder.join("ch.json_Files")).unwrap();
        std::fs::write(folder.join("ch.json_Files/a b.png"), b"x").unwrap();
        std::fs::write(root.join("identity.json"), b"keys").unwrap();
        assert!(local(&folder, "ch.json_Files/a b.png").is_some());
        assert!(local(&folder, "ch.json_Files/a%20b.png").is_some());
        assert!(local(&folder, "../identity.json").is_none());
        assert!(local(&folder, "ch.json_Files/../../identity.json").is_none());
        assert!(local(&folder, root.join("identity.json").to_str().unwrap()).is_none());
        assert!(local(&folder, "%2E%2E/identity.json").is_none());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("identity.json"), folder.join("ch.json_Files/link.png")).unwrap();
            assert!(local(&folder, "ch.json_Files/link.png").is_none());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
