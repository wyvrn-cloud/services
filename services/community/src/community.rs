//! community/1.0: what the server does with each message a member sends it.
//!
//! Nothing here touches the network. [`Community::handle`] takes one message and
//! says what to answer, who else to tell, and whether there is now something to
//! send to everyone; `server.rs` does the sending.

use serde_json::{json, Map, Value};

use crate::config::{CommunityConfig, VoiceConfig};
use crate::store::{Channel, Invite, Member, Mention, Post, Report, Role, Store, Waiting};
use wyvrn_service::profile::{Change, Changed, Kept};

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
pub const MEMBERS: &str = "https://wyvrn.app/community/1.0/members";
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
pub const RULES: &str = "https://wyvrn.app/community/1.0/rules";
pub const ACCEPT_RULES: &str = "https://wyvrn.app/community/1.0/accept-rules";
pub const SET_RULES: &str = "https://wyvrn.app/community/1.0/set-rules";
pub const REPORT: &str = "https://wyvrn.app/community/1.0/report";
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
const MAX_MENTIONS: usize = 50;
const MAX_MENTION_NAME: usize = 40;
/// As many different emoji as one post takes, and as one member puts on it.
const MAX_REACTIONS: usize = 20;
const MAX_THREAD_NAME: usize = 80;
/// As many changed posts as one `changes` carries.
const CHANGES_BATCH: usize = 100;

pub const USER_PROFILE: &str = "https://didcomm.org/user-profile/1.0";
pub const PROFILE: &str = "https://didcomm.org/user-profile/1.0/profile";
pub const PROBLEM_REPORT: &str = "https://didcomm.org/report-problem/2.0/problem-report";

pub const DEFAULT_HISTORY_LIMIT: usize = 30;
pub const MAX_HISTORY_LIMIT: usize = 100;
const MAX_NAME_LENGTH: usize = 80;
const MAX_CHANNEL_NAME: usize = 40;
const MAX_TOPIC: usize = 200;
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

