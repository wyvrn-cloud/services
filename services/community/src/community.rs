//! community/1.0: what the server does with each message a member sends it.
//!
//! Nothing here touches the network. [`Community::handle`] takes one message and
//! says what to answer, who else to tell, and whether there is now something to
//! send to everyone; `server.rs` does the sending.

use serde_json::{json, Map, Value};

use crate::config::{CommunityConfig, VoiceConfig};
use crate::store::{Channel, Invite, Member, Mention, Post, Report, Role, Store, Waiting};
use wyvrn_service::profile::{Change, Changed, Kept};

/// The setting naming the community's rules channel (`community`'s `rules_channel`).
pub const RULES_CHANNEL: &str = "rules_channel";
pub const PIURI: &str = "https://wyvrn.app/community/1.0";
pub const JOIN: &str = "https://wyvrn.app/community/1.0/join";
pub const LEAVE: &str = "https://wyvrn.app/community/1.0/leave";
pub const COMMUNITY: &str = "https://wyvrn.app/community/1.0/community";
pub const CREATE_CHANNEL: &str = "https://wyvrn.app/community/1.0/create-channel";
pub const UPDATE_CHANNEL: &str = "https://wyvrn.app/community/1.0/update-channel";
pub const POST: &str = "https://wyvrn.app/community/1.0/post";
pub const POSTED: &str = "https://wyvrn.app/community/1.0/posted";
pub const MESSAGE: &str = "https://wyvrn.app/community/1.0/message";
pub const HISTORY_REQUEST: &str = "https://wyvrn.app/community/1.0/history-request";
pub const HISTORY: &str = "https://wyvrn.app/community/1.0/history";
pub const MEMBERS_REQUEST: &str = "https://wyvrn.app/community/1.0/members-request";
/// Posts imported under a member's Discord account asked to be made theirs (claims.rs).
pub const CLAIM_IMPORTED: &str = "https://wyvrn.app/community/1.0/claim-imported";
/// A moderator's answer to one: `{ member, approve }`.
pub const DECIDE_CLAIM: &str = "https://wyvrn.app/community/1.0/decide-claim";
/// Raid mode, turned on for `{ minutes }` or off (`0`).
pub const SET_RAID: &str = "https://wyvrn.app/community/1.0/set-raid";
pub const MEMBERS: &str = "https://wyvrn.app/community/1.0/members";
pub const MEMBER_CHANGED: &str = "https://wyvrn.app/community/1.0/member-changed";
pub const SET_MEMBER_PROFILE: &str = "https://wyvrn.app/community/1.0/set-member-profile";
pub const MESSAGE_REQUEST: &str = "https://wyvrn.app/community/1.0/message-request";
pub const MESSAGE_REQUEST_SENT: &str = "https://wyvrn.app/community/1.0/message-request-sent";
pub const SET_MEMBER: &str = "https://wyvrn.app/community/1.0/set-member";
pub const SET_PROFILE: &str = "https://wyvrn.app/community/1.0/set-profile";
pub const WAITING: &str = "https://wyvrn.app/community/1.0/waiting";
pub const ASKED_TO_JOIN: &str = "https://wyvrn.app/community/1.0/asked-to-join";
pub const REMOVED: &str = "https://wyvrn.app/community/1.0/removed";
pub const SET_ADMISSION: &str = "https://wyvrn.app/community/1.0/set-admission";
pub const CREATE_INVITE: &str = "https://wyvrn.app/community/1.0/create-invite";
pub const REVOKE_INVITE: &str = "https://wyvrn.app/community/1.0/revoke-invite";
pub const INVITES_REQUEST: &str = "https://wyvrn.app/community/1.0/invites-request";
pub const INVITES: &str = "https://wyvrn.app/community/1.0/invites";
pub const SET_VOICE: &str = "https://wyvrn.app/community/1.0/set-voice";
pub const SET_WEB: &str = "https://wyvrn.app/community/1.0/set-web";
pub const SET_WELCOME: &str = "https://wyvrn.app/community/1.0/set-welcome";
pub const RULES: &str = "https://wyvrn.app/community/1.0/rules";
pub const ACCEPT_RULES: &str = "https://wyvrn.app/community/1.0/accept-rules";
pub const SET_RULES: &str = "https://wyvrn.app/community/1.0/set-rules";
pub const REPORT: &str = "https://wyvrn.app/community/1.0/report";
/// How many reports one member may make in `REPORTS_WINDOW` seconds.
const REPORTS_PER_WINDOW: usize = 5;
const REPORTS_WINDOW: i64 = 600;
pub const REPORT_RECEIVED: &str = "https://wyvrn.app/community/1.0/report-received";
pub const REPORTS_REQUEST: &str = "https://wyvrn.app/community/1.0/reports-request";
pub const REPORTS: &str = "https://wyvrn.app/community/1.0/reports";
pub const RESOLVE_REPORT: &str = "https://wyvrn.app/community/1.0/resolve-report";
// A post is answered, reacted to, changed, taken back and pinned as any message
// is, by the protocols that already say how, addressed to the server: it is the
// server that keeps the post.
pub const MESSAGE_EDITS: &str = "https://wyvrn.app/message-edits/1.0";
pub const EDIT: &str = "https://wyvrn.app/message-edits/1.0/edit";
pub const DELETE: &str = "https://wyvrn.app/message-edits/1.0/delete";
pub const MESSAGE_PINS: &str = "https://wyvrn.app/message-pins/1.0";
pub const PIN: &str = "https://wyvrn.app/message-pins/1.0/pin";
pub const UNPIN: &str = "https://wyvrn.app/message-pins/1.0/unpin";
pub const MESSAGE_REACTIONS: &str = "https://wyvrn.app/message-reactions/1.0";
pub const REACT: &str = "https://wyvrn.app/message-reactions/1.0/reactions";
/// As long as a quotation of the post answered is kept (chat-message/1.0).
const MAX_SNIPPET: usize = 100;
pub const CREATE_THREAD: &str = "https://wyvrn.app/community/1.0/create-thread";
pub const CHANGED: &str = "https://wyvrn.app/community/1.0/changed";
pub const CHANGES_REQUEST: &str = "https://wyvrn.app/community/1.0/changes-request";
pub const CHANGES: &str = "https://wyvrn.app/community/1.0/changes";

/// Who a post calls on when it calls on everyone in the community.
pub const EVERYONE: &str = "everyone";
/// As many as one post may call on by name.
pub const MAX_MENTIONS: usize = 50;
const MAX_MENTION_NAME: usize = 40;
/// As many different emoji as one post takes, and as one member puts on it.
const MAX_REACTIONS: usize = 20;
pub const MAX_THREAD_NAME: usize = 80;
/// As many changed posts as one `changes` carries.
const CHANGES_BATCH: usize = 100;

pub const USER_PROFILE: &str = "https://didcomm.org/user-profile/1.0";
pub const PROFILE: &str = "https://didcomm.org/user-profile/1.0/profile";
pub const REQUEST_PROFILE: &str = "https://didcomm.org/user-profile/1.0/request-profile";
pub const PROBLEM_REPORT: &str = "https://didcomm.org/report-problem/2.0/problem-report";

pub const DEFAULT_HISTORY_LIMIT: usize = 30;
pub const MAX_HISTORY_LIMIT: usize = 100;
const MAX_NAME_LENGTH: usize = 80;

/// Where a move to another server is marked (`Community::moving`): `1` while
/// moving. A copy handed to the server it moves to has it cleared.
pub const MOVING_SETTING: &str = "moving";

/// Whether a message of `message_type` only reads, or leaves: all a community that
/// is moving answers.
pub fn reads_only(message_type: &str) -> bool {
    message_type.ends_with("-request") || message_type == LEAVE || message_type == crate::posting::WATCH
}
pub(crate) const MAX_CHANNEL_NAME: usize = 100;
pub(crate) const MAX_TOPIC: usize = 1024;
/// A question's limits: how many at once, how many answers each offers, and how
/// long any of it may be.
const MAX_QUESTIONS: usize = 4;
const MAX_OPTIONS: usize = 8;
const MAX_QUESTION_TEXT: usize = 1_000;
const MAX_HEADER: usize = 40;
const MAX_LABEL: usize = 120;
const MAX_DESCRIPTION: usize = 400;
const MAX_INVITE_NOTE: usize = 200;
const MAX_RULES: usize = 20_000;
/// The most a voice channel's `limit` may be.
const MAX_VOICE_LIMIT: i64 = 500;

/// The kinds of channel a host may turn off (`disabled_channel_kinds`): every
/// kind but a plain text channel.
pub const CHANNEL_KINDS: &[&str] = &["announcement", "voice", "stage", "forum", "gallery", "calendar", "form", "wiki"];

/// What kind of channel `channel` is, as `create-channel`'s `kind` names it, with
/// an announcement channel's and a stage's own: `text`, `announcement`, `voice`,
/// `stage`, `forum`, `gallery`, `calendar`, `form` or `wiki`.
pub fn channel_kind(channel: &crate::store::Channel) -> &'static str {
    if channel.voice.is_some() {
        if crate::voice::is_stage(channel) { "stage" } else { "voice" }
    } else if channel.gallery {
        "gallery"
    } else if channel.forum.is_some() {
        "forum"
    } else if channel.calendar {
        "calendar"
    } else if channel.form.is_some() {
        "form"
    } else if channel.wiki {
        "wiki"
    } else if channel.announcement {
        "announcement"
    } else {
        "text"
    }
}

/// The permissions in a channel that add to it, or join it: what a channel of a
/// kind its host turned off is without, whoever holds them. Seeing it, and
/// keeping order in it (`manage_messages`, `moderate_voice`), stay.
pub const ADDING: &[&str] = &["post", "react", "create_threads", "create_private_threads", "send_in_threads", "create_polls", "attach_files", "mention_everyone", "bypass_slow_mode", "connect", "speak", "video", "manage_webhooks"];

/// A voice channel's settings as an admin gives them (community-voice/1.0, "A voice
/// channel"), laid over what it had: checked, and with nothing in them the protocol
/// doesn't name. `Err` says what is wrong.
/// How long a stage's topic may be, as Discord's.
const MAX_STAGE_TOPIC: usize = 120;

pub fn clean_voice(given: &Value, held: Option<&Value>) -> Result<Value, &'static str> {
    let mut out = held.and_then(Value::as_object).cloned().unwrap_or_default();
    if !given.is_object() && !given.is_null() {
        return Err("A voice channel's settings are an object.");
    }
    for (field, allowed) in [("transport", &["moq", "webrtc"][..]), ("speak", &["everyone", "admins"]), ("video", &["everyone", "admins"]), ("screen", &["everyone", "admins"])] {
        match given.get(field) {
            None => {}
            Some(Value::Null) => {
                out.remove(field);
            }
            Some(value) => match value.as_str().filter(|v| allowed.contains(v)) {
                Some(v) => {
                    out.insert(field.into(), json!(v));
                }
                None => return Err("A voice setting has a value it doesn't take."),
            },
        }
    }
    // A stage (community-voice/1.0's "Stages"), and what it is about.
    match given.get("stage") {
        None => {}
        Some(Value::Bool(true)) => {
            out.insert("stage".into(), json!(true));
        }
        Some(Value::Bool(false)) | Some(Value::Null) => {
            out.remove("stage");
        }
        Some(_) => return Err("A stage is true or false."),
    }
    match given.get("topic") {
        None => {}
        Some(Value::Null) => {
            out.remove("topic");
        }
        Some(Value::String(topic)) => {
            let topic: String = topic.trim().chars().take(MAX_STAGE_TOPIC).collect();
            if topic.is_empty() {
                out.remove("topic");
            } else {
                out.insert("topic".into(), json!(topic));
            }
        }
        Some(_) => return Err("A stage's topic is text."),
    }
    match given.get("limit") {
        None => {}
        Some(Value::Null) => {
            out.remove("limit");
        }
        Some(value) => match value.as_i64().filter(|n| (1..=MAX_VOICE_LIMIT).contains(n)) {
            Some(n) => {
                out.insert("limit".into(), json!(n));
            }
            None => return Err("A voice channel holds between 1 and 500."),
        },
    }
    Ok(Value::Object(out))
}
const MAX_REPORT_NOTE: usize = 1_000;
const REPORT_REASONS: [&str; 7] = ["spam", "harassment", "hate", "violence", "sexual", "illegal", "other"];

/// The community's rules, where it has any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    pub text: String,
    pub version: i64,
    pub required: bool,
}

/// Who a `join` from someone who isn't a member lets in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Nobody: an admin lets them in.
    Admin,
    /// Whoever brings a valid invite.
    Invite,
    /// Everyone.
    Open,
}

impl Admission {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "admin" => Some(Self::Admin),
            "invite" => Some(Self::Invite),
            "open" => Some(Self::Open),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Invite => "invite",
            Self::Open => "open",
        }
    }
}

/// A new invite's code: 256 bits of randomness, in hex.
/// A new invite code: 14 letters and digits (about 80 bits), short enough for a
/// link that fits a small QR code (`/invite/<code>`) and too many to guess. Codes
/// made before were 64 hex digits, and still work.
fn new_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut random = [0u8; 32];
    random[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    random[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    // (Bytes past the largest multiple of the alphabet's length are skipped, so
    // each letter is as likely as the next.)
    let most = 256 - 256 % ALPHABET.len();
    random.iter().filter(|b| (**b as usize) < most).take(14).map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect()
}

pub struct Community {
    pub store: Store,
    pub config: CommunityConfig,
    /// What its store holds, in bytes, as its host last measured it (0 unmeasured):
    /// what `config.max_store_bytes` is held to.
    pub used_bytes: std::sync::atomic::AtomicU64,
    /// What an admin has changed of its name, description and picture
    /// (`set-profile`), and where that is kept.
    changed: std::sync::RwLock<Changed>,
    kept: Option<Kept>,
    /// Voice channels, where the server has them (community-voice/1.0).
    pub voice: Option<VoiceConfig>,
    /// An AI assistant answering in its channels, as `community` says of it
    /// (community/1.0's `assistant`; assistant.rs).
    pub assistant: Option<Value>,
    /// The server's own DID, for what it posts itself (AutoMod's alerts).
    pub own: Option<String>,
    /// Files members attach, where the community keeps them (files.rs).
    pub files: Option<crate::files::Files>,
    /// The community's own sticker and emoji packs (expressions.rs).
    pub expressions: Option<crate::expressions::Expressions>,
    /// How many live connections each member has open now (who may be seen online).
    pub live: std::sync::RwLock<std::collections::HashMap<String, usize>>,
    /// Where its web view is, where the server shows one (web/).
    pub web_url: Option<String>,
    /// The server's own address, where members' pictures are (member_ids.rs).
    pub public_url: String,
    /// The DID it speaks as, where that isn't `own` (a did:webvh): never a person's.
    pub speaks_as: Option<String>,
    /// Held while a Discord template is imported: one import at a time.
    pub importing: std::sync::Mutex<()>,
    /// The seed of its DID's Ed25519 key, which signs what announcement channels
    /// send out to other services (announcements.rs).
    pub signing_seed: Option<[u8; 32]>,
    /// Messages to send to other DIDs from where no answer goes out (a workflow's
    /// step): sent by the background loop (outgoing.rs).
    pub outbox: std::sync::Mutex<Vec<(String, Value)>>,
    /// Wiki pages open now, and who has each open (wiki.rs).
    pub wiki_open: crate::wiki::OpenPages,
    /// Held while a change is made to a wiki page (wiki.rs): each is read, applied
    /// and written back whole, and two at once both wrote the same number and one
    /// change was lost.
    pub wiki_writing: std::sync::Mutex<()>,
}

/// What handling a message came to.
#[derive(Debug, Default)]
pub struct Outcome {
    /// What to answer the sender with, if anything.
    pub reply: Option<Value>,
    /// Messages for others, a DID each.
    pub notices: Vec<(String, Value)>,
    /// Whether there is now something to send to everyone who has joined.
    pub posted: bool,
    /// Whether the channels changed: everyone who has joined is told.
    pub channels_changed: bool,
    /// Whether the community's name, description or picture changed.
    pub profile_changed: bool,
    /// Posts that changed (edited, taken back, pinned, reacted to, grown into a
    /// thread): everyone who has joined is sent each as it now stands.
    pub changed: Vec<String>,
    /// The post just made, for an assistant to answer.
    pub posted_id: Option<String>,
    /// For every live connection open now (someone seen online, or not).
    pub live: Vec<Value>,
    /// For the live connections of one member each, and only those: dropped
    /// where they have none (a wiki's changes and cursors).
    pub live_to: Vec<(String, Value)>,
}

impl Outcome {
    pub(crate) fn reply(reply: Value) -> Self {
        Self { reply: Some(reply), ..Self::default() }
    }
}

fn thread_of(message: &Value) -> Option<&str> {
    message["thid"].as_str().or(message["id"].as_str())
}

pub(crate) fn reply_to(message: &Value, message_type: &str, body: Value) -> Value {
    let mut reply = json!({ "type": message_type, "body": body });
    if let Some(thid) = thread_of(message) {
        reply["thid"] = json!(thid);
    }
    reply
}

/// A report-problem/2.0 `problem-report` about `message`.
pub fn problem(message: &Value, code: &str, comment: &str) -> Value {
    let mut report = json!({ "type": PROBLEM_REPORT, "body": { "code": code, "comment": comment } });
    if let Some(thid) = thread_of(message) {
        report["pthid"] = json!(thid);
    }
    if let Some(id) = message["id"].as_str() {
        report["ack"] = json!([id]);
    }
    report
}

/// Whether `text` could be one emoji. What a reaction arrives as is someone's text,
/// shown to everyone: nothing with a letter, a space or a control character in it,
/// nothing long, and something that is at least a symbol. (A custom emoji is
/// written as it is in text, a picture alone: `![party_cat](https://…/cat.png)`.)
pub fn is_emoji(text: &str) -> bool {
    if let Some(custom) = text.strip_prefix("![").and_then(|rest| rest.strip_suffix(')')) {
        let Some((name, address)) = custom.split_once("](") else { return false };
        let named = !name.is_empty() && name.len() <= 34 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == ':');
        return named && text.len() <= 200 && address.starts_with("https://") && !address.contains(|c: char| c.is_whitespace() || c.is_control() || "()<>\"".contains(c));
    }
    let keycap = text.contains('\u{20E3}');
    !text.is_empty()
        && text.len() <= 64
        && text.chars().count() <= 16
        && text.chars().all(|c| !c.is_alphabetic() && !c.is_whitespace() && !c.is_control() && (!c.is_ascii() || (keycap && "#*0123456789".contains(c))))
        && text.chars().any(|c| c as u32 >= 0xA9)
}

