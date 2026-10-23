//! community/1.0: what the server does with each message a member sends it.
//!
//! Nothing here touches the network. [`Community::handle`] takes one message and
//! says what to answer, who else to tell, and whether there is now something to
//! send to everyone; `server.rs` does the sending.

use serde_json::{json, Map, Value};

use crate::config::CommunityConfig;
use crate::store::{Channel, Invite, Member, Mention, Post, Role, Store};
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
pub const SET_ADMISSION: &str = "https://wyvrn.app/community/1.0/set-admission";
pub const CREATE_INVITE: &str = "https://wyvrn.app/community/1.0/create-invite";
pub const REVOKE_INVITE: &str = "https://wyvrn.app/community/1.0/revoke-invite";
pub const INVITES_REQUEST: &str = "https://wyvrn.app/community/1.0/invites-request";
pub const INVITES: &str = "https://wyvrn.app/community/1.0/invites";
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
}

impl Outcome {
    fn reply(reply: Value) -> Self {
        Self { reply: Some(reply), ..Self::default() }
    }
}

fn thread_of(message: &Value) -> Option<&str> {
    message["thid"].as_str().or(message["id"].as_str())
}

fn reply_to(message: &Value, message_type: &str, body: Value) -> Value {
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

fn refuse(message: &Value, code: &str, comment: &str) -> anyhow::Result<Outcome> {
    Ok(Outcome::reply(problem(message, code, comment)))
}

/// Text someone typed for a name or a label: trimmed, on one line, and not longer
/// than `max` characters. `None` if that leaves nothing.
fn line(value: &Value, max: usize) -> Option<String> {
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
    fn channel_body(channel: &Channel) -> Value {
        json!({ "id": channel.id, "name": channel.name, "topic": channel.topic, "closed": channel.closed, "latest_seq": channel.latest_seq })
    }

    /// A community as its settings have it, with what an admin changed last time
    /// laid over them. `kept` is where a change is written; without it, one lasts
    /// until the server stops.
    pub fn new(store: Store, config: CommunityConfig, kept: Option<Kept>) -> anyhow::Result<Self> {
        let changed = match &kept {
            Some(kept) => kept.load()?,
            None => Changed::default(),
        };
        Ok(Self { store, config, changed: std::sync::RwLock::new(changed), kept })
    }

    /// What an admin has changed of the community's name, description and picture.
    pub fn changed(&self) -> Changed {
        self.changed.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    /// The community as `member` is told of it.
    pub fn community(&self, member: &Member) -> anyhow::Result<Value> {
        let channels: Vec<Value> = self.store.channels()?.iter().map(Self::channel_body).collect();
        let changed = self.changed();
        Ok(json!({
            "name": changed.name.unwrap_or_else(|| self.config.name.clone()),
            "description": changed.description.unwrap_or_else(|| self.config.description.clone()),
            "role": member.role.as_str(),
            "joined": member.joined,
            "members_may_create_channels": self.config.members_may_create_channels,
            "admission": self.admission()?.as_str(),
            "members_may_invite": self.members_may_invite()?,
            "channels": channels,
            // Where the last change to any post stands: an app holding less asks for
            // what it missed (`changes-request`).
            "latest_rev": self.store.latest_rev()?,
        }))
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
    /// member's DID, or `everyone` for an admin, and `name` what the post writes
    /// them as. Anyone else named is left out, not refused: a member taken out
    /// since the post was written is nobody's mistake.
    fn mentions_of(&self, body: &Value, member: &Member) -> anyhow::Result<Vec<Mention>> {
        let mut mentions: Vec<Mention> = Vec::new();
        for named in body["mentions"].as_array().into_iter().flatten().take(MAX_MENTIONS * 2) {
            let Some(who) = named["who"].as_str() else { continue };
            let allowed = if who == EVERYONE { member.role == Role::Admin } else { self.store.member(who)?.is_some() };
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
        Ok(match self.store.setting("members_may_invite")?.as_deref() {
            Some(set) => set == "true",
            None => self.config.members_may_invite,
        })
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
    fn who(&self, did: &str) -> anyhow::Result<Value> {
        let mut who = json!({ "did": did });
        if let Some(name) = self.store.member(did)?.and_then(|member| member.name) {
            who["name"] = json!(name);
        }
        Ok(who)
    }

    /// The invites `member` may revoke and is told of: all of them for an admin.
    fn invites_body(&self, member: &Member, now: i64) -> anyhow::Result<Value> {
        let by = (member.role != Role::Admin).then_some(member.did.as_str());
        let invites = self.store.invites(by, now)?.iter().map(|invite| self.invite_body(invite)).collect::<anyhow::Result<Vec<_>>>()?;
        Ok(json!({ "invites": invites }))
    }

    /// A `join` from someone who isn't a member: let in, by `admission` and the
    /// invite they bring, or put on the waiting list.
    fn join_from_outside(&self, sender: &str, message: &Value, now: i64) -> anyhow::Result<Outcome> {
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
        if let_in {
            self.store.remove_waiting(sender)?;
            self.store.set_member(sender, Role::Member, now)?;
            self.store.set_joined(sender, true)?;
            let member = self.store.member(sender)?.expect("just let in");
            return Ok(Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?)));
        }
        if self.store.waiting_one(sender)?.is_none() && self.store.waiting_count()? >= self.config.max_waiting as i64 {
            return refuse(message, "e.p.req.waiting-full", "As many are waiting to be let in as can.");
        }
        self.store.add_waiting(sender, invite.as_ref().map(|invite| invite.code.as_str()), now)?;
        Ok(Outcome::reply(reply_to(message, WAITING, json!({}))))
    }

    /// Who is waiting, as an admin is told: `waiting` in `members`.
    fn waiting_body(&self) -> anyhow::Result<Vec<Value>> {
        self.store
            .waiting()?
            .iter()
            .map(|waiting| {
                let mut listed = json!({ "did": waiting.did, "asked_at": waiting.asked_at });
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
            })
            .collect()
    }

    /// `members`, as `asker` is told: with who is waiting for an admin.
    fn members_for(&self, asker: &Member) -> anyhow::Result<Value> {
        let mut body = self.members_body()?;
        if asker.role == Role::Admin {
            body["waiting"] = json!(self.waiting_body()?);
        }
        Ok(body)
    }

    fn members_body(&self) -> anyhow::Result<Value> {
        let members: Vec<Value> = self
            .store
            .members()?
            .iter()
            .map(|member| {
                let mut listed = json!({ "did": member.did, "role": member.role.as_str(), "joined": member.joined });
                if let Some(name) = &member.name {
                    listed["name"] = json!(name);
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

    fn may_manage_channels(&self, member: &Member) -> bool {
        member.role == Role::Admin || self.config.members_may_create_channels
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
                JOIN => self.join_from_outside(sender, message, now),
                LEAVE if self.store.remove_waiting(sender)? => Ok(Outcome::default()),
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

        match message_type {
            JOIN | LEAVE => {
                self.store.set_joined(sender, message_type == JOIN)?;
                let member = Member { joined: message_type == JOIN, ..member };
                Ok(Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?)))
            }

            CREATE_CHANNEL => {
                if !self.may_manage_channels(&member) {
                    return refuse(message, "e.p.trust.not-authorized", "Only an admin can open a channel here.");
                }
                let Some(name) = line(&body["name"], MAX_CHANNEL_NAME) else {
                    return refuse(message, "e.p.msg.empty", "A channel needs a name.");
                };
                if self.store.channel_named(&name)?.is_some() {
                    return refuse(message, "e.p.msg.name-taken", "There is a channel called that already.");
                }
                if self.store.channels()?.iter().filter(|channel| !channel.closed).count() >= self.config.max_channels {
                    return refuse(message, "e.p.msg.too-many", "This community has as many channels as it can hold. Close one first.");
                }
                let id = uuid::Uuid::new_v4().to_string();
                self.store.add_channel(&id, &name, &line(&body["topic"], MAX_TOPIC).unwrap_or_default(), sender, now)?;
                let mut community = self.community(&member)?;
                community["created"] = json!(id);
                Ok(Outcome { reply: Some(reply_to(message, COMMUNITY, community)), channels_changed: true, ..Outcome::default() })
            }

            UPDATE_CHANNEL => {
                let Some(channel) = self.store.channel(body["channel"].as_str().unwrap_or_default())? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.");
                };
                // Whoever opened it may change it, as may an admin.
                if member.role != Role::Admin && channel.created_by != sender {
                    return refuse(message, "e.p.trust.not-authorized", "Only an admin, or whoever opened a channel, can change it.");
                }
                let name = match body.get("name") {
                    Some(name) => match line(name, MAX_CHANNEL_NAME) {
                        Some(name) => name,
                        None => return refuse(message, "e.p.msg.empty", "A channel needs a name."),
                    },
                    None => channel.name.clone(),
                };
                if !name.eq_ignore_ascii_case(&channel.name) && self.store.channel_named(&name)?.is_some() {
                    return refuse(message, "e.p.msg.name-taken", "There is a channel called that already.");
                }
                let topic = match body.get("topic") {
                    Some(topic) => line(topic, MAX_TOPIC).unwrap_or_default(),
                    None => channel.topic.clone(),
                };
                let closed = body["closed"].as_bool().unwrap_or(channel.closed);
                self.store.update_channel(&channel.id, &name, &topic, closed)?;
                Ok(Outcome { reply: Some(reply_to(message, COMMUNITY, self.community(&member)?)), channels_changed: true, ..Outcome::default() })
            }

            POST => {
                let Some(channel) = self.store.channel(body["channel"].as_str().unwrap_or_default())? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.");
                };
                if channel.closed {
                    return refuse(message, "e.p.msg.closed", "That channel is closed.");
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
                let mentions = self.mentions_of(body, &member)?;
                let post = self.store.add_post(&channel.id, &id, content, &member, question.as_ref(), None, now)?;
                let reply = answered.as_ref().map(|post| (post.id.as_str(), post.author_did.as_str(), snippet.as_deref()));
                self.store.set_written_with(&post.id, reply, thread.as_deref(), &mentions)?;
                let mut outcome = Outcome { reply: Some(reply_to(message, POSTED, json!({ "channel": post.channel, "seq": post.seq, "id": post.id }))), posted: true, ..Outcome::default() };
                // The post a thread grew from says how many are in it now.
                if let Some(thread) = thread {
                    self.store.touch_thread(&thread)?;
                    outcome.changed.push(thread);
                }
                Ok(outcome)
            }

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
                if removal && member.role != Role::Admin && !self.store.channel(&post.channel)?.is_some_and(|channel| channel.created_by == sender) {
                    return refuse(message, "e.p.trust.not-authorized", "Only whoever wrote a post takes it back; an admin, or whoever opened the channel, removes it.");
                }
                if post.deleted {
                    return refuse(message, "e.p.msg.deleted", "That post was taken back.");
                }
                if message_type == DELETE {
                    self.store.delete_post(&post.id, removal.then_some(sender))?;
                } else {
                    let content = body["content"].as_str().unwrap_or_default().trim();
                    if content.is_empty() {
                        return refuse(message, "e.p.msg.empty", "A post needs something in it.");
                    }
                    if content.chars().count() > self.config.max_post_length {
                        return refuse(message, "e.p.msg.too-long", "That is longer than a post can be here.");
                    }
                    // Who it calls on is said again with it; left out, it is as it was.
                    let mentions = if body.get("mentions").is_some() { self.mentions_of(body, &member)? } else { post.mentions.clone() };
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
                // Whoever may change the channel says what is pinned in it.
                let opened = self.store.channel(&post.channel)?.is_some_and(|channel| channel.created_by == sender);
                if member.role != Role::Admin && !opened {
                    return refuse(message, "e.p.trust.not-authorized", "Only an admin, or whoever opened the channel, pins a post in it.");
                }
                if post.deleted {
                    return refuse(message, "e.p.msg.deleted", "That post was taken back.");
                }
                let pinned = message_type == PIN;
                if pinned != post.pinned {
                    self.store.set_pinned(&post.id, pinned)?;
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
                if member.role != Role::Admin {
                    return refuse(message, "e.p.trust.not-authorized", "Only an admin decides who is let in.");
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
                    self.store.set_setting("members_may_invite", if may { "true" } else { "false" })?;
                }
                let mut outcome = Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?));
                // Everyone joined is told, as of any change to what the community is.
                outcome.channels_changed = true;
                Ok(outcome)
            }

            CREATE_INVITE => {
                if member.role != Role::Admin && !self.members_may_invite()? {
                    return refuse(message, "e.p.trust.not-authorized", "Only an admin makes invites here.");
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
                let mut answer = self.invites_body(&member, now)?;
                answer["created"] = json!(invite.code);
                Ok(Outcome::reply(reply_to(message, INVITES, answer)))
            }

            REVOKE_INVITE => {
                let code = body["code"].as_str().unwrap_or_default();
                let Some(invite) = self.store.invite(code)? else {
                    return refuse(message, "e.p.msg.unknown-invite", "There is no such invite.");
                };
                if member.role != Role::Admin && invite.created_by != sender {
                    return refuse(message, "e.p.trust.not-authorized", "Only an admin, or whoever made an invite, can revoke it.");
                }
                self.store.revoke_invite(code)?;
                Ok(Outcome::reply(reply_to(message, INVITES, self.invites_body(&member, now)?)))
            }

            INVITES_REQUEST => Ok(Outcome::reply(reply_to(message, INVITES, self.invites_body(&member, now)?))),

            SET_PROFILE => {
                if member.role != Role::Admin {
                    return refuse(message, "e.p.trust.not-authorized", "Only an admin changes what the community is called.");
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
                let mut outcome = Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?));
                // Everyone joined is told what it is called now, as they are of a
                // change to its channels.
                outcome.channels_changed = true;
                outcome.profile_changed = true;
                Ok(outcome)
            }

            SET_MEMBER => {
                if member.role != Role::Admin {
                    return refuse(message, "e.p.trust.not-authorized", "Only an admin decides who is let in.");
                }
                let Some(did) = body["did"].as_str().filter(|did| did.starts_with("did:") && did.len() <= 4096) else {
                    return refuse(message, "e.p.msg.invalid-member", "That isn't a DID.");
                };
                let existing = self.store.member(did)?;
                let was_waiting = existing.is_none() && self.store.waiting_one(did)?.is_some();
                let last_admin = existing.as_ref().is_some_and(|m| m.role == Role::Admin) && self.store.admin_count()? <= 1;
                match body["role"].as_str() {
                    Some(role) => {
                        let Some(role) = Role::parse(role) else {
                            return refuse(message, "e.p.msg.invalid-role", "A member is an admin or a member.");
                        };
                        if last_admin && role != Role::Admin {
                            return refuse(message, "e.p.msg.last-admin", "A community needs an admin: make someone else one first.");
                        }
                        if self.store.is_superseded(did)? {
                            return refuse(message, "e.p.trust.superseded", "That DID has been replaced by another.");
                        }
                        self.store.set_member(did, role, now)?;
                        // Someone let in from the waiting list asked to be sent posts:
                        // they are joined, and told what the community is.
                        if was_waiting {
                            self.store.remove_waiting(did)?;
                            self.store.set_joined(did, true)?;
                            let admitted = self.store.member(did)?.expect("just let in");
                            let mut outcome = Outcome::reply(reply_to(message, MEMBERS, self.members_for(&member)?));
                            outcome.notices.push((did.to_string(), json!({ "type": COMMUNITY, "body": self.community(&admitted)? })));
                            return Ok(outcome);
                        }
                    }
                    None => {
                        if last_admin {
                            return refuse(message, "e.p.msg.last-admin", "A community needs an admin: make someone else one first.");
                        }
                        // Turned away, if they were waiting; taken out, if they were in.
                        self.store.remove_waiting(did)?;
                        self.store.remove_member(did)?;
                    }
                }
                Ok(Outcome::reply(reply_to(message, MEMBERS, self.members_for(&member)?)))
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