/// A voice channel's settings as an admin gives them (community-voice/1.0, "A voice
/// channel"), laid over what it had: checked, and with nothing in them the protocol
/// doesn't name. `Err` says what is wrong.
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
    match given.get("limit") {
        None => {}
        Some(Value::Null) => {
            out.remove("limit");
        }
        Some(value) => match value.as_i64().filter(|n| (2..=MAX_VOICE_LIMIT).contains(n)) {
            Some(n) => {
                out.insert("limit".into(), json!(n));
            }
            None => return Err("A voice channel holds between 2 and 500."),
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
fn new_code() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

pub struct Community {
    pub store: Store,
    pub config: CommunityConfig,
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
/// Whether a name is one a text channel may be given (community/1.0, "A text
/// channel's name is one word"): lowercase letters of any script, letters of
/// scripts without case, their marks, digits, `_`, `.` and `-`.
fn text_channel_name(name: &str) -> bool {
    static WORD: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^[\p{Ll}\p{Lo}\p{Lm}\p{Mn}\p{Mc}\p{Nd}_.\-]+$").expect("a valid pattern"));
    WORD.is_match(name)
}

const INVALID_NAME: &str = "A text channel's name is one word in lowercase: letters, digits, _, . and -, as in design-review.";

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
        body
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
        let mut body = json!({ "transport": transport });
        if let Some(channel) = self.store.setting("voice_invites_to")?.filter(|c| !c.is_empty()) {
            body["invites_to"] = json!(channel);
        }
        Ok(Some(body))
    }

    /// A community as its settings have it, with what an admin changed last time
    /// laid over them. `kept` is where a change is written; without it, one lasts
    /// until the server stops.
    pub fn new(store: Store, config: CommunityConfig, kept: Option<Kept>) -> anyhow::Result<Self> {
        let changed = match &kept {
            Some(kept) => kept.load()?,
            None => Changed::default(),
        };
        let community = Self { store, config, changed: std::sync::RwLock::new(changed), kept, voice: None, assistant: None, own: None };
        community.ensure_roles()?;
        Ok(community)
    }

    /// What an admin has changed of the community's name, description and picture.
    pub fn changed(&self) -> Changed {
        self.changed.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    /// The community as `member` is told of it.
    pub fn community(&self, member: &Member) -> anyhow::Result<Value> {
        let powers = self.powers(&member.did)?;
        let mut channels = Vec::new();
        for channel in self.store.channels()? {
            let mut body = Self::channel_body(&channel, &member.did);
            body["permissions"] = json!(self.channel_permissions(&member.did, &powers, &channel)?);
            if self.read_only(&channel.id)? {
                body["read_only"] = json!(true);
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
            "roles": self.roles_body()?,
            "my_roles": powers.roles,
            "permissions": powers.listed(),
            "members_may_create_channels": self.everyone_has("create_channels")?,
            "admission": self.admission()?.as_str(),
            "members_may_invite": self.members_may_invite()?,
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
            if let Some(voice) = self.voice_defaults()? {
                body["voice"] = voice;
            }
            if let Some(assistant) = &self.assistant {
                body["assistant"] = assistant.clone();
            }
            if powers.has("admit_members") {
                body["waiting"] = json!(self.store.waiting_count()?);
            }
            if let Some(until) = self.store.timed_out_until(&member.did, crate::gates::now())? {
                body["timed_out_until"] = json!(until);
            }
            if let Some(wait) = self.post_wait()? {
                body["gates"] = json!({ "post_wait": wait });
            }
            let categories = self.store.categories()?;
            if !categories.is_empty() {
                body["categories"] = json!(categories.iter().map(|c| json!({ "id": c.id, "name": c.name })).collect::<Vec<_>>());
            }
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

    pub(crate) fn report_body(&self, report: &Report) -> anyhow::Result<Value> {
        let mut body = json!({ "id": report.id, "reported_at": report.reported_at, "by": self.who(&report.by)?, "reason": report.reason });
        if let Some(note) = &report.note {
            body["note"] = json!(note);
        }
        if let Some(post) = &report.post {
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

    pub(crate) fn reports_body(&self, resolved_too: bool) -> anyhow::Result<Value> {
        let reports = self.store.reports(resolved_too)?.iter().map(|report| self.report_body(report)).collect::<anyhow::Result<Vec<_>>>()?;
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
        if !post.mentions.is_empty() {
            body["mentions"] = json!(post.mentions);
        }
        if let Some(edited_at) = post.edited_at {
            body["edited_at"] = json!(edited_at);
        }
        if post.deleted {
            body["deleted"] = json!(true);
        }
        if let Some(removed_by) = &post.removed_by {
            body["removed_by"] = json!(removed_by);
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
            if let Some(name) = &thread.name {
                grew["name"] = json!(name);
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
    fn referred(&self, reference: &Value) -> anyhow::Result<Option<Post>> {
        let id = reference["id"].as_str().or(reference.as_str()).filter(|id| !id.is_empty());
        let Some(post) = id.map(|id| self.store.post_by_id(id)).transpose()?.flatten() else { return Ok(None) };
        Ok(match reference["author"].as_str() {
            Some(author) if author != post.author_did => None,
            _ => Some(post),
        })
    }

    fn named_post(&self, body: &Value) -> anyhow::Result<Option<Post>> {
        self.referred(&body["to"])
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
        let mut members = Vec::new();
        for member in self.store.members()? {
            let powers = self.powers(&member.did)?;
            members.push((member, powers));
        }
        let members: Vec<Value> = members
            .iter()
            .map(|(member, powers)| {
                let mut listed = json!({ "did": member.did, "role": powers.older_role(), "roles": powers.roles, "joined": member.joined });
                if let Some(name) = &member.name {
                    listed["name"] = json!(name);
                }
                if let Some(left_at) = member.left_at.filter(|_| !member.joined) {
                    listed["left_at"] = json!(left_at);
                }
                listed
            })
            .collect();
        Ok(json!({ "members": members }))
    }

    /// The answer to a message that changed the post `id`: the post as it now
    /// stands, which everyone else who has joined is sent too.
    fn changed_answer(&self, message: &Value, id: &str) -> anyhow::Result<Outcome> {
        let Some(post) = self.store.post_by_id(id)? else {
            return refuse(message, "e.p.msg.unknown-post", "There is no such post.");
        };
        Ok(Outcome { reply: Some(reply_to(message, CHANGED, Self::message_body(&post))), changed: vec![post.id], ..Outcome::default() })
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
            if !self.store.is_superseded(did)? {
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
        let Some(channel) = self.store.channel(body["channel"].as_str().unwrap_or_default())? else {
            return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.");
        };
        if channel.closed {
            return refuse(message, "e.p.msg.closed", "That channel is closed.");
        }
        if channel.voice.is_some() {
            return refuse(message, "e.p.msg.voice-channel", "A voice channel takes no posts.");
        }
        if !self.may_in(sender, &channel, "post")? {
            return refuse(message, "e.p.trust.not-authorized", "You can't post in this channel.");
        }
        // A post sent again after a lost answer isn't held to slow mode.
        let again = body["id"].as_str().map(|id| self.store.post_by_id(id)).transpose()?.flatten().is_some_and(|post| post.author_did == sender);
        if !again && checked {
            if let Some(refused) = self.posting_refused(member, &channel, now)? {
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
        if content.is_empty() {
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
            // An answer to a post in a thread is in that thread.
            None => answered.as_ref().and_then(|post| post.thread.clone()),
        };
        let mentions = self.mentions_of(body, member, &channel)?;
        let noted = if checked {
            match self.automod(member, &channel, message, mentions.len(), now)? {
                crate::automod::Verdict::Placed => None,
                crate::automod::Verdict::PlacedNoted(noted) => Some(noted),
                crate::automod::Verdict::Stopped(stopped) => return Ok(stopped),
            }
        } else {
            None
        };
        let post = self.store.add_post(&channel.id, &id, content, member, question.as_ref(), None, now)?;
        let reply = answered.as_ref().map(|post| (post.id.as_str(), post.author_did.as_str(), snippet.as_deref()));
        self.store.set_written_with(&post.id, reply, thread.as_deref(), &mentions)?;
        let mut outcome = Outcome {
            reply: Some(reply_to(message, POSTED, json!({ "channel": post.channel, "seq": post.seq, "id": post.id }))),
            posted: true,
            posted_id: Some(post.id.clone()),
            ..Outcome::default()
        };
        // The post a thread grew from says how many are in it now.
        if let Some(thread) = thread {
            self.store.touch_thread(&thread)?;
            outcome.changed.push(thread);
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
        let Some(member) = self.store.member(sender)? else {
            // Someone who isn't let in may ask to be, take back having asked, and
            // say what they are called for the admins who decide.
            return match message_type {
                JOIN => match self.rules_before_join(sender, message)? {
                    Some(rules) => Ok(rules),
                    None => match self.store.banned(sender, now)?.is_some() {
                        // (Not asked anything: refused as anyone is who isn't let in.)
                        true => self.join_from_outside(sender, message, now),
                        false => match self.questions_before_join(sender, message, now)? {
                            Some(asked) => Ok(asked),
                            None => self.join_from_outside(sender, message, now),
                        },
                    },
                },
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

        if let Some(outcome) = self.handle_moderation(&member, message_type, message, now)? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.handle_held(&member, message_type, message, now)? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.handle_structure(&member, message_type, message, now)? {
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
                    Some(_) => return refuse(message, "e.p.msg.invalid-kind", "A channel is a text channel or a voice channel."),
                };
                let Some(name) = line(&body["name"], MAX_CHANNEL_NAME) else {
                    return refuse(message, "e.p.msg.empty", "A channel needs a name.");
                };
                if voice.is_none() && !text_channel_name(&name) {
                    return refuse(message, "e.p.msg.invalid-name", INVALID_NAME);
                }
                if self.store.channel_named(&name)?.is_some() {
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
                let mut community = self.community(&member)?;
                community["created"] = json!(id);
                Ok(Outcome { reply: Some(reply_to(message, COMMUNITY, community)), channels_changed: true, ..Outcome::default() })
            }

            UPDATE_CHANNEL => {
                let Some(channel) = self.store.channel(body["channel"].as_str().unwrap_or_default())? else {
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
                let named = self.store.channel_named(&name)?.filter(|other| other.id != channel.id);
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
                Ok(Outcome { reply: Some(reply_to(message, COMMUNITY, self.community(&member)?)), channels_changed: true, ..Outcome::default() })
            }

            POST | CREATE_THREAD if self.rules_owed(sender)?.is_some() => {
                refuse(message, "e.p.msg.rules-not-accepted", "Accept this community's rules before posting.")
            }

            POST => self.post(&member, message, now, true),

            EDIT | DELETE => {
                let Some(post) = self.named_post(body)? else {
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
                    self.store.delete_post(&post.id, removal.then_some(sender))?;
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
                    if content != post.content || mentions != post.mentions {
                        self.store.edit_post(&post.id, content, &mentions, now)?;
                    }
                }
                self.changed_answer(message, &post.id)
            }

            PIN | UNPIN => {
                let Some(post) = self.named_post(body)? else {
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
                let Some(post) = self.named_post(body)? else {
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
                let Some(post) = self.named_post(body)? else {
                    return refuse(message, "e.p.msg.unknown-post", "There is no such post.");
                };
                if post.thread.is_some() {
                    return refuse(message, "e.p.msg.in-thread", "A thread doesn't grow from a post in a thread.");
                }
                if self.timed_out(sender, now)? {
                    return refuse(message, "e.p.msg.timed-out", "You are timed out here for now.");
                }
                if !self.store.channel(&post.channel)?.map(|channel| self.may_in(sender, &channel, "create_threads")).transpose()?.unwrap_or(false) {
                    return refuse(message, "e.p.trust.not-authorized", "You can't start threads in this channel.");
                }
                // Only someone in the conversation it grows from: whoever wrote the
                // post, what it answers, or an answer to it.
                let mut chain = self.store.chain_authors(&post.id)?;
                chain.extend(self.store.repliers(&post.id)?);
                if !chain.iter().any(|author| author == sender) {
                    return refuse(message, "e.p.trust.not-authorized", "A thread is started by someone who wrote in the replies it grows from.");
                }
                let name = line(&body["name"], MAX_THREAD_NAME);
                self.store.add_thread(&post.id, &post.channel, name.as_deref(), sender, now)?;
                self.changed_answer(message, &post.id)
            }

            CHANGES_REQUEST => {
                let after = body["after_rev"].as_i64().unwrap_or(0).max(0);
                let mut posts = self.store.posts_changed_after(after, CHANGES_BATCH + 1)?;
                let more = posts.len() > CHANGES_BATCH;
                posts.truncate(CHANGES_BATCH);
                let upto = posts.last().map_or(after, |post| post.rev);
                let messages: Vec<Value> = posts.iter().map(Self::message_body).collect();
                Ok(Outcome::reply(reply_to(message, CHANGES, json!({ "messages": messages, "rev": upto, "more": more }))))
            }

            HISTORY_REQUEST => {
                let Some(channel) = self.store.channel(body["channel"].as_str().unwrap_or_default())? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.");
                };
                let limit = body["limit"].as_u64().map_or(DEFAULT_HISTORY_LIMIT, |limit| (limit as usize).clamp(1, MAX_HISTORY_LIMIT));
                let thread = body["thread"].as_str().filter(|id| !id.is_empty());
                let mut posts = self.store.channel_posts_before(&channel.id, thread, body["before"].as_i64(), limit + 1)?;
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

            MEMBERS_REQUEST => Ok(Outcome::reply(reply_to(message, MEMBERS, self.members_for(&member)?))),

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
                if admission.is_none() && may_invite.is_none() {
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
                        let Some(post) = self.store.post_by_id(id)?.filter(|post| body["channel"].as_str().is_none_or(|channel| channel == post.channel)) else {
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
                let mut outcome = Outcome::reply(reply_to(message, REPORT_RECEIVED, json!({ "id": id })));
                let open = self.reports_body(false)?;
                for admin in self.joined_holding("manage_reports")?.into_iter().filter(|joined| joined.did != sender) {
                    outcome.notices.push((admin.did, json!({ "type": REPORTS, "id": uuid::Uuid::new_v4().to_string(), "body": open.clone() })));
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
                Ok(Outcome::reply(reply_to(message, REPORTS, self.reports_body(body["resolved"] == true)?)))
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
                Ok(outcome)
            }

            SET_MEMBER => {
                let Some(did) = body["did"].as_str().filter(|did| did.starts_with("did:") && did.len() <= 4096) else {
                    return refuse(message, "e.p.msg.invalid-member", "That isn't a DID.");
                };
                let powers = self.powers(sender)?;
                let existing = self.store.member(did)?;
                let waiting = if existing.is_none() { self.store.waiting_one(did)? } else { None };
                let was_waiting = waiting.is_some();
                if existing.is_some() && self.owner()?.as_deref() == Some(did) {
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

            PROFILE => {
                let profile = &body["profile"];
                // Absent means no change (user-profile/1.0); present and empty clears.
                if profile.get("displayName").is_some() {
                    self.store.set_name(sender, line(&profile["displayName"], MAX_NAME_LENGTH).as_deref())?;
                }
                Ok(Outcome::default())
            }

            other if other.starts_with(PIURI) => refuse(message, "e.p.msg.unsupported", "This server doesn't know that message."),
            // Anything else from a member (a receipt, a basic message) is not for a server to answer.
            _ => Ok(Outcome::default()),
        }
    }
}