pub(crate) fn refuse(message: &Value, code: &str, comment: &str) -> anyhow::Result<Outcome> {
    Ok(Outcome::reply(problem(message, code, comment)))
}

/// Text someone typed for a name or a label: trimmed, on one line, and not longer
/// than `max` characters. `None` if that leaves nothing.
/// What a text channel's name is made of (a regex class's inside): see
/// [`text_channel_name`].
pub(crate) const NAME_CHARACTERS: &str = r"\p{Ll}\p{Lo}\p{Lm}\p{Mn}\p{Mc}\p{Me}\p{Nd}_.\-\p{Extended_Pictographic}\p{Emoji_Modifier}\p{Regional_Indicator}\x{200D}\x{FE0F}";

/// Whether a name is one a text channel may be given (community/1.0, "A text
/// channel's name is one word"): lowercase letters of any script, letters of
/// scripts without case, their marks, digits, `_`, `.` and `-`, and emoji (📜rules).
pub(crate) fn text_channel_name(name: &str) -> bool {
    static WORD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(&format!("^[{NAME_CHARACTERS}]+$")).expect("a valid pattern")
    });
    WORD.is_match(name)
}

const INVALID_NAME: &str = "A text channel's name is one word in lowercase: letters, digits, _, . and -, and emoji, as in design-review or 📜rules.";

pub(crate) fn line(value: &Value, max: usize) -> Option<String> {
    let text: String = value.as_str()?.split_whitespace().collect::<Vec<_>>().join(" ");
    let text: String = text.chars().take(max).collect();
    (!text.is_empty()).then_some(text)
}

/// A question as a post may ask it, checked and with nothing in it but what the
/// protocol names. `Err` says what is wrong with it.
pub fn clean_question(question: &Value) -> Result<Value, &'static str> {
    let asked = question["questions"].as_array().filter(|asked| !asked.is_empty()).ok_or("A question needs at least one question in it.")?;
    if asked.len() > MAX_QUESTIONS {
        return Err("That is more questions than can be asked at once.");
    }
    let mut cleaned = Vec::new();
    for one in asked {
        let text: String = one["text"].as_str().unwrap_or_default().trim().chars().take(MAX_QUESTION_TEXT).collect();
        if text.is_empty() {
            return Err("Each question needs its text.");
        }
        let options = one["options"].as_array().map(Vec::as_slice).unwrap_or_default();
        if options.len() < 2 || options.len() > MAX_OPTIONS {
            return Err("Each question offers between two and eight answers.");
        }
        let mut labels: Vec<String> = Vec::new();
        let mut offered = Vec::new();
        for option in options {
            let label = line(&option["label"], MAX_LABEL).ok_or("Each answer needs a label.")?;
            if labels.iter().any(|seen| seen.eq_ignore_ascii_case(&label)) {
                return Err("A question's answers have to differ.");
            }
            let mut entry = Map::new();
            entry.insert("label".into(), json!(label));
            if let Some(description) = line(&option["description"], MAX_DESCRIPTION) {
                entry.insert("description".into(), json!(description));
            }
            labels.push(label);
            offered.push(Value::Object(entry));
        }
        let mut entry = Map::new();
        if let Some(header) = line(&one["header"], MAX_HEADER) {
            entry.insert("header".into(), json!(header));
        }
        entry.insert("text".into(), json!(text));
        entry.insert("options".into(), json!(offered));
        if one["multiple"] == true {
            entry.insert("multiple".into(), json!(true));
        }
        cleaned.push(Value::Object(entry));
    }
    Ok(json!({ "questions": cleaned }))
}

/// An answer, as text anyone can read: a line for each question.
pub fn answer_text(question: &Value, answers: &[Value]) -> String {
    let empty = Vec::new();
    let asked = question["questions"].as_array().unwrap_or(&empty);
    answers
        .iter()
        .enumerate()
        .map(|(at, answer)| {
            let about = asked.get(at).map(|q| q["header"].as_str().unwrap_or_else(|| q["text"].as_str().unwrap_or_default())).unwrap_or_default();
            let mut said: Vec<String> = answer["chosen"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
            if let Some(other) = answer["other"].as_str().filter(|other| !other.is_empty()) {
                said.push(other.to_string());
            }
            format!("**{}** {}", about.trim_end_matches(':'), said.join(", "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Community {
    /// A channel as `to` is told of it: `yours` only for the one who opened it,
    /// which is what they may change it by; nobody else is told who that is.
    fn channel_body(channel: &Channel, to: &str) -> Value {
        let mut body = json!({ "id": channel.id, "name": channel.name, "topic": channel.topic, "closed": channel.closed, "latest_seq": channel.latest_seq });
        if channel.created_by == to {
            body["yours"] = json!(true);
        }
        if let Some(voice) = &channel.voice {
            body["kind"] = json!("voice");
            body["voice"] = voice.clone();
        }
        if channel.slow_mode > 0 {
            body["slow_mode"] = json!(channel.slow_mode);
        }
        if let Some(category) = &channel.category {
            body["category"] = json!(category);
        }
        if let Some(tags) = &channel.forum {
            body["kind"] = json!(if channel.gallery { "gallery" } else { "forum" });
            body["tags"] = tags.clone();
            if let Some(sections) = channel.sections.as_ref().filter(|s| s.as_array().is_some_and(|a| !a.is_empty())) {
                body["sections"] = sections.clone();
            }
        }
        if channel.announcement {
            body["announcement"] = json!(true);
        }
        if channel.calendar {
            body["kind"] = json!("calendar");
        }
        if channel.form.is_some() {
            body["kind"] = json!("form");
        }
        if channel.wiki {
            body["kind"] = json!("wiki");
        }
        body
    }

    /// A post as `message` says it.
    pub fn post_body(&self, post: &Post, _now: i64) -> Value {
        Self::message_body(post)
    }

    /// The server with voice channels (community-voice/1.0).
    pub fn with_voice(mut self, voice: Option<VoiceConfig>) -> Self {
        self.voice = voice;
        self
    }

    /// The community's voice defaults, as `community` says them: the transport,
    /// and the text channel invitations are posted in.
    pub fn voice_defaults(&self) -> anyhow::Result<Option<Value>> {
        let Some(voice) = &self.voice else { return Ok(None) };
        let transport = self.store.setting("voice_transport")?.unwrap_or_else(|| voice.transport.clone());
        let mut body = json!({ "transport": transport, "text_chat": self.voice_text_chat()? });
        if let Some(channel) = self.store.setting("voice_invites_to")?.filter(|c| !c.is_empty()) {
            body["invites_to"] = json!(channel);
        }
        Ok(Some(body))
    }

    /// Whether voice channels (and stages) have a text chat of their own: off unless
    /// an admin turns it on (`set-voice`'s `text_chat`), since a chat beside a call
    /// is one more place to keep an eye on.
    pub fn voice_text_chat(&self) -> anyhow::Result<bool> {
        Ok(self.store.setting("voice_text")?.as_deref() == Some("true"))
    }

    /// A community as its settings have it, with what an admin changed last time
    /// laid over them. `kept` is where a change is written; without it, one lasts
    /// until the server stops.
    /// Whether what a deleted post said is kept for moderators (`deleted_text_days`).
    pub fn keeps_deleted_text(&self) -> bool {
        self.config.deleted_text_days > 0
    }

    /// How long what a deleted post said is kept, in seconds.
    pub fn deleted_text_kept_for(&self) -> i64 {
        i64::from(self.config.deleted_text_days) * 24 * 60 * 60
    }

    pub fn new(store: Store, config: CommunityConfig, kept: Option<Kept>) -> anyhow::Result<Self> {
        let changed = match &kept {
            Some(kept) => kept.load()?,
            None => Changed::default(),
        };
        let community = Self { store, config, used_bytes: Default::default(), changed: std::sync::RwLock::new(changed), kept, voice: None, assistant: None, own: None, files: None, expressions: None, live: Default::default(), web_url: None, public_url: String::new(), speaks_as: None, importing: Default::default(), signing_seed: None, outbox: Default::default(), wiki_open: Default::default(), wiki_writing: Default::default() };
        community.ensure_roles()?;
        Ok(community)
    }

    /// Sets what the community is called and says of itself, as `set-profile`
    /// would (a Discord template's name and description, discord.rs).
    pub(crate) fn set_name_and_description(&self, name: &str, description: &str) -> anyhow::Result<()> {
        let changed = {
            let mut changed = self.changed.write().unwrap_or_else(|poisoned| poisoned.into_inner());
            changed.name = Some(name.to_string());
            changed.description = Some(description.to_string());
            changed.clone()
        };
        if let Some(kept) = &self.kept {
            kept.save(&changed)?;
        }
        Ok(())
    }

    /// An out-of-band/2.0 invitation to join, from `did`, under invite `code` (its
    /// id, which joining names): what a short invite link gives an app. Carries
    /// who the community is (user-profile/1.0's profile, picture and all), as an
    /// invite the app makes itself does.
    pub fn invitation(&self, did: &str, code: &str) -> Value {
        let profile = self.profile_message();
        let name = profile["body"]["profile"]["displayName"].as_str().unwrap_or_default().to_string();
        let mut attached = json!({ "id": uuid::Uuid::new_v4().to_string(), "type": PROFILE, "body": profile["body"].clone() });
        if let Some(attachments) = profile.get("attachments") {
            attached["attachments"] = attachments.clone();
        }
        json!({
            "type": "https://didcomm.org/out-of-band/2.0/invitation",
            "id": code,
            "from": did,
            "body": {
                "goal_code": "wyvrn.community.join",
                "goal": if name.is_empty() { "Join this community".to_string() } else { format!("Join {name}") },
                "accept": ["didcomm/v2+cbor", "didcomm/v2"],
            },
            "attachments": [{ "id": "profile", "media_type": "application/json", "data": { "json": attached } }],
        })
    }

    /// Who the community is, as user-profile/1.0 says it: its name, description
    /// and picture now, a picture that is gone said to be gone.
    pub fn profile_message(&self) -> Value {
        let changed = self.changed();
        let name = changed.name.clone().unwrap_or_else(|| self.config.name.clone());
        let description = changed.description.clone().unwrap_or_else(|| self.config.description.clone());
        Changed::profile_message(&name, &description, changed.picture.as_ref())
    }

    /// What an admin has changed of the community's name, description and picture.
    pub fn changed(&self) -> Changed {
        self.changed.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    /// Whether the community is at its limit of members (`max_members`).
    pub fn full(&self) -> anyhow::Result<bool> {
        Ok(self.config.max_members > 0 && self.store.member_count()? >= self.config.max_members as i64)
    }

    /// Whether the community is moving to another server, and so read-only
    /// (community/1.0's "Hosted communities, and moving").
    pub fn moving(&self) -> anyhow::Result<bool> {
        Ok(self.store.setting(MOVING_SETTING)?.as_deref() == Some("1"))
    }

    /// Begins a move, or calls it off: whether that changed anything. Every joined
    /// member is then to be told (`community_notices`).
    pub fn set_moving(&self, moving: bool) -> anyhow::Result<bool> {
        if self.moving()? == moving {
            return Ok(false);
        }
        self.store.set_setting(MOVING_SETTING, if moving { "1" } else { "0" })?;
        Ok(true)
    }

    /// The community as `member` is told of it.
    pub fn community(&self, member: &Member) -> anyhow::Result<Value> {
        let powers = self.powers(&member.did)?;
        let mut channels = Vec::new();
        let public = match &self.web_url {
            Some(_) => crate::web::public_channels(self)?,
            None => Default::default(),
        };
        let mut seen = std::collections::HashSet::new();
        let follows = self.store.follows()?;
        let imported = self.store.imported_ranges()?;
        for channel in self.store.channels()? {
            // A channel they may not see isn't there for them.
            if !self.sees(&member.did, &powers, &channel)? {
                continue;
            }
            seen.insert(channel.id.clone());
            let mut body = Self::channel_body(&channel, &member.did);
            // Posts brought in from elsewhere: numbers that are history, read by
            // time, not news nor a hole to fill.
            if let Some(ranges) = imported.get(&channel.id) {
                body["imported_seqs"] = json!(ranges.iter().map(|(from, to)| json!([from, to])).collect::<Vec<_>>());
            }
            body["permissions"] = json!(self.channel_permissions(&member.did, &powers, &channel)?);
            // On the web: its page's address, and from when it is public.
            if let (Some(url), None) = (&self.web_url, &channel.voice) {
                body["url"] = json!(format!("{url}{}", crate::web::channel_path(&channel.id, &channel.name)));
                if let Some(since) = public.get(&channel.id) {
                    body["web"] = json!({ "public": true, "since": since });
                }
            }
            // Of a kind its host turned off: read-only, and why.
            if self.channel_off(&channel) {
                body["read_only"] = json!(true);
                body["kind_off"] = json!(true);
            } else if self.read_only(&channel.id)? {
                body["read_only"] = json!(true);
            }
            if let Some(entry) = self.form_entry(&channel, &member.did)? {
                for (key, value) in entry.as_object().into_iter().flatten() {
                    body[key] = value.clone();
                }
            }
            // What it follows, for whoever may manage it.
            if powers.has("manage_channels") || channel.created_by == member.did {
                let held = Self::follows_body(&follows, &channel.id);
                if !held.is_empty() {
                    body["follows"] = json!(held);
                }
            }
            channels.push(body);
        }
        let changed = self.changed();
        Ok(json!({
            "name": changed.name.unwrap_or_else(|| self.config.name.clone()),
            "description": changed.description.unwrap_or_else(|| self.config.description.clone()),
            "role": powers.older_role(),
            "joined": member.joined,
            "owner": self.owner()?,
            "owners": self.owners()?,
            "you": member.did,
            "message_requests": self.store.member_here(&member.did)?.is_none_or(|here| here.requests),
            // Where its short invite links are: an invite's code after it.
            "invite_url": format!("{}/invite/", self.public_url.trim_end_matches('/')),
            "roles": self.roles_body()?,
            "my_roles": powers.roles,
            "permissions": powers.listed(),
            "members_may_create_channels": self.everyone_has("create_channels")?,
            "admission": self.admission()?.as_str(),
            "requires_address": self.requires_address()?,
            "members_may_invite": self.members_may_invite()?,
            // The kinds of channel that may be opened here (its host's say).
            "channel_kinds": self.channel_kinds(),
            "channels": channels,
            // Where the last change to any post stands: an app holding less asks for
            // what it missed (`changes-request`).
            "latest_rev": self.store.latest_rev()?,
        }))
        .and_then(|mut body: Value| {
            if let Some(rules) = self.rules()? {
                body["rules"] = json!(rules.text);
                body["rules_version"] = json!(rules.version);
                body["rules_required"] = json!(rules.required);
            }
            if let Some(accepted) = self.store.rules_accepted(&member.did)? {
                body["rules_accepted"] = json!(accepted);
            }
            // The channel an admin made the rules channel, where this member sees it:
            // shown with a book in place of its #.
            if let Some(id) = self.store.setting(RULES_CHANNEL)?.filter(|id| seen.contains(id)) {
                body["rules_channel"] = json!(id);
            }
            if let Some(mut voice) = self.voice_defaults()? {
                if voice["invites_to"].as_str().is_some_and(|id| !seen.contains(id)) {
                    voice.as_object_mut().map(|v| v.remove("invites_to"));
                }
                body["voice"] = voice;
            }
            if let Some(assistant) = &self.assistant {
                body["assistant"] = assistant.clone();
            }
            // Appeals against bans waiting, and how they are taken (appeals.rs).
            if powers.has("ban_members") || powers.has("manage_community") {
                let (on, wait, cooldown) = self.appeal_settings()?;
                body["appeals"] = json!({ "on": on, "wait": wait, "cooldown": cooldown, "waiting": if powers.has("ban_members") { self.open_appeals()? } else { 0 } });
            }
            if powers.has("admit_members") {
                body["waiting"] = json!(self.store.waiting_count()?);
                // Imported posts members asked to have made theirs.
                let asked = self.store.claims_asked()?;
                if !asked.is_empty() {
                    body["claims"] = json!(asked
                        .iter()
                        .map(|claim| Ok(json!({ "member": claim.did, "discord_id": claim.discord_id, "posts": self.imported_count(&claim.discord_id)?, "at": claim.at })))
                        .collect::<anyhow::Result<Vec<_>>>()?);
                }
            }
            // Posts imported under this member's verified Discord account, and where
            // their claim on them stands.
            if let Some(claim) = self.store.claim_of(&member.did)? {
                let posts = if claim.state == "claimed" { 0 } else { self.imported_count(&claim.discord_id)? };
                if claim.state == "claimed" || posts > 0 {
                    body["imported_claim"] = json!({ "from": "discord", "state": claim.state, "posts": posts });
                }
            }
            if self.moving()? {
                body["moving"] = json!(true);
            }
            if let Some(url) = &self.web_url {
                body["web"] = crate::web::community_body(url, &self.store);
            }
            // How newcomers are welcomed, to whoever may change it.
            if powers.has("manage_community") {
                let set = self.welcome_setting()?;
                body["welcome"] = json!({ "channel": set["channel"], "lines": set["lines"], "built_in": crate::welcome::BUILT_IN });
            }
            if let Some(level) = self.store.notify_level(&member.did)? {
                let following: Vec<String> = self.store.followed_threads(&member.did)?.into_iter().filter(|thread| self.store.post_by_id(thread).ok().flatten().is_some_and(|post| seen.contains(&post.channel))).collect();
                body["notifications"] = json!({ "level": level, "following": following });
            }
            if let Some(until) = self.store.timed_out_until(&member.did, crate::gates::now())? {
                body["timed_out_until"] = json!(until);
            }
            {
                let gates = self.gates()?;
                let mut said = json!({});
                if let Some(wait) = self.post_wait()? {
                    said["post_wait"] = json!(wait);
                }
                if gates["linked_account"] == true {
                    said["linked_account"] = json!(true);
                }
                if let Some(hours) = gates["new_member_hours"].as_i64().filter(|h| *h > 0) {
                    said["new_member_hours"] = json!(hours);
                }
                if said.as_object().is_some_and(|o| !o.is_empty()) {
                    body["gates"] = said;
                }
            }
            // Raid mode, and a burst of joins that may be one, to whoever may stop it.
            if powers.has("ban_members") || powers.has("manage_community") {
                let now = crate::gates::now();
                if let Some(raid) = self.raid(now)? {
                    body["raid"] = json!({ "until": raid["until"] });
                } else if let Some(joins) = self.raid_suggested(now)? {
                    body["raid_suggested"] = json!({ "joins": joins, "minutes": crate::protection::BURST_WINDOW / 60 });
                }
            }
            // A category with nothing in it they may see isn't there for them either,
            // unless they arrange channels.
            let mut categories = self.store.categories()?;
            if !powers.has("manage_channels") {
                let holding: std::collections::HashSet<String> = self.store.channels()?.into_iter().filter(|c| seen.contains(&c.id)).filter_map(|c| c.category).collect();
                categories.retain(|c| holding.contains(&c.id));
            }
            if !categories.is_empty() {
                body["categories"] = json!(categories.iter().map(|c| json!({ "id": c.id, "name": c.name })).collect::<Vec<_>>());
            }
            if let Some(files) = &self.files {
                body["files"] = json!({ "max_file_bytes": files.max_file_bytes() });
            }
            if let (Some(expressions), Some(own)) = (&self.expressions, &self.own) {
                let packs = expressions.packs(own)?;
                if !packs.is_empty() {
                    body["expressions"] = json!({ "packs": packs });
                }
            }
            let (share, status) = self.store.presence(&member.did)?;
            body["presence"] = json!({ "share": share, "status": status });
            if self.has_onboarding()? {
                body["onboarding"] = json!({ "done": self.store.onboarded(&member.did)? });
            }
            Ok(body)
        })
    }

    /// The rules as an admin last set them, if there are any.
    pub fn rules(&self) -> anyhow::Result<Option<Rules>> {
        let Some(text) = self.store.setting("rules")?.filter(|text| !text.is_empty()) else { return Ok(None) };
        let version = self.store.setting("rules_version")?.and_then(|v| v.parse().ok()).unwrap_or(1);
        let required = self.store.setting("rules_required")?.as_deref() == Some("true");
        Ok(Some(Rules { text, version, required }))
    }

    /// Whether `did` has yet to accept rules that are required.
    pub fn rules_owed_by(&self, did: &str) -> anyhow::Result<bool> {
        Ok(self.rules_owed(did)?.is_some())
    }

    /// The text channel voice invitations are posted in: the one an admin chose,
    /// else the first open text channel.
    pub fn invitations_channel(&self) -> anyhow::Result<Option<Channel>> {
        if let Some(id) = self.store.setting("voice_invites_to")?.filter(|id| !id.is_empty()) {
            if let Some(channel) = self.store.channel(&id)?.filter(|c| !c.closed && c.voice.is_none()) {
                return Ok(Some(channel));
            }
        }
        Ok(self.store.channels()?.into_iter().find(|c| !c.closed && c.voice.is_none()))
    }

    /// Posts, as `inviter`, an invitation into the voice channel `voice` for those
    /// who won't be rung (community-voice/1.0's `invite`), naming them.
    pub fn post_voice_invite(&self, text: &Channel, inviter: &Member, voice: &Channel, invitees: &[String], now: i64) -> anyhow::Result<()> {
        let mut mentions = Vec::new();
        let mut names = Vec::new();
        for did in invitees {
            let name = self.store.member(did)?.and_then(|m| m.name).unwrap_or_else(|| "someone".to_string());
            names.push(format!("@{name}"));
            mentions.push(Mention { who: did.clone(), name });
        }
        let content = format!("{} invites {} to #{}", inviter.name.as_deref().unwrap_or("Someone"), names.join(", "), voice.name);
        let id = uuid::Uuid::new_v4().to_string();
        let post = self.store.add_post(&text.id, &id, &content, inviter, None, None, now)?;
        self.store.set_written_with(&post.id, None, None, &mentions)?;
        self.store.set_post_voice_invite(&post.id, &voice.id)?;
        Ok(())
    }

    /// The rules `did` has yet to accept, where they are required.
    fn rules_owed(&self, did: &str) -> anyhow::Result<Option<Rules>> {
        let Some(rules) = self.rules()?.filter(|rules| rules.required) else { return Ok(None) };
        Ok((self.store.rules_accepted(did)?.unwrap_or(0) < rules.version).then_some(rules))
    }

    /// A `join` where rules are owed: accepted by it (`accept_rules`, the current
    /// version), or answered with them. `Some` is the answer.
    fn rules_before_join(&self, sender: &str, message: &Value) -> anyhow::Result<Option<Outcome>> {
        let Some(rules) = self.rules_owed(sender)? else { return Ok(None) };
        if message["body"]["accept_rules"].as_i64() == Some(rules.version) {
            self.store.accept_rules(sender, rules.version)?;
            return Ok(None);
        }
        let name = self.changed().name.unwrap_or_else(|| self.config.name.clone());
        Ok(Some(Outcome::reply(reply_to(message, RULES, json!({ "name": name, "rules": rules.text, "rules_version": rules.version })))))
    }

    /// A report as `viewer` is shown it: a post reported in a channel they can't
    /// see is named by its channel alone, not what it said (WYV-50).
    pub(crate) fn report_body(&self, report: &Report, viewer: &str) -> anyhow::Result<Value> {
        let mut body = json!({ "id": report.id, "reported_at": report.reported_at, "by": self.who(&report.by)?, "reason": report.reason });
        if let Some(note) = &report.note {
            body["note"] = json!(note);
        }
        let channel_of = |post: &Value| post["channel"].as_str().map(str::to_string);
        if let Some(post) = report.post.as_ref().filter(|post| channel_of(post).is_some_and(|channel| !self.sees_id(viewer, &channel).unwrap_or(false))) {
            body["post"] = json!({ "channel": post["channel"], "hidden": true });
        } else if let Some(post) = &report.post {
            let mut post = post.clone();
            let removed = match &report.post_id {
                Some(id) => self.store.post_by_id(id)?.is_none_or(|now| now.deleted),
                None => false,
            };
            post["removed"] = json!(removed);
            body["post"] = post;
        }
        if let Some(member) = &report.member {
            body["member"] = self.who(member)?;
        }
        if let Some(at) = report.resolved_at {
            body["resolved_at"] = json!(at);
        }
        if let Some(by) = &report.resolved_by {
            body["resolved_by"] = json!(by);
        }
        Ok(body)
    }

    pub(crate) fn reports_body(&self, resolved_too: bool, viewer: &str) -> anyhow::Result<Value> {
        let reports = self.store.reports(resolved_too)?.iter().map(|report| self.report_body(report, viewer)).collect::<anyhow::Result<Vec<_>>>()?;
        Ok(json!({ "reports": reports }))
    }

    fn message_body(post: &Post) -> Value {
        let mut author = json!({ "did": post.author_did });
        if let Some(name) = &post.author_name {
            author["name"] = json!(name);
        }
        let mut body = json!({
            "channel": post.channel,
            "seq": post.seq,
            "id": post.id,
            "content": post.content,
            "posted_at": post.posted_at,
            "author": author,
        });
        if let Some(question) = &post.question {
            let mut question = question.clone();
            if let Some(by) = &post.answered_by {
                question["answered_by"] = json!(by);
            }
            body["question"] = question;
        }
        if let Some(answer) = &post.answer {
            body["answer"] = answer.clone();
        }
        // What has been said of it since: each only where there is something to say.
        if post.rev > 0 {
            body["rev"] = json!(post.rev);
        }
        // A message reference, as chat-message/1.0 has it.
        if let Some(reply_to) = &post.reply_to {
            let mut reference = json!({ "id": reply_to });
            if let Some(author) = &post.reply_author {
                reference["author"] = json!(author);
            }
            if let Some(snippet) = &post.reply_snippet {
                reference["snippet"] = json!(snippet);
            }
            body["reply_to"] = reference;
        }
        if let Some(thread) = &post.thread {
            body["thread"] = json!(thread);
        }
        crate::posting::extra_body(post, &mut body, crate::gates::now());
        if !post.mentions.is_empty() {
            body["mentions"] = json!(post.mentions);
        }
        if let Some(edited_at) = post.edited_at {
            body["edited_at"] = json!(edited_at);
        }
        if post.deleted {
            body["deleted"] = json!(true);
        }
        if let Some(at) = post.published_at {
            body["published_at"] = json!(at);
        }
        if let Some(removed_by) = &post.removed_by {
            body["removed_by"] = json!(removed_by);
        }
        // What AutoMod censored of it is gone for everyone; whoever manages posts
        // in the channel may ask what it said (`original-request`).
        if let Some(rules) = post.censored.as_ref().filter(|_| !post.deleted) {
            body["censored"] = json!({ "by": rules });
        }
        if let Some(voice) = &post.voice_invite {
            body["voice_invite"] = json!({ "channel": voice });
        }
        if post.pinned {
            body["pinned"] = json!(true);
        }
        if !post.reactions.is_empty() {
            body["reactions"] = post.reactions.iter().map(|(emoji, by)| json!({ "emoji": emoji, "by": by })).collect();
        }
        if let Some(thread) = &post.grew {
            let mut grew = json!({ "created_by": thread.created_by, "created_at": thread.created_at, "count": thread.count });
            // A private thread: that there is one, and who started it; what it is
            // called and who wrote in it last are for those who read it
            // (`thread-members` and its history say them).
            if thread.private {
                grew["private"] = json!(true);
                grew.as_object_mut().map(|g| g.remove("count"));
            } else {
                if let Some(last_at) = thread.last_at {
                    grew["last_at"] = json!(last_at);
                }
                if let Some(name) = &thread.name {
                    grew["name"] = json!(name);
                }
                if let Some(last_by) = &thread.last_by {
                    grew["last_by"] = json!(last_by);
                }
            }
            if thread.sticky {
                grew["sticky"] = json!(true);
            }
            if thread.locked {
                grew["locked"] = json!(true);
            }
            if let Some(section) = &thread.section {
                grew["section"] = json!(section);
            }
            body["grew"] = grew;
        }
        body
    }

    /// A post as it now stands, sent to everyone who has joined when it changes.
    pub fn changed_notice(&self, post: &Post) -> Value {
        json!({ "type": CHANGED, "id": uuid::Uuid::new_v4().to_string(), "body": Self::message_body(post) })
    }

    /// Who a post may call on, from what it asks to: each `{ who, name }`, `who` a
    /// member's DID, `everyone`, or `role:<id>` (`everyone`, and a role that isn't
    /// mentionable, for those holding `mention_everyone` in the channel), and `name`
    /// what the post writes them as. Anyone else named is left out, not refused: a
    /// member taken out since the post was written is nobody's mistake.
    fn mentions_of(&self, body: &Value, member: &Member, channel: &Channel) -> anyhow::Result<Vec<Mention>> {
        let mut mentions: Vec<Mention> = Vec::new();
        let mut everyone_allowed = None;
        for named in body["mentions"].as_array().into_iter().flatten().take(MAX_MENTIONS * 2) {
            let Some(who) = named["who"].as_str() else { continue };
            let mut may_everyone = || -> anyhow::Result<bool> {
                if everyone_allowed.is_none() {
                    everyone_allowed = Some(self.may_in(&member.did, channel, "mention_everyone")?);
                }
                Ok(everyone_allowed == Some(true))
            };
            let allowed = if who == EVERYONE {
                may_everyone()?
            } else if let Some(role) = who.strip_prefix("role:") {
                match self.store.role(role)?.filter(|role| role.id != crate::roles::EVERYONE_ROLE) {
                    Some(role) => role.mentionable || may_everyone()?,
                    None => false,
                }
            } else {
                self.store.member(who)?.is_some()
            };
            if allowed && !mentions.iter().any(|known| known.who == who) && mentions.len() < MAX_MENTIONS {
                let name = line(&named["name"], MAX_MENTION_NAME).unwrap_or_else(|| if who == EVERYONE { EVERYONE.to_string() } else { String::new() });
                mentions.push(Mention { who: who.to_string(), name });
            }
        }
        Ok(mentions)
    }

    /// The post a message reference names (`{ id, author }`): the one with that id,
    /// if whoever is said to have written it did. A reference that names another
    /// author is to no post here.
    pub(crate) fn referred(&self, reference: &Value) -> anyhow::Result<Option<Post>> {
        let id = reference["id"].as_str().or(reference.as_str()).filter(|id| !id.is_empty());
        let Some(post) = id.map(|id| self.store.post_by_id(id)).transpose()?.flatten() else { return Ok(None) };
        Ok(match reference["author"].as_str() {
            Some(author) if author != post.author_did => None,
            _ => Some(post),
        })
    }

    /// The post a request names (`to`), if `viewer` may see its channel: one they
    /// may not is no post, as unknown as one that isn't.
    fn named_post(&self, body: &Value, viewer: &str) -> anyhow::Result<Option<Post>> {
        Ok(match self.referred(&body["to"])? {
            Some(post) if self.reads_post_id(viewer, &post)? => Some(post),
            _ => None,
        })
    }

    /// A post, as it is sent to everyone who has joined.
    pub fn message(&self, post: &Post) -> Value {
        json!({ "type": MESSAGE, "id": uuid::Uuid::new_v4().to_string(), "created_time": post.posted_at, "body": Self::message_body(post) })
    }

    /// What every joined member is told when the channels change.
    pub fn community_notices(&self) -> anyhow::Result<Vec<(String, Value)>> {
        let mut notices = Vec::new();
        for member in self.store.joined()? {
            notices.push((member.did.clone(), json!({ "type": COMMUNITY, "body": self.community(&member)? })));
        }
        Ok(notices)
    }

    /// Who a `join` lets in: as an admin last set it, or as configured.
    /// Whether someone is let in only with a verified public address (WYV-62):
    /// `set-admission`'s `requires_address`.
    pub fn requires_address(&self) -> anyhow::Result<bool> {
        Ok(self.store.setting("admission_requires_address")?.as_deref() == Some("1"))
    }

    pub fn admission(&self) -> anyhow::Result<Admission> {
        let set = self.store.setting("admission")?;
        Ok(set.as_deref().or(Some(self.config.admission.as_str())).and_then(Admission::parse).unwrap_or(Admission::Admin))
    }

    pub fn members_may_invite(&self) -> anyhow::Result<bool> {
        self.everyone_has("create_invites")
    }

    fn invite_body(&self, invite: &Invite) -> anyhow::Result<Value> {
        let mut body = json!({
            "code": invite.code,
            "uses_left": invite.uses_left,
            "expires_at": invite.expires_at,
            "created_at": invite.created_at,
            "created_by": self.who(&invite.created_by)?,
        });
        if let Some(note) = &invite.note {
            body["note"] = json!(note);
        }
        Ok(body)
    }

    /// `{ did, name? }` for a member, as the server knows them.
    pub(crate) fn who(&self, did: &str) -> anyhow::Result<Value> {
        let mut who = json!({ "did": did });
        if let Some(name) = self.store.member(did)?.and_then(|member| member.name) {
            who["name"] = json!(name);
        }
        Ok(who)
    }

    /// The invites `member` may revoke and is told of: all of them for an admin.
    fn invites_body(&self, member: &Member, now: i64) -> anyhow::Result<Value> {
        let by = (!self.powers(&member.did)?.has("manage_community")).then_some(member.did.as_str());
        let invites = self.store.invites(by, now)?.iter().map(|invite| self.invite_body(invite)).collect::<anyhow::Result<Vec<_>>>()?;
        Ok(json!({ "invites": invites }))
    }

    /// A `join` from someone who isn't a member: let in, by `admission` and the
    /// invite they bring, or put on the waiting list.
    fn join_from_outside(&self, sender: &str, message: &Value, now: i64) -> anyhow::Result<Outcome> {
        // Someone banned is answered as anyone is who isn't let in.
        if self.store.banned(sender, now)?.is_some() {
            return refuse(message, "e.p.trust.not-authorized", "This community is by invitation: you are not on its list.");
        }
        let code = message["body"]["invite"].as_str().filter(|code| !code.is_empty() && code.len() <= 255);
        let invite = match code {
            Some(code) => self.store.invite(code)?.filter(|invite| invite.valid(now)),
            None => None,
        };
        // Full, nobody more is let in; waiting still goes on.
        let lets_in = match self.admission()? {
            Admission::Open => true,
            Admission::Invite => invite.is_some(),
            Admission::Admin => false,
        };
        if lets_in && self.full()? {
            return refuse(message, "e.p.req.community-full", "This community has as many members as it can.");
        }
        let let_in = match self.admission()? {
            Admission::Open => {
                // Used up if it was brought, though nothing needed it.
                if let Some(invite) = &invite {
                    self.store.use_invite(&invite.code, now)?;
                }
                true
            }
            Admission::Invite => match &invite {
                Some(invite) => self.store.use_invite(&invite.code, now)?,
                None => false,
            },
            Admission::Admin => false,
        };
        let was_waiting = self.store.waiting_one(sender)?;
        if let_in {
            let named = was_waiting.as_ref().and_then(|waiting| waiting.name.clone());
            self.store.remove_waiting(sender)?;
            self.store.set_member(sender, Role::Member, now)?;
            if named.is_some() {
                self.store.set_name(sender, named.as_deref())?;
            }
            self.store.set_joined(sender, true)?;
            let member = self.store.member(sender)?.expect("just let in");
            let mut outcome = Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?));
            // Said in the welcome channel, where there is one.
            outcome.posted = self.welcome(sender, now)?;
            if was_waiting.is_some() {
                outcome.notices = self.admins_told()?;
            }
            return Ok(outcome);
        }
        if was_waiting.is_none() && self.store.waiting_count()? >= self.config.max_waiting as i64 {
            return refuse(message, "e.p.req.waiting-full", "As many are waiting to be let in as can.");
        }
        self.store.add_waiting(sender, invite.as_ref().map(|invite| invite.code.as_str()), now)?;
        let mut outcome = Outcome::reply(reply_to(message, WAITING, json!({})));
        // Someone new asks: every admin hears of it, and how many wait now.
        if was_waiting.is_none() {
            let asked = self.store.waiting_one(sender)?.expect("just added");
            let entry = self.waiting_entry(&asked)?;
            for (admin, community) in self.admins_told()? {
                outcome.notices.push((admin.clone(), json!({ "type": ASKED_TO_JOIN, "body": entry })));
                outcome.notices.push((admin, community));
            }
        }
        Ok(outcome)
    }

    /// Who is waiting, as an admin is told: `waiting` in `members`.
    fn waiting_body(&self) -> anyhow::Result<Vec<Value>> {
        self.store.waiting()?.iter().map(|waiting| self.waiting_entry(waiting)).collect()
    }

    /// One who is waiting, as `waiting` in `members` and `asked-to-join` say them.
    fn waiting_entry(&self, waiting: &Waiting) -> anyhow::Result<Value> {
        let mut listed = json!({ "did": waiting.did, "asked_at": waiting.asked_at });
        if let Some(answers) = self.store.join_answers(&waiting.did)?.filter(|answers| answers.as_array().is_some_and(|a| !a.is_empty())) {
            listed["answers"] = answers;
        }
        if let Some(name) = &waiting.name {
            listed["name"] = json!(name);
        }
        if let Some(code) = &waiting.invite {
            let created_by = match self.store.invite(code)? {
                Some(invite) => self.who(&invite.created_by)?,
                None => Value::Null,
            };
            listed["invite"] = json!({ "code": code, "created_by": created_by });
        }
        Ok(listed)
    }

    /// How many posts were imported under the Discord account `discord_id` and are
    /// still its.
    pub(crate) fn imported_count(&self, discord_id: &str) -> anyhow::Result<i64> {
        Ok(self.store.db.lock().query_value("SELECT COUNT(*) FROM posts WHERE author_did = ?1", wyvrn_service::params![format!("{}{discord_id}", crate::discord_history::AUTHOR_PREFIX)])?)
    }

    /// `community` for every joined member holding `admit_members`, the waiting
    /// list having changed: its `waiting` says how many wait now.
    pub(crate) fn admins_told(&self) -> anyhow::Result<Vec<(String, Value)>> {
        self.joined_holding("admit_members")?
            .iter()
            .map(|admin| Ok((admin.did.clone(), json!({ "type": COMMUNITY, "body": self.community(admin)? }))))
            .collect()
    }

    /// `members`, as `asker` is told: with who is waiting for an admin.
    pub(crate) fn members_for(&self, asker: &Member) -> anyhow::Result<Value> {
        let mut body = self.members_body()?;
        if self.powers(&asker.did)?.has("timeout_members") {
            let now = crate::gates::now();
            for listed in body["members"].as_array_mut().into_iter().flatten() {
                if let Some(until) = self.store.timed_out_until(listed["did"].as_str().unwrap_or_default(), now)? {
                    listed["timed_out_until"] = json!(until);
                }
            }
        }
        if self.powers(&asker.did)?.has("admit_members") {
            body["waiting"] = json!(self.waiting_body()?);
        }
        Ok(body)
    }

    fn members_body(&self) -> anyhow::Result<Value> {
        let online = self.seen_online()?;
        // Not the community itself: it is kept as a member so that what it posts
        // (a welcome, AutoMod's alerts, a webhook's or the assistant's posts) has an
        // author, but it is nobody in the list (found 2026-11-20: it was listed by
        // its DID).
        let own = self.own_dids();
        let mut members = Vec::new();
        for member in self.store.members()?.into_iter().filter(|member| !own.contains(&member.did)) {
            let powers = self.powers(&member.did)?;
            members.push((member, powers));
        }
        let members = members.iter().map(|(member, powers)| self.member_entry(member, powers, &online)).collect::<anyhow::Result<Vec<Value>>>()?;
        Ok(json!({ "members": members }))
    }

    /// One member as `members` lists them: by DID here, which member_ids.rs turns
    /// into their member id for whoever is sent it; with what their profile says.
    pub(crate) fn member_entry(&self, member: &Member, powers: &crate::roles::Powers, online: &std::collections::HashMap<String, String>) -> anyhow::Result<Value> {
        let mut listed = json!({ "did": member.did, "role": powers.older_role(), "roles": powers.roles, "joined": member.joined, "since": member.since });
        if let Some(status) = online.get(&member.did) {
            listed["presence"] = json!(status);
        }
        let (name, profile, here) = self.profile_entry(&member.did, member.name.as_deref())?;
        if let Some(name) = name {
            listed["name"] = json!(name);
        }
        if let Some(profile) = profile {
            listed["profile"] = profile;
        }
        if let Some(here) = here {
            listed["here"] = here;
        }
        if let Some(left_at) = member.left_at.filter(|_| !member.joined) {
            listed["left_at"] = json!(left_at);
        }
        if let Some(signature) = self.store.signature(&member.did)? {
            listed["signature"] = json!(signature);
        }
        Ok(listed)
    }

    /// The answer to a message that changed the post `id`: the post as it now
    /// stands, which everyone else who has joined is sent too.
    pub(crate) fn changed_answer(&self, message: &Value, id: &str) -> anyhow::Result<Outcome> {
        let Some(post) = self.store.post_by_id(id)? else {
            return refuse(message, "e.p.msg.unknown-post", "There is no such post.");
        };
        Ok(Outcome { reply: Some(reply_to(message, CHANGED, Self::message_body(&post))), changed: vec![post.id], ..Outcome::default() })
    }

    /// Whether the kind of channel `kind` is turned off here by its host.
    pub fn kind_off(&self, kind: &str) -> bool {
        kind != "text" && self.config.disabled_channel_kinds.iter().any(|off| off == kind)
    }

    /// Whether `channel` is of a kind its host turned off: read-only, then.
    pub fn channel_off(&self, channel: &crate::store::Channel) -> bool {
        self.kind_off(channel_kind(channel))
    }

    /// The kinds of channel that may be opened here, a text channel first.
    pub fn channel_kinds(&self) -> Vec<&'static str> {
        std::iter::once("text").chain(CHANNEL_KINDS.iter().copied().filter(|kind| !self.kind_off(kind))).collect()
    }

    /// The refusal of a kind of channel its host turned off.
    fn kind_off_refusal(message: &Value, kind: &str) -> anyhow::Result<Outcome> {
        refuse(message, "e.p.msg.kind-off", &format!("Whoever hosts this community has turned off {kind} channels."))
    }

    fn may_open_channels(&self, member: &Member) -> anyhow::Result<bool> {
        let powers = self.powers(&member.did)?;
        Ok(powers.has("create_channels") || powers.has("manage_channels"))
    }

    /// Lets in everyone the configuration names, and makes its admins admins. It
    /// never takes anyone out, or lets in a DID a member has since left.
    ///
    /// Someone named by a public address is not let in here: nobody writes from
    /// one, so that would be a second member for whoever it names, who is let in
    /// under the DID they do write from (`wyvrn_service::named`).
    pub fn admit_configured(&self, now: i64) -> anyhow::Result<()> {
        let written_from = |did: &&String| !wyvrn_service::named::is_hosted(did);
        for did in self.config.admins.iter().chain(&self.config.members).filter(|did| !written_from(did)) {
            // (One let in here before this was so, whose app never asked for anything.)
            self.store.forget_never_joined(did)?;
        }
        for did in self.config.admins.iter().filter(written_from) {
            if !self.store.is_superseded(did)? {
                self.store.set_member(did, Role::Admin, now)?;
            }
        }
        for did in self.config.members.iter().filter(written_from) {
            // A ban stands over being named here (WYV-50).
            if !self.store.is_superseded(did)? && self.store.banned(did, now)?.is_none() {
                self.store.admit(did, Role::Member, now)?;
            }
        }
        Ok(())
    }

    /// A `post` from `member`: checked, placed and answered. `checked` is false
    /// only for a post AutoMod held that a moderator let through, which isn't
    /// held to slow mode, timeouts or AutoMod again.
    pub(crate) fn post(&self, member: &Member, message: &Value, now: i64, checked: bool) -> anyhow::Result<Outcome> {
        let sender = member.did.as_str();
        let body = &message["body"];
        let Some(channel) = self.channel_for(sender, body["channel"].as_str().unwrap_or_default())? else {
            return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.");
        };
        if channel.closed {
            return refuse(message, "e.p.msg.closed", "That channel is closed.");
        }
        // A voice channel (and a stage) has a text chat of its own, as Discord's do,
        // where the community turned it on: posted in as any channel, by the same
        // permissions.
        if channel.voice.is_some() && !self.voice_text_chat()? {
            return refuse(message, "e.p.msg.voice-channel", "This community's voice channels have no text chat.");
        }
        if channel.calendar {
            return refuse(message, "e.p.msg.calendar-channel", "A calendar channel takes no posts: it shows the community's events.");
        }
        if channel.form.is_some() {
            return refuse(message, "e.p.msg.form-channel", "A form channel takes no posts: it is filled in.");
        }
        if channel.wiki {
            return refuse(message, "e.p.msg.wiki-channel", "A wiki takes no posts: its pages are edited.");
        }
        // Into a thread takes `send_in_threads`, said once the thread is known;
        // anything else `post`.
        let towards_thread = body["thread"].as_str().is_some_and(|t| !t.is_empty()) || body.get("reply_to").is_some_and(|r| !r.is_null());
        if !towards_thread && !self.may_in(sender, &channel, "post")? {
            return refuse(message, "e.p.trust.not-authorized", "You can't post in this channel.");
        }
        if body.get("poll").is_some_and(|p| !p.is_null()) && !self.may_in(sender, &channel, "create_polls")? {
            return refuse(message, "e.p.trust.not-authorized", "You can't make polls in this channel.");
        }
        // A post sent again after a lost answer isn't held to slow mode.
        let again = body["id"].as_str().map(|id| self.store.post_by_id(id)).transpose()?.flatten().is_some_and(|post| post.author_did == sender);
        if !again && checked {
            if let Some(refused) = self.posting_refused(member, &channel, now)? {
                return refused.answer(message);
            }
            if let Some(refused) = self.protection_refused(member, body, now)? {
                return refused.answer(message);
            }
        }
        let content = body["content"].as_str().unwrap_or_default().trim();
        let question = match body.get("question").filter(|question| !question.is_null()) {
            Some(question) => match clean_question(question) {
                Ok(question) => Some(question),
                Err(why) => return refuse(message, "e.p.msg.invalid-question", why),
            },
            None => None,
        };
        // (Something to show besides text will do: media, or a poll.)
        let carries = body.get("media").is_some_and(|m| !m.is_null()) || body.get("poll").is_some_and(|p| !p.is_null());
        if content.is_empty() && !carries {
            return refuse(message, "e.p.msg.empty", "A post needs something in it.");
        }
        if content.chars().count() > self.config.max_post_length {
            return refuse(message, "e.p.msg.too-long", "That is longer than a post can be here.");
        }
        // Sent twice (a retry), it is the same post: answered as it was.
        let id = body["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 64).map(str::to_string).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if let Some(existing) = self.store.post_by_id(&id)? {
            if existing.author_did != sender {
                return refuse(message, "e.p.msg.id-taken", "That id has been used.");
            }
            return Ok(Outcome::reply(reply_to(message, POSTED, json!({ "channel": existing.channel, "seq": existing.seq, "id": existing.id }))));
        }
        // What it answers, and the thread it is in: both posts of this channel.
        let answered = match body.get("reply_to").filter(|reference| !reference.is_null()) {
            Some(reference) => match self.referred(reference)?.filter(|post| post.channel == channel.id) {
                Some(post) => Some(post),
                None => return refuse(message, "e.p.msg.unknown-post", "The post that answers isn't in this channel."),
            },
            None => None,
        };
        // The quotation is the answerer's: what the post said as they answered it.
        let snippet = line(&body["reply_to"]["snippet"], MAX_SNIPPET);
        let thread = match body["thread"].as_str().filter(|id| !id.is_empty()) {
            Some(thread) => match self.store.thread(thread)?.filter(|thread| thread.channel == channel.id) {
                Some(thread) => Some(thread.id),
                None => return refuse(message, "e.p.msg.unknown-thread", "There is no such thread in this channel."),
            },
            // An answer to a post in a thread is in that thread; in a forum, an
            // answer to a thread's first post is in its thread too.
            None => answered.as_ref().and_then(|post| post.thread.clone().or_else(|| (channel.forum.is_some() && post.grew.is_some()).then(|| post.id.clone()))),
        };
        match thread.as_deref() {
            Some(id) => {
                let powers = self.powers(sender)?;
                // A private thread they aren't in is as unknown as one that isn't there.
                if !self.reads_thread(sender, &powers, &channel, id)? {
                    return refuse(message, "e.p.msg.unknown-thread", "There is no such thread in this channel.");
                }
                if !self.may_in(sender, &channel, "send_in_threads")? {
                    return refuse(message, "e.p.trust.not-authorized", "You can't post in threads in this channel.");
                }
            }
            None if !self.may_in(sender, &channel, "post")? => return refuse(message, "e.p.trust.not-authorized", "You can't post in this channel."),
            None => {}
        }
        // A locked thread takes posts only from whoever manages them.
        let locked = thread.as_deref().map(|id| self.store.thread(id)).transpose()?.flatten().is_some_and(|t| t.locked);
        if locked && !(self.may_in(sender, &channel, "manage_messages")? || self.may_in(sender, &channel, "manage_threads")?) {
            return refuse(message, "e.p.msg.locked", "This thread is locked.");
        }
        // A forum thread begun in one of its sections.
        let section = match body["section"].as_str() {
            Some(id) if thread.is_none() && channel.forum.is_some() => match channel.sections.as_ref().and_then(|s| s.as_array()).is_some_and(|s| s.iter().any(|x| x["id"] == id)) {
                true => Some(id.to_string()),
                false => return refuse(message, "e.p.msg.unknown-section", "There is no such section in this forum."),
            },
            _ => None,
        };
        let mut extra = match crate::posting::read_extra(body, &channel, thread.is_some(), now) {
            Ok(extra) => extra,
            Err((code, why)) => return refuse(message, code, &why),
        };
        // A bot's post may carry buttons (bot-commands/1.0); anyone else's doesn't.
        if let Some(buttons) = crate::bots::buttons_from(&body["buttons"]).filter(|_| matches!(self.store.bot(sender), Ok(Some(_)))) {
            extra.get_or_insert_with(|| json!({}))["buttons"] = buttons;
        }
        if extra.as_ref().is_some_and(|extra| extra.get("media").is_some()) && !self.may_in(sender, &channel, "attach_files")? {
            return refuse(message, "e.p.trust.not-authorized", "You can't post files in this channel.");
        }
        let mentions = self.mentions_of(body, member, &channel)?;
        let (noted, censored) = if checked {
            match self.automod(member, &channel, message, content, mentions.len(), now, false)? {
                crate::automod::Verdict::Placed { noted, censored } => (noted, censored),
                crate::automod::Verdict::Stopped(stopped) => return Ok(stopped),
            }
        } else {
            (None, None)
        };
        let written = content;
        let content = censored.as_ref().map_or(content, |censored| censored.content.as_str());
        let post = self.store.add_post(&channel.id, &id, content, member, question.as_ref(), None, now)?;
        // The sender's own time kept beside the server's (posted_at), which alone is
        // sent: a device's clock can be anything.
        if let Some(sent_at) = message["created_time"].as_i64() {
            self.store.set_post_sent(&post.id, sent_at)?;
        }
        if let Some(censored) = &censored {
            self.store.set_censored(&post.id, Some((written, &censored.rules)))?;
        }
        let reply = answered.as_ref().map(|post| (post.id.as_str(), post.author_did.as_str(), snippet.as_deref()));
        self.store.set_written_with(&post.id, reply, thread.as_deref(), &mentions)?;
        if let Some(extra) = &extra {
            self.store.set_post_extra(&post.id, extra)?;
        }
        // A post in a forum that isn't in a thread begins one, named its title.
        let begins_thread = channel.forum.is_some() && thread.is_none();
        if begins_thread {
            let title = extra.as_ref().and_then(|extra| extra["title"].as_str()).map(str::to_string);
            self.store.add_thread(&post.id, &channel.id, title.as_deref(), sender, now)?;
            if section.is_some() {
                self.store.set_thread_flags(&post.id, false, false, section.as_deref())?;
            }
        }
        // Whoever posts in a thread, or begins one, follows it.
        if let Some(followed) = thread.as_deref().or(begins_thread.then_some(post.id.as_str())) {
            self.store.follow_thread(sender, followed, true)?;
        }
        let mut outcome = Outcome {
            reply: Some(reply_to(message, POSTED, {
                let mut placed = json!({ "channel": post.channel, "seq": post.seq, "id": post.id });
                // Censored, its author's app shows it as everyone sees it.
                if let Some(censored) = &censored {
                    placed["content"] = json!(censored.content);
                    placed["censored"] = json!({ "by": censored.rules });
                }
                placed
            })),
            posted: true,
            posted_id: Some(post.id.clone()),
            ..Outcome::default()
        };
        // The post a thread grew from says how many are in it now.
        if let Some(thread) = thread {
            self.store.touch_thread(&thread)?;
            outcome.changed.push(thread);
        }
        if begins_thread {
            outcome.changed.push(post.id.clone());
        }
        // What AutoMod noted as it let the post through: an alert posted, say.
        if let Some(noted) = noted {
            outcome.posted |= noted.posted;
            outcome.notices.extend(noted.notices);
        }
        Ok(outcome)
    }

    /// A verified `from_prior`: whoever was `prior` is `new` now. Returns what to
    /// tell them, if they were a member -- which also shows their app the news
    /// arrived, so it can stop repeating it.
    pub fn rotated(&self, prior: &str, new: &str, now: i64) -> anyhow::Result<Option<Value>> {
        if !self.store.rotate(prior, new, now)? {
            return Ok(None);
        }
        match self.store.member(new)? {
            Some(member) => Ok(Some(json!({ "type": COMMUNITY, "body": self.community(&member)? }))),
            None => Ok(None),
        }
    }

    /// Posts `answers` to the question `post` asks, as `member`: the first to
    /// answer it. `None` if someone already had.
    pub fn answer_question(&self, member: &Member, post: &Post, answers: &[Value], now: i64) -> anyhow::Result<Option<Post>> {
        let Some(question) = &post.question else {
            return Ok(None);
        };
        if !self.store.set_answered(&post.id, &member.did)? {
            return Ok(None);
        }
        let answer = json!({ "to": post.id, "answers": answers });
        let id = uuid::Uuid::new_v4().to_string();
        Ok(Some(self.store.add_post(&post.channel, &id, &answer_text(question, answers), member, None, Some(&answer), now)?))
    }

    /// Acts on one message from `sender`. Someone who isn't let in is told so, and
    /// nothing else.
    pub fn handle(&self, sender: &str, message: &Value, now: i64) -> anyhow::Result<Outcome> {
        let message_type = message["type"].as_str().unwrap_or_default();
        let body = &message["body"];
        if self.store.is_superseded(sender)? {
            return refuse(message, "e.p.trust.superseded", "This DID has been replaced by another.");
        }
        // Another community's server: following, and what announcement channels send.
        if crate::announcements::between_servers(message_type) || message_type == PROBLEM_REPORT {
            if let Some(outcome) = self.from_server(sender, message, now)? {
                return Ok(outcome);
            }
        }
        // Moving: read, and leave, and nothing else.
        if !reads_only(message_type) && self.moving()? {
            if message_type == PROFILE {
                return Ok(Outcome::default());
            }
            return refuse(message, "e.p.req.moving", "This community is moving to another server. It can be read, and nothing changed, until it has.");
        }
        let Some(member) = self.store.member(sender)? else {
            // Someone who isn't let in may ask to be, take back having asked, and
            // say what they are called for the admins who decide.
            return match message_type {
                JOIN => match self.rules_before_join(sender, message)? {
                    Some(rules) => Ok(rules),
                    // Raid mode: nobody new for now.
                    None if self.raid(now)?.is_some() && self.store.waiting_one(sender)?.is_none() => {
                        refuse(message, "e.p.req.raid", "This community isn't taking new members for a while. Try again later.")
                    }
                    None => match self.store.banned(sender, now)?.is_some() {
                        // (Not asked anything: refused as anyone is who isn't let in.)
                        true => self.join_from_outside(sender, message, now),
                        false => match self.questions_before_join(sender, message, now)? {
                            Some(asked) => Ok(asked),
                            None => self.join_from_outside(sender, message, now),
                        },
                    },
                },
                // Someone banned may ask how it stands, and appeal (appeals.rs).
                crate::appeals::BAN_STATUS_REQUEST | crate::appeals::APPEAL => Ok(self.handle_banned(sender, message, now)?.unwrap_or_default()),
                LEAVE if self.store.remove_waiting(sender)? => Ok(Outcome { notices: self.admins_told()?, ..Outcome::default() }),
                PROFILE if self.store.waiting_one(sender)?.is_some() => {
                    let profile = &body["profile"];
                    if profile.get("displayName").is_some() {
                        self.store.set_waiting_name(sender, line(&profile["displayName"], MAX_NAME_LENGTH).as_deref())?;
                    }
                    Ok(Outcome::default())
                }
                _ => refuse(message, "e.p.trust.not-authorized", "This community is by invitation: you are not on its list."),
            };
        };

        if let Some(outcome) = self.handle_banned(sender, message, now)? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.handle_appeals(sender, message_type, message, now)? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.handle_moderation(&member, message_type, message, now)? {
            return Ok(outcome);
        }
        if message_type == crate::automod::ORIGINAL_REQUEST {
            return self.handle_original(&member, message);
        }
        if let Some(outcome) = self.handle_held(&member, message_type, message, now)? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.handle_structure(&member, message_type, message, now)? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.handle_posting(&member, message_type, message, now)? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.handle_event_message(&member, message, now)? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.handle_form_message(&member, message, now)? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.handle_wiki_message(&member, message, now)? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.handle_bot_message(&member, message, now)? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.handle_board_message(&member, message, now)? {
            return Ok(outcome);
        }

        match message_type {
            JOIN | LEAVE => {
                if message_type == JOIN {
                    if let Some(rules) = self.rules_before_join(sender, message)? {
                        return Ok(rules);
                    }
                }
                // Someone who had joined and leaves stays a member, sent nothing more.
                if message_type == LEAVE && member.joined {
                    self.store.note_left(sender, now)?;
                }
                self.store.set_joined(sender, message_type == JOIN)?;
                if member.joined != (message_type == JOIN) {
                    self.member_event(sender, message_type == JOIN, now);
                }
                let member = Member { joined: message_type == JOIN, ..member };
                Ok(Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?)))
            }

            CREATE_CHANNEL => {
                if !self.may_open_channels(&member)? {
                    return refuse(message, "e.p.trust.not-authorized", "You can't open a channel here.");
                }
                let voice = match body["kind"].as_str() {
                    None | Some("text") => None,
                    Some("voice") => {
                        if self.voice.is_none() {
                            return refuse(message, "e.p.msg.unsupported", "This server has no voice channels.");
                        }
                        if !self.powers(sender)?.has("manage_channels") {
                            return refuse(message, "e.p.trust.not-authorized", "Opening a voice channel takes Manage channels.");
                        }
                        match clean_voice(&body["voice"], None) {
                            Ok(voice) => Some(voice),
                            Err(why) => return refuse(message, "e.p.msg.invalid-voice", why),
                        }
                    }
                    Some("forum") | Some("gallery") | Some("calendar") | Some("form") | Some("wiki") => {
                        if !self.powers(sender)?.has("manage_channels") {
                            return refuse(message, "e.p.trust.not-authorized", "Opening a forum, a gallery or a calendar channel takes Manage channels.");
                        }
                        None
                    }
                    Some(_) => return refuse(message, "e.p.msg.invalid-kind", "A channel is a text channel, a forum, a gallery, a calendar or a voice channel."),
                };
                let gallery = body["kind"].as_str() == Some("gallery");
                let forum = gallery || body["kind"].as_str() == Some("forum");
                // An announcement channel is a text channel others follow.
                let announcement = body["announcement"] == true;
                // What kind it would be, as its host may have turned off.
                let asked_kind = match body["kind"].as_str().unwrap_or("text") {
                    "voice" if voice.as_ref().is_some_and(|v| v["stage"] == true) => "stage",
                    "text" if announcement => "announcement",
                    kind => kind,
                };
                if self.kind_off(asked_kind) {
                    return Self::kind_off_refusal(message, asked_kind);
                }
                if announcement && (voice.is_some() || forum) {
                    return refuse(message, "e.p.msg.invalid-announcement", "An announcement channel is a text channel.");
                }
                if announcement && !self.powers(sender)?.has("manage_channels") {
                    return refuse(message, "e.p.trust.not-authorized", "Opening an announcement channel takes Manage channels.");
                }
                // A form channel's form, checked before anything is made.
                let form = match body["kind"].as_str() {
                    Some("form") => match self.checked_form(&body["form"], sender) {
                        Ok(form) => Some(form),
                        Err(why) => return refuse(message, "e.p.msg.invalid-form", &why),
                    },
                    _ => None,
                };
                let Some(name) = line(&body["name"], MAX_CHANNEL_NAME) else {
                    return refuse(message, "e.p.msg.empty", "A channel needs a name.");
                };
                if voice.is_none() && !text_channel_name(&name) {
                    return refuse(message, "e.p.msg.invalid-name", INVALID_NAME);
                }
                if self.store.channel_named_among(&name, voice.is_some())?.is_some() {
                    return refuse(message, "e.p.msg.name-taken", "There is a channel called that already.");
                }
                if self.store.channels()?.iter().filter(|channel| !channel.closed).count() >= self.config.max_channels {
                    return refuse(message, "e.p.msg.too-many", "This community has as many channels as it can hold. Close one first.");
                }
                let id = uuid::Uuid::new_v4().to_string();
                self.store.add_channel(&id, &name, &line(&body["topic"], MAX_TOPIC).unwrap_or_default(), sender, now)?;
                self.audit(now, sender, "channel.create", Some(&id), Some(json!({ "name": name, "kind": if voice.is_some() { "voice" } else { "text" } })));
                if let Some(voice) = &voice {
                    self.store.set_channel_voice(&id, voice)?;
                }
                if forum {
                    let tags = match body.get("tags").filter(|t| !t.is_null()) {
                        Some(given) => match crate::posting::read_tags(given, None) {
                            Ok(tags) => tags,
                            Err(why) => return refuse(message, "e.p.msg.invalid-tags", why),
                        },
                        None => json!([]),
                    };
                    self.store.set_forum(&id, &tags)?;
                    if gallery {
                        self.store.set_gallery(&id, true)?;
                    }
                }
                if announcement {
                    self.store.set_announcement(&id, true)?;
                }
                if body["kind"].as_str() == Some("calendar") {
                    self.store.set_calendar_channel(&id)?;
                }
                if let Some(form) = form {
                    self.store.set_form(&id, &form)?;
                }
                if body["kind"].as_str() == Some("wiki") {
                    self.store.set_wiki_channel(&id)?;
                }
                let mut community = self.community(&member)?;
                community["created"] = json!(id);
                Ok(Outcome { reply: Some(reply_to(message, COMMUNITY, community)), channels_changed: true, ..Outcome::default() })
            }

            UPDATE_CHANNEL => {
                let Some(channel) = self.channel_for(sender, body["channel"].as_str().unwrap_or_default())? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.");
                };
                // Whoever opened it may change it, as may whoever manages channels.
                if channel.created_by != sender && !self.powers(sender)?.has("manage_channels") {
                    return refuse(message, "e.p.trust.not-authorized", "Only whoever opened a channel, or someone who manages channels, can change it.");
                }
                let name = match body.get("name") {
                    Some(name) => match line(name, MAX_CHANNEL_NAME) {
                        Some(name) => name,
                        None => return refuse(message, "e.p.msg.empty", "A channel needs a name."),
                    },
                    None => channel.name.clone(),
                };
                // A name from before the rule stands until the channel is renamed.
                if name != channel.name && channel.voice.is_none() && !text_channel_name(&name) {
                    return refuse(message, "e.p.msg.invalid-name", INVALID_NAME);
                }
                let closed = body["closed"].as_bool().unwrap_or(channel.closed);
                // Renamed to, or opened again with, the name of another open channel.
                let named = self.store.channel_named_among(&name, channel.voice.is_some())?.filter(|other| other.id != channel.id);
                if named.is_some() && (!name.eq_ignore_ascii_case(&channel.name) || (channel.closed && !closed)) {
                    return refuse(message, "e.p.msg.name-taken", "There is a channel called that already.");
                }
                let topic = match body.get("topic") {
                    Some(topic) => line(topic, MAX_TOPIC).unwrap_or_default(),
                    None => channel.topic.clone(),
                };
                if let Some(given) = body.get("voice").filter(|v| !v.is_null()) {
                    let Some(held) = &channel.voice else {
                        return refuse(message, "e.p.msg.invalid-voice", "That isn't a voice channel.");
                    };
                    match clean_voice(given, Some(held)) {
                        Ok(voice) => self.store.set_channel_voice(&channel.id, &voice)?,
                        Err(why) => return refuse(message, "e.p.msg.invalid-voice", why),
                    }
                }
                if let Some(given) = body.get("sections").filter(|v| !v.is_null()) {
                    if channel.forum.is_none() {
                        return refuse(message, "e.p.msg.invalid-sections", "Only a forum has sections.");
                    }
                    if !self.powers(sender)?.has("manage_channels") {
                        return refuse(message, "e.p.trust.not-authorized", "A forum's sections take Manage channels.");
                    }
                    match crate::board::read_sections(given) {
                        Ok(sections) => self.store.set_forum_sections(&channel.id, &sections)?,
                        Err(why) => return refuse(message, "e.p.msg.invalid-sections", why),
                    }
                }
                if let Some(given) = body.get("tags").filter(|v| !v.is_null()) {
                    let Some(held) = &channel.forum else {
                        return refuse(message, "e.p.msg.invalid-tags", "Only a forum has tags.");
                    };
                    if !self.powers(sender)?.has("manage_channels") {
                        return refuse(message, "e.p.trust.not-authorized", "A forum's tags take Manage channels.");
                    }
                    match crate::posting::read_tags(given, Some(held)) {
                        Ok(tags) => self.store.set_forum(&channel.id, &tags)?,
                        Err(why) => return refuse(message, "e.p.msg.invalid-tags", why),
                    }
                }
                if let Some(given) = body.get("web").filter(|v| !v.is_null()) {
                    if self.web_url.is_none() {
                        return refuse(message, "e.p.msg.no-web", "This server shows nothing on the web.");
                    }
                    if channel.voice.is_some() {
                        return refuse(message, "e.p.msg.invalid-web", "A voice channel takes no posts to show.");
                    }
                    if !self.powers(sender)?.has("manage_channels") {
                        return refuse(message, "e.p.trust.not-authorized", "Making a channel public takes Manage channels.");
                    }
                    let Some(public) = given["public"].as_bool() else {
                        return refuse(message, "e.p.msg.invalid-web", "web is { public, history? }.");
                    };
                    let since = public.then(|| if given["history"].as_bool() == Some(true) { 0 } else { now });
                    self.store.set_web_channel(&channel.id, since)?;
                    self.audit(now, sender, "channel.web", Some(&channel.id), Some(json!({ "public": public, "history": given["history"].as_bool() == Some(true) })));
                }
                if let Some(given) = body.get("rules").filter(|v| !v.is_null()) {
                    let Some(rules) = given.as_bool() else {
                        return refuse(message, "e.p.msg.invalid-rules-channel", "rules is true or false.");
                    };
                    if channel.voice.is_some() {
                        return refuse(message, "e.p.msg.invalid-rules-channel", "The rules channel is a text channel.");
                    }
                    if !self.powers(sender)?.has("manage_channels") {
                        return refuse(message, "e.p.trust.not-authorized", "Choosing the rules channel takes Manage channels.");
                    }
                    let current = self.store.setting(RULES_CHANNEL)?;
                    if rules {
                        self.store.set_setting(RULES_CHANNEL, &channel.id)?;
                    } else if current.as_deref() == Some(channel.id.as_str()) {
                        self.store.delete_setting(RULES_CHANNEL)?;
                    }
                    self.audit(now, sender, "channel.rules", Some(&channel.id), Some(json!({ "rules": rules })));
                }
                if let Some(given) = body.get("form").filter(|v| !v.is_null()) {
                    if channel.form.is_none() {
                        return refuse(message, "e.p.msg.invalid-form", "Only a form channel has a form.");
                    }
                    if !self.powers(sender)?.has("manage_channels") {
                        return refuse(message, "e.p.trust.not-authorized", "Changing a form takes Manage channels.");
                    }
                    match self.checked_form(given, sender) {
                        Ok(form) => self.store.set_form(&channel.id, &form)?,
                        Err(why) => return refuse(message, "e.p.msg.invalid-form", &why),
                    }
                }
                let mut notices = Vec::new();
                if let Some(given) = body.get("announcement").filter(|v| !v.is_null()) {
                    let Some(announcement) = given.as_bool() else {
                        return refuse(message, "e.p.msg.invalid-announcement", "announcement is true or false.");
                    };
                    if channel.voice.is_some() || channel.forum.is_some() {
                        return refuse(message, "e.p.msg.invalid-announcement", "An announcement channel is a text channel.");
                    }
                    if !self.powers(sender)?.has("manage_channels") {
                        return refuse(message, "e.p.trust.not-authorized", "Making an announcement channel takes Manage channels.");
                    }
                    if announcement && !channel.announcement && self.kind_off("announcement") {
                        return Self::kind_off_refusal(message, "announcement");
                    }
                    if announcement != channel.announcement {
                        if !announcement {
                            notices = self.drop_followers(&channel.id)?;
                            // Read-only for its kind's being turned off by the host: made an
                            // ordinary channel, it stays read-only, now through its permissions
                            // (`everyone` denied `post`), which an admin can lift there.
                            if self.channel_off(&channel) {
                                let said = self.store.overrides(&channel.id)?.into_iter().find(|o| o.role == crate::roles::EVERYONE_ROLE);
                                let (mut allow, mut deny) = said.map(|o| (o.allow, o.deny)).unwrap_or_default();
                                allow.retain(|p| p != "post");
                                if !deny.iter().any(|p| p == "post") {
                                    deny.push("post".into());
                                }
                                self.store.set_override(&channel.id, crate::roles::EVERYONE_ROLE, &allow, &deny)?;
                            }
                        }
                        self.store.set_announcement(&channel.id, announcement)?;
                        self.audit(now, sender, "channel.announcement", Some(&channel.id), Some(json!({ "announcement": announcement })));
                    }
                }
                if let Some(given) = body.get("slow_mode").filter(|v| !v.is_null()) {
                    if channel.voice.is_some() {
                        return refuse(message, "e.p.msg.invalid-slow-mode", "A voice channel takes no posts, nor slow mode.");
                    }
                    if !self.powers(sender)?.has("manage_channels") {
                        return refuse(message, "e.p.trust.not-authorized", "Slow mode takes Manage channels.");
                    }
                    match given.as_i64().filter(|s| (0..=crate::moderation::MAX_SLOW_MODE).contains(s)) {
                        Some(seconds) => self.store.set_slow_mode(&channel.id, seconds)?,
                        None => return refuse(message, "e.p.msg.invalid-slow-mode", "Slow mode is from 0 to 21,600 seconds."),
                    }
                }
                self.store.update_channel(&channel.id, &name, &topic, closed)?;
                let mut details = serde_json::Map::new();
                for field in ["name", "topic", "closed", "voice", "slow_mode"] {
                    if let Some(value) = body.get(field) {
                        details.insert(field.into(), value.clone());
                    }
                }
                self.audit(now, sender, "channel.update", Some(&channel.id), Some(Value::Object(details)));
                Ok(Outcome { reply: Some(reply_to(message, COMMUNITY, self.community(&member)?)), notices, channels_changed: true, ..Outcome::default() })
            }

            POST | CREATE_THREAD if self.rules_owed(sender)?.is_some() => {
                refuse(message, "e.p.msg.rules-not-accepted", "Accept this community's rules before posting.")
            }

            POST => self.post(&member, message, now, true),
            crate::announcements::PUBLISH => self.publish(&member, message, now),
            crate::announcements::FOLLOW_CODE_REQUEST => self.follow_code(&member, message, now),
            crate::announcements::FOLLOW => self.follow(&member, message, now),
            crate::announcements::UNFOLLOW => self.unfollow(&member, message, now),

            EDIT | DELETE => {
                let Some(post) = self.named_post(body, sender)? else {
                    return refuse(message, "e.p.msg.unknown-post", "There is no such post.");
                };
                // A post is its author's to change, and nobody else's. Someone
                // else's is removed by whoever may change the channel.
                let removal = post.author_did != sender;
                if removal && message_type == EDIT {
                    return refuse(message, "e.p.trust.not-authorized", "Only whoever wrote a post changes it.");
                }
                let Some(channel) = self.store.channel(&post.channel)? else {
                    return refuse(message, "e.p.msg.unknown-post", "There is no such post.");
                };
                if removal && !self.may_in(sender, &channel, "manage_messages")? {
                    return refuse(message, "e.p.trust.not-authorized", "Only whoever wrote a post takes it back; someone who manages posts in the channel removes it.");
                }
                if post.deleted {
                    return refuse(message, "e.p.msg.deleted", "That post was taken back.");
                }
                if message_type == DELETE {
                    self.store.delete_post(&post.id, removal.then_some(sender), self.keeps_deleted_text())?;
                    if removal {
                        self.audit(now, sender, "post.remove", Some(&post.id), Some(json!({ "channel": post.channel, "author": post.author_did })));
                    }
                } else {
                    let content = body["content"].as_str().unwrap_or_default().trim();
                    if content.is_empty() {
                        return refuse(message, "e.p.msg.empty", "A post needs something in it.");
                    }
                    if content.chars().count() > self.config.max_post_length {
                        return refuse(message, "e.p.msg.too-long", "That is longer than a post can be here.");
                    }
                    // Who it calls on is said again with it; left out, it is as it was.
                    let mentions = if body.get("mentions").is_some() { self.mentions_of(body, &member, &channel)? } else { post.mentions.clone() };
                    if content == post.content {
                        // (What AutoMod censored stays as it was, the original with it.)
                        if mentions != post.mentions {
                            self.store.edit_post(&post.id, content, &mentions, now)?;
                        }
                    } else {
                        // An edit is checked as a post is, so a censored post isn't
                        // written back as it was.
                        let (noted, censored) = match self.automod(&member, &channel, message, content, mentions.len(), now, true)? {
                            crate::automod::Verdict::Placed { noted, censored } => (noted, censored),
                            crate::automod::Verdict::Stopped(stopped) => return Ok(stopped),
                        };
                        let placed = censored.as_ref().map_or(content, |censored| censored.content.as_str());
                        self.store.edit_post(&post.id, placed, &mentions, now)?;
                        self.store.set_censored(&post.id, censored.as_ref().map(|censored| (content, censored.rules.as_slice())))?;
                        let mut outcome = self.changed_answer(message, &post.id)?;
                        if let Some(noted) = noted {
                            outcome.posted |= noted.posted;
                            outcome.notices.extend(noted.notices);
                        }
                        return Ok(outcome);
                    }
                }
                self.changed_answer(message, &post.id)
            }

            PIN | UNPIN => {
                let Some(post) = self.named_post(body, sender)? else {
                    return refuse(message, "e.p.msg.unknown-post", "There is no such post.");
                };
                // Whoever manages posts in the channel says what is pinned in it.
                let Some(channel) = self.store.channel(&post.channel)? else {
                    return refuse(message, "e.p.msg.unknown-post", "There is no such post.");
                };
                if !self.may_in(sender, &channel, "manage_messages")? {
                    return refuse(message, "e.p.trust.not-authorized", "Pinning takes managing posts in the channel.");
                }
                if post.deleted {
                    return refuse(message, "e.p.msg.deleted", "That post was taken back.");
                }
                let pinned = message_type == PIN;
                if pinned != post.pinned {
                    self.store.set_pinned(&post.id, pinned)?;
                    self.audit(now, sender, if pinned { "post.pin" } else { "post.unpin" }, Some(&post.id), Some(json!({ "channel": post.channel, "author": post.author_did })));
                }
                self.changed_answer(message, &post.id)
            }

            REACT => {
                let Some(post) = self.named_post(body, sender)? else {
                    return refuse(message, "e.p.msg.unknown-post", "There is no such post.");
                };
                if post.deleted {
                    return refuse(message, "e.p.msg.deleted", "That post was taken back.");
                }
                if self.timed_out(sender, now)? {
                    return refuse(message, "e.p.msg.timed-out", "You are timed out here for now.");
                }
                if !self.store.channel(&post.channel)?.map(|channel| self.may_in(sender, &channel, "react")).transpose()?.unwrap_or(false) {
                    return refuse(message, "e.p.trust.not-authorized", "You can't react in this channel.");
                }
                let Some(listed) = body["emoji"].as_array() else {
                    return refuse(message, "e.p.msg.invalid-reaction", "A reaction is a list of emoji: all of this member's on the post, which may be none.");
                };
                let emoji: Vec<String> = listed.iter().filter_map(Value::as_str).map(str::to_string).collect();
                if emoji.len() != listed.len() || emoji.len() > MAX_REACTIONS || !emoji.iter().all(|emoji| is_emoji(emoji)) {
                    return refuse(message, "e.p.msg.invalid-reaction", "A reaction is an emoji, and a post takes twenty of them from one member.");
                }
                // A post takes so many different ones, whoever they are from.
                let known: Vec<&String> = post.reactions.iter().map(|(emoji, _)| emoji).collect();
                let new = emoji.iter().filter(|emoji| !known.contains(emoji)).count();
                if known.len() + new > MAX_REACTIONS && new > 0 {
                    return refuse(message, "e.p.msg.too-many", "That post has as many different reactions as one takes.");
                }
                self.store.set_reactions(&post.id, sender, &emoji, now)?;
                self.changed_answer(message, &post.id)
            }

            CREATE_THREAD => {
                let Some(post) = self.named_post(body, sender)? else {
                    return refuse(message, "e.p.msg.unknown-post", "There is no such post.");
                };
                if post.thread.is_some() {
                    return refuse(message, "e.p.msg.in-thread", "A thread doesn't grow from a post in a thread.");
                }
                if self.timed_out(sender, now)? {
                    return refuse(message, "e.p.msg.timed-out", "You are timed out here for now.");
                }
                // A private thread (Discord's): read by those in it, and whoever may
                // manage threads; not in a forum, whose posts are threads for all.
                let private = body["private"].as_bool() == Some(true);
                let Some(channel) = self.store.channel(&post.channel)? else {
                    return refuse(message, "e.p.msg.unknown-post", "There is no such post.");
                };
                if private && channel.forum.is_some() {
                    return refuse(message, "e.p.msg.invalid-thread", "A forum's threads are for everyone who sees it.");
                }
                if !self.may_in(sender, &channel, if private { "create_private_threads" } else { "create_threads" })? {
                    return refuse(message, "e.p.trust.not-authorized", if private { "You can't start private threads in this channel." } else { "You can't start threads in this channel." });
                }
                // Only someone in the conversation it grows from: whoever wrote the
                // post, what it answers, or an answer to it.
                let mut chain = self.store.chain_authors(&post.id)?;
                chain.extend(self.store.repliers(&post.id)?);
                if !chain.iter().any(|author| author == sender) {
                    return refuse(message, "e.p.trust.not-authorized", "A thread is started by someone who wrote in the replies it grows from.");
                }
                let name = line(&body["name"], MAX_THREAD_NAME);
                if private {
                    // Private before it is there, so nothing of it is ever told to anyone else.
                    self.store.make_thread_private(&post.id, sender, now)?;
                }
                self.store.add_thread(&post.id, &post.channel, name.as_deref(), sender, now)?;
                // Whoever grew it follows it, and so does whoever wrote the post (a
                // private one, only who started it: the rest are added to it).
                self.store.follow_thread(sender, &post.id, true)?;
                if !private {
                    self.store.follow_thread(&post.author_did, &post.id, true)?;
                }
                self.changed_answer(message, &post.id)
            }

            CHANGES_REQUEST => {
                let after = body["after_rev"].as_i64().unwrap_or(0).max(0);
                let mut posts = self.store.posts_changed_after(after, CHANGES_BATCH + 1)?;
                let more = posts.len() > CHANGES_BATCH;
                posts.truncate(CHANGES_BATCH);
                let upto = posts.last().map_or(after, |post| post.rev);
                // (What they may not see is passed over, `rev` past it all the same.)
                let seen = self.seen_by(sender)?;
                let mut messages = Vec::new();
                for post in posts.iter().filter(|post| seen.contains(&post.channel)) {
                    // (Nor posts of a private thread they aren't in.)
                    if post.thread.is_none() || self.reads_post_id(sender, post)? {
                        messages.push(Self::message_body(post));
                    }
                }
                Ok(Outcome::reply(reply_to(message, CHANGES, json!({ "messages": messages, "rev": upto, "more": more }))))
            }

            HISTORY_REQUEST => {
                let Some(channel) = self.channel_for(sender, body["channel"].as_str().unwrap_or_default())? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.");
                };
                let limit = body["limit"].as_u64().map_or(DEFAULT_HISTORY_LIMIT, |limit| (limit as usize).clamp(1, MAX_HISTORY_LIMIT));
                let thread = body["thread"].as_str().filter(|id| !id.is_empty());
                // A private thread they aren't in is no thread.
                if let Some(thread) = thread {
                    if !self.reads_thread(sender, &self.powers(sender)?, &channel, thread)? {
                        return refuse(message, "e.p.msg.unknown-thread", "There is no such thread in this channel.");
                    }
                }
                // A thread read a page at a time, as a forum's is: from a place in
                // it, or the page holding the first post after one already read (by
                // its seq, or by its id: what a read mark synced between a member's
                // devices names, on a device that holds none of the thread).
                if let Some(thread) = thread.filter(|_| ["offset", "page_after", "page_after_post"].iter().any(|key| body.get(key).is_some())) {
                    let total = self.store.thread(thread)?.filter(|t| t.channel == channel.id).map_or(0, |t| t.count.max(0) as usize);
                    let after = match body["page_after_post"].as_str() {
                        Some(post) => self.store.thread_post_seq(&channel.id, thread, post)?,
                        None => body["page_after"].as_i64(),
                    };
                    let offset = match after {
                        Some(seq) => {
                            let first_unread = self.store.thread_posts_through(&channel.id, thread, seq)?.max(0) as usize;
                            first_unread.min(total.saturating_sub(1)) / limit * limit
                        }
                        None => body["offset"].as_u64().map_or(0, |offset| offset as usize).min(total),
                    };
                    let posts = self.store.thread_page(&channel.id, thread, offset, limit)?;
                    let messages: Vec<Value> = posts.iter().map(Self::message_body).collect();
                    let more = offset + posts.len() < total;
                    return Ok(Outcome::reply(reply_to(
                        message,
                        HISTORY,
                        json!({ "channel": channel.id, "thread": thread, "messages": messages, "more": more, "offset": offset, "total": total }),
                    )));
                }
                // By time, newest first, from `before_at` (and `before_seq` among posts
                // of that second) or from the newest: where posts were brought in from
                // elsewhere, their numbers are newer than their times. By number with
                // `before`, as an app filling a hole asks.
                let mut posts = match body["before"].as_i64() {
                    Some(before) if body.get("before_at").is_none() => self.store.channel_posts_before(&channel.id, thread, Some(before), limit + 1)?,
                    _ => {
                        let before = body["before_at"].as_i64().map(|at| (at, body["before_seq"].as_i64().unwrap_or(i64::MAX)));
                        self.store.channel_posts_by_time(&channel.id, thread, before, limit + 1)?
                    }
                };
                let more = posts.len() > limit;
                posts.truncate(limit);
                // Oldest first, as they are read.
                posts.reverse();
                let messages: Vec<Value> = posts.iter().map(Self::message_body).collect();
                let mut history = json!({ "channel": channel.id, "messages": messages, "more": more });
                if let Some(thread) = thread {
                    history["thread"] = json!(thread);
                }
                Ok(Outcome::reply(reply_to(message, HISTORY, history)))
            }

            SET_RAID => {
                let powers = self.powers(sender)?;
                if !(powers.has("ban_members") || powers.has("manage_community")) {
                    return refuse(message, "e.p.trust.not-authorized", "Raid mode is for whoever may ban members, or manage the community.");
                }
                let Some(minutes) = body["minutes"].as_i64().filter(|m| (0..=crate::protection::RAID_LONGEST / 60).contains(m)) else {
                    return refuse(message, "e.p.msg.invalid-raid", "Raid mode is on for up to a day (1,440 minutes), or 0 to end it.");
                };
                self.set_raid(sender, minutes, now)?;
                self.audit(now, sender, if minutes > 0 { "raid.on" } else { "raid.off" }, None, Some(json!({ "minutes": minutes })));
                let mut outcome = Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?));
                let mut told = std::collections::HashSet::new();
                for moderator in self.joined_holding("ban_members")?.into_iter().chain(self.joined_holding("manage_community")?) {
                    if moderator.did != sender && told.insert(moderator.did.clone()) {
                        outcome.notices.push((moderator.did.clone(), json!({ "type": COMMUNITY, "body": self.community(&moderator)? })));
                    }
                }
                Ok(outcome)
            }

            CLAIM_IMPORTED => {
                let Some(mut claim) = self.store.claim_of(sender)?.filter(|c| c.state == "verified" || c.state == "refused") else {
                    return refuse(
                        message,
                        "e.p.msg.no-claim",
                        "No Discord account of yours with posts here has been checked: show your verified Discord account in your profile first.",
                    );
                };
                if self.imported_count(&claim.discord_id)? == 0 {
                    return refuse(message, "e.p.msg.no-claim", "Nothing was brought in from your Discord account.");
                }
                claim.state = "asked".into();
                claim.at = now;
                claim.decided_by = None;
                self.store.set_claim(&claim)?;
                self.audit(now, sender, "claim.ask", None, Some(json!({ "discord_id": claim.discord_id })));
                let mut outcome = Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?));
                outcome.notices = self.admins_told()?;
                Ok(outcome)
            }

            DECIDE_CLAIM => {
                if !self.powers(sender)?.has("admit_members") {
                    return refuse(message, "e.p.trust.not-authorized", "Deciding a claim on imported posts takes Let members in.");
                }
                let who = body["member"].as_str().unwrap_or_default();
                let Some(mut claim) = self.store.claim_of(who)?.filter(|c| c.state == "asked") else {
                    return refuse(message, "e.p.msg.no-claim", "That member hasn't asked for any imported posts.");
                };
                let approve = body["approve"].as_bool() == Some(true);
                let mut outcome = Outcome::default();
                if approve {
                    outcome.changed = self.store.claim_imported_posts(&claim.discord_id, who, now)?;
                }
                claim.state = if approve { "claimed" } else { "refused" }.into();
                claim.at = now;
                claim.decided_by = Some(sender.to_string());
                self.store.set_claim(&claim)?;
                self.audit(now, sender, if approve { "claim.approve" } else { "claim.refuse" }, None, Some(json!({ "member": who, "discord_id": claim.discord_id })));
                outcome.reply = Some(reply_to(message, COMMUNITY, self.community(&member)?));
                if let Some(claimant) = self.store.member(who)? {
                    outcome.notices.push((claimant.did.clone(), json!({ "type": COMMUNITY, "body": self.community(&claimant)? })));
                }
                outcome.notices.extend(self.admins_told()?.into_iter().filter(|(did, _)| did != sender));
                Ok(outcome)
            }

            MEMBERS_REQUEST => {
                // Those who can see one channel (its member list, who can be
                // called on there): asked of a channel the asker can see.
                let channel = match message["body"]["channel"].as_str() {
                    Some(id) => match self.channel_for(sender, id)? {
                        Some(channel) => Some(channel),
                        None => return refuse(message, "e.p.msg.unknown-channel", "There is no such channel."),
                    },
                    None => None,
                };
                let mut body = self.members_for(&member)?;
                if let Some(channel) = channel {
                    let mut seeing = Vec::new();
                    for listed in body["members"].as_array().into_iter().flatten() {
                        let did = listed["did"].as_str().unwrap_or_default();
                        if self.sees(did, &self.powers(did)?, &channel)? {
                            seeing.push(listed.clone());
                        }
                    }
                    body["members"] = json!(seeing);
                    body["channel"] = json!(channel.id);
                    if let Some(object) = body.as_object_mut() {
                        object.remove("waiting");
                    }
                }
                Ok(Outcome::reply(reply_to(message, MEMBERS, body)))
            }

            SET_ADMISSION => {
                if !self.powers(sender)?.has("manage_community") {
                    return refuse(message, "e.p.trust.not-authorized", "Deciding who is let in takes Manage the community.");
                }
                let admission = match body.get("admission").filter(|v| !v.is_null()) {
                    Some(value) => match value.as_str().and_then(Admission::parse) {
                        Some(admission) => Some(admission),
                        None => return refuse(message, "e.p.msg.invalid-admission", "Admission is admin, invite or open."),
                    },
                    None => None,
                };
                let may_invite = match body.get("members_may_invite").filter(|v| !v.is_null()) {
                    Some(value) => match value.as_bool() {
                        Some(may) => Some(may),
                        None => return refuse(message, "e.p.msg.invalid-admission", "members_may_invite is true or false."),
                    },
                    None => None,
                };
                let requires_address = match body.get("requires_address").filter(|v| !v.is_null()) {
                    Some(value) => match value.as_bool() {
                        Some(requires) => Some(requires),
                        None => return refuse(message, "e.p.msg.invalid-admission", "requires_address is true or false."),
                    },
                    None => None,
                };
                if let Some(requires) = requires_address {
                    self.store.set_setting("admission_requires_address", if requires { "1" } else { "0" })?;
                }
                if admission.is_none() && may_invite.is_none() && requires_address.is_none() {
                    return refuse(message, "e.p.msg.invalid-admission", "That changes nothing.");
                }
                if let Some(admission) = admission {
                    self.store.set_setting("admission", admission.as_str())?;
                }
                if let Some(may) = may_invite {
                    self.set_everyone("create_invites", may)?;
                }
                self.audit(now, sender, "community.admission", None, Some(body.clone()));
                let mut outcome = Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?));
                // Everyone joined is told, as of any change to what the community is.
                outcome.channels_changed = true;
                Ok(outcome)
            }

            CREATE_INVITE => {
                if !self.powers(sender)?.has("create_invites") {
                    return refuse(message, "e.p.trust.not-authorized", "You can't make invites here.");
                }
                let uses = match body.get("uses").filter(|v| !v.is_null()) {
                    Some(value) => match value.as_i64().filter(|uses| *uses >= 1) {
                        Some(uses) => Some(uses),
                        None => return refuse(message, "e.p.msg.invalid-invite", "An invite lets in at least one person."),
                    },
                    None => None,
                };
                let expires_at = match body.get("expires_at").filter(|v| !v.is_null()) {
                    Some(value) => match value.as_i64().filter(|at| *at > now) {
                        Some(at) => Some(at),
                        None => return refuse(message, "e.p.msg.invalid-invite", "An invite can't expire before it is made."),
                    },
                    None => None,
                };
                let note = match body.get("note").filter(|v| !v.is_null()) {
                    Some(value) => match value.as_str() {
                        Some(note) if note.chars().count() <= MAX_INVITE_NOTE => Some(note.trim().to_string()).filter(|n| !n.is_empty()),
                        _ => return refuse(message, "e.p.msg.invalid-invite", "An invite's note is at most 200 characters."),
                    },
                    None => None,
                };
                let invite = Invite { code: new_code(), uses_left: uses, expires_at, note, created_at: now, created_by: sender.to_string() };
                self.store.add_invite(&invite)?;
                self.audit(now, sender, "invite.create", Some(&invite.code), None);
                let mut answer = self.invites_body(&member, now)?;
                answer["created"] = json!(invite.code);
                Ok(Outcome::reply(reply_to(message, INVITES, answer)))
            }

            REVOKE_INVITE => {
                let code = body["code"].as_str().unwrap_or_default();
                let Some(invite) = self.store.invite(code)? else {
                    return refuse(message, "e.p.msg.unknown-invite", "There is no such invite.");
                };
                if invite.created_by != sender && !self.powers(sender)?.has("manage_community") {
                    return refuse(message, "e.p.trust.not-authorized", "Only whoever made an invite, or someone who manages the community, can revoke it.");
                }
                self.store.revoke_invite(code)?;
                self.audit(now, sender, "invite.revoke", Some(code), None);
                Ok(Outcome::reply(reply_to(message, INVITES, self.invites_body(&member, now)?)))
            }

            INVITES_REQUEST => Ok(Outcome::reply(reply_to(message, INVITES, self.invites_body(&member, now)?))),

            SET_WELCOME => {
                if !self.powers(sender)?.has("manage_community") {
                    return refuse(message, "e.p.trust.not-authorized", "Welcome messages take Manage the community.");
                }
                let current = self.welcome_setting()?;
                let channel = match body.get("channel") {
                    None => current["channel"].as_str().map(str::to_string),
                    Some(Value::Null) => None,
                    Some(given) => match given.as_str().map(|id| self.store.channel(id)).transpose()?.flatten() {
                        Some(channel) if !channel.closed && channel.voice.is_none() => Some(channel.id),
                        _ => return refuse(message, "e.p.msg.invalid-welcome", "The welcome goes in an open text channel."),
                    },
                };
                let lines = match body.get("lines") {
                    None => current["lines"].as_array().map(|lines| lines.iter().filter_map(Value::as_str).map(str::to_string).collect()),
                    Some(given) => match crate::welcome::read_lines(given) {
                        Ok(lines) => lines,
                        Err(why) => return refuse(message, "e.p.msg.invalid-welcome", why),
                    },
                };
                self.set_welcome(channel.as_deref(), lines)?;
                self.audit(now, sender, "community.welcome", None, Some(body.clone()));
                Ok(Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?)))
            }

            SET_WEB => {
                if !self.powers(sender)?.has("manage_community") {
                    return refuse(message, "e.p.trust.not-authorized", "The community on the web takes Manage the community.");
                }
                if self.web_url.is_none() {
                    return refuse(message, "e.p.msg.no-web", "This server shows nothing on the web.");
                }
                let page = body.get("page").filter(|v| !v.is_null());
                let voice = body.get("voice").filter(|v| !v.is_null());
                let public = body.get("public_channels").filter(|v| !v.is_null());
                if page.is_none() && voice.is_none() && public.is_none() {
                    return refuse(message, "e.p.msg.invalid-web", "Say page, voice or public_channels.");
                }
                if let Some(public) = public {
                    let Some(choice) = public.as_str().filter(|v| crate::web::PUBLIC_CHOICES.contains(v)) else {
                        return refuse(message, "e.p.msg.invalid-web", "public_channels is everyone or chosen.");
                    };
                    self.store.set_setting(crate::web::PUBLIC, choice)?;
                }
                if let Some(page) = page {
                    let Some(on) = page.as_bool() else { return refuse(message, "e.p.msg.invalid-web", "page is true or false.") };
                    self.store.set_setting(crate::web::PAGE, if on { "1" } else { "0" })?;
                }
                if let Some(voice) = voice {
                    let Some(choice) = voice.as_str().filter(|v| crate::web::VOICE_CHOICES.contains(v)) else {
                        return refuse(message, "e.p.msg.invalid-web", "voice is off, members, counts or names.");
                    };
                    self.store.set_setting(crate::web::VOICE, choice)?;
                }
                self.audit(now, sender, "community.web", None, Some(body.clone()));
                let mut outcome = Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?));
                outcome.channels_changed = true;
                Ok(outcome)
            }

            SET_VOICE => {
                if !self.powers(sender)?.has("manage_community") {
                    return refuse(message, "e.p.trust.not-authorized", "Setting how voice channels work takes Manage the community.");
                }
                if self.voice.is_none() {
                    return refuse(message, "e.p.msg.unsupported", "This server has no voice channels.");
                }
                match body.get("transport") {
                    None => {}
                    Some(Value::String(t)) if t == "moq" || t == "webrtc" => self.store.set_setting("voice_transport", t)?,
                    Some(_) => return refuse(message, "e.p.msg.invalid-voice", "The transport is moq or webrtc."),
                }
                match body.get("text_chat") {
                    None => {}
                    Some(Value::Bool(on)) => self.store.set_setting("voice_text", if *on { "true" } else { "false" })?,
                    Some(_) => return refuse(message, "e.p.msg.invalid-voice", "text_chat is true or false."),
                }
                match body.get("invites_to") {
                    None => {}
                    Some(Value::Null) => self.store.set_setting("voice_invites_to", "")?,
                    Some(Value::String(id)) => match self.store.channel(id)? {
                        Some(channel) if channel.voice.is_none() => self.store.set_setting("voice_invites_to", id)?,
                        _ => return refuse(message, "e.p.msg.unknown-channel", "That isn't a text channel here."),
                    },
                    Some(_) => return refuse(message, "e.p.msg.invalid-voice", "invites_to is a channel's id."),
                }
                self.audit(now, sender, "community.voice", None, Some(body.clone()));
                let mut outcome = Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?));
                outcome.channels_changed = true;
                Ok(outcome)
            }

            ACCEPT_RULES => {
                let current = self.rules()?.map(|rules| rules.version);
                if current.is_none() || body["rules_version"].as_i64() != current {
                    return refuse(message, "e.p.msg.invalid-rules", "Those aren't the rules as they stand.");
                }
                self.store.accept_rules(sender, current.unwrap_or_default())?;
                Ok(Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?)))
            }

            SET_RULES => {
                if !self.powers(sender)?.has("manage_community") {
                    return refuse(message, "e.p.trust.not-authorized", "Setting the rules takes Manage the community.");
                }
                let text = match body.get("rules").filter(|v| !v.is_null()) {
                    Some(value) => match value.as_str() {
                        Some(text) if text.chars().count() > MAX_RULES => return refuse(message, "e.p.msg.too-long", "Rules are at most 20,000 characters."),
                        Some(text) => Some(text.trim().to_string()),
                        None => return refuse(message, "e.p.msg.invalid-rules", "The rules are text."),
                    },
                    None => None,
                };
                let required = match body.get("required").filter(|v| !v.is_null()) {
                    Some(value) => match value.as_bool() {
                        Some(required) => Some(required),
                        None => return refuse(message, "e.p.msg.invalid-rules", "required is true or false."),
                    },
                    None => None,
                };
                if text.is_none() && required.is_none() {
                    return refuse(message, "e.p.msg.invalid-rules", "That changes nothing.");
                }
                let before = self.rules()?;
                let after_text = text.clone().unwrap_or_else(|| before.as_ref().map(|rules| rules.text.clone()).unwrap_or_default());
                if required == Some(true) && after_text.is_empty() {
                    return refuse(message, "e.p.msg.invalid-rules", "There are no rules to require.");
                }
                if let Some(text) = &text {
                    if before.as_ref().map(|rules| &rules.text) != Some(text) && !text.is_empty() {
                        // A new text is a new version, counting on from the last one
                        // there ever was.
                        let last: i64 = self.store.setting("rules_version")?.and_then(|v| v.parse().ok()).unwrap_or(0);
                        self.store.set_setting("rules_version", &(last + 1).to_string())?;
                    }
                    self.store.set_setting("rules", text)?;
                }
                if let Some(required) = required {
                    self.store.set_setting("rules_required", if required { "true" } else { "false" })?;
                }
                if text.as_deref() == Some("") {
                    self.store.set_setting("rules_required", "false")?;
                }
                // Whoever set them has read them.
                if let Some(rules) = self.rules()? {
                    self.store.accept_rules(sender, rules.version)?;
                }
                self.audit(now, sender, "community.rules", None, Some(json!({ "version": self.rules()?.map(|rules| rules.version), "required": self.rules()?.is_some_and(|rules| rules.required) })));
                let mut outcome = Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?));
                outcome.channels_changed = true;
                Ok(outcome)
            }

            REPORT => {
                // At most a few at a time from anyone (WYV-50): each one goes to every
                // moderator.
                let lately = self.store.reports(true)?.iter().filter(|report| report.by == sender && report.reported_at > now - REPORTS_WINDOW).count();
                if lately >= REPORTS_PER_WINDOW {
                    return refuse(message, "e.p.msg.too-many-reports", "You've reported a lot just now: wait a few minutes.");
                }
                let reason = body["reason"].as_str().unwrap_or_default();
                if !REPORT_REASONS.contains(&reason) {
                    return refuse(message, "e.p.msg.invalid-report", "A report gives one of the reasons listed.");
                }
                let note = match body.get("note").filter(|v| !v.is_null()) {
                    Some(value) => match value.as_str() {
                        Some(note) if note.chars().count() <= MAX_REPORT_NOTE => Some(note.trim().to_string()).filter(|n| !n.is_empty()),
                        _ => return refuse(message, "e.p.msg.invalid-report", "A report's note is at most 1,000 characters."),
                    },
                    None => None,
                };
                let post_id = body["post"].as_str().filter(|id| !id.is_empty());
                let reported_member = body["member"].as_str().filter(|did| !did.is_empty());
                let mut report = Report {
                    id: uuid::Uuid::new_v4().to_string(),
                    reported_at: now,
                    by: sender.to_string(),
                    reason: reason.to_string(),
                    note,
                    post_id: None,
                    post: None,
                    member: None,
                    resolved_at: None,
                    resolved_by: None,
                };
                match (post_id, reported_member) {
                    (Some(id), None) => {
                        let Some(post) = self.store.post_by_id(id)?.filter(|post| body["channel"].as_str().is_none_or(|channel| channel == post.channel)).filter(|post| self.sees_id(sender, &post.channel).unwrap_or(false)) else {
                            return refuse(message, "e.p.msg.unknown-post", "There is no such post.");
                        };
                        if post.author_did == sender {
                            return refuse(message, "e.p.msg.invalid-report", "A post of one's own isn't reported.");
                        }
                        let mut author = json!({ "did": post.author_did });
                        if let Some(name) = &post.author_name {
                            author["name"] = json!(name);
                        }
                        report.post_id = Some(post.id.clone());
                        report.post = Some(json!({ "channel": post.channel, "id": post.id, "seq": post.seq, "content": post.content, "author": author }));
                    }
                    (None, Some(did)) => {
                        if did == sender {
                            return refuse(message, "e.p.msg.invalid-report", "Nobody reports themselves.");
                        }
                        if self.store.member(did)?.is_none() {
                            return refuse(message, "e.p.msg.invalid-member", "There is no such member.");
                        }
                        report.member = Some(did.to_string());
                    }
                    _ => return refuse(message, "e.p.msg.invalid-report", "A report names a post or a member, one of them."),
                }
                let id = self.store.add_report(&report)?;
                report.id = id.clone();
                let mut outcome = Outcome::reply(reply_to(message, REPORT_RECEIVED, json!({ "id": id })));
                // Each moderator is sent the new report alone, as they may see it, and
                // how many are open: not every open report again each time.
                let open = self.store.reports(false)?.len();
                for admin in self.joined_holding("manage_reports")?.into_iter().filter(|joined| joined.did != sender) {
                    let body = json!({ "reports": [self.report_body(&report, &admin.did)?], "open": open });
                    outcome.notices.push((admin.did, json!({ "type": REPORTS, "id": uuid::Uuid::new_v4().to_string(), "body": body })));
                }
                Ok(outcome)
            }

            REPORTS_REQUEST | RESOLVE_REPORT => {
                if !self.powers(sender)?.has("manage_reports") {
                    return refuse(message, "e.p.trust.not-authorized", "Reports are for those who deal with them.");
                }
                if message_type == RESOLVE_REPORT {
                    let id = body["id"].as_str().unwrap_or_default();
                    if self.store.report(id)?.is_none() {
                        return refuse(message, "e.p.msg.unknown-report", "There is no such report.");
                    }
                    self.store.resolve_report(id, sender, now)?;
                    self.audit(now, sender, "report.resolve", Some(id), None);
                }
                Ok(Outcome::reply(reply_to(message, REPORTS, self.reports_body(body["resolved"] == true, sender)?)))
            }

            SET_PROFILE => {
                if !self.powers(sender)?.has("manage_community") {
                    return refuse(message, "e.p.trust.not-authorized", "Changing what the community is called takes Manage the community.");
                }
                let change = match Change::read(message) {
                    Ok(change) => change,
                    Err(refused) => return refuse(message, refused.code, &refused.comment),
                };
                let changed = {
                    let mut changed = self.changed.write().unwrap_or_else(|poisoned| poisoned.into_inner());
                    changed.apply(change);
                    changed.clone()
                };
                if let Some(kept) = &self.kept {
                    kept.save(&changed)?;
                }
                let mut said = body["profile"].clone();
                if let Some(profile) = said.as_object_mut() {
                    profile.remove("displayPicture");
                }
                self.audit(now, sender, "community.profile", None, Some(said));
                let mut outcome = Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?));
                // Everyone joined is told what it is called now, as they are of a
                // change to its channels.
                outcome.channels_changed = true;
                outcome.profile_changed = true;
                // And who it is, picture and all, as a contact tells of a changed
                // profile: `community` carries no picture, so a picture changed
                // after someone joined never reached their app (found 2026-11-20).
                let told = self.profile_message();
                let own = self.own_dids();
                for joined in self.store.joined()?.into_iter().filter(|joined| !own.contains(&joined.did)) {
                    outcome.notices.push((joined.did, told.clone()));
                }
                Ok(outcome)
            }

            SET_MEMBER => {
                let Some(did) = body["did"].as_str().filter(|did| did.starts_with("did:") && did.len() <= 4096) else {
                    return refuse(message, "e.p.msg.invalid-member", "That isn't a DID.");
                };
                let powers = self.powers(sender)?;
                let existing = self.store.member(did)?;
                // Someone banned is let back in only by lifting the ban (WYV-50).
                if existing.is_none() && body["role"].is_string() && self.store.banned(did, now)?.is_some() {
                    return refuse(message, "e.p.msg.banned", "They're banned: lift the ban first.");
                }
                let waiting = if existing.is_none() { self.store.waiting_one(did)? } else { None };
                let was_waiting = waiting.is_some();
                if existing.is_some() && self.is_owner(did)? {
                    return refuse(message, "e.p.msg.owner", "The owner can't be taken out or given roles this way.");
                }
                let not_allowed = |why: &str| refuse(message, "e.p.trust.not-authorized", why);
                match body["role"].as_str() {
                    Some(role) => {
                        if !matches!(role, "admin" | "moderator" | "member") {
                            return refuse(message, "e.p.msg.invalid-role", "A role here is admin, moderator or member.");
                        }
                        // Letting in someone waiting as a member is the waiting list's;
                        // anything else is giving roles.
                        let plain_admission = was_waiting && role == "member";
                        if plain_admission && !powers.has("admit_members") {
                            return not_allowed("Letting people in takes Let people in from the waiting list.");
                        }
                        if !plain_admission && !powers.has("manage_roles") {
                            return not_allowed("Giving roles takes Manage roles.");
                        }
                        if existing.is_some() && !self.outranks(&powers, did)? {
                            return not_allowed("You can only change the roles of someone ranked below you.");
                        }
                        let roles = match role {
                            "admin" => vec![crate::roles::ADMIN_ROLE],
                            "moderator" => vec![crate::roles::MODERATOR_ROLE],
                            _ => Vec::new(),
                        };
                        let mut given = Vec::new();
                        for id in &roles {
                            match self.store.role(id)? {
                                Some(def) if powers.above(def.position) => given.push(def.id),
                                Some(_) => return not_allowed("You can only give roles ranked below your own."),
                                None => return refuse(message, "e.p.msg.invalid-role", "This community has no such role now."),
                            }
                        }
                        // `member` takes Admin and Moderator away; the others stay.
                        let taken: Vec<&str> = [crate::roles::ADMIN_ROLE, crate::roles::MODERATOR_ROLE].into_iter().filter(|id| !roles.contains(id)).collect();
                        if existing.is_some() {
                            let held = self.store.member_roles(did)?;
                            for id in taken.iter().filter(|id| held.iter().any(|h| h == *id)) {
                                if !self.store.role(id)?.is_some_and(|def| powers.above(def.position)) {
                                    return not_allowed("You can only take away roles ranked below your own.");
                                }
                            }
                        }
                        if self.store.is_superseded(did)? {
                            return refuse(message, "e.p.trust.superseded", "That DID has been replaced by another.");
                        }
                        if existing.is_none() {
                            if self.full()? {
                                return refuse(message, "e.p.req.community-full", "This community has as many members as it can.");
                            }
                            self.store.set_member(did, Role::Member, now)?;
                        }
                        let held = self.store.member_roles(did)?;
                        let really_taken: Vec<&&str> = taken.iter().filter(|id| held.iter().any(|h| h == **id)).collect();
                        let really_given: Vec<&String> = given.iter().filter(|id| !held.contains(id)).collect();
                        for id in &taken {
                            self.store.take_role(did, id)?;
                        }
                        for id in &given {
                            self.store.give_role(did, id)?;
                        }
                        if waiting.is_some() || existing.is_none() {
                            self.audit(now, sender, "member.admit", Some(did), None);
                        }
                        if !really_taken.is_empty() || !really_given.is_empty() {
                            self.audit(now, sender, "member.roles", Some(did), Some(json!({ "given": really_given, "taken": really_taken })));
                        }
                        // Someone let in from the waiting list asked to be sent posts:
                        // they are joined, and told what the community is.
                        if let Some(waiting) = &waiting {
                            self.store.remove_waiting(did)?;
                            self.store.set_joined(did, true)?;
                            // What they said they are called while waiting.
                            if waiting.name.is_some() {
                                self.store.set_name(did, waiting.name.as_deref())?;
                            }
                            let admitted = self.store.member(did)?.expect("just let in");
                            let mut outcome = Outcome::reply(reply_to(message, MEMBERS, self.members_for(&member)?));
                            outcome.posted = self.welcome(did, now)?;
                            outcome.notices.push((did.to_string(), json!({ "type": COMMUNITY, "body": self.community(&admitted)? })));
                            outcome.notices.extend(self.admins_told()?);
                            return Ok(outcome);
                        }
                        // Someone whose roles changed is told what they may do now.
                        let mut outcome = Outcome::reply(reply_to(message, MEMBERS, self.members_for(&member)?));
                        if let Some(changed) = self.store.member(did)?.filter(|m| m.joined && m.did != sender) {
                            outcome.notices.push((did.to_string(), json!({ "type": COMMUNITY, "body": self.community(&changed)? })));
                        }
                        Ok(outcome)
                    }
                    None => {
                        if was_waiting && !powers.has("admit_members") {
                            return not_allowed("Turning people away takes Let people in from the waiting list.");
                        }
                        if existing.is_some() && !(powers.has("kick_members") && self.outranks(&powers, did)?) {
                            return not_allowed("Taking someone out takes Take members out, and ranking above them.");
                        }
                        if !was_waiting && existing.is_none() && !powers.has("kick_members") && !powers.has("admit_members") {
                            return not_allowed("You can't take people out.");
                        }
                        // Turned away, if they were waiting; taken out, if they were in.
                        // Either way they are told, and nothing more of the community.
                        self.store.remove_waiting(did)?;
                        self.store.remove_member(did)?;
                        if was_waiting {
                            self.audit(now, sender, "member.turn-away", Some(did), None);
                        } else if existing.is_some() {
                            self.audit(now, sender, "member.remove", Some(did), None);
                        }
                        let mut outcome = Outcome::reply(reply_to(message, MEMBERS, self.members_for(&member)?));
                        if was_waiting {
                            outcome.notices.push((did.to_string(), json!({ "type": REMOVED, "body": { "was": "waiting" } })));
                            outcome.notices.extend(self.admins_told()?);
                        } else if existing.is_some() {
                            outcome.notices.push((did.to_string(), json!({ "type": REMOVED, "body": { "was": "member" } })));
                        }
                        Ok(outcome)
                    }
                }
            }

            // A member's app asking who the community is: its profile, picture and all.
            REQUEST_PROFILE => {
                let mut told = self.profile_message();
                if let Some(thid) = thread_of(message) {
                    told["thid"] = json!(thid);
                }
                Ok(Outcome::reply(told))
            }

            PROFILE => {
                let profile = &body["profile"];
                // Absent means no change (user-profile/1.0); present and empty clears.
                if profile.get("displayName").is_some() {
                    self.store.set_name(sender, line(&profile["displayName"], MAX_NAME_LENGTH).as_deref())?;
                }
                // Their picture, shown beside their posts on the web: one the
                // message carries, never fetched from a link.
                if profile.get("displayPicture").is_some() {
                    match wyvrn_service::profile::Change::read(message).map(|change| change.picture) {
                        Ok(wyvrn_service::profile::Field::Set(picture)) => self.store.set_member_picture(sender, Some((&picture.media_type, &picture.base64)), now)?,
                        Ok(wyvrn_service::profile::Field::Removed) => self.store.set_member_picture(sender, None, now)?,
                        _ => {}
                    }
                }
                // The rest of it, which everyone else in the community is told
                // (community/1.0's Profiles), and they are told now if connected.
                self.take_profile_rest(sender, message, now)?;
                let mut outcome = Outcome::default();
                if self.store.member(sender)?.is_some() {
                    outcome.live.push(self.member_changed(sender)?);
                }
                Ok(outcome)
            }

            SET_MEMBER_PROFILE => self.set_member_profile(sender, message, now),
            MESSAGE_REQUEST => self.message_request(sender, message, now),

            other if other.starts_with(PIURI) => refuse(message, "e.p.msg.unsupported", "This server doesn't know that message."),
            // Anything else from a member (a receipt, a basic message) is not for a server to answer.
            _ => Ok(Outcome::default()),
        }
    }
}

#[cfg(test)]
mod name_tests {
    #[test]
    fn text_channel_names_take_emoji_as_discord_names_do() {
        for name in ["📜rules", "subbies🌟", "❄contest-entries❄", "gen3r4-ル", "👩‍💻-devs", "🇺🇸-chat", "1️⃣-first", "design-review"] {
            assert!(super::text_channel_name(name), "{name}");
        }
        for name in ["Design", "two words", "a#b", "😀 x"] {
            assert!(!super::text_channel_name(name), "{name}");
        }
    }
}
