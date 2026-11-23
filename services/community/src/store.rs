//! What the server keeps: who is let in, the channels, and what was said in them.

use serde_json::Value;
use wyvrn_service::db::{Db, Location, Result, Row, Schema, MYSQL_DID, MYSQL_TABLE};
use wyvrn_service::params;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
    Member,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Member => "member",
        }
    }

    pub fn parse(role: &str) -> Option<Self> {
        match role {
            "admin" => Some(Role::Admin),
            "member" => Some(Role::Member),
            _ => None,
        }
    }
}

/// Someone let in.
#[derive(Debug, Clone)]
pub struct Member {
    pub did: String,
    pub role: Role,
    /// What they call themselves (user-profile/1.0), if they have said.
    pub name: Option<String>,
    /// Whether their app has asked to be sent what is posted.
    pub joined: bool,
    pub since: i64,
    /// The last post sent to them, by its serial.
    pub delivered: i64,
    /// When they left, having joined, while they stay away.
    pub left_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub topic: String,
    pub closed: bool,
    pub created_at: i64,
    pub created_by: String,
    /// The number of its newest post; 0 with none.
    pub latest_seq: i64,
    /// A voice channel's settings (community-voice/1.0); `None` for a text channel.
    pub voice: Option<Value>,
    /// Seconds a member waits between posts here; 0 for none.
    pub slow_mode: i64,
    /// A forum's tags, as JSON (`[{ id, name, emoji? }]`); `None` for a channel that isn't one.
    pub forum: Option<Value>,
    /// Whether it is an announcement channel (announcements.rs).
    pub announcement: bool,
    /// Whether it is a gallery: a forum each of whose posts is a picture or video.
    pub gallery: bool,
    /// Whether it is a calendar channel: no posts, the community's events.
    pub calendar: bool,
    /// A form channel's form (forms.rs); `None` for any other.
    pub form: Option<Value>,
    /// Whether it is a wiki (wiki.rs).
    pub wiki: bool,
    /// A forum's sections, its sub-forums (`[{ id, name, description? }]`).
    pub sections: Option<Value>,
    /// Its place in the order an admin set, if one did, and its category.
    pub position: Option<i64>,
    pub category: Option<String>,
}

/// What follows an announcement channel (announcements.rs): another community
/// (`kind` `community`, `target` its DID, `name` its name and channel's), a
/// Discord webhook (`discord`, its URL) or another service (`webhook`, its URL,
/// signed with `secret`).
#[derive(Debug, Clone, PartialEq)]
pub struct Follower {
    pub id: String,
    pub channel: String,
    pub kind: String,
    pub target: String,
    pub name: String,
    pub secret: Option<String>,
    pub created_by: String,
    pub created_at: i64,
    pub failures: i64,
    pub off_at: Option<i64>,
}

/// What a channel here follows: an announcement channel of `source`, the
/// follow's id there (`remote`) once the source has answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Follow {
    pub id: String,
    pub channel: String,
    pub source: String,
    pub name: String,
    pub channel_name: String,
    pub remote: Option<String>,
    pub asked_by: String,
    pub asked_thid: Option<String>,
    pub pending: bool,
    pub created_at: i64,
}

/// A page of a wiki channel (wiki.rs).
#[derive(Debug, Clone, PartialEq)]
pub struct WikiPage {
    pub id: String,
    pub channel: String,
    pub parent: Option<String>,
    pub title: String,
    pub position: i64,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted: bool,
}

/// A comment on a wiki page's text: its anchors (Yjs relative positions, base64),
/// or the comment it answers.
#[derive(Debug, Clone, PartialEq)]
pub struct WikiComment {
    pub id: String,
    pub page: String,
    pub start: Option<String>,
    pub end: Option<String>,
    pub quote: Option<String>,
    pub text: String,
    pub did: String,
    pub at: i64,
    pub reply_to: Option<String>,
    pub resolved: bool,
}

/// An answer to a form channel's form (forms.rs): whose, what, how it stands, and
/// the notes about it (`[{ by, text, at }]`, `by` a DID).
#[derive(Debug, Clone, PartialEq)]
pub struct FormAnswer {
    pub id: String,
    pub channel: String,
    pub did: String,
    pub data: Value,
    pub status: String,
    pub notes: Vec<Value>,
    pub at: i64,
}

/// A community event (events.rs): its fields as JSON, who made it (a DID).
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub id: String,
    pub data: Value,
    pub organizer: String,
    pub sequence: i64,
    pub cancelled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A webhook (`webhooks`): what posts into `channel` as `name`, from whoever
/// holds its id and `token`.
#[derive(Debug, Clone, PartialEq)]
pub struct Webhook {
    pub id: String,
    pub channel: String,
    pub name: String,
    pub avatar: Option<String>,
    pub token: String,
    pub created_by: String,
    pub created_at: i64,
}

/// Where events are sent out to (outgoing.rs): a receiver's URL, the secret each
/// event is signed with, the events it takes (`post.created`, ...), only those
/// of `channel` where one is named, from whoever made it.
#[derive(Debug, Clone)]
pub struct EventHook {
    pub id: String,
    pub url: String,
    pub secret: String,
    pub events: Vec<String>,
    pub channel: Option<String>,
    pub created_by: String,
    pub created_at: i64,
    /// Events in a row it didn't take; past enough, it is turned off (`off_at`).
    pub failures: i64,
    pub off_at: Option<i64>,
}

/// Something made or taken over from a Discord template (`imported`).
#[derive(Debug, Clone)]
pub struct Imported {
    pub local: String,
    pub source_id: String,
    pub source: Value,
    pub applied: Value,
}

/// A named group of channels, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub id: String,
    pub name: String,
    pub position: i64,
}

#[derive(Debug, Clone)]
pub struct Post {
    /// Its place among every post in every channel: the order they are sent out in.
    pub serial: i64,
    pub channel: String,
    /// Its place in its channel, from 1.
    pub seq: i64,
    pub id: String,
    pub content: String,
    pub posted_at: i64,
    pub author_did: String,
    pub author_name: Option<String>,
    /// A question it asks (community/1.0's `question`), if it asks one.
    pub question: Option<Value>,
    /// The answer it is, to another post's question.
    pub answer: Option<Value>,
    /// Who answered the question it asks, once someone has.
    pub answered_by: Option<String>,
    /// Where it stands among every change made here: each post, and each change to
    /// one, takes the next number. 0 for a post from before changes were counted.
    pub rev: i64,
    /// The post it answers, who wrote that, and what it said as this was written.
    pub reply_to: Option<String>,
    pub reply_author: Option<String>,
    pub reply_snippet: Option<String>,
    /// The thread it is in: the id of the post the thread grew from.
    pub thread: Option<String>,
    /// Who it calls on, and what it writes each as.
    pub mentions: Vec<Mention>,
    pub edited_at: Option<i64>,
    /// Taken back, or removed. What it said is gone; its place is kept.
    pub deleted: bool,
    /// Who removed it, when it wasn't its author: an admin, or whoever opened the channel.
    pub removed_by: Option<String>,
    pub pinned: bool,
    /// Each emoji on it, and who put it there, in the order they were first put.
    pub reactions: Vec<(String, Vec<String>)>,
    /// The thread that grew from it, if one did.
    pub grew: Option<Thread>,
    /// The voice channel the post invites people into (community-voice/1.0).
    pub voice_invite: Option<String>,
    /// What else it carries, as JSON: `media` and `attachments`, a `poll`, a
    /// forum thread's `title` and `tags`.
    pub extra: Option<Value>,
    /// The AutoMod rules that censored it, where any did (its original is kept
    /// apart, for moderators: `Store::censored_original`).
    pub censored: Option<Vec<String>>,
    /// In an announcement channel: when it was published, if it was.
    pub published_at: Option<i64>,
    /// Each member's vote, where it is a poll: `(did, answer ids)`.
    pub votes: Vec<(String, Vec<String>)>,
}

/// Someone a post calls on: a member's DID or `everyone`, and the name the post
/// writes them as (`@name`), which is how an app finds them in its text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Mention {
    pub who: String,
    pub name: String,
}

/// The kinds of count kept in `cached_counts`.
pub const COUNT_THREAD: &str = "thread";
pub const COUNT_FORUM_POSTS: &str = "forum_posts";
pub const COUNT_SECTION: &str = "section";
/// Between a forum's id and a section's in a section count's `subject`.
const SECTION_MARK: char = '\u{1f}';
/// A post in a forum that counts towards its author's posts.
const IN_FORUMS: &str = "channel IN (SELECT channel FROM channel_forum) AND channel NOT IN (SELECT channel FROM gallery_channels) \
    AND channel NOT IN (SELECT channel FROM channel_trash)";

fn section_subject(channel: &str, section: Option<&str>) -> String {
    format!("{channel}{SECTION_MARK}{}", section.unwrap_or_default())
}

/// `?1, ?2, ...` for `count` values.
fn marks(count: usize) -> String {
    (1..=count).map(|n| format!("?{n}")).collect::<Vec<_>>().join(", ")
}

/// What is counted of a thread: its posts (taken back ones too: they stay in it
/// as gaps), and its last.
#[derive(Debug, Clone)]
pub struct ThreadTally {
    pub count: i64,
    pub last_at: Option<i64>,
    pub last_by: Option<String>,
}

/// What is counted of a forum's section (`None`: the threads in none).
#[derive(Debug, Clone, PartialEq)]
pub struct SectionTally {
    pub section: Option<String>,
    pub threads: i64,
    pub posts: i64,
    pub last_at: Option<i64>,
    pub last_thread: Option<String>,
}

/// A thread: a side conversation grown from one post of a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// The post it grew from.
    pub id: String,
    pub channel: String,
    pub name: Option<String>,
    pub created_by: String,
    pub created_at: i64,
    /// How many posts are in it.
    pub count: i64,
    /// When its last post was posted.
    pub last_at: Option<i64>,
    /// Who posted last (a DID).
    pub last_by: Option<String>,
    /// Listed first, in a forum.
    pub sticky: bool,
    /// Only those who manage posts post in it.
    pub locked: bool,
    /// The forum section it is in.
    pub section: Option<String>,
    /// Read only by those in it, and whoever manages threads (`private_threads`).
    pub private: bool,
}

/// A role (community/1.0's roles): a name, permissions and a rank, `position`,
/// higher ranking higher. `everyone` is at 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleDef {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub position: i64,
    pub permissions: Vec<String>,
    pub mentionable: bool,
    pub hoist: bool,
    pub self_assignable: bool,
}

/// What a channel allows and denies a role, of the permissions a channel gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Override {
    pub channel: String,
    pub role: String,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

/// An administration workflow under way (workflow/1.0, the server Processor):
/// whose, which template and about what, where it has got to and what it holds.
#[derive(Debug, Clone)]
pub struct Flow {
    pub id: String,
    pub owner: String,
    pub template: String,
    pub context: Value,
    pub state: String,
    pub data: Value,
    /// `active`, `completed` or `canceled`.
    pub status: String,
    pub updated: i64,
}

/// One thing done, as the audit log keeps it.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub id: i64,
    pub at: i64,
    pub actor: String,
    pub action: String,
    pub target: Option<String>,
    pub details: Option<Value>,
}

/// A ban in force: who, by whom, since when, until when (`None`: until lifted).
#[derive(Debug, Clone)]
pub struct Ban {
    pub did: String,
    /// What they were called when banned.
    pub name: Option<String>,
    pub by: String,
    pub at: i64,
    pub until: Option<i64>,
    pub reason: Option<String>,
}

/// A post AutoMod held for a moderator, with what is needed to place it later.
#[derive(Debug, Clone)]
pub struct Held {
    pub id: String,
    pub channel: String,
    pub author: String,
    pub held_at: i64,
    pub rule: String,
    /// The `post` as it was sent.
    pub body: Value,
}

fn ban_from(row: &Row) -> Result<Ban> {
    Ok(Ban { did: row.get(0)?, name: row.get(1)?, by: row.get(2)?, at: row.get(3)?, until: row.get(4)?, reason: row.get(5)? })
}

const ROLE_COLUMNS: &str = "id, name, color, icon, position, permissions, mentionable, hoist, self_assignable";
const FLOW_COLUMNS: &str = "id, owner, template, context, state, data, status, updated";

fn list_from(text: Option<String>) -> Vec<String> {
    text.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn role_from(row: &Row) -> Result<RoleDef> {
    Ok(RoleDef {
        id: row.get(0)?,
        name: row.get(1)?,
        color: row.get(2)?,
        icon: row.get(3)?,
        position: row.get(4)?,
        permissions: list_from(row.get(5)?),
        mentionable: row.get(6)?,
        hoist: row.get(7)?,
        self_assignable: row.get(8)?,
    })
}

fn flow_from(row: &Row) -> Result<Flow> {
    Ok(Flow {
        id: row.get(0)?,
        owner: row.get(1)?,
        template: row.get(2)?,
        context: json_from(row.get(3)?).unwrap_or(Value::Null),
        state: row.get(4)?,
        data: json_from(row.get(5)?).unwrap_or(Value::Null),
        status: row.get(6)?,
        updated: row.get(7)?,
    })
}

/// Someone part-way through answering a question (a workflow/1.0 instance).
#[derive(Debug, Clone)]
pub struct Answering {
    pub id: String,
    pub owner: String,
    /// The post that asks.
    pub post: String,
    /// Which of its questions is being asked now.
    pub at: usize,
    /// What has been answered so far, in order.
    pub answers: Vec<Value>,
    /// `active`, `completed` or `canceled`.
    pub status: String,
    pub updated: i64,
}

/// How long time in voice is kept for insights: 90 days.
pub const VOICE_TIME_KEPT: i64 = 90 * 86_400;

pub struct Store {
    pub(crate) db: Db,
}

/// Not a post brought in from elsewhere (`imported_ranges`).
const NOT_IMPORTED: &str = "NOT EXISTS (SELECT 1 FROM imported_ranges r WHERE r.channel = posts.channel AND posts.seq BETWEEN r.from_seq AND r.to_seq)";

const MEMBER_COLUMNS: &str = "did, role, name, joined, since, delivered, (SELECT at FROM member_left WHERE member_left.did = members.did)";
// A post's author is called what they are called now, where they are still a
// member; what they were called when they wrote it, otherwise.
const POST_COLUMNS: &str = "serial, channel, seq, id, content, posted_at, author_did, \
    COALESCE((SELECT name FROM members WHERE members.did = posts.author_did), author_name), question, answer, answered_by, \
    (SELECT rev FROM post_state WHERE post_state.post = posts.id), \
    (SELECT reply_to FROM post_state WHERE post_state.post = posts.id), \
    (SELECT thread FROM post_state WHERE post_state.post = posts.id), \
    (SELECT mentions FROM post_state WHERE post_state.post = posts.id), \
    (SELECT edited_at FROM post_state WHERE post_state.post = posts.id), \
    (SELECT deleted FROM post_state WHERE post_state.post = posts.id), \
    (SELECT pinned FROM post_state WHERE post_state.post = posts.id), \
    (SELECT reply_author FROM post_state WHERE post_state.post = posts.id), \
    (SELECT reply_snippet FROM post_state WHERE post_state.post = posts.id), \
    (SELECT removed_by FROM post_removals WHERE post_removals.post = posts.id), \
    (SELECT channel FROM post_voice_invites WHERE post_voice_invites.post = posts.id), \
    (SELECT extra FROM post_extra WHERE post_extra.post = posts.id), \
    (SELECT rules FROM post_censored WHERE post_censored.post = posts.id), \
    (SELECT at FROM announce_published WHERE announce_published.post = posts.id)";
/// A channel not in the trash (`channel_trash`), said of `channels`.
const NOT_TRASHED: &str = "NOT EXISTS (SELECT 1 FROM channel_trash WHERE channel_trash.channel = channels.id)";
const CHANNEL_COLUMNS: &str = "id, name, topic, closed, created_at, created_by, (SELECT COALESCE(MAX(seq), 0) FROM posts WHERE posts.channel = channels.id), \
    (SELECT settings FROM channel_voice WHERE channel_voice.channel = channels.id), \
    (SELECT seconds FROM channel_slow WHERE channel_slow.channel = channels.id), \
    (SELECT position FROM channel_order WHERE channel_order.channel = channels.id), \
    (SELECT category FROM channel_order WHERE channel_order.channel = channels.id), \
    (SELECT tags FROM channel_forum WHERE channel_forum.channel = channels.id), \
    (SELECT COUNT(*) FROM announce_channels WHERE announce_channels.channel = channels.id), \
    (SELECT COUNT(*) FROM gallery_channels WHERE gallery_channels.channel = channels.id), \
    (SELECT COUNT(*) FROM calendar_channels WHERE calendar_channels.channel = channels.id), \
    (SELECT form FROM form_channels WHERE form_channels.channel = channels.id), \
    (SELECT COUNT(*) FROM wiki_channels WHERE wiki_channels.channel = channels.id), \
    (SELECT sections FROM forum_sections WHERE forum_sections.channel = channels.id)";

fn member_from(row: &Row) -> Result<Member> {
    let role: String = row.get(1)?;
    Ok(Member {
        did: row.get(0)?,
        role: Role::parse(&role).unwrap_or(Role::Member),
        name: row.get(2)?,
        joined: row.get(3)?,
        since: row.get(4)?,
        delivered: row.get(5)?,
        left_at: row.get(6)?,
    })
}

fn json_from(text: Option<String>) -> Option<Value> {
    text.and_then(|text| serde_json::from_str(&text).ok())
}

fn post_from(row: &Row) -> Result<Post> {
    Ok(Post {
        serial: row.get(0)?,
        channel: row.get(1)?,
        seq: row.get(2)?,
        id: row.get(3)?,
        content: row.get(4)?,
        posted_at: row.get(5)?,
        author_did: row.get(6)?,
        author_name: row.get(7)?,
        question: json_from(row.get(8)?),
        answer: json_from(row.get(9)?),
        answered_by: row.get(10)?,
        rev: row.get::<Option<i64>>(11)?.unwrap_or(0),
        reply_to: row.get(12)?,
        thread: row.get(13)?,
        mentions: row.get::<Option<String>>(14)?.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default(),
        edited_at: row.get(15)?,
        deleted: row.get::<Option<bool>>(16)?.unwrap_or(false),
        pinned: row.get::<Option<bool>>(17)?.unwrap_or(false),
        reply_author: row.get(18)?,
        reply_snippet: row.get(19)?,
        removed_by: row.get(20)?,
        voice_invite: row.get(21)?,
        extra: json_from(row.get(22)?),
        censored: json_from(row.get(23)?).and_then(|rules| serde_json::from_value(rules).ok()),
        published_at: row.get(24)?,
        votes: Vec::new(),
        // Read after, each by a query of its own (`Store::filled`).
        reactions: Vec::new(),
        grew: None,
    })
}

fn channel_from(row: &Row) -> Result<Channel> {
    Ok(Channel {
        id: row.get(0)?,
        name: row.get(1)?,
        topic: row.get(2)?,
        closed: row.get(3)?,
        created_at: row.get(4)?,
        created_by: row.get(5)?,
        latest_seq: row.get(6)?,
        voice: json_from(row.get(7)?),
        slow_mode: row.get::<Option<i64>>(8)?.unwrap_or(0),
        position: row.get(9)?,
        category: row.get(10)?,
        forum: json_from(row.get(11)?),
        announcement: row.get::<Option<i64>>(12)?.unwrap_or(0) > 0,
        gallery: row.get::<Option<i64>>(13)?.unwrap_or(0) > 0,
        calendar: row.get::<Option<i64>>(14)?.unwrap_or(0) > 0,
        form: json_from(row.get(15)?),
        wiki: row.get::<Option<i64>>(16)?.unwrap_or(0) > 0,
        sections: json_from(row.get(17)?),
    })
}

fn answering_from(row: &Row) -> Result<Answering> {
    let at: i64 = row.get(3)?;
    let answers: String = row.get(4)?;
    Ok(Answering {
        id: row.get(0)?,
        owner: row.get(1)?,
        post: row.get(2)?,
        at: at.max(0) as usize,
        answers: serde_json::from_str(&answers).unwrap_or_default(),
        status: row.get(5)?,
        updated: row.get(6)?,
    })
}

const ANSWERING_COLUMNS: &str = "id, owner, post, at, answers, status, updated";

/// Someone who asked to join and hasn't been let in.
#[derive(Debug, Clone)]
pub struct Waiting {
    pub did: String,
    pub name: Option<String>,
    pub asked_at: i64,
    /// The code of the valid invite they came with, if any.
    pub invite: Option<String>,
}

/// An invite: a code that lets in whoever brings it, while it lasts.
#[derive(Debug, Clone)]
pub struct Invite {
    pub code: String,
    /// How many more it may let in; `None` for as many as use it.
    pub uses_left: Option<i64>,
    pub expires_at: Option<i64>,
    pub note: Option<String>,
    pub created_at: i64,
    pub created_by: String,
}

impl Invite {
    /// Whether it would let someone in at `now`.
    pub fn valid(&self, now: i64) -> bool {
        self.uses_left.is_none_or(|left| left > 0) && self.expires_at.is_none_or(|at| at > now)
    }
}

/// What a member told the admins of.
#[derive(Debug, Clone)]
pub struct Report {
    pub id: String,
    pub reported_at: i64,
    pub by: String,
    pub reason: String,
    pub note: Option<String>,
    /// The post reported, by id, and as it stood then.
    pub post_id: Option<String>,
    pub post: Option<Value>,
    /// The member reported.
    pub member: Option<String>,
    pub resolved_at: Option<i64>,
    pub resolved_by: Option<String>,
}

const REPORT_COLUMNS: &str = "id, reported_at, by_did, reason, note, post_id, post, member, resolved_at, resolved_by";

fn report_from(row: &Row) -> Result<Report> {
    Ok(Report {
        id: row.get(0)?,
        reported_at: row.get(1)?,
        by: row.get(2)?,
        reason: row.get(3)?,
        note: row.get(4)?,
        post_id: row.get(5)?,
        post: json_from(row.get(6)?),
        member: row.get(7)?,
        resolved_at: row.get(8)?,
        resolved_by: row.get(9)?,
    })
}

const WAITING_COLUMNS: &str = "did, name, asked_at, invite";
const INVITE_COLUMNS: &str = "code, uses_left, expires_at, note, created_at, created_by";

fn waiting_from(row: &Row) -> Result<Waiting> {
    Ok(Waiting { did: row.get(0)?, name: row.get(1)?, asked_at: row.get(2)?, invite: row.get(3)? })
}

fn invite_from(row: &Row) -> Result<Invite> {
    Ok(Invite { code: row.get(0)?, uses_left: row.get(1)?, expires_at: row.get(2)?, note: row.get(3)?, created_at: row.get(4)?, created_by: row.get(5)? })
}

/// The tables whose answers are kept ([`Store::with_cache`]): small, and read far
/// more often than they change.
const CACHED_TABLES: &[&str] = &[
    "settings",
    "members",
    "member_left",
    "superseded",
    "member_ids",
    "member_profiles",
    "member_pictures",
    "member_here",
    "roles",
    "member_roles",
    "channel_overrides",
    "categories",
    "channel_order",
    "channel_voice",
    "channel_slow",
    "channel_forum",
    "notify_prefs",
    "web_channels",
    "timeouts",
    "rules_accepted",
    "presence",
];

/// How long an answer read from [`CACHED_TABLES`] is kept.
const CACHE_LIFETIME: std::time::Duration = std::time::Duration::from_secs(30);

const SQLITE: &str = "CREATE TABLE IF NOT EXISTS members (
        did TEXT PRIMARY KEY,
        role TEXT NOT NULL,
        name TEXT,
        joined INTEGER NOT NULL DEFAULT 0,
        since INTEGER NOT NULL,
        delivered INTEGER NOT NULL DEFAULT 0
    );
    -- DIDs a member has left (a verified from_prior). Nothing from one is
    -- acted on again: a device removed from an account still has its keys.
    CREATE TABLE IF NOT EXISTS superseded (
        did TEXT PRIMARY KEY,
        became TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS channels (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        topic TEXT NOT NULL DEFAULT '',
        closed INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        created_by TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS posts (
        serial INTEGER PRIMARY KEY AUTOINCREMENT,
        channel TEXT NOT NULL,
        seq INTEGER NOT NULL,
        id TEXT NOT NULL UNIQUE,
        content TEXT NOT NULL,
        posted_at INTEGER NOT NULL,
        author_did TEXT NOT NULL,
        author_name TEXT,
        question TEXT,
        answer TEXT,
        answered_by TEXT,
        UNIQUE (channel, seq)
    );
    -- What has been said of a post since it was written, and where it stands among
    -- every change (rev). Beside posts, not in it: a store from before any of this
    -- gains the table, and a post with no row here is as it was written.
    CREATE TABLE IF NOT EXISTS post_state (
        post TEXT PRIMARY KEY,
        rev INTEGER NOT NULL DEFAULT 0,
        reply_to TEXT,
        reply_author TEXT,
        reply_snippet TEXT,
        thread TEXT,
        mentions TEXT,
        edited_at INTEGER,
        deleted INTEGER NOT NULL DEFAULT 0,
        pinned INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS reactions (
        post TEXT NOT NULL,
        did TEXT NOT NULL,
        emoji TEXT NOT NULL,
        at INTEGER NOT NULL,
        PRIMARY KEY (post, did, emoji)
    );
    -- Who removed a post that wasn't theirs. A table of its own for the reason
    -- post_state is one.
    CREATE TABLE IF NOT EXISTS post_removals (
        post TEXT PRIMARY KEY,
        removed_by TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS threads (
        id TEXT PRIMARY KEY,
        channel TEXT NOT NULL,
        name TEXT,
        created_by TEXT NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS answering (
        id TEXT PRIMARY KEY,
        owner TEXT NOT NULL,
        post TEXT NOT NULL,
        at INTEGER NOT NULL DEFAULT 0,
        answers TEXT NOT NULL DEFAULT '[]',
        status TEXT NOT NULL,
        updated INTEGER NOT NULL
    );
    -- Who asked to join and hasn't been let in (community/1.0's waiting list).
    CREATE TABLE IF NOT EXISTS waiting (
        did TEXT PRIMARY KEY,
        name TEXT,
        asked_at INTEGER NOT NULL,
        invite TEXT
    );
    CREATE TABLE IF NOT EXISTS invites (
        code TEXT PRIMARY KEY,
        uses_left INTEGER,
        expires_at INTEGER,
        note TEXT,
        created_at INTEGER NOT NULL,
        created_by TEXT NOT NULL,
        revoked INTEGER NOT NULL DEFAULT 0
    );
    -- When a member who had joined left. A table of its own for the reason
    -- post_state is one.
    CREATE TABLE IF NOT EXISTS member_left (
        did TEXT PRIMARY KEY,
        at INTEGER NOT NULL
    );
    -- What an admin has set that the configuration otherwise says (admission).
    CREATE TABLE IF NOT EXISTS settings (
        name TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    -- Which channels are voice channels (community-voice/1.0), and their settings
    -- as JSON. A table of its own, as member_left is.
    CREATE TABLE IF NOT EXISTS channel_voice (
        channel TEXT PRIMARY KEY,
        settings TEXT NOT NULL
    );
    -- Whether a member lets voice invitations ring them (community-voice/1.0's
    -- set-preferences); absent means yes.
    CREATE TABLE IF NOT EXISTS voice_prefs (
        did TEXT PRIMARY KEY,
        ring INTEGER NOT NULL
    );
    -- What reaches a member through the mediator while they aren't watching
    -- (community/1.0's set-notifications): all, mentions or nothing. Absent while
    -- their app has never said, and they are sent everything.
    CREATE TABLE IF NOT EXISTS notify_prefs (
        did TEXT PRIMARY KEY,
        level TEXT NOT NULL
    );
    -- The threads each member follows.
    CREATE TABLE IF NOT EXISTS thread_follows (
        did TEXT NOT NULL,
        thread TEXT NOT NULL,
        PRIMARY KEY (did, thread)
    );
    -- A post that invites people into a voice channel, and which channel.
    CREATE TABLE IF NOT EXISTS post_voice_invites (
        post TEXT PRIMARY KEY,
        channel TEXT NOT NULL
    );
    -- The latest version of the rules each has accepted, member or not yet.
    CREATE TABLE IF NOT EXISTS rules_accepted (
        did TEXT PRIMARY KEY,
        version INTEGER NOT NULL
    );
    -- What members told the admins of (community/1.0's reports). `post` is the
    -- post as it stood, as JSON.
    CREATE TABLE IF NOT EXISTS reports (
        id TEXT PRIMARY KEY,
        reported_at INTEGER NOT NULL,
        by_did TEXT NOT NULL,
        reason TEXT NOT NULL,
        note TEXT,
        post_id TEXT,
        post TEXT,
        member TEXT,
        resolved_at INTEGER,
        resolved_by TEXT
    );
    -- Roles (community/1.0's roles and permissions); `permissions` is a JSON list.
    CREATE TABLE IF NOT EXISTS roles (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        color TEXT,
        icon TEXT,
        position INTEGER NOT NULL,
        permissions TEXT NOT NULL,
        mentionable INTEGER NOT NULL DEFAULT 0,
        hoist INTEGER NOT NULL DEFAULT 0,
        self_assignable INTEGER NOT NULL DEFAULT 0
    );
    -- Who holds which role, everyone left out.
    CREATE TABLE IF NOT EXISTS member_roles (
        did TEXT NOT NULL,
        role TEXT NOT NULL,
        PRIMARY KEY (did, role)
    );
    -- What a channel allows and denies a role, each a JSON list.
    CREATE TABLE IF NOT EXISTS channel_overrides (
        channel TEXT NOT NULL,
        role TEXT NOT NULL,
        allow TEXT NOT NULL,
        deny TEXT NOT NULL,
        PRIMARY KEY (channel, role)
    );
    -- The audit log: who did what, kept for as long as the community is.
    CREATE TABLE IF NOT EXISTS audit (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        at INTEGER NOT NULL,
        actor TEXT NOT NULL,
        action TEXT NOT NULL,
        target TEXT,
        details TEXT
    );
    CREATE TABLE IF NOT EXISTS bans (
        did TEXT PRIMARY KEY,
        name TEXT,
        by_did TEXT NOT NULL,
        at INTEGER NOT NULL,
        until INTEGER,
        reason TEXT
    );
    CREATE TABLE IF NOT EXISTS timeouts (
        did TEXT PRIMARY KEY,
        until INTEGER NOT NULL,
        reason TEXT
    );
    -- A text channel's slow mode, in seconds.
    CREATE TABLE IF NOT EXISTS channel_slow (
        channel TEXT PRIMARY KEY,
        seconds INTEGER NOT NULL
    );
    -- AutoMod's rules, each as JSON.
    CREATE TABLE IF NOT EXISTS automod_rules (
        id TEXT PRIMARY KEY,
        position INTEGER NOT NULL,
        rule TEXT NOT NULL
    );
    -- Posts AutoMod held for a moderator.
    CREATE TABLE IF NOT EXISTS held (
        id TEXT PRIMARY KEY,
        channel TEXT NOT NULL,
        author TEXT NOT NULL,
        held_at INTEGER NOT NULL,
        rule TEXT NOT NULL,
        body TEXT NOT NULL
    );
    -- Categories of channels, and each channel's place and category.
    CREATE TABLE IF NOT EXISTS categories (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        position INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS channel_order (
        channel TEXT PRIMARY KEY,
        position INTEGER NOT NULL,
        category TEXT
    );
    -- Webhooks (webhooks.rs): each posts into its channel, as its name, from
    -- whatever holds its URL (its id and token).
    CREATE TABLE IF NOT EXISTS webhooks (
        id TEXT PRIMARY KEY,
        channel TEXT NOT NULL,
        name TEXT NOT NULL,
        avatar TEXT,
        token TEXT NOT NULL,
        created_by TEXT NOT NULL,
        created_at INTEGER NOT NULL
    );
    -- Events sent out (outgoing.rs): where each receiver is, the secret its
    -- events are signed with, which it takes, and whether it is still sent to.
    CREATE TABLE IF NOT EXISTS event_hooks (
        id TEXT PRIMARY KEY,
        url TEXT NOT NULL,
        secret TEXT NOT NULL,
        events TEXT NOT NULL,
        channel TEXT,
        created_by TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        failures INTEGER NOT NULL DEFAULT 0,
        off_at INTEGER
    );
    -- And each event waiting to go, tried again later when its receiver failed.
    CREATE TABLE IF NOT EXISTS event_queue (
        id TEXT PRIMARY KEY,
        hook TEXT NOT NULL,
        body TEXT NOT NULL,
        attempts INTEGER NOT NULL DEFAULT 0,
        next_at INTEGER NOT NULL
    );
    -- What came from a Discord server template (discord.rs): each role, category
    -- and channel made or taken over from one, what the template said of it (its
    -- JSON) and what was set here from that (JSON), to tell on importing again
    -- what changed where.
    CREATE TABLE IF NOT EXISTS imported (
        kind TEXT NOT NULL,
        local TEXT NOT NULL,
        source_id TEXT NOT NULL,
        source TEXT NOT NULL,
        applied TEXT NOT NULL,
        PRIMARY KEY (kind, local)
    );
    -- What AutoMod censored: a post's text as its author wrote it, for moderators,
    -- and the rules (as JSON) that censored it.
    CREATE TABLE IF NOT EXISTS post_censored (
        post TEXT PRIMARY KEY,
        original TEXT NOT NULL,
        rules TEXT NOT NULL
    );
    -- What a deleted post said, for whoever manages posts in its channel to see
    -- (original-request), for DELETED_TEXT_KEPT_FOR after it was deleted.
    CREATE TABLE IF NOT EXISTS post_deleted_text (
        post TEXT PRIMARY KEY,
        content TEXT NOT NULL,
        deleted_at INTEGER NOT NULL
    );
    -- What a post carries beyond its text, as JSON (media, a poll, a forum title and tags).
    -- When the sender's own device says it sent a post (its message's
    -- created_time): kept beside posts.posted_at, the server's own time, which is
    -- the only one sent to anyone (asked 2026-11-17).
    CREATE TABLE IF NOT EXISTS post_sent (
        post TEXT NOT NULL PRIMARY KEY,
        sent_at INTEGER NOT NULL
    );
    -- Announcement channels (announcements.rs): which channels are, the posts
    -- published in them, codes given to follow one, what follows each (another
    -- community, a Discord webhook, another service), what was last sent each of
    -- a post and as what it is known there, and what is waiting to be sent.
    CREATE TABLE IF NOT EXISTS announce_channels (
        channel TEXT PRIMARY KEY
    );
    -- Forums that are galleries: each post a picture or video (community/1.0's
    -- Gallery channels).
    CREATE TABLE IF NOT EXISTS gallery_channels (
        channel TEXT PRIMARY KEY
    );
    -- Events (events.rs): each its fields as JSON, who made it, its sequence and
    -- whether it was cancelled; who answered what; which reminders went; each
    -- member's calendar feed address; and the calendar channels.
    CREATE TABLE IF NOT EXISTS events (
        id TEXT PRIMARY KEY,
        data TEXT NOT NULL,
        organizer TEXT NOT NULL,
        sequence INTEGER NOT NULL,
        cancelled INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS event_rsvps (
        event TEXT NOT NULL,
        did TEXT NOT NULL,
        status TEXT NOT NULL,
        at INTEGER NOT NULL,
        PRIMARY KEY (event, did)
    );
    CREATE TABLE IF NOT EXISTS event_reminders (
        reminder TEXT PRIMARY KEY,
        at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS calendar_feeds (
        did TEXT PRIMARY KEY,
        token TEXT NOT NULL UNIQUE
    );
    CREATE TABLE IF NOT EXISTS calendar_channels (
        channel TEXT PRIMARY KEY
    );
    -- Form channels (forms.rs): each one's form, and the answers to it.
    CREATE TABLE IF NOT EXISTS form_channels (
        channel TEXT PRIMARY KEY,
        form TEXT NOT NULL
    );
    -- Wiki channels (wiki.rs): their pages; each change to a page (a Yjs update,
    -- base64) with who sent it; the document they make now; which Yjs client id
    -- is whose on a page; and the comments on its text.
    CREATE TABLE IF NOT EXISTS wiki_channels (
        channel TEXT PRIMARY KEY
    );
    -- Forums as message boards (forums_board.rs): a forum's sections, a thread's
    -- standing, and each member's signature.
    CREATE TABLE IF NOT EXISTS forum_sections (
        channel TEXT PRIMARY KEY,
        sections TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS thread_flags (
        thread TEXT PRIMARY KEY,
        sticky INTEGER NOT NULL DEFAULT 0,
        locked INTEGER NOT NULL DEFAULT 0,
        section TEXT
    );
    CREATE TABLE IF NOT EXISTS member_signatures (
        did TEXT PRIMARY KEY,
        signature TEXT NOT NULL
    );
    -- Counts kept rather than worked out on every ask (the store's kept counts):
    -- a thread's posts, a member's forum posts, a forum section's threads and
    -- posts. Any row may be deleted at any time; it is worked out again when
    -- next asked for.
    CREATE TABLE IF NOT EXISTS cached_counts (
        kind TEXT NOT NULL,
        subject TEXT NOT NULL,
        n INTEGER NOT NULL,
        m INTEGER,
        at INTEGER,
        ref_id TEXT,
        PRIMARY KEY (kind, subject)
    );
    CREATE TABLE IF NOT EXISTS wiki_pages (
        id TEXT PRIMARY KEY,
        channel TEXT NOT NULL,
        parent TEXT,
        title TEXT NOT NULL,
        position INTEGER NOT NULL,
        created_by TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        deleted INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS wiki_updates (
        page TEXT NOT NULL,
        seq INTEGER NOT NULL,
        data TEXT NOT NULL,
        did TEXT NOT NULL,
        at INTEGER NOT NULL,
        PRIMARY KEY (page, seq)
    );
    -- Which of a page's changes were restores (wiki.rs): each stands as a version
    -- of its own in the history, not run together with the edits around it.
    CREATE TABLE IF NOT EXISTS wiki_restores (
        page TEXT NOT NULL,
        seq INTEGER NOT NULL,
        PRIMARY KEY (page, seq)
    );
    CREATE TABLE IF NOT EXISTS wiki_state (
        page TEXT PRIMARY KEY,
        state TEXT NOT NULL,
        seq INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS wiki_clients (
        page TEXT NOT NULL,
        client INTEGER NOT NULL,
        did TEXT NOT NULL,
        PRIMARY KEY (page, client)
    );
    CREATE TABLE IF NOT EXISTS wiki_comments (
        id TEXT PRIMARY KEY,
        page TEXT NOT NULL,
        start_at TEXT,
        end_at TEXT,
        quote TEXT,
        text TEXT NOT NULL,
        did TEXT NOT NULL,
        at INTEGER NOT NULL,
        reply_to TEXT,
        resolved INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS form_answers (
        id TEXT PRIMARY KEY,
        channel TEXT NOT NULL,
        did TEXT NOT NULL,
        data TEXT NOT NULL,
        status TEXT NOT NULL,
        notes TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS announce_published (
        post TEXT PRIMARY KEY,
        channel TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS follow_codes (
        code TEXT PRIMARY KEY,
        channel TEXT NOT NULL,
        did TEXT NOT NULL,
        expires INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS announce_followers (
        id TEXT PRIMARY KEY,
        channel TEXT NOT NULL,
        kind TEXT NOT NULL,
        target TEXT NOT NULL,
        name TEXT NOT NULL,
        secret TEXT,
        created_by TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        failures INTEGER NOT NULL DEFAULT 0,
        off_at INTEGER
    );
    CREATE TABLE IF NOT EXISTS announce_copies (
        follower TEXT NOT NULL,
        post TEXT NOT NULL,
        remote TEXT,
        sent TEXT NOT NULL,
        PRIMARY KEY (follower, post)
    );
    CREATE TABLE IF NOT EXISTS announce_queue (
        follower TEXT NOT NULL,
        post TEXT NOT NULL,
        attempts INTEGER NOT NULL DEFAULT 0,
        next_at INTEGER NOT NULL,
        PRIMARY KEY (follower, post)
    );
    -- And the other side: what this community's channels follow, asked for
    -- (`pending`, until the source answers) or held, and the posts copied in.
    CREATE TABLE IF NOT EXISTS follows (
        id TEXT PRIMARY KEY,
        channel TEXT NOT NULL,
        source TEXT NOT NULL,
        name TEXT NOT NULL,
        channel_name TEXT NOT NULL,
        remote TEXT,
        asked_by TEXT NOT NULL,
        asked_thid TEXT,
        pending INTEGER NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS followed_posts (
        follow TEXT NOT NULL,
        source_post TEXT NOT NULL,
        post TEXT NOT NULL,
        PRIMARY KEY (follow, source_post)
    );
    CREATE TABLE IF NOT EXISTS post_extra (
        post TEXT PRIMARY KEY,
        extra TEXT NOT NULL
    );
    -- Posts brought in from elsewhere (a Discord server's history): each run's
    -- numbers in a channel. Read by time among the rest, never sent out as new,
    -- and not a hole for an app to fill.
    CREATE TABLE IF NOT EXISTS imported_ranges (
        channel TEXT NOT NULL,
        from_seq INTEGER NOT NULL,
        to_seq INTEGER NOT NULL,
        at INTEGER NOT NULL,
        PRIMARY KEY (channel, from_seq)
    );
    -- Private threads (community/1.0's `private`), and who is in each: only they,
    -- and whoever may manage threads in its channel, read it.
    CREATE TABLE IF NOT EXISTS private_threads (
        thread TEXT PRIMARY KEY
    );
    CREATE TABLE IF NOT EXISTS thread_members (
        thread TEXT NOT NULL,
        did TEXT NOT NULL,
        added_by TEXT NOT NULL,
        at INTEGER NOT NULL,
        PRIMARY KEY (thread, did)
    );
    -- Linked accounts a member's profile was checked to prove, and those banned
    -- with someone (protection.rs).
    CREATE TABLE IF NOT EXISTS member_accounts (
        did TEXT NOT NULL,
        service TEXT NOT NULL,
        account TEXT NOT NULL,
        at INTEGER NOT NULL,
        PRIMARY KEY (did, service, account)
    );
    CREATE TABLE IF NOT EXISTS banned_accounts (
        service TEXT NOT NULL,
        account TEXT NOT NULL,
        banned_did TEXT NOT NULL,
        until INTEGER,
        at INTEGER NOT NULL,
        PRIMARY KEY (service, account)
    );
    -- Members that are bots, and their commands (bots.rs).
    CREATE TABLE IF NOT EXISTS member_bots (
        did TEXT PRIMARY KEY,
        commands TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    -- Bots let be bots, and by whom (bots.rs, WYV-89): a profile's bot: true takes
    -- effect only once someone who may manage roles approves it.
    CREATE TABLE IF NOT EXISTS bot_approvals (
        did TEXT PRIMARY KEY,
        by_did TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    -- What a ban says to whoever it bans, beside the ban (appeals.rs): whether its
    -- reason is shown them, whether appeals are closed, and whether it was the
    -- server's own (an account match).
    CREATE TABLE IF NOT EXISTS ban_terms (
        did TEXT PRIMARY KEY,
        shared INTEGER NOT NULL DEFAULT 0,
        closed INTEGER NOT NULL DEFAULT 0,
        auto INTEGER NOT NULL DEFAULT 0
    );
    -- Appeals against bans, kept after they are decided (appeals.rs).
    CREATE TABLE IF NOT EXISTS ban_appeals (
        id TEXT PRIMARY KEY,
        did TEXT NOT NULL,
        ban_at INTEGER NOT NULL,
        text TEXT NOT NULL,
        at INTEGER NOT NULL,
        state TEXT NOT NULL,
        decided_by TEXT,
        decided_at INTEGER,
        reply TEXT
    );
    -- Imported posts claimed by whoever wrote them (claims.rs).
    CREATE TABLE IF NOT EXISTS discord_claims (
        discord_id TEXT PRIMARY KEY,
        did TEXT NOT NULL,
        state TEXT NOT NULL,
        at INTEGER NOT NULL,
        decided_by TEXT
    );
    -- Each member's vote on a poll, and the polls whose end everyone has been told of.
    CREATE TABLE IF NOT EXISTS votes (
        post TEXT NOT NULL,
        did TEXT NOT NULL,
        answers TEXT NOT NULL,
        at INTEGER NOT NULL,
        PRIMARY KEY (post, did)
    );
    CREATE TABLE IF NOT EXISTS polls_ended (
        post TEXT PRIMARY KEY
    );
    -- Forums, and their tags as JSON.
    CREATE TABLE IF NOT EXISTS channel_forum (
        channel TEXT PRIMARY KEY,
        tags TEXT NOT NULL
    );
    -- Who chose to be seen online, and as what.
    CREATE TABLE IF NOT EXISTS presence (
        did TEXT PRIMARY KEY,
        share INTEGER NOT NULL,
        status TEXT NOT NULL
    );
    -- Who has been through onboarding.
    CREATE TABLE IF NOT EXISTS onboarded (
        did TEXT PRIMARY KEY,
        at INTEGER NOT NULL
    );
    -- Each coming in and leaving, for insights: `joined` or `left`.
    CREATE TABLE IF NOT EXISTS member_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        kind TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    -- Time spent in voice channels, a day at a time, for insights: sampled as the
    -- rooms are swept, kept for VOICE_TIME_KEPT.
    CREATE TABLE IF NOT EXISTS voice_time (
        day INTEGER NOT NULL,
        channel TEXT NOT NULL,
        did TEXT NOT NULL,
        seconds INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (day, channel, did)
    );
    -- Answers to the questions on joining, as JSON.
    CREATE TABLE IF NOT EXISTS join_answers (
        did TEXT PRIMARY KEY,
        answers TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    -- Administration workflows under way.
    CREATE TABLE IF NOT EXISTS flows (
        id TEXT PRIMARY KEY,
        owner TEXT NOT NULL,
        template TEXT NOT NULL,
        context TEXT NOT NULL,
        state TEXT NOT NULL,
        data TEXT NOT NULL,
        status TEXT NOT NULL,
        updated INTEGER NOT NULL
    );
    -- The web view (web/). A member's lasting key, which follows changes of DID:
    -- what sessions, consents and the identity provider's ids are kept under.
    CREATE TABLE IF NOT EXISTS member_keys (
        did TEXT PRIMARY KEY,
        mkey TEXT NOT NULL UNIQUE
    );
    -- The picture a member's profile carries (user-profile/1.0), shown beside
    -- their posts on the web.
    CREATE TABLE IF NOT EXISTS member_pictures (
        did TEXT PRIMARY KEY,
        media_type TEXT NOT NULL,
        data TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    -- Channels made public on the web, and from when.
    CREATE TABLE IF NOT EXISTS web_channels (
        channel TEXT PRIMARY KEY,
        since INTEGER NOT NULL
    );
    -- Channels deleted, kept 30 days before they go for good (a channel here is
    -- listed to nobody but in the trash).
    CREATE TABLE IF NOT EXISTS channel_trash (
        channel TEXT PRIMARY KEY,
        trashed_at INTEGER NOT NULL,
        trashed_by TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS web_sessions (
        token TEXT PRIMARY KEY,
        mkey TEXT NOT NULL,
        csrf TEXT NOT NULL,
        created INTEGER NOT NULL,
        expires INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS web_pictures (
        token TEXT PRIMARY KEY,
        mkey TEXT NOT NULL
    );
    -- Pack files (custom emoji, stickers) a public page shows, fetched once and
    -- kept by the start of their hash (web/outside.rs).
    CREATE TABLE IF NOT EXISTS web_outside (
        stem TEXT PRIMARY KEY,
        media_type TEXT NOT NULL,
        data TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    -- What the links in public posts come to (web/preview.rs), by their address's hash.
    CREATE TABLE IF NOT EXISTS web_previews (
        link TEXT PRIMARY KEY,
        url TEXT NOT NULL,
        data TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    -- The identity provider (community-sign-in/1.0).
    CREATE TABLE IF NOT EXISTS oidc_clients (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        name TEXT NOT NULL,
        secret TEXT,
        redirect_uris TEXT NOT NULL,
        created INTEGER NOT NULL,
        created_by TEXT
    );
    CREATE TABLE IF NOT EXISTS oidc_consents (
        mkey TEXT NOT NULL,
        client TEXT NOT NULL,
        scopes TEXT NOT NULL,
        at INTEGER NOT NULL,
        PRIMARY KEY (mkey, client)
    );
    CREATE TABLE IF NOT EXISTS oidc_codes (
        code TEXT PRIMARY KEY,
        mkey TEXT NOT NULL,
        client TEXT NOT NULL,
        redirect_uri TEXT NOT NULL,
        scopes TEXT NOT NULL,
        nonce TEXT,
        challenge TEXT NOT NULL,
        expires INTEGER NOT NULL,
        used INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS oidc_tokens (
        token TEXT PRIMARY KEY,
        mkey TEXT NOT NULL,
        client TEXT NOT NULL,
        scopes TEXT NOT NULL,
        code TEXT,
        expires INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS oidc_keys (
        kid TEXT PRIMARY KEY,
        jwk TEXT NOT NULL,
        created INTEGER NOT NULL
    );
    -- Who members are to each other (community/1.0): each person's member id,
    -- kept under the DID it was made for. A DID they have since left keeps its
    -- row, so an id given out before still finds them (through superseded).
    CREATE TABLE IF NOT EXISTS member_ids (
        did TEXT PRIMARY KEY,
        member TEXT NOT NULL UNIQUE,
        created INTEGER NOT NULL
    );
    -- The rest of a member's profile, beside their name (members) and picture
    -- (member_pictures): what everyone else in the community is told.
    CREATE TABLE IF NOT EXISTS member_profiles (
        did TEXT PRIMARY KEY,
        description TEXT,
        banner_type TEXT,
        banner_data TEXT,
        linked TEXT,
        public_address TEXT,
        at INTEGER NOT NULL
    );
    -- What a member is in this community alone (set-member-profile).
    CREATE TABLE IF NOT EXISTS member_here (
        did TEXT PRIMARY KEY,
        nickname TEXT,
        picture_type TEXT,
        picture_data TEXT,
        requests INTEGER NOT NULL DEFAULT 1,
        at INTEGER NOT NULL
    );
    -- Message requests passed on, one a day from a member to the same member.
    CREATE TABLE IF NOT EXISTS message_requests (
        from_did TEXT NOT NULL,
        to_did TEXT NOT NULL,
        at INTEGER NOT NULL,
        PRIMARY KEY (from_did, to_did)
    );
    -- What rows are found by, beyond each table's key: kept small on a community
    -- of a few dozen, and what keeps a big one from reading a whole table to
    -- answer (a thread's posts, what changed since, a member's posts, …). Made
    -- on a store from before them when it is next opened.
    CREATE INDEX IF NOT EXISTS post_state_by_thread ON post_state (thread);
    CREATE INDEX IF NOT EXISTS post_state_by_rev ON post_state (rev);
    CREATE INDEX IF NOT EXISTS post_state_by_reply ON post_state (reply_to);
    CREATE INDEX IF NOT EXISTS posts_by_author ON posts (author_did, posted_at);
    CREATE INDEX IF NOT EXISTS posts_by_answerer ON posts (answered_by);
    CREATE INDEX IF NOT EXISTS posts_by_time ON posts (posted_at);
    CREATE INDEX IF NOT EXISTS posts_by_channel_time ON posts (channel, posted_at, seq);
    CREATE INDEX IF NOT EXISTS reactions_by_time ON reactions (at);
    CREATE INDEX IF NOT EXISTS threads_by_channel ON threads (channel);
    CREATE INDEX IF NOT EXISTS answering_by_owner ON answering (owner, post);
    CREATE INDEX IF NOT EXISTS flows_by_owner ON flows (owner);
    CREATE INDEX IF NOT EXISTS message_requests_by_to ON message_requests (to_did);
    CREATE INDEX IF NOT EXISTS form_answers_by_channel ON form_answers (channel, at);
    CREATE INDEX IF NOT EXISTS wiki_pages_by_channel ON wiki_pages (channel);
    CREATE INDEX IF NOT EXISTS wiki_comments_by_page ON wiki_comments (page, at);
    CREATE INDEX IF NOT EXISTS announce_followers_by_channel ON announce_followers (channel);
    CREATE INDEX IF NOT EXISTS announce_queue_by_due ON announce_queue (next_at);
    CREATE INDEX IF NOT EXISTS event_queue_by_due ON event_queue (next_at)";

fn schema() -> Schema {
    static MYSQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let mysql = MYSQL.get_or_init(|| {
        format!(
            "CREATE TABLE IF NOT EXISTS members (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                role VARCHAR(16) NOT NULL,
                name TEXT,
                joined BIGINT NOT NULL DEFAULT 0,
                since BIGINT NOT NULL,
                delivered BIGINT NOT NULL DEFAULT 0
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS superseded (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                became {MYSQL_DID} NOT NULL,
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS channels (
                id VARCHAR(255) NOT NULL PRIMARY KEY,
                name TEXT NOT NULL,
                topic TEXT NOT NULL,
                closed BIGINT NOT NULL DEFAULT 0,
                created_at BIGINT NOT NULL,
                created_by {MYSQL_DID} NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS posts (
                serial BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
                channel VARCHAR(255) NOT NULL,
                seq BIGINT NOT NULL,
                id VARCHAR(255) NOT NULL UNIQUE,
                content MEDIUMTEXT NOT NULL,
                posted_at BIGINT NOT NULL,
                author_did {MYSQL_DID} NOT NULL,
                author_name TEXT,
                question MEDIUMTEXT,
                answer MEDIUMTEXT,
                answered_by {MYSQL_DID},
                UNIQUE (channel, seq)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS post_state (
                post VARCHAR(255) NOT NULL PRIMARY KEY,
                rev BIGINT NOT NULL DEFAULT 0,
                reply_to VARCHAR(255),
                reply_author {MYSQL_DID},
                reply_snippet TEXT,
                thread VARCHAR(255),
                mentions MEDIUMTEXT,
                edited_at BIGINT,
                deleted BIGINT NOT NULL DEFAULT 0,
                pinned BIGINT NOT NULL DEFAULT 0
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS reactions (
                post VARCHAR(255) NOT NULL,
                did {MYSQL_DID} NOT NULL,
                emoji VARCHAR(200) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL,
                at BIGINT NOT NULL,
                PRIMARY KEY (post, did(255), emoji)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS threads (
                id VARCHAR(255) NOT NULL PRIMARY KEY,
                channel VARCHAR(255) NOT NULL,
                name TEXT,
                created_by {MYSQL_DID} NOT NULL,
                created_at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS post_removals (
                post VARCHAR(255) NOT NULL PRIMARY KEY,
                removed_by {MYSQL_DID} NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS answering (
                id VARCHAR(255) NOT NULL PRIMARY KEY,
                owner {MYSQL_DID} NOT NULL,
                post VARCHAR(255) NOT NULL,
                at BIGINT NOT NULL DEFAULT 0,
                answers MEDIUMTEXT NOT NULL,
                status VARCHAR(32) NOT NULL,
                updated BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS waiting (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                name TEXT,
                asked_at BIGINT NOT NULL,
                invite VARCHAR(255)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS invites (
                code VARCHAR(255) NOT NULL PRIMARY KEY,
                uses_left BIGINT,
                expires_at BIGINT,
                note TEXT,
                created_at BIGINT NOT NULL,
                created_by {MYSQL_DID} NOT NULL,
                revoked BIGINT NOT NULL DEFAULT 0
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS member_left (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS settings (
                name VARCHAR(64) NOT NULL PRIMARY KEY,
                value MEDIUMTEXT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS channel_voice (
                channel VARCHAR(255) NOT NULL PRIMARY KEY,
                settings TEXT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS voice_prefs (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                ring BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS notify_prefs (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                level VARCHAR(16) NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS thread_follows (
                did {MYSQL_DID} NOT NULL,
                thread VARCHAR(255) NOT NULL,
                PRIMARY KEY (did(255), thread)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS post_voice_invites (
                post VARCHAR(255) NOT NULL PRIMARY KEY,
                channel VARCHAR(255) NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS rules_accepted (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                version BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS reports (
                id VARCHAR(255) NOT NULL PRIMARY KEY,
                reported_at BIGINT NOT NULL,
                by_did {MYSQL_DID} NOT NULL,
                reason VARCHAR(32) NOT NULL,
                note TEXT,
                post_id VARCHAR(255),
                post MEDIUMTEXT,
                member {MYSQL_DID},
                resolved_at BIGINT,
                resolved_by {MYSQL_DID}
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS roles (
                id VARCHAR(64) NOT NULL PRIMARY KEY,
                name TEXT NOT NULL,
                color VARCHAR(16),
                icon VARCHAR(64) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin,
                position BIGINT NOT NULL,
                permissions TEXT NOT NULL,
                mentionable BIGINT NOT NULL DEFAULT 0,
                hoist BIGINT NOT NULL DEFAULT 0,
                self_assignable BIGINT NOT NULL DEFAULT 0
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS member_roles (
                did {MYSQL_DID} NOT NULL,
                role VARCHAR(64) NOT NULL,
                PRIMARY KEY (did(255), role)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS channel_overrides (
                channel VARCHAR(255) NOT NULL,
                role VARCHAR(64) NOT NULL,
                allow TEXT NOT NULL,
                deny TEXT NOT NULL,
                PRIMARY KEY (channel, role)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS audit (
                id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
                at BIGINT NOT NULL,
                actor {MYSQL_DID} NOT NULL,
                action VARCHAR(64) NOT NULL,
                target TEXT,
                details MEDIUMTEXT
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS bans (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                name TEXT,
                by_did {MYSQL_DID} NOT NULL,
                at BIGINT NOT NULL,
                until BIGINT,
                reason TEXT
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS timeouts (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                until BIGINT NOT NULL,
                reason TEXT
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS channel_slow (
                channel VARCHAR(255) NOT NULL PRIMARY KEY,
                seconds BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS automod_rules (
                id VARCHAR(64) NOT NULL PRIMARY KEY,
                position BIGINT NOT NULL,
                rule MEDIUMTEXT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS held (
                id VARCHAR(255) NOT NULL PRIMARY KEY,
                channel VARCHAR(255) NOT NULL,
                author {MYSQL_DID} NOT NULL,
                held_at BIGINT NOT NULL,
                rule VARCHAR(64) NOT NULL,
                body MEDIUMTEXT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS categories (
                id VARCHAR(64) NOT NULL PRIMARY KEY,
                name TEXT NOT NULL,
                position BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS channel_order (
                channel VARCHAR(255) NOT NULL PRIMARY KEY,
                position BIGINT NOT NULL,
                category VARCHAR(64)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS post_sent (
                post VARCHAR(255) NOT NULL PRIMARY KEY,
                sent_at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS announce_channels (
                channel VARCHAR(255) NOT NULL PRIMARY KEY
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS gallery_channels (
                channel VARCHAR(255) NOT NULL PRIMARY KEY
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS events (
                id VARCHAR(64) NOT NULL PRIMARY KEY,
                data MEDIUMTEXT NOT NULL,
                organizer VARCHAR(3000) NOT NULL,
                sequence BIGINT NOT NULL,
                cancelled BIGINT NOT NULL DEFAULT 0,
                created_at BIGINT NOT NULL,
                updated_at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS event_rsvps (
                event VARCHAR(64) NOT NULL,
                did {MYSQL_DID} NOT NULL,
                status VARCHAR(16) NOT NULL,
                at BIGINT NOT NULL,
                PRIMARY KEY (event, did(255))
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS event_reminders (
                reminder VARCHAR(128) NOT NULL PRIMARY KEY,
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS calendar_feeds (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                token VARCHAR(64) NOT NULL UNIQUE
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS calendar_channels (
                channel VARCHAR(255) NOT NULL PRIMARY KEY
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS form_channels (
                channel VARCHAR(255) NOT NULL PRIMARY KEY,
                form MEDIUMTEXT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS wiki_channels (
                channel VARCHAR(255) NOT NULL PRIMARY KEY
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS forum_sections (
                channel VARCHAR(255) NOT NULL PRIMARY KEY,
                sections MEDIUMTEXT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS thread_flags (
                thread VARCHAR(255) NOT NULL PRIMARY KEY,
                sticky BIGINT NOT NULL DEFAULT 0,
                locked BIGINT NOT NULL DEFAULT 0,
                section VARCHAR(64)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS member_signatures (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                signature TEXT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS cached_counts (
                kind VARCHAR(32) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
                subject {MYSQL_DID} NOT NULL,
                n BIGINT NOT NULL,
                m BIGINT,
                at BIGINT,
                ref_id {MYSQL_DID},
                PRIMARY KEY (kind, subject)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS wiki_pages (
                id VARCHAR(64) NOT NULL PRIMARY KEY,
                channel VARCHAR(255) NOT NULL,
                parent VARCHAR(64),
                title VARCHAR(255) NOT NULL,
                position BIGINT NOT NULL,
                created_by VARCHAR(3000) NOT NULL,
                created_at BIGINT NOT NULL,
                updated_at BIGINT NOT NULL,
                deleted BIGINT NOT NULL DEFAULT 0
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS wiki_updates (
                page VARCHAR(64) NOT NULL,
                seq BIGINT NOT NULL,
                data MEDIUMTEXT NOT NULL,
                did VARCHAR(3000) NOT NULL,
                at BIGINT NOT NULL,
                PRIMARY KEY (page, seq)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS wiki_restores (
                page VARCHAR(64) NOT NULL,
                seq BIGINT NOT NULL,
                PRIMARY KEY (page, seq)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS wiki_state (
                page VARCHAR(64) NOT NULL PRIMARY KEY,
                state LONGTEXT NOT NULL,
                seq BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS wiki_clients (
                page VARCHAR(64) NOT NULL,
                client BIGINT NOT NULL,
                did VARCHAR(3000) NOT NULL,
                PRIMARY KEY (page, client)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS wiki_comments (
                id VARCHAR(64) NOT NULL PRIMARY KEY,
                page VARCHAR(64) NOT NULL,
                start_at VARCHAR(600),
                end_at VARCHAR(600),
                quote VARCHAR(1000),
                text TEXT NOT NULL,
                did VARCHAR(3000) NOT NULL,
                at BIGINT NOT NULL,
                reply_to VARCHAR(64),
                resolved BIGINT NOT NULL DEFAULT 0
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS form_answers (
                id VARCHAR(64) NOT NULL PRIMARY KEY,
                channel VARCHAR(255) NOT NULL,
                did {MYSQL_DID} NOT NULL,
                data MEDIUMTEXT NOT NULL,
                status VARCHAR(16) NOT NULL,
                notes MEDIUMTEXT NOT NULL,
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS announce_published (
                post VARCHAR(255) NOT NULL PRIMARY KEY,
                channel VARCHAR(255) NOT NULL,
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS follow_codes (
                code VARCHAR(64) NOT NULL PRIMARY KEY,
                channel VARCHAR(255) NOT NULL,
                did {MYSQL_DID} NOT NULL,
                expires BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS announce_followers (
                id VARCHAR(64) NOT NULL PRIMARY KEY,
                channel VARCHAR(255) NOT NULL,
                kind VARCHAR(16) NOT NULL,
                target TEXT NOT NULL,
                name VARCHAR(255) NOT NULL,
                secret TEXT,
                created_by VARCHAR(3000) NOT NULL,
                created_at BIGINT NOT NULL,
                failures BIGINT NOT NULL DEFAULT 0,
                off_at BIGINT
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS announce_copies (
                follower VARCHAR(64) NOT NULL,
                post VARCHAR(255) NOT NULL,
                remote VARCHAR(255),
                sent VARCHAR(64) NOT NULL,
                PRIMARY KEY (follower, post)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS announce_queue (
                follower VARCHAR(64) NOT NULL,
                post VARCHAR(255) NOT NULL,
                attempts BIGINT NOT NULL DEFAULT 0,
                next_at BIGINT NOT NULL,
                PRIMARY KEY (follower, post)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS follows (
                id VARCHAR(64) NOT NULL PRIMARY KEY,
                channel VARCHAR(255) NOT NULL,
                source {MYSQL_DID} NOT NULL,
                name VARCHAR(255) NOT NULL,
                channel_name VARCHAR(255) NOT NULL,
                remote VARCHAR(64),
                asked_by VARCHAR(3000) NOT NULL,
                asked_thid VARCHAR(255),
                pending BIGINT NOT NULL,
                created_at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS followed_posts (
                follow VARCHAR(64) NOT NULL,
                source_post VARCHAR(255) NOT NULL,
                post VARCHAR(255) NOT NULL,
                PRIMARY KEY (follow, source_post)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS post_extra (
                post VARCHAR(255) NOT NULL PRIMARY KEY,
                extra MEDIUMTEXT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS imported_ranges (
                channel VARCHAR(255) NOT NULL,
                from_seq BIGINT NOT NULL,
                to_seq BIGINT NOT NULL,
                at BIGINT NOT NULL,
                PRIMARY KEY (channel, from_seq)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS member_accounts (
                did {MYSQL_DID} NOT NULL,
                service VARCHAR(255) NOT NULL,
                account VARCHAR(255) NOT NULL,
                at BIGINT NOT NULL,
                PRIMARY KEY (did(255), service(100), account(100))
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS banned_accounts (
                service VARCHAR(255) NOT NULL,
                account VARCHAR(255) NOT NULL,
                banned_did {MYSQL_DID} NOT NULL,
                until BIGINT,
                at BIGINT NOT NULL,
                PRIMARY KEY (service(150), account(150))
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS member_bots (
                did {MYSQL_DID} NOT NULL,
                commands MEDIUMTEXT NOT NULL,
                at BIGINT NOT NULL,
                PRIMARY KEY (did(255))
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS bot_approvals (
                did {MYSQL_DID} NOT NULL,
                by_did {MYSQL_DID} NOT NULL,
                at BIGINT NOT NULL,
                PRIMARY KEY (did(255))
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS ban_terms (
                did {MYSQL_DID} NOT NULL,
                shared BIGINT NOT NULL DEFAULT 0,
                closed BIGINT NOT NULL DEFAULT 0,
                auto BIGINT NOT NULL DEFAULT 0,
                PRIMARY KEY (did(255))
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS ban_appeals (
                id VARCHAR(255) NOT NULL PRIMARY KEY,
                did {MYSQL_DID} NOT NULL,
                ban_at BIGINT NOT NULL,
                text MEDIUMTEXT NOT NULL,
                at BIGINT NOT NULL,
                state VARCHAR(16) NOT NULL,
                decided_by {MYSQL_DID},
                decided_at BIGINT,
                reply MEDIUMTEXT
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS private_threads (
                thread VARCHAR(255) NOT NULL PRIMARY KEY
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS thread_members (
                thread VARCHAR(255) NOT NULL,
                did {MYSQL_DID} NOT NULL,
                added_by {MYSQL_DID} NOT NULL,
                at BIGINT NOT NULL,
                PRIMARY KEY (thread, did(255))
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS discord_claims (
                discord_id VARCHAR(64) NOT NULL PRIMARY KEY,
                did {MYSQL_DID} NOT NULL,
                state VARCHAR(16) NOT NULL,
                at BIGINT NOT NULL,
                decided_by {MYSQL_DID}
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS webhooks (
                id VARCHAR(64) NOT NULL PRIMARY KEY,
                channel VARCHAR(255) NOT NULL,
                name VARCHAR(255) NOT NULL,
                avatar TEXT,
                token VARCHAR(128) NOT NULL,
                created_by VARCHAR(3000) NOT NULL,
                created_at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS event_hooks (
                id VARCHAR(64) NOT NULL PRIMARY KEY,
                url TEXT NOT NULL,
                secret TEXT NOT NULL,
                events TEXT NOT NULL,
                channel VARCHAR(255),
                created_by VARCHAR(3000) NOT NULL,
                created_at BIGINT NOT NULL,
                failures BIGINT NOT NULL DEFAULT 0,
                off_at BIGINT
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS event_queue (
                id VARCHAR(64) NOT NULL PRIMARY KEY,
                hook VARCHAR(64) NOT NULL,
                body MEDIUMTEXT NOT NULL,
                attempts BIGINT NOT NULL DEFAULT 0,
                next_at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS imported (
                kind VARCHAR(32) NOT NULL,
                local VARCHAR(255) NOT NULL,
                source_id VARCHAR(64) NOT NULL,
                source MEDIUMTEXT NOT NULL,
                applied MEDIUMTEXT NOT NULL,
                PRIMARY KEY (kind, local)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS post_censored (
                post VARCHAR(255) NOT NULL PRIMARY KEY,
                original MEDIUMTEXT NOT NULL,
                rules TEXT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS post_deleted_text (
                post VARCHAR(255) NOT NULL PRIMARY KEY,
                content MEDIUMTEXT NOT NULL,
                deleted_at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS votes (
                post VARCHAR(255) NOT NULL,
                did {MYSQL_DID} NOT NULL,
                answers TEXT NOT NULL,
                at BIGINT NOT NULL,
                PRIMARY KEY (post, did(255))
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS polls_ended (
                post VARCHAR(255) NOT NULL PRIMARY KEY
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS channel_forum (
                channel VARCHAR(255) NOT NULL PRIMARY KEY,
                tags TEXT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS presence (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                share BIGINT NOT NULL,
                status VARCHAR(16) NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS onboarded (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS member_events (
                id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
                kind VARCHAR(16) NOT NULL,
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS voice_time (
                day BIGINT NOT NULL,
                channel VARCHAR(255) NOT NULL,
                did {MYSQL_DID} NOT NULL,
                seconds BIGINT NOT NULL DEFAULT 0,
                PRIMARY KEY (day, channel, did(255))
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS join_answers (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                answers MEDIUMTEXT NOT NULL,
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS flows (
                id VARCHAR(255) NOT NULL PRIMARY KEY,
                owner {MYSQL_DID} NOT NULL,
                template VARCHAR(64) NOT NULL,
                context MEDIUMTEXT NOT NULL,
                state VARCHAR(64) NOT NULL,
                data MEDIUMTEXT NOT NULL,
                status VARCHAR(32) NOT NULL,
                updated BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS member_keys (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                mkey VARCHAR(64) NOT NULL UNIQUE
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS member_pictures (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                media_type VARCHAR(64) NOT NULL,
                data MEDIUMTEXT NOT NULL,
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS web_channels (
                channel VARCHAR(255) NOT NULL PRIMARY KEY,
                since BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS channel_trash (
                channel VARCHAR(255) NOT NULL PRIMARY KEY,
                trashed_at BIGINT NOT NULL,
                trashed_by {MYSQL_DID} NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS web_sessions (
                token VARCHAR(64) NOT NULL PRIMARY KEY,
                mkey VARCHAR(64) NOT NULL,
                csrf VARCHAR(64) NOT NULL,
                created BIGINT NOT NULL,
                expires BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS web_pictures (
                token VARCHAR(64) NOT NULL PRIMARY KEY,
                mkey VARCHAR(64) NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS web_outside (
                stem VARCHAR(64) NOT NULL PRIMARY KEY,
                media_type VARCHAR(32) NOT NULL,
                data MEDIUMTEXT NOT NULL,
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS web_previews (
                link VARCHAR(64) NOT NULL PRIMARY KEY,
                url TEXT NOT NULL,
                data MEDIUMTEXT NOT NULL,
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS oidc_clients (
                id VARCHAR(255) NOT NULL PRIMARY KEY,
                kind VARCHAR(16) NOT NULL,
                name TEXT NOT NULL,
                secret VARCHAR(64),
                redirect_uris MEDIUMTEXT NOT NULL,
                created BIGINT NOT NULL,
                created_by {MYSQL_DID}
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS oidc_consents (
                mkey VARCHAR(64) NOT NULL,
                client VARCHAR(255) NOT NULL,
                scopes TEXT NOT NULL,
                at BIGINT NOT NULL,
                PRIMARY KEY (mkey, client)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS oidc_codes (
                code VARCHAR(64) NOT NULL PRIMARY KEY,
                mkey VARCHAR(64) NOT NULL,
                client VARCHAR(255) NOT NULL,
                redirect_uri TEXT NOT NULL,
                scopes TEXT NOT NULL,
                nonce TEXT,
                challenge VARCHAR(128) NOT NULL,
                expires BIGINT NOT NULL,
                used BIGINT NOT NULL DEFAULT 0
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS oidc_tokens (
                token VARCHAR(64) NOT NULL PRIMARY KEY,
                mkey VARCHAR(64) NOT NULL,
                client VARCHAR(255) NOT NULL,
                scopes TEXT NOT NULL,
                code VARCHAR(64),
                expires BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS oidc_keys (
                kid VARCHAR(64) NOT NULL PRIMARY KEY,
                jwk TEXT NOT NULL,
                created BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS member_ids (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                member VARCHAR(64) NOT NULL UNIQUE,
                created BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS member_profiles (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                description TEXT,
                banner_type VARCHAR(64),
                banner_data MEDIUMTEXT,
                linked MEDIUMTEXT,
                public_address VARCHAR(1024),
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS member_here (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                nickname VARCHAR(255),
                picture_type VARCHAR(64),
                picture_data MEDIUMTEXT,
                requests BIGINT NOT NULL DEFAULT 1,
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS message_requests (
                from_did {MYSQL_DID} NOT NULL,
                to_did {MYSQL_DID} NOT NULL,
                at BIGINT NOT NULL,
                PRIMARY KEY (from_did(255), to_did(255))
            ) {MYSQL_TABLE};
            CREATE INDEX IF NOT EXISTS post_state_by_thread ON post_state (thread);
            CREATE INDEX IF NOT EXISTS post_state_by_rev ON post_state (rev);
            CREATE INDEX IF NOT EXISTS post_state_by_reply ON post_state (reply_to);
            CREATE INDEX IF NOT EXISTS posts_by_author ON posts (author_did(255), posted_at);
            CREATE INDEX IF NOT EXISTS posts_by_answerer ON posts (answered_by(255));
            CREATE INDEX IF NOT EXISTS posts_by_time ON posts (posted_at);
            CREATE INDEX IF NOT EXISTS posts_by_channel_time ON posts (channel, posted_at, seq);
            CREATE INDEX IF NOT EXISTS reactions_by_time ON reactions (at);
            CREATE INDEX IF NOT EXISTS threads_by_channel ON threads (channel);
            CREATE INDEX IF NOT EXISTS answering_by_owner ON answering (owner(255), post);
            CREATE INDEX IF NOT EXISTS flows_by_owner ON flows (owner(255));
            CREATE INDEX IF NOT EXISTS message_requests_by_to ON message_requests (to_did(255));
            CREATE INDEX IF NOT EXISTS form_answers_by_channel ON form_answers (channel, at);
            CREATE INDEX IF NOT EXISTS wiki_pages_by_channel ON wiki_pages (channel);
            CREATE INDEX IF NOT EXISTS wiki_comments_by_page ON wiki_comments (page, at);
            CREATE INDEX IF NOT EXISTS announce_followers_by_channel ON announce_followers (channel);
            CREATE INDEX IF NOT EXISTS announce_queue_by_due ON announce_queue (next_at);
            CREATE INDEX IF NOT EXISTS event_queue_by_due ON event_queue (next_at);
            -- Signing secrets are kept sealed (WYV-32), longer than the 128 these
            -- once had: widened on a store from before, and nothing done after.
            ALTER TABLE event_hooks MODIFY secret TEXT NOT NULL;
            ALTER TABLE announce_followers MODIFY secret TEXT"
        )
    });
    Schema { sqlite: SQLITE, mysql }
}

impl Store {
    pub fn open(location: &Location) -> anyhow::Result<Self> {
        Ok(Self::with_cache(Db::open(location, &schema(), "community")?))
    }

    /// Who is who, what each role may do, and the community's settings are asked
    /// for many times in one request (once per member, per channel): kept for a
    /// while rather than asked of the database each time, which may be far away.
    /// This process's own writes are seen at once; another's (`wyvrn-community add`
    /// while the server runs) within [`CACHE_LIFETIME`].
    fn with_cache(db: Db) -> Self {
        db.cache_reads(CACHED_TABLES, CACHE_LIFETIME);
        Self { db }
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        Ok(Self::with_cache(Db::scratch(&schema())?))
    }

    // --- Members ---------------------------------------------------------------

    pub fn member(&self, did: &str) -> Result<Option<Member>> {
        self.db.lock().query_row(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE did = ?1"), params![did], member_from)
    }

    pub fn members(&self) -> Result<Vec<Member>> {
        self.db.lock().query(&format!("SELECT {MEMBER_COLUMNS} FROM members ORDER BY role, since, did"), params![], member_from)
    }

    /// Everyone whose app has asked to be sent what is posted.
    pub fn joined(&self) -> Result<Vec<Member>> {
        Ok(self.members()?.into_iter().filter(|member| member.joined).collect())
    }

    /// How many members there are, those who left included.
    pub fn member_count(&self) -> Result<i64> {
        self.db.lock().query_value("SELECT COUNT(*) FROM members", params![])
    }

    pub fn admin_count(&self) -> Result<i64> {
        self.db.lock().query_value("SELECT COUNT(*) FROM members WHERE role = 'admin'", params![])
    }

    const ADMIT: &'static str =
        "INSERT OR IGNORE INTO members (did, role, since, delivered) VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(serial), 0) FROM posts))";

    /// Lets `did` in as `role`, or changes the role of someone already in. Someone
    /// new is sent only what is posted from now on.
    pub fn set_member(&self, did: &str, role: Role, now: i64) -> Result<()> {
        let mut db = self.db.lock();
        if db.execute("UPDATE members SET role = ?2 WHERE did = ?1", params![did, role.as_str()])? == 0 && db.execute(Self::ADMIT, params![did, role.as_str(), now])? > 0 {
            db.execute("INSERT INTO member_events (kind, at) VALUES ('joined', ?1)", params![now])?;
        }
        // An admin by the older word holds the role `admin`, while there is one.
        if role == Role::Admin {
            db.execute("INSERT OR IGNORE INTO member_roles (did, role) SELECT ?1, id FROM roles WHERE id = 'admin'", params![did])?;
        }
        Ok(())
    }

    /// Lets `did` in as `role` unless they are in already, whatever as.
    pub fn admit(&self, did: &str, role: Role, now: i64) -> Result<bool> {
        let mut db = self.db.lock();
        let admitted = db.execute(Self::ADMIT, params![did, role.as_str(), now])? > 0;
        if admitted {
            db.execute("INSERT INTO member_events (kind, at) VALUES ('joined', ?1)", params![now])?;
        }
        Ok(admitted)
    }

    pub fn remove_member(&self, did: &str) -> Result<bool> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM member_left WHERE did = ?1", params![did])?;
        db.execute("DELETE FROM member_roles WHERE did = ?1", params![did])?;
        // Signed in on the web view: signed out with it (WYV-51).
        db.execute("DELETE FROM web_sessions WHERE mkey IN (SELECT mkey FROM member_keys WHERE did = ?1)", params![did])?;
        let removed = db.execute("DELETE FROM members WHERE did = ?1", params![did])? > 0;
        if removed {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            db.execute("INSERT INTO member_events (kind, at) VALUES ('left', ?1)", params![now])?;
        }
        Ok(removed)
    }

    /// Forgets a member who was only ever named, and whose app never joined.
    pub fn forget_never_joined(&self, did: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM members WHERE did = ?1 AND joined = 0", params![did])?;
        Ok(())
    }

    pub fn set_joined(&self, did: &str, joined: bool) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("UPDATE members SET joined = ?2 WHERE did = ?1", params![did, joined])?;
        if joined {
            db.execute("DELETE FROM member_left WHERE did = ?1", params![did])?;
        }
        Ok(())
    }

    /// A member who had joined has left, at `now`.
    pub fn note_left(&self, did: &str, now: i64) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("INSERT OR REPLACE INTO member_left (did, at) VALUES (?1, ?2)", params![did, now])?;
        db.execute("INSERT INTO member_events (kind, at) VALUES ('left', ?1)", params![now])?;
        Ok(())
    }

    pub fn set_name(&self, did: &str, name: Option<&str>) -> Result<()> {
        self.db.lock().execute("UPDATE members SET name = ?2 WHERE did = ?1", params![did, name])?;
        Ok(())
    }

    pub fn set_delivered(&self, did: &str, serial: i64) -> Result<()> {
        self.db.lock().execute("UPDATE members SET delivered = ?2 WHERE did = ?1 AND delivered < ?2", params![did, serial])?;
        Ok(())
    }

    pub fn is_superseded(&self, did: &str) -> Result<bool> {
        Ok(self.db.lock().query_row("SELECT 1 FROM superseded WHERE did = ?1", params![did], |_| Ok(()))?.is_some())
    }

    /// A member has changed DID: what the server holds for `prior` is `new`'s now,
    /// and `prior` is never listened to again. Returns whether `prior` was a member.
    /// Where `new` is one already, it keeps the higher role of the two.
    pub fn rotate(&self, prior: &str, new: &str, now: i64) -> Result<bool> {
        // Heard of out of order -- `new` already left for another DID -- the move is
        // to wherever they are now, or the two halves of one person stay apart.
        let new = &self.current_did(new)?;
        if new == prior {
            return Ok(false);
        }
        // A DID moves once (security review WYV-27): a second move from it, to
        // somewhere its first didn't lead, is whoever else still holds its old keys,
        // and is not taken.
        let before: Option<String> = self.db.lock().query_row("SELECT became FROM superseded WHERE did = ?1", params![prior], |row| row.get(0))?;
        if let Some(became) = before {
            if &self.current_did(&became)? != new {
                tracing::warn!(%prior, %new, "a second, different move from a DID that has moved already: not taken");
                return Ok(false);
            }
        }
        self.db.lock().transaction(|tx| {
            let old = tx.query_row(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE did = ?1"), params![prior], member_from)?;
            if let Some(old) = &old {
                let existing = tx.query_row(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE did = ?1"), params![new], member_from)?;
                tx.execute("DELETE FROM members WHERE did = ?1", params![prior])?;
                match existing {
                    None => {
                        tx.execute(
                            "INSERT INTO members (did, role, name, joined, since, delivered) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            params![new, old.role.as_str(), old.name, old.joined, old.since, old.delivered],
                        )?;
                    }
                    Some(existing) => {
                        let role = if old.role == Role::Admin { Role::Admin } else { existing.role };
                        tx.execute(
                            "UPDATE members SET role = ?2, joined = ?3, name = ?4, since = ?5, delivered = ?6 WHERE did = ?1",
                            params![
                                new,
                                role.as_str(),
                                old.joined || existing.joined,
                                existing.name.as_ref().or(old.name.as_ref()),
                                existing.since.min(old.since),
                                existing.delivered.max(old.delivered)
                            ],
                        )?;
                    }
                }
                // When they left goes with them, unless the new DID is joined.
                tx.execute("DELETE FROM member_left WHERE did = ?1 AND EXISTS (SELECT 1 FROM members WHERE did = ?2 AND joined = 1)", params![prior, new])?;
                tx.execute("UPDATE OR IGNORE member_left SET did = ?2 WHERE did = ?1", params![prior, new])?;
                tx.execute("DELETE FROM member_left WHERE did = ?1", params![prior])?;
                // What they wrote, and what they were answering, is theirs under the new DID.
                tx.execute("UPDATE posts SET author_did = ?2 WHERE author_did = ?1", params![prior, new])?;
                tx.execute("UPDATE posts SET answered_by = ?2 WHERE answered_by = ?1", params![prior, new])?;
                // (Their counts are worked out again under the new DID.)
                tx.execute(&format!("DELETE FROM cached_counts WHERE kind = '{COUNT_FORUM_POSTS}' AND subject IN (?1, ?2)"), params![prior, new])?;
                tx.execute(&format!("DELETE FROM cached_counts WHERE kind = '{COUNT_THREAD}' AND ref_id = ?1"), params![prior])?;
                tx.execute("UPDATE answering SET owner = ?2 WHERE owner = ?1", params![prior, new])?;
                // Their voice preferences follow them.
                tx.execute("UPDATE OR IGNORE voice_prefs SET did = ?2 WHERE did = ?1", params![prior, new])?;
                tx.execute("DELETE FROM voice_prefs WHERE did = ?1", params![prior])?;
                // So do what reaches them, and the threads they follow.
                tx.execute("UPDATE OR IGNORE notify_prefs SET did = ?2 WHERE did = ?1", params![prior, new])?;
                tx.execute("DELETE FROM notify_prefs WHERE did = ?1", params![prior])?;
                tx.execute("UPDATE OR IGNORE thread_follows SET did = ?2 WHERE did = ?1", params![prior, new])?;
                tx.execute("DELETE FROM thread_follows WHERE did = ?1", params![prior])?;
                // Their roles follow them, and so does owning the community.
                tx.execute("UPDATE OR IGNORE member_roles SET did = ?2 WHERE did = ?1", params![prior, new])?;
                tx.execute("DELETE FROM member_roles WHERE did = ?1", params![prior])?;
                tx.execute("UPDATE settings SET value = ?2 WHERE name = 'owner' AND value = ?1", params![prior, new])?;
                tx.execute("UPDATE flows SET owner = ?2 WHERE owner = ?1", params![prior, new])?;
                // Reports they made, or that name them, follow them.
                tx.execute("UPDATE reports SET by_did = ?2 WHERE by_did = ?1", params![prior, new])?;
                tx.execute("UPDATE reports SET member = ?2 WHERE member = ?1", params![prior, new])?;
            }
            // The rules they accepted, whether or not they were in yet.
            if let Some(version) = tx.query_row("SELECT version FROM rules_accepted WHERE did = ?1", params![prior], |row| row.get::<i64>(0))? {
                let kept = tx.query_row("SELECT version FROM rules_accepted WHERE did = ?1", params![new], |row| row.get::<i64>(0))?.unwrap_or(0);
                tx.execute("INSERT OR REPLACE INTO rules_accepted (did, version) VALUES (?1, ?2)", params![new, version.max(kept)])?;
                tx.execute("DELETE FROM rules_accepted WHERE did = ?1", params![prior])?;
            }
            // A ban or a timeout follows the DID, member or not.
            tx.execute("UPDATE OR IGNORE bans SET did = ?2 WHERE did = ?1", params![prior, new])?;
            tx.execute("UPDATE OR IGNORE timeouts SET did = ?2 WHERE did = ?1", params![prior, new])?;
            tx.execute("UPDATE OR IGNORE join_answers SET did = ?2 WHERE did = ?1", params![prior, new])?;
            // What they made goes on working for them (WYV-115): the channels they
            // opened, their webhooks, event receivers and announcement followers
            // (paused while their maker isn't a member, WYV-20, WYV-99, WYV-125),
            // the bots they approved; and, for a bot, being one and the commands it
            // offers.
            tx.execute("UPDATE channels SET created_by = ?2 WHERE created_by = ?1", params![prior, new])?;
            tx.execute("UPDATE webhooks SET created_by = ?2 WHERE created_by = ?1", params![prior, new])?;
            tx.execute("UPDATE event_hooks SET created_by = ?2 WHERE created_by = ?1", params![prior, new])?;
            tx.execute("UPDATE announce_followers SET created_by = ?2 WHERE created_by = ?1", params![prior, new])?;
            tx.execute("UPDATE bot_approvals SET by_did = ?2 WHERE by_did = ?1", params![prior, new])?;
            for table in ["member_bots", "bot_approvals"] {
                tx.execute(&format!("UPDATE OR IGNORE {table} SET did = ?2 WHERE did = ?1"), params![prior, new])?;
                tx.execute(&format!("DELETE FROM {table} WHERE did = ?1"), params![prior])?;
            }
            // On the web: their lasting key (and with it their sessions, the websites
            // they let sign them in, and the ids those know them by), and their picture.
            tx.execute("UPDATE OR IGNORE member_keys SET did = ?2 WHERE did = ?1", params![prior, new])?;
            tx.execute("DELETE FROM member_keys WHERE did = ?1", params![prior])?;
            tx.execute("UPDATE OR IGNORE member_pictures SET did = ?2 WHERE did = ?1", params![prior, new])?;
            tx.execute("DELETE FROM member_pictures WHERE did = ?1", params![prior])?;
            // The rest of their profile, and what they are here (member_ids.rs):
            // the new DID's, where it has its own already.
            for table in ["member_profiles", "member_here"] {
                tx.execute(&format!("UPDATE OR IGNORE {table} SET did = ?2 WHERE did = ?1"), params![prior, new])?;
                tx.execute(&format!("DELETE FROM {table} WHERE did = ?1"), params![prior])?;
            }
            tx.execute("UPDATE OR IGNORE message_requests SET from_did = ?2 WHERE from_did = ?1", params![prior, new])?;
            tx.execute("DELETE FROM message_requests WHERE from_did = ?1", params![prior])?;
            tx.execute("UPDATE OR IGNORE message_requests SET to_did = ?2 WHERE to_did = ?1", params![prior, new])?;
            tx.execute("DELETE FROM message_requests WHERE to_did = ?1", params![prior])?;
            // Their member id: the one they had, unless the new DID was given one
            // first (it wrote before the server heard of the change), in which case
            // the older of the two -- and the other still finds them, through the
            // prior DID's row and superseded.
            let prior_id = tx.query_row("SELECT member, created FROM member_ids WHERE did = ?1", params![prior], |row| Ok((row.get::<String>(0)?, row.get::<i64>(1)?)))?;
            let new_id = tx.query_row("SELECT member, created FROM member_ids WHERE did = ?1", params![new], |row| Ok((row.get::<String>(0)?, row.get::<i64>(1)?)))?;
            match (prior_id, new_id) {
                (Some(_), None) => {
                    tx.execute("UPDATE member_ids SET did = ?2 WHERE did = ?1", params![prior, new])?;
                }
                (Some((prior_member, prior_created)), Some((new_member, new_created))) if prior_created <= new_created => {
                    tx.execute("DELETE FROM member_ids WHERE did = ?1 OR did = ?2", params![prior, new])?;
                    tx.execute("INSERT INTO member_ids (did, member, created) VALUES (?1, ?2, ?3)", params![new, prior_member, prior_created])?;
                    tx.execute("INSERT INTO member_ids (did, member, created) VALUES (?1, ?2, ?3)", params![prior, new_member, new_created])?;
                }
                _ => {}
            }
            tx.execute("INSERT OR REPLACE INTO superseded (did, became, at) VALUES (?1, ?2, ?3)", params![prior, new, now])?;
            Ok(old.is_some())
        })
    }

    // --- The waiting list, invites and settings ------------------------------

    /// Puts `did` on the waiting list, or notes a newer valid invite of theirs.
    pub fn add_waiting(&self, did: &str, invite: Option<&str>, now: i64) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("INSERT OR IGNORE INTO waiting (did, asked_at, invite) VALUES (?1, ?2, ?3)", params![did, now, invite])?;
        if invite.is_some() {
            db.execute("UPDATE waiting SET invite = ?2 WHERE did = ?1", params![did, invite])?;
        }
        Ok(())
    }

    pub fn waiting_one(&self, did: &str) -> Result<Option<Waiting>> {
        self.db.lock().query_row(&format!("SELECT {WAITING_COLUMNS} FROM waiting WHERE did = ?1"), params![did], waiting_from)
    }

    /// Everyone waiting, who asked first first.
    pub fn waiting(&self) -> Result<Vec<Waiting>> {
        self.db.lock().query(&format!("SELECT {WAITING_COLUMNS} FROM waiting ORDER BY asked_at, did"), params![], waiting_from)
    }

    pub fn waiting_count(&self) -> Result<i64> {
        self.db.lock().query_value("SELECT COUNT(*) FROM waiting", params![])
    }

    pub fn remove_waiting(&self, did: &str) -> Result<bool> {
        Ok(self.db.lock().execute("DELETE FROM waiting WHERE did = ?1", params![did])? > 0)
    }

    pub fn set_waiting_name(&self, did: &str, name: Option<&str>) -> Result<()> {
        self.db.lock().execute("UPDATE waiting SET name = ?2 WHERE did = ?1", params![did, name])?;
        Ok(())
    }

    pub fn add_invite(&self, invite: &Invite) -> Result<()> {
        self.db.lock().execute(
            "INSERT INTO invites (code, uses_left, expires_at, note, created_at, created_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![invite.code, invite.uses_left, invite.expires_at, invite.note, invite.created_at, invite.created_by],
        )?;
        Ok(())
    }

    /// An invite that hasn't been revoked, used up or not.
    pub fn invite(&self, code: &str) -> Result<Option<Invite>> {
        self.db.lock().query_row(&format!("SELECT {INVITE_COLUMNS} FROM invites WHERE code = ?1 AND revoked = 0"), params![code], invite_from)
    }

    /// Every invite still good at `now`, newest first: everyone's, or `by`'s.
    pub fn invites(&self, by: Option<&str>, now: i64) -> Result<Vec<Invite>> {
        let all = self.db.lock().query(&format!("SELECT {INVITE_COLUMNS} FROM invites WHERE revoked = 0 ORDER BY created_at DESC, code"), params![], invite_from)?;
        Ok(all.into_iter().filter(|invite| invite.valid(now) && by.is_none_or(|by| invite.created_by == by)).collect())
    }

    /// Uses an invite once, if it is still good at `now`. Returns whether it was.
    pub fn use_invite(&self, code: &str, now: i64) -> Result<bool> {
        Ok(self.db.lock().execute(
            "UPDATE invites SET uses_left = uses_left - 1 WHERE code = ?1 AND revoked = 0 \
             AND (uses_left IS NULL OR uses_left > 0) AND (expires_at IS NULL OR expires_at > ?2)",
            params![code, now],
        )? > 0)
    }

    pub fn revoke_invite(&self, code: &str) -> Result<bool> {
        Ok(self.db.lock().execute("UPDATE invites SET revoked = 1 WHERE code = ?1 AND revoked = 0", params![code])? > 0)
    }

    pub fn setting(&self, name: &str) -> Result<Option<String>> {
        self.db.lock().query_row("SELECT value FROM settings WHERE name = ?1", params![name], |row| row.get(0))
    }

    pub fn set_setting(&self, name: &str, value: &str) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO settings (name, value) VALUES (?1, ?2)", params![name, value])?;
        Ok(())
    }

    pub fn delete_setting(&self, name: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM settings WHERE name = ?1", params![name])?;
        Ok(())
    }

    // --- Roles --------------------------------------------------------------------

    /// Every role, highest first, `everyone` last.
    pub fn roles(&self) -> Result<Vec<RoleDef>> {
        self.db.lock().query(&format!("SELECT {ROLE_COLUMNS} FROM roles ORDER BY position DESC, id"), params![], role_from)
    }

    pub fn role(&self, id: &str) -> Result<Option<RoleDef>> {
        self.db.lock().query_row(&format!("SELECT {ROLE_COLUMNS} FROM roles WHERE id = ?1"), params![id], role_from)
    }

    pub fn save_role(&self, role: &RoleDef) -> Result<()> {
        let permissions = serde_json::to_string(&role.permissions).unwrap_or_else(|_| "[]".into());
        self.db.lock().execute(
            &format!("INSERT OR REPLACE INTO roles ({ROLE_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"),
            params![role.id, role.name, role.color, role.icon, role.position, permissions, role.mentionable, role.hoist, role.self_assignable],
        )?;
        Ok(())
    }

    /// Ranks the roles as `ids` lists them, lowest first, from 1 (`everyone` keeps 0).
    pub fn rank_roles(&self, ids: &[String]) -> Result<()> {
        self.db.lock().transaction(|tx| {
            for (n, id) in ids.iter().enumerate() {
                tx.execute("UPDATE roles SET position = ?2 WHERE id = ?1", params![id, n as i64 + 1])?;
            }
            Ok(())
        })
    }

    /// Deletes a role, with whoever held it and whatever channels said of it.
    pub fn delete_role(&self, id: &str) -> Result<bool> {
        self.db.lock().transaction(|tx| {
            tx.execute("DELETE FROM member_roles WHERE role = ?1", params![id])?;
            tx.execute("DELETE FROM channel_overrides WHERE role = ?1", params![id])?;
            Ok(tx.execute("DELETE FROM roles WHERE id = ?1", params![id])? > 0)
        })
    }

    /// The roles `did` holds, everyone left out.
    pub fn member_roles(&self, did: &str) -> Result<Vec<String>> {
        self.db.lock().query("SELECT role FROM member_roles WHERE did = ?1 ORDER BY role", params![did], |row| row.get(0))
    }

    /// Who holds what: every `(did, role)`.
    pub fn all_member_roles(&self) -> Result<Vec<(String, String)>> {
        self.db.lock().query("SELECT did, role FROM member_roles", params![], |row| Ok((row.get(0)?, row.get(1)?)))
    }

    pub fn give_role(&self, did: &str, role: &str) -> Result<()> {
        self.db.lock().execute("INSERT OR IGNORE INTO member_roles (did, role) VALUES (?1, ?2)", params![did, role])?;
        Ok(())
    }

    pub fn take_role(&self, did: &str, role: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM member_roles WHERE did = ?1 AND role = ?2", params![did, role])?;
        Ok(())
    }

    /// What `channel` allows and denies each role it says anything of.
    pub fn overrides(&self, channel: &str) -> Result<Vec<Override>> {
        self.db.lock().query("SELECT channel, role, allow, deny FROM channel_overrides WHERE channel = ?1", params![channel], |row| {
            Ok(Override { channel: row.get(0)?, role: row.get(1)?, allow: list_from(row.get(2)?), deny: list_from(row.get(3)?) })
        })
    }

    /// Sets what `channel` allows and denies `role`; with neither, forgets it.
    pub fn set_override(&self, channel: &str, role: &str, allow: &[String], deny: &[String]) -> Result<()> {
        let mut db = self.db.lock();
        if allow.is_empty() && deny.is_empty() {
            db.execute("DELETE FROM channel_overrides WHERE channel = ?1 AND role = ?2", params![channel, role])?;
        } else {
            let (allow, deny) = (serde_json::to_string(allow).unwrap_or_default(), serde_json::to_string(deny).unwrap_or_default());
            db.execute("INSERT OR REPLACE INTO channel_overrides (channel, role, allow, deny) VALUES (?1, ?2, ?3, ?4)", params![channel, role, allow, deny])?;
        }
        Ok(())
    }

    // --- Moderation ---------------------------------------------------------------

    /// Writes one thing done in the audit log.
    pub fn audit(&self, at: i64, actor: &str, action: &str, target: Option<&str>, details: Option<&Value>) -> Result<()> {
        let details = details.map(Value::to_string);
        self.db.lock().execute("INSERT INTO audit (at, actor, action, target, details) VALUES (?1, ?2, ?3, ?4, ?5)", params![at, actor, action, target, details])?;
        Ok(())
    }

    /// The audit log, newest first: entries before `before` (an id), at most
    /// `limit`, narrowed by actor, target and action where given.
    pub fn audit_entries(&self, before: Option<i64>, limit: usize, actor: Option<&str>, target: Option<&str>, action: Option<&str>) -> Result<Vec<AuditEntry>> {
        let before = before.unwrap_or(i64::MAX);
        self.db.lock().query(
            "SELECT id, at, actor, action, target, details FROM audit WHERE id < ?1 \
             AND (?2 IS NULL OR actor = ?2) AND (?3 IS NULL OR target = ?3) AND (?4 IS NULL OR action = ?4) \
             ORDER BY id DESC LIMIT ?5",
            params![before, actor, target, action, limit as i64],
            |row| Ok(AuditEntry { id: row.get(0)?, at: row.get(1)?, actor: row.get(2)?, action: row.get(3)?, target: row.get(4)?, details: json_from(row.get(5)?) }),
        )
    }

    pub fn ban(&self, ban: &Ban) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO bans (did, name, by_did, at, until, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![ban.did, ban.name, ban.by, ban.at, ban.until, ban.reason],
        )?;
        Ok(())
    }

    pub fn unban(&self, did: &str) -> Result<bool> {
        Ok(self.db.lock().execute("DELETE FROM bans WHERE did = ?1", params![did])? > 0)
    }

    /// The bans in force at `now`, newest first.
    pub fn bans(&self, now: i64) -> Result<Vec<Ban>> {
        self.db.lock().query(
            "SELECT did, name, by_did, at, until, reason FROM bans WHERE until IS NULL OR until > ?1 ORDER BY at DESC",
            params![now],
            ban_from,
        )
    }

    /// `did`'s ban, if one is in force at `now`.
    pub fn banned(&self, did: &str, now: i64) -> Result<Option<Ban>> {
        self.db.lock().query_row(
            "SELECT did, name, by_did, at, until, reason FROM bans WHERE did = ?1 AND (until IS NULL OR until > ?2)",
            params![did, now],
            ban_from,
        )
    }

    pub fn set_timeout(&self, did: &str, until: Option<i64>, reason: Option<&str>) -> Result<()> {
        let mut db = self.db.lock();
        match until {
            Some(until) => db.execute("INSERT OR REPLACE INTO timeouts (did, until, reason) VALUES (?1, ?2, ?3)", params![did, until, reason])?,
            None => db.execute("DELETE FROM timeouts WHERE did = ?1", params![did])?,
        };
        Ok(())
    }

    /// When `did`'s timeout ends, if they are timed out at `now`.
    pub fn timed_out_until(&self, did: &str, now: i64) -> Result<Option<i64>> {
        // Compared here, not in the statement, so that the answer can be kept
        // ([`CACHED_TABLES`]): with `now` in it, it would be another question each second.
        let until: Option<i64> = self.db.lock().query_row("SELECT until FROM timeouts WHERE did = ?1", params![did], |row| row.get(0))?;
        Ok(until.filter(|until| *until > now))
    }

    pub fn set_slow_mode(&self, channel: &str, seconds: i64) -> Result<()> {
        let mut db = self.db.lock();
        if seconds > 0 {
            db.execute("INSERT OR REPLACE INTO channel_slow (channel, seconds) VALUES (?1, ?2)", params![channel, seconds])?;
        } else {
            db.execute("DELETE FROM channel_slow WHERE channel = ?1", params![channel])?;
        }
        Ok(())
    }

    /// When `did` last posted in `channel`, threads included.
    /// When `did` last posted anywhere here.
    pub fn last_post_by(&self, did: &str) -> Result<Option<i64>> {
        self.db.lock().query_value("SELECT MAX(posted_at) FROM posts WHERE author_did = ?1", params![did])
    }

    pub fn last_post_in(&self, channel: &str, did: &str) -> Result<Option<i64>> {
        self.db.lock().query_value("SELECT MAX(posted_at) FROM posts WHERE channel = ?1 AND author_did = ?2", params![channel, did])
    }

    /// How many posts `did` has made, anywhere, since `since`.
    /// How much the store takes up, in bytes (a host's limit, measured now and then).
    pub fn bytes_used(&self) -> Result<u64> {
        self.db.lock().bytes_used()
    }

    /// Every post since `since`, from anyone (a host's limit on the whole community).
    pub fn all_posts_since(&self, since: i64) -> Result<i64> {
        self.db.lock().query_value("SELECT COUNT(*) FROM posts WHERE posted_at >= ?1", params![since])
    }

    pub fn posts_since(&self, did: &str, since: i64) -> Result<i64> {
        self.db.lock().query_value("SELECT COUNT(*) FROM posts WHERE author_did = ?1 AND posted_at >= ?2", params![did, since])
    }

    /// `did`'s posts in `channel` since `since`, not taken back, newest first.
    pub fn recent_posts_by(&self, channel: Option<&str>, did: &str, since: i64, limit: usize) -> Result<Vec<String>> {
        self.db.lock().query(
            "SELECT id FROM posts WHERE author_did = ?1 AND posted_at >= ?2 AND (?3 IS NULL OR channel = ?3) \
             AND NOT EXISTS (SELECT 1 FROM post_state WHERE post_state.post = posts.id AND deleted = 1) ORDER BY serial DESC LIMIT ?4",
            params![did, since, channel, limit as i64],
            |row| row.get(0),
        )
    }

    /// AutoMod's rules, in order.
    pub fn automod_rules(&self) -> Result<Vec<Value>> {
        self.db.lock().query("SELECT rule FROM automod_rules ORDER BY position, id", params![], |row| Ok(json_from(row.get(0)?).unwrap_or(Value::Null)))
    }

    pub fn save_automod_rule(&self, id: &str, position: i64, rule: &Value) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO automod_rules (id, position, rule) VALUES (?1, ?2, ?3)", params![id, position, rule.to_string()])?;
        Ok(())
    }

    pub fn delete_automod_rule(&self, id: &str) -> Result<bool> {
        Ok(self.db.lock().execute("DELETE FROM automod_rules WHERE id = ?1", params![id])? > 0)
    }

    pub fn hold(&self, held: &Held) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO held (id, channel, author, held_at, rule, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![held.id, held.channel, held.author, held.held_at, held.rule, held.body.to_string()],
        )?;
        Ok(())
    }

    /// Every held post, oldest first.
    pub fn held(&self) -> Result<Vec<Held>> {
        self.db.lock().query("SELECT id, channel, author, held_at, rule, body FROM held ORDER BY held_at, id", params![], |row| {
            Ok(Held { id: row.get(0)?, channel: row.get(1)?, author: row.get(2)?, held_at: row.get(3)?, rule: row.get(4)?, body: json_from(row.get(5)?).unwrap_or(Value::Null) })
        })
    }

    pub fn unhold(&self, id: &str) -> Result<bool> {
        Ok(self.db.lock().execute("DELETE FROM held WHERE id = ?1", params![id])? > 0)
    }

    pub fn set_join_answers(&self, did: &str, answers: &Value, at: i64) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO join_answers (did, answers, at) VALUES (?1, ?2, ?3)", params![did, answers.to_string(), at])?;
        Ok(())
    }

    pub fn join_answers(&self, did: &str) -> Result<Option<Value>> {
        Ok(self.db.lock().query_row("SELECT answers FROM join_answers WHERE did = ?1", params![did], |row| row.get::<String>(0))?.and_then(|text| serde_json::from_str(&text).ok()))
    }

    // --- Administration workflows -------------------------------------------------

    pub fn flow(&self, id: &str) -> Result<Option<Flow>> {
        self.db.lock().query_row(&format!("SELECT {FLOW_COLUMNS} FROM flows WHERE id = ?1"), params![id], flow_from)
    }

    /// `owner`'s active workflow of `template` about `context`, if there is one.
    pub fn active_flow(&self, owner: &str, template: &str, context: &Value) -> Result<Option<Flow>> {
        let context = context.to_string();
        self.db.lock().query_row(
            &format!("SELECT {FLOW_COLUMNS} FROM flows WHERE owner = ?1 AND template = ?2 AND context = ?3 AND status = 'active'"),
            params![owner, template, context],
            flow_from,
        )
    }

    pub fn save_flow(&self, flow: &Flow) -> Result<()> {
        self.db.lock().execute(
            &format!("INSERT OR REPLACE INTO flows ({FLOW_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
            params![flow.id, flow.owner, flow.template, flow.context.to_string(), flow.state, flow.data.to_string(), flow.status, flow.updated],
        )?;
        Ok(())
    }

    /// Forgets workflows last touched before `before`.
    pub fn prune_flows(&self, before: i64) -> Result<usize> {
        self.db.lock().execute("DELETE FROM flows WHERE updated < ?1", params![before])
    }

    // --- Voice ------------------------------------------------------------------

    /// Whether `did` lets voice invitations ring them.
    pub fn voice_rings(&self, did: &str) -> Result<bool> {
        Ok(self.db.lock().query_row("SELECT ring FROM voice_prefs WHERE did = ?1", params![did], |row| row.get::<bool>(0))?.unwrap_or(true))
    }

    pub fn set_voice_rings(&self, did: &str, ring: bool) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO voice_prefs (did, ring) VALUES (?1, ?2)", params![did, ring])?;
        Ok(())
    }

    // --- What reaches a member ----------------------------------------------------

    /// What reaches `did` through the mediator while they aren't watching: `all`,
    /// `mentions` or `nothing`; `None` while their app has never said.
    pub fn notify_level(&self, did: &str) -> Result<Option<String>> {
        self.db.lock().query_row("SELECT level FROM notify_prefs WHERE did = ?1", params![did], |row| row.get(0))
    }

    pub fn set_notify_level(&self, did: &str, level: &str) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO notify_prefs (did, level) VALUES (?1, ?2)", params![did, level])?;
        Ok(())
    }

    /// Their app has spoken of this: from now on `level` (the default) unless they
    /// chose before.
    pub fn settle_notify_level(&self, did: &str, level: &str) -> Result<()> {
        self.db.lock().execute("INSERT OR IGNORE INTO notify_prefs (did, level) VALUES (?1, ?2)", params![did, level])?;
        Ok(())
    }

    /// The threads `did` follows, by the id of the post each grew from.
    pub fn followed_threads(&self, did: &str) -> Result<Vec<String>> {
        self.db.lock().query("SELECT thread FROM thread_follows WHERE did = ?1 ORDER BY thread", params![did], |row| row.get(0))
    }

    pub fn follows_thread(&self, did: &str, thread: &str) -> Result<bool> {
        Ok(self.db.lock().query_row("SELECT 1 FROM thread_follows WHERE did = ?1 AND thread = ?2", params![did, thread], |row| row.get::<i64>(0))?.is_some())
    }

    /// Follows the thread `thread`, or stops following it. Only a thread there is.
    pub fn follow_thread(&self, did: &str, thread: &str, follow: bool) -> Result<()> {
        let mut db = self.db.lock();
        if follow {
            db.execute("INSERT OR IGNORE INTO thread_follows (did, thread) SELECT ?1, id FROM threads WHERE id = ?2", params![did, thread])?;
        } else {
            db.execute("DELETE FROM thread_follows WHERE did = ?1 AND thread = ?2", params![did, thread])?;
        }
        Ok(())
    }

    pub fn set_post_voice_invite(&self, post: &str, channel: &str) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO post_voice_invites (post, channel) VALUES (?1, ?2)", params![post, channel])?;
        Ok(())
    }

    /// The voice channel a post invites people into, if it is such a post.
    pub fn post_voice_invite(&self, post: &str) -> Result<Option<String>> {
        self.db.lock().query_row("SELECT channel FROM post_voice_invites WHERE post = ?1", params![post], |row| row.get(0))
    }

    // --- Rules and reports ---------------------------------------------------

    /// The latest version of the rules `did` has accepted, if any.
    pub fn rules_accepted(&self, did: &str) -> Result<Option<i64>> {
        self.db.lock().query_row("SELECT version FROM rules_accepted WHERE did = ?1", params![did], |row| row.get(0))
    }

    pub fn accept_rules(&self, did: &str, version: i64) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO rules_accepted (did, version) VALUES (?1, ?2)", params![did, version])?;
        Ok(())
    }

    /// Files a report, in place of an open one by the same member of the same post
    /// or member. Returns its id.
    pub fn add_report(&self, report: &Report) -> Result<String> {
        self.db.lock().transaction(|tx| {
            let open = tx.query_row(
                "SELECT id FROM reports WHERE by_did = ?1 AND resolved_at IS NULL AND (post_id = ?2 OR member = ?3)",
                params![report.by, report.post_id, report.member],
                |row| row.get::<String>(0),
            )?;
            if let Some(id) = &open {
                tx.execute("DELETE FROM reports WHERE id = ?1", params![id])?;
            }
            tx.execute(
                "INSERT INTO reports (id, reported_at, by_did, reason, note, post_id, post, member) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    report.id,
                    report.reported_at,
                    report.by,
                    report.reason,
                    report.note,
                    report.post_id,
                    report.post.as_ref().map(Value::to_string),
                    report.member
                ],
            )?;
            Ok(report.id.clone())
        })
    }

    /// Reports, newest first: the open ones, or all of them.
    pub fn reports(&self, resolved_too: bool) -> Result<Vec<Report>> {
        let filter = if resolved_too { "" } else { "WHERE resolved_at IS NULL" };
        self.db.lock().query(&format!("SELECT {REPORT_COLUMNS} FROM reports {filter} ORDER BY reported_at DESC, id"), params![], report_from)
    }

    pub fn report(&self, id: &str) -> Result<Option<Report>> {
        self.db.lock().query_row(&format!("SELECT {REPORT_COLUMNS} FROM reports WHERE id = ?1"), params![id], report_from)
    }

    pub fn resolve_report(&self, id: &str, by: &str, now: i64) -> Result<bool> {
        Ok(self.db.lock().execute("UPDATE reports SET resolved_at = ?2, resolved_by = ?3 WHERE id = ?1 AND resolved_at IS NULL", params![id, now, by])? > 0)
    }

    // --- Channels --------------------------------------------------------------

    /// Every channel: open ones first, then in no category, then by category, each
    /// group in the order set (a channel never placed last, oldest first).
    pub fn channels(&self) -> Result<Vec<Channel>> {
        let mut channels = self.db.lock().query(&format!("SELECT {CHANNEL_COLUMNS} FROM channels WHERE {NOT_TRASHED} ORDER BY closed, created_at, id"), params![], channel_from)?;
        let categories = self.categories()?;
        let rank = |channel: &Channel| {
            let category = channel.category.as_ref().and_then(|id| categories.iter().find(|c| c.id == *id)).map_or(-1, |c| c.position);
            (channel.closed, category, channel.position.unwrap_or(i64::MAX))
        };
        channels.sort_by_key(|channel| rank(channel));
        Ok(channels)
    }

    /// The categories, in order.
    pub fn categories(&self) -> Result<Vec<Category>> {
        self.db.lock().query("SELECT id, name, position FROM categories ORDER BY position, id", params![], |row| {
            Ok(Category { id: row.get(0)?, name: row.get(1)?, position: row.get(2)? })
        })
    }

    pub fn save_category(&self, category: &Category) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO categories (id, name, position) VALUES (?1, ?2, ?3)", params![category.id, category.name, category.position])?;
        Ok(())
    }

    /// Deletes a category; its channels go to none.
    pub fn delete_category(&self, id: &str) -> Result<()> {
        self.db.lock().transaction(|tx| {
            tx.execute("UPDATE channel_order SET category = NULL WHERE category = ?1", params![id])?;
            tx.execute("DELETE FROM categories WHERE id = ?1", params![id])?;
            // Its permissions, kept as a channel's are, under `category:<id>`.
            tx.execute("DELETE FROM channel_overrides WHERE channel = ?1", params![format!("category:{id}")])?;
            Ok(())
        })
    }

    /// Puts one channel in `category` (none: in none), keeping its place; one
    /// without a place yet goes last.
    pub fn set_channel_category(&self, channel: &str, category: Option<&str>) -> Result<()> {
        self.db.lock().transaction(|tx| {
            if tx.query_row("SELECT 1 FROM channel_order WHERE channel = ?1", params![channel], |_| Ok(()))?.is_some() {
                tx.execute("UPDATE channel_order SET category = ?2 WHERE channel = ?1", params![channel, category])?;
            } else {
                let last: i64 = tx.query_value("SELECT COALESCE(MAX(position), -1) FROM channel_order", params![])?;
                tx.execute("INSERT INTO channel_order (channel, position, category) VALUES (?1, ?2, ?3)", params![channel, last + 1, category])?;
            }
            Ok(())
        })
    }

    /// Places every channel as `order` lists them, each with its category.
    pub fn order_channels(&self, order: &[(String, Option<String>)]) -> Result<()> {
        self.db.lock().transaction(|tx| {
            for (n, (channel, category)) in order.iter().enumerate() {
                tx.execute("INSERT OR REPLACE INTO channel_order (channel, position, category) VALUES (?1, ?2, ?3)", params![channel, n as i64, category])?;
            }
            Ok(())
        })
    }

    pub fn onboarded(&self, did: &str) -> Result<bool> {
        Ok(self.db.lock().query_row("SELECT 1 FROM onboarded WHERE did = ?1", params![did], |_| Ok(()))?.is_some())
    }

    pub fn set_onboarded(&self, did: &str, now: i64) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO onboarded (did, at) VALUES (?1, ?2)", params![did, now])?;
        Ok(())
    }

    /// Posts since `since`, a day (UTC) and channel at a time: `(day, channel, count)`,
    /// `day` the second it starts.
    pub fn posts_by_day(&self, since: i64) -> Result<Vec<(i64, String, i64)>> {
        self.db.lock().query(
            "SELECT posted_at - posted_at % 86400 AS day, channel, COUNT(*) FROM posts WHERE posted_at >= ?1 GROUP BY day, channel",
            params![since],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
    }

    /// How many different members posted each day since `since`: `(day, count)`.
    pub fn posters_by_day(&self, since: i64) -> Result<Vec<(i64, i64)>> {
        self.db.lock().query(
            "SELECT posted_at - posted_at % 86400 AS day, COUNT(DISTINCT author_did) FROM posts WHERE posted_at >= ?1 GROUP BY day",
            params![since],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    /// `seconds` more in a voice channel for each of `dids`, on the day `at` is in;
    /// what is older than `VOICE_TIME_KEPT` goes.
    pub fn add_voice_time(&self, channel: &str, dids: &[String], at: i64, seconds: i64) -> Result<()> {
        let day = at - at.rem_euclid(86_400);
        let mut db = self.db.lock();
        for did in dids {
            db.execute("INSERT OR IGNORE INTO voice_time (day, channel, did, seconds) VALUES (?1, ?2, ?3, 0)", params![day, channel, did])?;
            db.execute("UPDATE voice_time SET seconds = seconds + ?4 WHERE day = ?1 AND channel = ?2 AND did = ?3", params![day, channel, did, seconds])?;
        }
        db.execute("DELETE FROM voice_time WHERE day < ?1", params![day - VOICE_TIME_KEPT])?;
        Ok(())
    }

    /// Time in voice since `since`, a day and a channel at a time: `(day, channel, seconds, people)`.
    pub fn voice_by_day(&self, since: i64) -> Result<Vec<(i64, String, i64, i64)>> {
        self.db.lock().query(
            "SELECT day, channel, CAST(SUM(seconds) AS SIGNED), COUNT(*) FROM voice_time WHERE day >= ?1 GROUP BY day, channel",
            params![since],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
    }

    /// How many were in voice at all, a day at a time: `(day, people)`.
    pub fn voice_people_by_day(&self, since: i64) -> Result<Vec<(i64, i64)>> {
        self.db.lock().query("SELECT day, COUNT(DISTINCT did) FROM voice_time WHERE day >= ?1 GROUP BY day", params![since], |row| Ok((row.get(0)?, row.get(1)?)))
    }

    /// Reactions given since `since`, a day at a time: `(day, count)`.
    pub fn reactions_by_day(&self, since: i64) -> Result<Vec<(i64, i64)>> {
        self.db.lock().query("SELECT at - at % 86400 AS day, COUNT(*) FROM reactions WHERE at >= ?1 GROUP BY day", params![since], |row| Ok((row.get(0)?, row.get(1)?)))
    }

    /// Posts since `since` by the hour of the day they were posted in (UTC): `(hour, count)`.
    pub fn posts_by_hour(&self, since: i64) -> Result<Vec<(i64, i64)>> {
        self.db.lock().query(
            "SELECT CAST(((posted_at % 86400) - (posted_at % 3600)) / 3600 AS SIGNED) AS hour, COUNT(*) FROM posts WHERE posted_at >= ?1 GROUP BY hour",
            params![since],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    /// Comings in and leavings since `since`, a day at a time: `(day, kind, count)`.
    pub fn member_events_by_day(&self, since: i64) -> Result<Vec<(i64, String, i64)>> {
        self.db.lock().query(
            "SELECT at - at % 86400 AS day, kind, COUNT(*) FROM member_events WHERE at >= ?1 GROUP BY day, kind",
            params![since],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
    }

    /// How many channels there are that aren't closed (nor in the trash).
    pub fn open_channel_count(&self) -> Result<usize> {
        let count: i64 = self.db.lock().query_value(&format!("SELECT COUNT(*) FROM channels WHERE closed = 0 AND {NOT_TRASHED}"), params![])?;
        Ok(count as usize)
    }

    pub fn channel(&self, id: &str) -> Result<Option<Channel>> {
        self.db.lock().query_row(&format!("SELECT {CHANNEL_COLUMNS} FROM channels WHERE id = ?1 AND {NOT_TRASHED}"), params![id], channel_from)
    }

    /// Every channel in the trash, with when it was put there and by whom, newest first.
    pub fn trashed_channels(&self) -> Result<Vec<(Channel, i64, String)>> {
        let mut found = Vec::new();
        let trashed: Vec<(String, i64, String)> = self.db.lock().query("SELECT channel, trashed_at, trashed_by FROM channel_trash ORDER BY trashed_at DESC", params![], |row| Ok((row.get::<String>(0)?, row.get::<i64>(1)?, row.get::<String>(2)?)))?;
        for (id, at, by) in trashed {
            if let Some(channel) = self.db.lock().query_row(&format!("SELECT {CHANNEL_COLUMNS} FROM channels WHERE id = ?1"), params![id], channel_from)? {
                found.push((channel, at, by));
            }
        }
        Ok(found)
    }

    /// Puts a channel in the trash: listed to nobody, and gone for good once
    /// [`Self::purge_trashed`] passes it.
    pub fn trash_channel(&self, id: &str, by: &str, now: i64) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("INSERT OR REPLACE INTO channel_trash (channel, trashed_at, trashed_by) VALUES (?1, ?2, ?3)", params![id, now, by])?;
        Self::forget_forum_posts(&mut db)?;
        Ok(())
    }

    /// Takes a channel out of the trash, as it was.
    pub fn restore_channel(&self, id: &str) -> Result<bool> {
        let mut db = self.db.lock();
        let restored = db.execute("DELETE FROM channel_trash WHERE channel = ?1", params![id])? > 0;
        Self::forget_forum_posts(&mut db)?;
        Ok(restored)
    }

    /// Deletes for good every channel put in the trash before `before`: its posts
    /// and everything of them, and its settings. Reports and the audit log keep
    /// what they say of it. Returns the channels deleted.
    pub fn purge_trashed(&self, before: i64) -> Result<Vec<String>> {
        let due: Vec<String> = self.db.lock().query("SELECT channel FROM channel_trash WHERE trashed_at < ?1", params![before], |row| row.get(0))?;
        for id in &due {
            self.db.lock().transaction(|tx| {
                for table in ["post_state", "reactions", "post_removals", "post_voice_invites", "post_extra", "post_sent", "post_censored", "post_deleted_text", "votes", "polls_ended", "answering"] {
                    tx.execute(&format!("DELETE FROM {table} WHERE post IN (SELECT id FROM posts WHERE channel = ?1)"), params![id])?;
                }
                tx.execute("DELETE FROM thread_follows WHERE thread IN (SELECT id FROM threads WHERE channel = ?1)", params![id])?;
                tx.execute(&format!("DELETE FROM cached_counts WHERE kind = '{COUNT_THREAD}' AND subject IN (SELECT id FROM threads WHERE channel = ?1)"), params![id])?;
                Self::forget_sections(tx, id)?;
                for table in ["threads", "posts", "held", "channel_voice", "channel_overrides", "channel_slow", "channel_order", "channel_forum", "web_channels", "webhooks"] {
                    tx.execute(&format!("DELETE FROM {table} WHERE channel = ?1"), params![id])?;
                }
                tx.execute("DELETE FROM channels WHERE id = ?1", params![id])?;
                tx.execute("DELETE FROM channel_trash WHERE channel = ?1", params![id])?;
                Ok::<(), wyvrn_service::db::Error>(())
            })?;
        }
        Ok(due)
    }

    /// The open channel called `name`, ignoring case, among the voice channels
    /// (`voice`) or the others: a voice channel and a text one may share a name.
    pub fn channel_named_among(&self, name: &str, voice: bool) -> Result<Option<Channel>> {
        let found = self.db.lock().query(&format!("SELECT {CHANNEL_COLUMNS} FROM channels WHERE closed = 0 AND LOWER(name) = LOWER(?1) AND {NOT_TRASHED}"), params![name], channel_from)?;
        Ok(found.into_iter().find(|channel| channel.voice.is_some() == voice))
    }

    /// The open channel called `name`, whatever its case.
    pub fn channel_named(&self, name: &str) -> Result<Option<Channel>> {
        self.db.lock().query_row(&format!("SELECT {CHANNEL_COLUMNS} FROM channels WHERE closed = 0 AND LOWER(name) = LOWER(?1) AND {NOT_TRASHED}"), params![name], channel_from)
    }

    pub fn add_channel(&self, id: &str, name: &str, topic: &str, by: &str, now: i64) -> Result<()> {
        self.db.lock().execute("INSERT INTO channels (id, name, topic, created_at, created_by) VALUES (?1, ?2, ?3, ?4, ?5)", params![id, name, topic, now, by])?;
        Ok(())
    }

    /// Makes `id` a voice channel with `settings`, or changes them.
    pub fn set_channel_voice(&self, id: &str, settings: &Value) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO channel_voice (channel, settings) VALUES (?1, ?2)", params![id, settings.to_string()])?;
        Ok(())
    }

    pub fn update_channel(&self, id: &str, name: &str, topic: &str, closed: bool) -> Result<()> {
        self.db.lock().execute("UPDATE channels SET name = ?2, topic = ?3, closed = ?4 WHERE id = ?1", params![id, name, topic, closed])?;
        Ok(())
    }

    // --- Posts -----------------------------------------------------------------

    pub fn latest_serial(&self) -> Result<i64> {
        self.db.lock().query_value("SELECT COALESCE(MAX(serial), 0) FROM posts", params![])
    }

    /// A post just read, with what is read apart from it: its reactions, and the
    /// thread that grew from it.
    fn filled(&self, mut post: Post) -> Result<Post> {
        let mut db = self.db.lock();
        let put = db.query("SELECT emoji, did FROM reactions WHERE post = ?1 ORDER BY at, emoji, did", params![post.id.clone()], |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)))?;
        for (emoji, did) in put {
            match post.reactions.iter_mut().find(|(known, _)| *known == emoji) {
                Some((_, by)) => by.push(did),
                None => post.reactions.push((emoji, vec![did])),
            }
        }
        if post.extra.as_ref().is_some_and(|extra| extra.get("poll").is_some()) {
            post.votes = db.query("SELECT did, answers FROM votes WHERE post = ?1 ORDER BY at, did", params![post.id.clone()], |row| {
                Ok((row.get::<String>(0)?, list_from(row.get(1)?)))
            })?;
        }
        drop(db);
        post.grew = self.thread(&post.id)?;
        Ok(post)
    }

    /// [`Store::filled`] for every one of `posts`, in three statements rather than
    /// three for each: each is a round trip, and a database may be far away.
    fn all_filled(&self, mut posts: Vec<Post>) -> Result<Vec<Post>> {
        if posts.len() < 2 {
            return posts.into_iter().map(|post| self.filled(post)).collect();
        }
        let ids: Vec<wyvrn_service::db::Value> = posts.iter().map(|post| wyvrn_service::db::ToValue::to_value(&post.id)).collect();
        let place: std::collections::HashMap<String, usize> = posts.iter().enumerate().map(|(n, post)| (post.id.clone(), n)).collect();
        let at = |id: &str| place.get(id).copied();
        let mut db = self.db.lock();
        let reactions = db.query(
            &format!("SELECT post, emoji, did FROM reactions WHERE post IN ({}) ORDER BY at, emoji, did", marks(ids.len())),
            &ids,
            |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?, row.get::<String>(2)?)),
        )?;
        for (id, emoji, did) in reactions {
            let Some(n) = at(&id) else { continue };
            let post = &mut posts[n];
            match post.reactions.iter_mut().find(|(known, _)| *known == emoji) {
                Some((_, by)) => by.push(did),
                None => post.reactions.push((emoji, vec![did])),
            }
        }
        let polls: Vec<wyvrn_service::db::Value> = posts
            .iter()
            .filter(|post| post.extra.as_ref().is_some_and(|extra| extra.get("poll").is_some()))
            .map(|post| wyvrn_service::db::ToValue::to_value(&post.id))
            .collect();
        if !polls.is_empty() {
            let votes = db.query(&format!("SELECT post, did, answers FROM votes WHERE post IN ({}) ORDER BY at, did", marks(polls.len())), &polls, |row| {
                Ok((row.get::<String>(0)?, row.get::<String>(1)?, list_from(row.get(2)?)))
            })?;
            for (id, did, answers) in votes {
                if let Some(n) = at(&id) {
                    posts[n].votes.push((did, answers));
                }
            }
        }
        let threads = db.query(
            &format!(
                "SELECT id, channel, name, created_by, created_at, (SELECT sticky FROM thread_flags WHERE thread_flags.thread = threads.id), (SELECT locked FROM thread_flags WHERE thread_flags.thread = threads.id), \
                 (SELECT section FROM thread_flags WHERE thread_flags.thread = threads.id), (SELECT 1 FROM private_threads WHERE private_threads.thread = threads.id) FROM threads WHERE id IN ({})",
                marks(ids.len())
            ),
            &ids,
            |row| {
                Ok(Thread {
                    id: row.get(0)?,
                    channel: row.get(1)?,
                    name: row.get(2)?,
                    created_by: row.get(3)?,
                    created_at: row.get(4)?,
                    count: 0,
                    last_at: None,
                    last_by: None,
                    sticky: row.get::<Option<i64>>(5)?.unwrap_or(0) != 0,
                    locked: row.get::<Option<i64>>(6)?.unwrap_or(0) != 0,
                    section: row.get(7)?,
                    private: row.get::<Option<i64>>(8)?.is_some(),
                })
            },
        )?;
        let grown: Vec<String> = threads.iter().map(|thread| thread.id.clone()).collect();
        let mut tallies = Self::thread_tallies(&mut db, &grown)?;
        for mut thread in threads {
            if let Some(tally) = tallies.remove(&thread.id) {
                (thread.count, thread.last_at, thread.last_by) = (tally.count, tally.last_at, tally.last_by);
            }
            if let Some(n) = at(&thread.id) {
                posts[n].grew = Some(thread);
            }
        }
        Ok(posts)
    }

    /// Gives `post` the next place among changes, making the record of what is said
    /// of it if there is none yet.
    fn touch(db: &mut wyvrn_service::db::Session<'_>, post: &str) -> Result<i64> {
        let rev: i64 = db.query_value("SELECT COALESCE(MAX(rev), 0) + 1 FROM post_state", params![])?;
        if db.execute("UPDATE post_state SET rev = ?2 WHERE post = ?1", params![post, rev])? == 0 {
            db.execute("INSERT INTO post_state (post, rev) VALUES (?1, ?2)", params![post, rev])?;
        }
        Ok(rev)
    }

    /// The place the last change took: what someone who has all of them holds.
    pub fn latest_rev(&self) -> Result<i64> {
        self.db.lock().query_value("SELECT COALESCE(MAX(rev), 0) FROM post_state", params![])
    }

    /// The latest change to any post of `channel` (its `rev`): what the web view's
    /// live page watches.
    pub fn channel_rev(&self, channel: &str) -> Result<i64> {
        self.db.lock().query_value("SELECT COALESCE(MAX(state.rev), 0) FROM post_state state JOIN posts ON posts.id = state.post WHERE posts.channel = ?1", params![channel])
    }

    /// Every post written or changed after `rev`, in the order that happened.
    pub fn posts_changed_after(&self, rev: i64, limit: usize) -> Result<Vec<Post>> {
        let posts = self.db.lock().query(
            &format!("SELECT {POST_COLUMNS} FROM posts JOIN post_state state ON state.post = posts.id WHERE state.rev > ?1 ORDER BY state.rev LIMIT ?2"),
            params![rev, limit as i64],
            post_from,
        )?;
        self.all_filled(posts)
    }

    /// What a post is said to answer, be in and call on, set as it is written.
    /// `reply` is the post answered, its author, and the answerer's quotation of it.
    pub fn set_written_with(&self, id: &str, reply: Option<(&str, &str, Option<&str>)>, thread: Option<&str>, mentions: &[Mention]) -> Result<()> {
        let mut db = self.db.lock();
        Self::touch(&mut db, id)?;
        let mentions = (!mentions.is_empty()).then(|| serde_json::to_string(mentions).unwrap_or_default());
        let (reply_to, author, snippet) = match reply {
            Some((id, author, snippet)) => (Some(id), Some(author), snippet),
            None => (None, None, None),
        };
        db.execute(
            "UPDATE post_state SET reply_to = ?2, thread = ?3, mentions = ?4, reply_author = ?5, reply_snippet = ?6 WHERE post = ?1",
            params![id, reply_to, thread, mentions, author, snippet],
        )?;
        if let Some(thread) = thread {
            Self::forget_thread(&mut db, thread)?;
        }
        Ok(())
    }

    pub fn edit_post(&self, id: &str, content: &str, mentions: &[Mention], now: i64) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("UPDATE posts SET content = ?2 WHERE id = ?1", params![id, content])?;
        Self::touch(&mut db, id)?;
        let mentions = (!mentions.is_empty()).then(|| serde_json::to_string(mentions).unwrap_or_default());
        db.execute("UPDATE post_state SET edited_at = ?2, mentions = ?3 WHERE post = ?1", params![id, now, mentions])?;
        Ok(())
    }

    /// Takes a post back: what it said and asked is gone, and so is what was put on it.
    /// `removed_by` is who did, when it wasn't its author.
    /// `keep_text`: what it said is kept for moderators (`deleted_text`), for as long
    /// as the community's `deleted_text_days`.
    pub fn delete_post(&self, id: &str, removed_by: Option<&str>, keep_text: bool) -> Result<()> {
        let mut db = self.db.lock();
        if let Some(removed_by) = removed_by {
            db.execute("INSERT INTO post_removals (post, removed_by) VALUES (?1, ?2)", params![id, removed_by])?;
        }
        // What it said, kept a while for moderators (asked 2026-11-21: "see what was
        // deleted"); nobody else is ever given it.
        if keep_text {
            db.execute(
                "INSERT OR REPLACE INTO post_deleted_text (post, content, deleted_at) SELECT id, content, ?2 FROM posts WHERE id = ?1 AND content <> ''",
                params![id, crate::gates::now()],
            )?;
        }
        db.execute("UPDATE posts SET content = '', question = NULL, answer = NULL WHERE id = ?1", params![id])?;
        db.execute("DELETE FROM reactions WHERE post = ?1", params![id])?;
        db.execute("DELETE FROM post_extra WHERE post = ?1", params![id])?;
        db.execute("DELETE FROM post_censored WHERE post = ?1", params![id])?;
        db.execute("DELETE FROM votes WHERE post = ?1", params![id])?;
        Self::touch(&mut db, id)?;
        db.execute("UPDATE post_state SET deleted = 1, pinned = 0, mentions = NULL WHERE post = ?1", params![id])?;
        db.execute(&format!("DELETE FROM cached_counts WHERE kind = '{COUNT_FORUM_POSTS}' AND subject = (SELECT author_did FROM posts WHERE id = ?1)"), params![id])?;
        Ok(())
    }

    /// Marks a post changed (its `rev`), for what changed of it beside its own
    /// fields (published).
    pub fn touch_post(&self, id: &str) -> Result<()> {
        let mut db = self.db.lock();
        Self::touch(&mut db, id)?;
        Ok(())
    }

    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<()> {
        let mut db = self.db.lock();
        Self::touch(&mut db, id)?;
        db.execute("UPDATE post_state SET pinned = ?2 WHERE post = ?1", params![id, pinned])?;
        Ok(())
    }

    /// Makes `emoji` the whole of what `did` has put on a post. `false` if that is
    /// what was there already.
    pub fn set_reactions(&self, id: &str, did: &str, emoji: &[String], now: i64) -> Result<bool> {
        let mut db = self.db.lock();
        let had: Vec<String> = db.query("SELECT emoji FROM reactions WHERE post = ?1 AND did = ?2 ORDER BY emoji", params![id, did], |row| row.get::<String>(0))?;
        let mut wanted = emoji.to_vec();
        wanted.sort();
        wanted.dedup();
        if had == wanted {
            return Ok(false);
        }
        for gone in had.iter().filter(|emoji| !wanted.contains(emoji)) {
            db.execute("DELETE FROM reactions WHERE post = ?1 AND did = ?2 AND emoji = ?3", params![id, did, gone.clone()])?;
        }
        for new in wanted.iter().filter(|emoji| !had.contains(emoji)) {
            db.execute("INSERT OR IGNORE INTO reactions (post, did, emoji, at) VALUES (?1, ?2, ?3, ?4)", params![id, did, new.clone(), now])?;
        }
        Self::touch(&mut db, id)?;
        Ok(true)
    }

    /// How many different emoji a post has on it.
    pub fn reaction_kinds(&self, id: &str) -> Result<i64> {
        self.db.lock().query_value("SELECT COUNT(DISTINCT emoji) FROM reactions WHERE post = ?1", params![id])
    }

    // --- Threads ---------------------------------------------------------------

    pub fn thread(&self, id: &str) -> Result<Option<Thread>> {
        let mut db = self.db.lock();
        let found = db.query_row(
            "SELECT id, channel, name, created_by, created_at, (SELECT sticky FROM thread_flags WHERE thread_flags.thread = threads.id), (SELECT locked FROM thread_flags WHERE thread_flags.thread = threads.id), \
             (SELECT section FROM thread_flags WHERE thread_flags.thread = threads.id), (SELECT 1 FROM private_threads WHERE private_threads.thread = threads.id) FROM threads WHERE id = ?1",
            params![id],
            |row| {
                Ok(Thread {
                    id: row.get(0)?,
                    channel: row.get(1)?,
                    name: row.get(2)?,
                    created_by: row.get(3)?,
                    created_at: row.get(4)?,
                    count: 0,
                    last_at: None,
                    last_by: None,
                    sticky: row.get::<Option<i64>>(5)?.unwrap_or(0) != 0,
                    locked: row.get::<Option<i64>>(6)?.unwrap_or(0) != 0,
                    section: row.get(7)?,
                    private: row.get::<Option<i64>>(8)?.is_some(),
                })
            },
        )?;
        let Some(mut thread) = found else { return Ok(None) };
        if let Some(tally) = Self::thread_tallies(&mut db, std::slice::from_ref(&thread.id))?.remove(&thread.id) {
            (thread.count, thread.last_at, thread.last_by) = (tally.count, tally.last_at, tally.last_by);
        }
        Ok(Some(thread))
    }

    /// A thread's standing in a forum: sticky, locked, its section.
    pub fn set_thread_flags(&self, thread: &str, sticky: bool, locked: bool, section: Option<&str>) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("INSERT OR REPLACE INTO thread_flags (thread, sticky, locked, section) VALUES (?1, ?2, ?3, ?4)", params![thread, sticky as i64, locked as i64, section])?;
        Self::forget_thread(&mut db, thread)?;
        Ok(())
    }

    pub fn set_forum_sections(&self, channel: &str, sections: &Value) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO forum_sections (channel, sections) VALUES (?1, ?2)", params![channel, sections.to_string()])?;
        Ok(())
    }

    pub fn signature(&self, did: &str) -> Result<Option<String>> {
        self.db.lock().query_row("SELECT signature FROM member_signatures WHERE did = ?1", params![did], |row| row.get(0))
    }

    pub fn set_signature(&self, did: &str, signature: Option<&str>) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM member_signatures WHERE did = ?1", params![did])?;
        if let Some(signature) = signature {
            db.execute("INSERT INTO member_signatures (did, signature) VALUES (?1, ?2)", params![did, signature])?;
        }
        Ok(())
    }

    /// How many posts each of the first 500 to post in `in_channel` has in the
    /// community's forums, from the kept counts ([`Self::rebuild_counts`]).
    pub fn forum_post_counts(&self, in_channel: &str) -> Result<Vec<(String, i64)>> {
        let mut db = self.db.lock();
        let authors: Vec<String> = db.query("SELECT DISTINCT author_did FROM posts WHERE channel = ?1 LIMIT 500", params![in_channel], |row| row.get(0))?;
        let mut kept = std::collections::HashMap::new();
        for chunk in authors.chunks(200) {
            let values: Vec<wyvrn_service::db::Value> = chunk.iter().map(wyvrn_service::db::ToValue::to_value).collect();
            for (did, n) in db.query(
                &format!("SELECT subject, n FROM cached_counts WHERE kind = '{COUNT_FORUM_POSTS}' AND subject IN ({})", marks(chunk.len())),
                &values,
                |row| Ok((row.get::<String>(0)?, row.get::<i64>(1)?)),
            )? {
                kept.insert(did, n);
            }
        }
        let mut out = Vec::new();
        for author in authors {
            let n = match kept.get(&author) {
                Some(n) => *n,
                None => Self::count_forum_posts(&mut db, &author)?,
            };
            out.push((author, n));
        }
        Ok(out)
    }

    /// Each of `sections` of the forum `channel` (and `None`, the threads in no
    /// section): its threads, posts and newest thread, from the kept counts.
    pub fn section_tallies(&self, channel: &str, sections: &[String]) -> Result<Vec<SectionTally>> {
        let mut db = self.db.lock();
        let mut out = Vec::new();
        for section in std::iter::once(None).chain(sections.iter().map(|s| Some(s.as_str()))) {
            out.push(Self::section_tally(&mut db, channel, section)?);
        }
        Ok(out)
    }

    /// Clears every kept count and works each out again from what is stored:
    /// for when they are thought wrong. Returns how many threads, members and
    /// sections were counted.
    pub fn rebuild_counts(&self) -> Result<(usize, usize, usize)> {
        let forums: Vec<(String, Vec<String>)> = self
            .channels()?
            .into_iter()
            .filter(|channel| channel.forum.is_some())
            .map(|channel| {
                let sections = channel.sections.as_ref().and_then(Value::as_array).map(|list| list.iter().filter_map(|s| s["id"].as_str().map(str::to_string)).collect()).unwrap_or_default();
                (channel.id, sections)
            })
            .collect();
        let mut db = self.db.lock();
        db.execute("DELETE FROM cached_counts", params![])?;
        let threads: Vec<String> = db.query("SELECT id FROM threads", params![], |row| row.get(0))?;
        for chunk in threads.chunks(200) {
            Self::thread_tallies(&mut db, chunk)?;
        }
        let authors: Vec<String> = db.query(&format!("SELECT DISTINCT author_did FROM posts WHERE {IN_FORUMS}"), params![], |row| row.get(0))?;
        for author in &authors {
            Self::count_forum_posts(&mut db, author)?;
        }
        let mut sections = 0;
        for (channel, ids) in &forums {
            for section in std::iter::once(None).chain(ids.iter().map(|s| Some(s.as_str()))) {
                Self::section_tally(&mut db, channel, section)?;
                sections += 1;
            }
        }
        Ok((threads.len(), authors.len(), sections))
    }

    /// The threads `ids`' counts, kept or else worked out (and then kept).
    fn thread_tallies(db: &mut wyvrn_service::db::Session<'_>, ids: &[String]) -> Result<std::collections::HashMap<String, ThreadTally>> {
        let mut out = std::collections::HashMap::new();
        for chunk in ids.chunks(200) {
            let values: Vec<wyvrn_service::db::Value> = chunk.iter().map(wyvrn_service::db::ToValue::to_value).collect();
            for (id, tally) in db.query(
                &format!("SELECT subject, n, at, ref_id FROM cached_counts WHERE kind = '{COUNT_THREAD}' AND subject IN ({})", marks(chunk.len())),
                &values,
                |row| Ok((row.get::<String>(0)?, ThreadTally { count: row.get(1)?, last_at: row.get(2)?, last_by: row.get(3)? })),
            )? {
                out.insert(id, tally);
            }
        }
        for id in ids {
            if out.contains_key(id) {
                continue;
            }
            const IN: &str = "SELECT post FROM post_state WHERE thread = ?1";
            let count: i64 = db.query_value("SELECT COUNT(*) FROM post_state WHERE thread = ?1", params![id])?;
            let last = db.query_row(&format!("SELECT posted_at, author_did FROM posts WHERE id IN ({IN}) ORDER BY serial DESC LIMIT 1"), params![id], |row| {
                Ok((row.get::<i64>(0)?, row.get::<String>(1)?))
            })?;
            let tally = ThreadTally { count, last_at: last.as_ref().map(|(at, _)| *at), last_by: last.map(|(_, by)| by) };
            db.execute(
                &format!("INSERT OR REPLACE INTO cached_counts (kind, subject, n, m, at, ref_id) VALUES ('{COUNT_THREAD}', ?1, ?2, NULL, ?3, ?4)"),
                params![id, tally.count, tally.last_at, tally.last_by.clone()],
            )?;
            out.insert(id.clone(), tally);
        }
        Ok(out)
    }

    /// How many posts `did` has in the forums (not galleries, not the trash, not
    /// taken back), worked out and kept.
    fn count_forum_posts(db: &mut wyvrn_service::db::Session<'_>, did: &str) -> Result<i64> {
        let n: i64 = db.query_value(
            &format!("SELECT COUNT(*) FROM posts WHERE author_did = ?1 AND {IN_FORUMS} AND NOT EXISTS (SELECT 1 FROM post_state WHERE post_state.post = posts.id AND post_state.deleted = 1)"),
            params![did],
        )?;
        db.execute(&format!("INSERT OR REPLACE INTO cached_counts (kind, subject, n) VALUES ('{COUNT_FORUM_POSTS}', ?1, ?2)"), params![did, n])?;
        Ok(n)
    }

    /// A section's counts, kept or else worked out (and then kept). A section's
    /// posts are its threads' first posts and every post in them.
    fn section_tally(db: &mut wyvrn_service::db::Session<'_>, channel: &str, section: Option<&str>) -> Result<SectionTally> {
        let subject = section_subject(channel, section);
        let kept = db.query_row(
            &format!("SELECT n, m, at, ref_id FROM cached_counts WHERE kind = '{COUNT_SECTION}' AND subject = ?1"),
            params![subject],
            |row| Ok((row.get::<i64>(0)?, row.get::<Option<i64>>(1)?, row.get::<Option<i64>>(2)?, row.get::<Option<String>>(3)?)),
        )?;
        let (threads, posts, last_at, last_thread) = match kept {
            Some((n, m, at, newest)) => (n, m.unwrap_or(n), at, newest),
            None => {
                const IN_SECTION: &str = "threads.channel = ?1 AND COALESCE((SELECT section FROM thread_flags WHERE thread_flags.thread = threads.id), '') = ?2";
                let given = section.unwrap_or_default();
                let threads: i64 = db.query_value(&format!("SELECT COUNT(*) FROM threads WHERE {IN_SECTION}"), params![channel, given])?;
                let answers: i64 = db.query_value(&format!("SELECT COUNT(*) FROM post_state WHERE thread IN (SELECT id FROM threads WHERE {IN_SECTION})"), params![channel, given])?;
                let newest = db.query_row(
                    &format!(
                        "SELECT id, COALESCE((SELECT MAX(posted_at) FROM posts WHERE posts.id IN (SELECT post FROM post_state WHERE post_state.thread = threads.id)), created_at) AS last \
                         FROM threads WHERE {IN_SECTION} ORDER BY last DESC LIMIT 1"
                    ),
                    params![channel, given],
                    |row| Ok((row.get::<String>(0)?, row.get::<i64>(1)?)),
                )?;
                let (last_thread, last_at) = match newest {
                    Some((id, at)) => (Some(id), Some(at)),
                    None => (None, None),
                };
                db.execute(
                    &format!("INSERT OR REPLACE INTO cached_counts (kind, subject, n, m, at, ref_id) VALUES ('{COUNT_SECTION}', ?1, ?2, ?3, ?4, ?5)"),
                    params![subject, threads, threads + answers, last_at, last_thread.clone()],
                )?;
                (threads, threads + answers, last_at, last_thread)
            }
        };
        Ok(SectionTally { section: section.map(str::to_string), threads, posts, last_at, last_thread })
    }

    /// Forgets the kept counts a change to `thread` makes wrong: its own, and its
    /// forum's sections'.
    fn forget_thread(db: &mut wyvrn_service::db::Session<'_>, thread: &str) -> Result<()> {
        db.execute(&format!("DELETE FROM cached_counts WHERE kind = '{COUNT_THREAD}' AND subject = ?1"), params![thread])?;
        if let Some(channel) = db.query_row("SELECT channel FROM threads WHERE id = ?1", params![thread], |row| row.get::<String>(0))? {
            Self::forget_sections(db, &channel)?;
        }
        Ok(())
    }

    /// Forgets the kept counts of every section of the forum `channel`.
    fn forget_sections(db: &mut wyvrn_service::db::Session<'_>, channel: &str) -> Result<()> {
        db.execute(
            &format!("DELETE FROM cached_counts WHERE kind = '{COUNT_SECTION}' AND subject >= ?1 AND subject < ?2"),
            params![format!("{channel}{SECTION_MARK}"), format!("{channel}{}", char::from(SECTION_MARK as u8 + 1))],
        )?;
        Ok(())
    }

    /// Forgets every member's forum post count: for when which channels are
    /// forums, or in the trash, changes.
    fn forget_forum_posts(db: &mut wyvrn_service::db::Session<'_>) -> Result<()> {
        db.execute(&format!("DELETE FROM cached_counts WHERE kind = '{COUNT_FORUM_POSTS}'"), params![])?;
        Ok(())
    }

    /// Grows a thread from the post `id`. `false` if one had grown from it already.
    pub fn add_thread(&self, id: &str, channel: &str, name: Option<&str>, by: &str, now: i64) -> Result<bool> {
        let mut db = self.db.lock();
        let made = db.execute("INSERT OR IGNORE INTO threads (id, channel, name, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5)", params![id, channel, name, by, now])? > 0;
        if made {
            Self::touch(&mut db, id)?;
            Self::forget_sections(&mut db, channel)?;
        }
        Ok(made)
    }

    pub fn rename_thread(&self, id: &str, name: Option<&str>) -> Result<()> {
        self.db.lock().execute("UPDATE threads SET name = ?2 WHERE id = ?1", params![id, name])?;
        Ok(())
    }

    /// Whether the thread `id` is private.
    pub fn is_private_thread(&self, id: &str) -> Result<bool> {
        Ok(self.db.lock().query_row("SELECT 1 FROM private_threads WHERE thread = ?1", params![id], |_| Ok(()))?.is_some())
    }

    /// Makes the thread `id` private, with `by` in it.
    pub fn make_thread_private(&self, id: &str, by: &str, now: i64) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("INSERT OR IGNORE INTO private_threads (thread) VALUES (?1)", params![id])?;
        db.execute("INSERT OR IGNORE INTO thread_members (thread, did, added_by, at) VALUES (?1, ?2, ?2, ?3)", params![id, by, now])?;
        Ok(())
    }

    /// Whether `did` is in the private thread `id`.
    pub fn in_thread(&self, id: &str, did: &str) -> Result<bool> {
        Ok(self.db.lock().query_row("SELECT 1 FROM thread_members WHERE thread = ?1 AND did = ?2", params![id, did], |_| Ok(()))?.is_some())
    }

    /// Who is in the private thread `id`.
    pub fn thread_members(&self, id: &str) -> Result<Vec<String>> {
        self.db.lock().query("SELECT did FROM thread_members WHERE thread = ?1 ORDER BY at", params![id], |row| row.get(0))
    }

    /// Puts `did` in the private thread `id`, or takes them out.
    pub fn set_in_thread(&self, id: &str, did: &str, by: &str, inside: bool, now: i64) -> Result<()> {
        let mut db = self.db.lock();
        if inside {
            db.execute("INSERT OR IGNORE INTO thread_members (thread, did, added_by, at) VALUES (?1, ?2, ?3, ?4)", params![id, did, by, now])?;
        } else {
            db.execute("DELETE FROM thread_members WHERE thread = ?1 AND did = ?2", params![id, did])?;
        }
        Ok(())
    }

    /// Notes that the thread `id` has changed (a post was added to it), so that the
    /// post it grew from is told of again with its count.
    pub fn touch_thread(&self, id: &str) -> Result<()> {
        Self::touch(&mut self.db.lock(), id).map(|_| ())
    }

    /// The posts a reply chain is made of: `id`, what it answers, and so on back.
    pub fn chain_authors(&self, id: &str) -> Result<Vec<String>> {
        let mut authors = Vec::new();
        let mut at = Some(id.to_string());
        // (A chain is as long as people made it; a loop in one is nobody's doing.)
        for _ in 0..200 {
            let Some(id) = at else { break };
            let Some(post) = self.post_by_id(&id)? else { break };
            authors.push(post.author_did);
            at = post.reply_to;
        }
        Ok(authors)
    }

    /// Everyone who has answered `id`, or answered an answer to it: the rest of its chain.
    pub fn repliers(&self, id: &str) -> Result<Vec<String>> {
        let mut authors = Vec::new();
        let mut frontier = vec![id.to_string()];
        for _ in 0..200 {
            let Some(at) = frontier.pop() else { break };
            let answers = self.db.lock().query(
                "SELECT posts.id, posts.author_did FROM posts JOIN post_state state ON state.post = posts.id WHERE state.reply_to = ?1",
                params![at],
                |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)),
            )?;
            for (answer, author) in answers {
                authors.push(author);
                frontier.push(answer);
            }
        }
        Ok(authors)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_post(&self, channel: &str, id: &str, content: &str, author: &Member, question: Option<&Value>, answer: Option<&Value>, now: i64) -> Result<Post> {
        let mut db = self.db.lock();
        // Asked first: MySQL won't read a table in the values of an insert into it.
        let seq: i64 = db.query_value("SELECT COALESCE(MAX(seq), 0) + 1 FROM posts WHERE channel = ?1", params![channel])?;
        db.execute(
            "INSERT INTO posts (channel, seq, id, content, posted_at, author_did, author_name, question, answer)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![channel, seq, id, content, now, author.did, author.name, question.map(Value::to_string), answer.map(Value::to_string)],
        )?;
        db.execute(&format!("DELETE FROM cached_counts WHERE kind = '{COUNT_FORUM_POSTS}' AND subject = ?1"), params![author.did])?;
        db.query_row(&format!("SELECT {POST_COLUMNS} FROM posts WHERE id = ?1"), params![id], post_from)?
            .ok_or_else(|| wyvrn_service::db::Error::Other("a post just written is not there".into()))
    }

    /// The post at `seq` in `channel`, in a thread or not.
    pub fn post_at(&self, channel: &str, seq: i64) -> Result<Option<Post>> {
        let post = self.db.lock().query_row(&format!("SELECT {POST_COLUMNS} FROM posts WHERE channel = ?1 AND seq = ?2"), params![channel, seq], post_from)?;
        post.map(|post| self.filled(post)).transpose()
    }

    pub fn post_by_id(&self, id: &str) -> Result<Option<Post>> {
        let post = self.db.lock().query_row(&format!("SELECT {POST_COLUMNS} FROM posts WHERE id = ?1"), params![id], post_from)?;
        post.map(|post| self.filled(post)).transpose()
    }

    /// Records who answered the question `id` asks. `false` if someone already had.
    pub fn set_answered(&self, id: &str, by: &str) -> Result<bool> {
        Ok(self.db.lock().execute("UPDATE posts SET answered_by = ?2 WHERE id = ?1 AND answered_by IS NULL", params![id, by])? > 0)
    }

    /// Every post after `serial`, oldest first: what is still to send to someone.
    pub fn posts_after(&self, serial: i64, limit: usize) -> Result<Vec<Post>> {
        // Imported history is read back, never sent as new.
        let posts = self.db.lock().query(
            &format!("SELECT {POST_COLUMNS} FROM posts WHERE serial > ?1 AND {NOT_IMPORTED} ORDER BY serial LIMIT ?2"),
            params![serial, limit as i64],
            post_from,
        )?;
        self.all_filled(posts)
    }

    /// A channel's posts before `before` (its newest, with none given), newest first.
    ///
    /// With `thread`, the posts in that thread; without, the channel's own, which
    /// are the ones in no thread.
    pub fn channel_posts_before(&self, channel: &str, thread: Option<&str>, before: Option<i64>, limit: usize) -> Result<Vec<Post>> {
        const IN_THREAD: &str = "(SELECT thread FROM post_state WHERE post_state.post = posts.id)";
        let posts = match thread {
            Some(thread) => self.db.lock().query(
                &format!("SELECT {POST_COLUMNS} FROM posts WHERE channel = ?1 AND seq < ?2 AND {IN_THREAD} = ?4 ORDER BY seq DESC LIMIT ?3"),
                params![channel, before.unwrap_or(i64::MAX), limit as i64, thread],
                post_from,
            )?,
            None => self.db.lock().query(
                &format!("SELECT {POST_COLUMNS} FROM posts WHERE channel = ?1 AND seq < ?2 AND {IN_THREAD} IS NULL ORDER BY seq DESC LIMIT ?3"),
                params![channel, before.unwrap_or(i64::MAX), limit as i64],
                post_from,
            )?,
        };
        self.all_filled(posts)
    }

    /// A channel's posts by time (then number, for posts at the same second):
    /// those before `before`, `(posted_at, seq)`, or its newest with none given,
    /// newest first. As history is read where posts were brought in from elsewhere,
    /// whose numbers are newer than their times.
    pub fn channel_posts_by_time(&self, channel: &str, thread: Option<&str>, before: Option<(i64, i64)>, limit: usize) -> Result<Vec<Post>> {
        const IN_THREAD: &str = "(SELECT thread FROM post_state WHERE post_state.post = posts.id)";
        let in_thread = match thread {
            Some(_) => format!("{IN_THREAD} = ?5"),
            None => format!("{IN_THREAD} IS NULL AND ?5 IS NULL"),
        };
        let (at, seq) = before.unwrap_or((i64::MAX, i64::MAX));
        let posts = self.db.lock().query(
            &format!(
                "SELECT {POST_COLUMNS} FROM posts WHERE channel = ?1 AND (posted_at < ?2 OR (posted_at = ?2 AND seq < ?3)) AND {in_thread} \
                 ORDER BY posted_at DESC, seq DESC LIMIT ?4"
            ),
            params![channel, at, seq, limit as i64, thread],
            post_from,
        )?;
        self.all_filled(posts)
    }

    /// The posts brought in from elsewhere, by channel: each run's first and last `seq`.
    pub fn imported_ranges(&self) -> Result<std::collections::HashMap<String, Vec<(i64, i64)>>> {
        let rows: Vec<(String, i64, i64)> =
            self.db.lock().query("SELECT channel, from_seq, to_seq FROM imported_ranges ORDER BY channel, from_seq", params![], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        let mut ranges: std::collections::HashMap<String, Vec<(i64, i64)>> = std::collections::HashMap::new();
        for (channel, from, to) in rows {
            ranges.entry(channel).or_default().push((from, to));
        }
        Ok(ranges)
    }

    /// A page of the thread `thread` of `channel`, oldest first: `limit` posts
    /// from the `offset`th.
    pub fn thread_page(&self, channel: &str, thread: &str, offset: usize, limit: usize) -> Result<Vec<Post>> {
        // (Found from the thread's posts, by post_state_by_thread, not from the channel's.)
        let posts = self.db.lock().query(
            &format!("SELECT {POST_COLUMNS} FROM posts WHERE channel = ?1 AND id IN (SELECT post FROM post_state WHERE thread = ?2) ORDER BY seq LIMIT ?3 OFFSET ?4"),
            params![channel, thread, limit as i64, offset as i64],
            post_from,
        )?;
        self.all_filled(posts)
    }

    /// How many posts of the thread `thread` of `channel` are at or before `seq`:
    /// where the first after it is, counting from 0.
    pub fn thread_posts_through(&self, channel: &str, thread: &str, seq: i64) -> Result<i64> {
        self.db.lock().query_value(
            "SELECT COUNT(*) FROM posts WHERE channel = ?1 AND seq <= ?3 AND id IN (SELECT post FROM post_state WHERE thread = ?2)",
            params![channel, thread, seq],
        )
    }

    /// Where the post `post` is in `channel`'s thread `thread` (its `seq`), if it is in it.
    pub fn thread_post_seq(&self, channel: &str, thread: &str, post: &str) -> Result<Option<i64>> {
        self.db.lock().query_row(
            "SELECT seq FROM posts WHERE id = ?1 AND channel = ?2 AND id IN (SELECT post FROM post_state WHERE thread = ?3)",
            params![post, channel, thread],
            |row| row.get(0),
        )
    }

    /// The posts of `channel` (in `thread`, or in none) around the time `at`: up to
    /// `half` before it and `half` from it, oldest first; and whether there are
    /// older ones, and newer ones, beyond those.
    pub fn channel_posts_around(&self, channel: &str, thread: Option<&str>, at: i64, half: usize) -> Result<(Vec<Post>, bool, bool)> {
        const IN_THREAD: &str = "(SELECT thread FROM post_state WHERE post_state.post = posts.id)";
        let in_thread = match thread {
            Some(_) => format!("{IN_THREAD} = ?4"),
            None => format!("{IN_THREAD} IS NULL AND ?4 IS NULL"),
        };
        let mut before = self.db.lock().query(
            &format!("SELECT {POST_COLUMNS} FROM posts WHERE channel = ?1 AND posted_at < ?2 AND {in_thread} ORDER BY posted_at DESC, seq DESC LIMIT ?3"),
            params![channel, at, half as i64 + 1, thread],
            post_from,
        )?;
        let mut after = self.db.lock().query(
            &format!("SELECT {POST_COLUMNS} FROM posts WHERE channel = ?1 AND posted_at >= ?2 AND {in_thread} ORDER BY posted_at, seq LIMIT ?3"),
            params![channel, at, half as i64 + 1, thread],
            post_from,
        )?;
        let (older, newer) = (before.len() > half, after.len() > half);
        before.truncate(half);
        after.truncate(half);
        before.reverse();
        before.extend(after);
        Ok((self.all_filled(before)?, older, newer))
    }

    /// Posts whose text has each of `words`, whatever its case, not taken back:
    /// newest first, older than `before` (a serial), in `channel` and by `author`
    /// where given.
    /// `within`, if given, the channels to search (and no others).
    pub fn search(&self, words: &[String], channel: Option<&str>, author: Option<&str>, within: Option<&[String]>, before: Option<i64>, limit: usize) -> Result<Vec<Post>> {
        let mut sql = format!(
            "SELECT {POST_COLUMNS} FROM posts WHERE serial < ?1 AND (?2 IS NULL OR channel = ?2) AND (?3 IS NULL OR author_did = ?3) \
             AND NOT EXISTS (SELECT 1 FROM post_state WHERE post_state.post = posts.id AND deleted = 1)"
        );
        let mut values = params![before.unwrap_or(i64::MAX), channel, author, limit as i64].to_vec();
        for word in words {
            // (`!` escapes, not `\\`: MySQL reads a backslash in a string as an escape itself.)
            let pattern = format!("%{}%", word.to_lowercase().replace('!', "!!").replace('%', "!%").replace('_', "!_"));
            values.push(wyvrn_service::db::ToValue::to_value(&pattern));
            sql.push_str(&format!(" AND LOWER(content) LIKE ?{} ESCAPE '!'", values.len()));
        }
        if let Some(within) = within {
            if within.is_empty() {
                return Ok(Vec::new());
            }
            let mut marks = Vec::new();
            for channel in within {
                values.push(wyvrn_service::db::ToValue::to_value(channel));
                marks.push(format!("?{}", values.len()));
            }
            sql.push_str(&format!(" AND channel IN ({})", marks.join(", ")));
        }
        sql.push_str(" ORDER BY serial DESC LIMIT ?4");
        let posts = self.db.lock().query(&sql, &values, post_from)?;
        self.all_filled(posts)
    }

    /// Records what AutoMod censored of a post: what its author wrote, and the
    /// rules that censored it. `None` says it wasn't censored (an edit AutoMod let be).
    pub fn set_censored(&self, id: &str, censored: Option<(&str, &[String])>) -> Result<()> {
        let mut db = self.db.lock();
        match censored {
            Some((original, rules)) => {
                let rules = serde_json::to_string(rules).unwrap_or_default();
                db.execute("INSERT OR REPLACE INTO post_censored (post, original, rules) VALUES (?1, ?2, ?3)", params![id, original, rules])?;
            }
            None => {
                db.execute("DELETE FROM post_censored WHERE post = ?1", params![id])?;
            }
        }
        Ok(())
    }

    /// What a post said before AutoMod censored it, if it did.
    /// Every webhook token still kept as itself, kept as its hash instead
    /// (security review WYV-63): its URL goes on working, and a copy of the
    /// database posts nowhere. Says how many.
    pub fn hash_plain_webhook_tokens(&self) -> Result<usize> {
        let plain: Vec<(String, String)> =
            self.db.lock().query("SELECT id, token FROM webhooks WHERE token NOT LIKE 'sha256:%'", params![], |row| Ok((row.get(0)?, row.get(1)?)))?;
        for (id, token) in &plain {
            self.db.lock().execute("UPDATE webhooks SET token = ?2 WHERE id = ?1", params![id, crate::webhooks::kept_token(token)])?;
        }
        Ok(plain.len())
    }

    pub fn webhooks(&self) -> Result<Vec<Webhook>> {
        self.db.lock().query("SELECT id, channel, name, avatar, token, created_by, created_at FROM webhooks ORDER BY created_at, id", params![], webhook_from)
    }

    pub fn webhook(&self, id: &str) -> Result<Option<Webhook>> {
        self.db.lock().query_row("SELECT id, channel, name, avatar, token, created_by, created_at FROM webhooks WHERE id = ?1", params![id], webhook_from)
    }

    pub fn save_webhook(&self, hook: &Webhook) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO webhooks (id, channel, name, avatar, token, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![hook.id, hook.channel, hook.name, hook.avatar, hook.token, hook.created_by, hook.created_at],
        )?;
        Ok(())
    }

    pub fn delete_webhook(&self, id: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM webhooks WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn event_hooks(&self) -> Result<Vec<EventHook>> {
        self.db.lock().query(&format!("SELECT {EVENT_HOOK_COLUMNS} FROM event_hooks ORDER BY created_at, id"), params![], event_hook_from)
    }

    pub fn event_hook(&self, id: &str) -> Result<Option<EventHook>> {
        self.db.lock().query_row(&format!("SELECT {EVENT_HOOK_COLUMNS} FROM event_hooks WHERE id = ?1"), params![id], event_hook_from)
    }

    pub fn save_event_hook(&self, hook: &EventHook) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO event_hooks (id, url, secret, events, channel, created_by, created_at, failures, off_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![hook.id, hook.url, hook.secret, serde_json::to_string(&hook.events).unwrap_or_default(), hook.channel, hook.created_by, hook.created_at, hook.failures, hook.off_at],
        )?;
        Ok(())
    }

    pub fn delete_event_hook(&self, id: &str) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM event_queue WHERE hook = ?1", params![id])?;
        db.execute("DELETE FROM event_hooks WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn queue_event(&self, id: &str, hook: &str, body: &str, now: i64) -> Result<()> {
        self.db.lock().execute("INSERT INTO event_queue (id, hook, body, attempts, next_at) VALUES (?1, ?2, ?3, 0, ?4)", params![id, hook, body, now])?;
        Ok(())
    }

    /// Events due to go now, oldest first: `(id, hook, body, attempts)`.
    pub fn due_events(&self, now: i64, limit: i64) -> Result<Vec<(String, String, String, i64)>> {
        self.db.lock().query("SELECT id, hook, body, attempts FROM event_queue WHERE next_at <= ?1 ORDER BY next_at, id LIMIT ?2", params![now, limit], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
    }

    pub fn event_done(&self, id: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM event_queue WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn event_later(&self, id: &str, attempts: i64, next_at: i64) -> Result<()> {
        self.db.lock().execute("UPDATE event_queue SET attempts = ?2, next_at = ?3 WHERE id = ?1", params![id, attempts, next_at])?;
        Ok(())
    }

    // --- Announcement channels (announcements.rs) --------------------------------

    pub fn announcement_channels(&self) -> Result<std::collections::HashSet<String>> {
        Ok(self.db.lock().query("SELECT channel FROM announce_channels", params![], |row| row.get(0))?.into_iter().collect())
    }

    pub fn is_announcement(&self, channel: &str) -> Result<bool> {
        Ok(self.db.lock().query_row("SELECT channel FROM announce_channels WHERE channel = ?1", params![channel], |row| row.get::<String>(0))?.is_some())
    }

    // --- Events (events.rs) -----------------------------------------------------

    pub fn events(&self) -> Result<Vec<StoredEvent>> {
        self.db.lock().query("SELECT id, data, organizer, sequence, cancelled, created_at, updated_at FROM events ORDER BY created_at, id", params![], event_from)
    }

    pub fn event(&self, id: &str) -> Result<Option<StoredEvent>> {
        self.db.lock().query_row("SELECT id, data, organizer, sequence, cancelled, created_at, updated_at FROM events WHERE id = ?1", params![id], event_from)
    }

    pub fn save_event(&self, e: &StoredEvent) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO events (id, data, organizer, sequence, cancelled, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![e.id, e.data.to_string(), e.organizer, e.sequence, e.cancelled as i64, e.created_at, e.updated_at],
        )?;
        Ok(())
    }

    /// Who answered `event` with what (`accepted`, `tentative`, `declined`).
    pub fn event_rsvps(&self, event: &str) -> Result<Vec<(String, String)>> {
        self.db.lock().query("SELECT did, status FROM event_rsvps WHERE event = ?1 ORDER BY at, did", params![event], |row| Ok((row.get(0)?, row.get(1)?)))
    }

    pub fn set_event_rsvp(&self, event: &str, did: &str, status: &str, now: i64) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM event_rsvps WHERE event = ?1 AND did = ?2", params![event, did])?;
        db.execute("INSERT INTO event_rsvps (event, did, status, at) VALUES (?1, ?2, ?3, ?4)", params![event, did, status, now])?;
        Ok(())
    }

    /// Notes a reminder as sent: whether it wasn't already. Those over a week old are let go.
    pub fn mark_reminder(&self, key: &str, now: i64) -> Result<bool> {
        let mut db = self.db.lock();
        if db.query_row("SELECT at FROM event_reminders WHERE reminder = ?1", params![key], |row| row.get::<i64>(0))?.is_some() {
            return Ok(false);
        }
        db.execute("DELETE FROM event_reminders WHERE at < ?1", params![now - 8 * 24 * 3600])?;
        db.execute("INSERT INTO event_reminders (reminder, at) VALUES (?1, ?2)", params![key, now])?;
        Ok(true)
    }

    pub fn calendar_feed_of(&self, did: &str) -> Result<Option<String>> {
        self.db.lock().query_row("SELECT token FROM calendar_feeds WHERE did = ?1", params![did], |row| row.get(0))
    }

    pub fn calendar_feed_by_token(&self, token: &str) -> Result<Option<String>> {
        self.db.lock().query_row("SELECT did FROM calendar_feeds WHERE token = ?1", params![token], |row| row.get(0))
    }

    pub fn set_calendar_feed(&self, did: &str, token: &str) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM calendar_feeds WHERE did = ?1", params![did])?;
        db.execute("INSERT INTO calendar_feeds (did, token) VALUES (?1, ?2)", params![did, token])?;
        Ok(())
    }

    // --- Wiki channels (wiki.rs) --------------------------------------------------

    pub fn set_wiki_channel(&self, channel: &str) -> Result<()> {
        self.db.lock().execute("INSERT INTO wiki_channels (channel) VALUES (?1)", params![channel])?;
        Ok(())
    }

    pub fn wiki_pages(&self, channel: &str) -> Result<Vec<WikiPage>> {
        self.db.lock().query(&format!("SELECT {WIKI_PAGE_COLUMNS} FROM wiki_pages WHERE channel = ?1 ORDER BY position, created_at, id"), params![channel], wiki_page_from)
    }

    pub fn wiki_page(&self, id: &str) -> Result<Option<WikiPage>> {
        self.db.lock().query_row(&format!("SELECT {WIKI_PAGE_COLUMNS} FROM wiki_pages WHERE id = ?1"), params![id], wiki_page_from)
    }

    pub fn save_wiki_page(&self, p: &WikiPage) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO wiki_pages (id, channel, parent, title, position, created_by, created_at, updated_at, deleted) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![p.id, p.channel, p.parent, p.title, p.position, p.created_by, p.created_at, p.updated_at, p.deleted as i64],
        )?;
        Ok(())
    }

    /// The document a page's changes make now, and how many there are.
    pub fn wiki_state(&self, page: &str) -> Result<Option<(Vec<u8>, i64)>> {
        use base64::Engine;
        self.db.lock().query_row("SELECT state, seq FROM wiki_state WHERE page = ?1", params![page], |row| {
            let state: String = row.get(0)?;
            Ok((base64::engine::general_purpose::STANDARD.decode(state).unwrap_or_default(), row.get(1)?))
        })
    }

    pub fn set_wiki_state(&self, page: &str, state: &[u8], seq: i64) -> Result<()> {
        use base64::Engine;
        self.db.lock().execute("INSERT OR REPLACE INTO wiki_state (page, state, seq) VALUES (?1, ?2, ?3)", params![page, base64::engine::general_purpose::STANDARD.encode(state), seq])?;
        Ok(())
    }

    pub fn add_wiki_update(&self, page: &str, seq: i64, update: &[u8], did: &str, at: i64) -> Result<()> {
        use base64::Engine;
        self.db.lock().execute("INSERT INTO wiki_updates (page, seq, data, did, at) VALUES (?1, ?2, ?3, ?4, ?5)", params![page, seq, base64::engine::general_purpose::STANDARD.encode(update), did, at])?;
        Ok(())
    }

    /// Each change to a page: its number, who sent it, and when, in order.
    pub fn wiki_update_heads(&self, page: &str) -> Result<Vec<(i64, String, i64)>> {
        self.db.lock().query("SELECT seq, did, at FROM wiki_updates WHERE page = ?1 ORDER BY seq", params![page], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
    }

    /// That the change numbered `seq` on `page` was a restore.
    pub fn add_wiki_restore(&self, page: &str, seq: i64) -> Result<()> {
        self.db.lock().execute("INSERT INTO wiki_restores (page, seq) VALUES (?1, ?2)", params![page, seq])?;
        Ok(())
    }

    /// The numbers of the changes to `page` that were restores.
    pub fn wiki_restores(&self, page: &str) -> Result<Vec<i64>> {
        self.db.lock().query("SELECT seq FROM wiki_restores WHERE page = ?1 ORDER BY seq", params![page], |row| row.get(0))
    }

    /// A page's changes up to and with `seq`, in order.
    pub fn wiki_updates_to(&self, page: &str, seq: i64) -> Result<Vec<Vec<u8>>> {
        use base64::Engine;
        self.db.lock().query("SELECT data FROM wiki_updates WHERE page = ?1 AND seq <= ?2 ORDER BY seq", params![page, seq], |row| {
            let data: String = row.get(0)?;
            Ok(base64::engine::general_purpose::STANDARD.decode(data).unwrap_or_default())
        })
    }

    /// Every change to a page, in order, with its number.
    pub fn wiki_updates_numbered(&self, page: &str) -> Result<Vec<(i64, Vec<u8>)>> {
        use base64::Engine;
        self.db.lock().query("SELECT seq, data FROM wiki_updates WHERE page = ?1 ORDER BY seq", params![page], |row| {
            let data: String = row.get(1)?;
            Ok((row.get(0)?, base64::engine::general_purpose::STANDARD.decode(data).unwrap_or_default()))
        })
    }

    /// Whose Yjs client id `client` is on `page`.
    pub fn wiki_client(&self, page: &str, client: u64) -> Result<Option<String>> {
        self.db.lock().query_row("SELECT did FROM wiki_clients WHERE page = ?1 AND client = ?2", params![page, client as i64], |row| row.get(0))
    }

    pub fn add_wiki_client(&self, page: &str, client: u64, did: &str) -> Result<()> {
        self.db.lock().execute("INSERT INTO wiki_clients (page, client, did) VALUES (?1, ?2, ?3)", params![page, client as i64, did])?;
        Ok(())
    }

    pub fn wiki_clients(&self, page: &str) -> Result<Vec<(u64, String)>> {
        self.db.lock().query("SELECT client, did FROM wiki_clients WHERE page = ?1", params![page], |row| Ok((row.get::<i64>(0)? as u64, row.get(1)?)))
    }

    pub fn wiki_comments(&self, page: &str) -> Result<Vec<WikiComment>> {
        self.db.lock().query(&format!("SELECT {WIKI_COMMENT_COLUMNS} FROM wiki_comments WHERE page = ?1 ORDER BY at, id"), params![page], wiki_comment_from)
    }

    pub fn wiki_comment(&self, id: &str) -> Result<Option<WikiComment>> {
        self.db.lock().query_row(&format!("SELECT {WIKI_COMMENT_COLUMNS} FROM wiki_comments WHERE id = ?1"), params![id], wiki_comment_from)
    }

    pub fn save_wiki_comment(&self, c: &WikiComment) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO wiki_comments (id, page, start_at, end_at, quote, text, did, at, reply_to, resolved) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![c.id, c.page, c.start, c.end, c.quote, c.text, c.did, c.at, c.reply_to, c.resolved as i64],
        )?;
        Ok(())
    }

    /// Deletes a comment and its answers.
    pub fn delete_wiki_comment(&self, id: &str) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM wiki_comments WHERE reply_to = ?1", params![id])?;
        db.execute("DELETE FROM wiki_comments WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn set_form(&self, channel: &str, form: &Value) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM form_channels WHERE channel = ?1", params![channel])?;
        db.execute("INSERT INTO form_channels (channel, form) VALUES (?1, ?2)", params![channel, form.to_string()])?;
        Ok(())
    }

    pub fn form_answers(&self, channel: &str) -> Result<Vec<FormAnswer>> {
        self.db.lock().query("SELECT id, channel, did, data, status, notes, at FROM form_answers WHERE channel = ?1 ORDER BY at, id", params![channel], form_answer_from)
    }

    pub fn form_answer(&self, id: &str) -> Result<Option<FormAnswer>> {
        self.db.lock().query_row("SELECT id, channel, did, data, status, notes, at FROM form_answers WHERE id = ?1", params![id], form_answer_from)
    }

    pub fn save_form_answer(&self, a: &FormAnswer) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO form_answers (id, channel, did, data, status, notes, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![a.id, a.channel, a.did, a.data.to_string(), a.status, Value::Array(a.notes.clone()).to_string(), a.at],
        )?;
        Ok(())
    }

    pub fn set_calendar_channel(&self, channel: &str) -> Result<()> {
        self.db.lock().execute("INSERT INTO calendar_channels (channel) VALUES (?1)", params![channel])?;
        Ok(())
    }

    /// Makes a forum a gallery, or an ordinary forum again.
    pub fn set_gallery(&self, channel: &str, on: bool) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM gallery_channels WHERE channel = ?1", params![channel])?;
        if on {
            db.execute("INSERT INTO gallery_channels (channel) VALUES (?1)", params![channel])?;
        }
        Self::forget_forum_posts(&mut db)?;
        Ok(())
    }

    pub fn set_announcement(&self, channel: &str, on: bool) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM announce_channels WHERE channel = ?1", params![channel])?;
        if on {
            db.execute("INSERT INTO announce_channels (channel) VALUES (?1)", params![channel])?;
        }
        Ok(())
    }

    /// When `post` was published, if it was.
    pub fn published_at(&self, post: &str) -> Result<Option<i64>> {
        self.db.lock().query_row("SELECT at FROM announce_published WHERE post = ?1", params![post], |row| row.get(0))
    }

    pub fn set_published(&self, post: &str, channel: &str, at: i64) -> Result<()> {
        self.db.lock().execute("INSERT INTO announce_published (post, channel, at) VALUES (?1, ?2, ?3)", params![post, channel, at])?;
        Ok(())
    }

    pub fn add_follow_code(&self, code: &str, channel: &str, did: &str, expires: i64) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM follow_codes WHERE expires < ?1", params![expires - 1200])?;
        db.execute("INSERT INTO follow_codes (code, channel, did, expires) VALUES (?1, ?2, ?3, ?4)", params![code, channel, did, expires])?;
        Ok(())
    }

    /// Uses up a follow code: its channel and who asked for it, if it is good now.
    pub fn take_follow_code(&self, code: &str, now: i64) -> Result<Option<(String, String)>> {
        let mut db = self.db.lock();
        let found: Option<(String, String, i64)> = db.query_row("SELECT channel, did, expires FROM follow_codes WHERE code = ?1", params![code], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        db.execute("DELETE FROM follow_codes WHERE code = ?1", params![code])?;
        Ok(found.filter(|(_, _, expires)| *expires >= now).map(|(channel, did, _)| (channel, did)))
    }

    pub fn followers(&self) -> Result<Vec<Follower>> {
        self.db.lock().query(&format!("SELECT {FOLLOWER_COLUMNS} FROM announce_followers ORDER BY created_at, id"), params![], follower_from)
    }

    pub fn followers_of(&self, channel: &str) -> Result<Vec<Follower>> {
        self.db.lock().query(&format!("SELECT {FOLLOWER_COLUMNS} FROM announce_followers WHERE channel = ?1 ORDER BY created_at, id"), params![channel], follower_from)
    }

    pub fn follower(&self, id: &str) -> Result<Option<Follower>> {
        self.db.lock().query_row(&format!("SELECT {FOLLOWER_COLUMNS} FROM announce_followers WHERE id = ?1"), params![id], follower_from)
    }

    pub fn save_follower(&self, f: &Follower) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO announce_followers (id, channel, kind, target, name, secret, created_by, created_at, failures, off_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![f.id, f.channel, f.kind, f.target, f.name, f.secret, f.created_by, f.created_at, f.failures, f.off_at],
        )?;
        Ok(())
    }

    pub fn delete_follower(&self, id: &str) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM announce_queue WHERE follower = ?1", params![id])?;
        db.execute("DELETE FROM announce_copies WHERE follower = ?1", params![id])?;
        db.execute("DELETE FROM announce_followers WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// What was last sent `follower` of `post` (a digest), and what it is known as there.
    pub fn announce_copy(&self, follower: &str, post: &str) -> Result<Option<(Option<String>, String)>> {
        self.db.lock().query_row("SELECT remote, sent FROM announce_copies WHERE follower = ?1 AND post = ?2", params![follower, post], |row| Ok((row.get(0)?, row.get(1)?)))
    }

    pub fn set_announce_copy(&self, follower: &str, post: &str, remote: Option<&str>, sent: &str) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO announce_copies (follower, post, remote, sent) VALUES (?1, ?2, ?3, ?4)", params![follower, post, remote, sent])?;
        Ok(())
    }

    /// Queues `post` to be sent to `follower` as it then stands (once, however
    /// often it is queued before it goes).
    pub fn queue_announcement(&self, follower: &str, post: &str, now: i64) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM announce_queue WHERE follower = ?1 AND post = ?2", params![follower, post])?;
        db.execute("INSERT INTO announce_queue (follower, post, attempts, next_at) VALUES (?1, ?2, 0, ?3)", params![follower, post, now])?;
        Ok(())
    }

    /// What is due to be sent now: `(follower, post, attempts)`.
    pub fn due_announcements(&self, now: i64, limit: i64) -> Result<Vec<(String, String, i64)>> {
        self.db.lock().query("SELECT follower, post, attempts FROM announce_queue WHERE next_at <= ?1 ORDER BY next_at, post LIMIT ?2", params![now, limit], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
    }

    pub fn announcement_done(&self, follower: &str, post: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM announce_queue WHERE follower = ?1 AND post = ?2", params![follower, post])?;
        Ok(())
    }

    pub fn announcement_later(&self, follower: &str, post: &str, attempts: i64, next_at: i64) -> Result<()> {
        self.db.lock().execute("UPDATE announce_queue SET attempts = ?3, next_at = ?4 WHERE follower = ?1 AND post = ?2", params![follower, post, attempts, next_at])?;
        Ok(())
    }

    pub fn follows(&self) -> Result<Vec<Follow>> {
        self.db.lock().query(&format!("SELECT {FOLLOW_COLUMNS} FROM follows ORDER BY created_at, id"), params![], follow_from)
    }

    pub fn follow(&self, id: &str) -> Result<Option<Follow>> {
        self.db.lock().query_row(&format!("SELECT {FOLLOW_COLUMNS} FROM follows WHERE id = ?1"), params![id], follow_from)
    }

    pub fn save_follow(&self, f: &Follow) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO follows (id, channel, source, name, channel_name, remote, asked_by, asked_thid, pending, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![f.id, f.channel, f.source, f.name, f.channel_name, f.remote, f.asked_by, f.asked_thid, f.pending as i64, f.created_at],
        )?;
        Ok(())
    }

    pub fn delete_follow(&self, id: &str) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM followed_posts WHERE follow = ?1", params![id])?;
        db.execute("DELETE FROM follows WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// The post here copied from `source_post` through `follow`, if there is one.
    pub fn followed_post(&self, follow: &str, source_post: &str) -> Result<Option<String>> {
        self.db.lock().query_row("SELECT post FROM followed_posts WHERE follow = ?1 AND source_post = ?2", params![follow, source_post], |row| row.get(0))
    }

    pub fn set_followed_post(&self, follow: &str, source_post: &str, post: &str) -> Result<()> {
        self.db.lock().execute("INSERT INTO followed_posts (follow, source_post, post) VALUES (?1, ?2, ?3)", params![follow, source_post, post])?;
        Ok(())
    }

    /// What came from a Discord template, of `kind` (`role`, `category`,
    /// `channel`, `community`).
    pub fn imported(&self, kind: &str) -> Result<Vec<Imported>> {
        self.db.lock().query("SELECT local, source_id, source, applied FROM imported WHERE kind = ?1", params![kind], |row| {
            let (local, source_id, source, applied): (String, String, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
            Ok(Imported { local, source_id, source: serde_json::from_str(&source).unwrap_or_default(), applied: serde_json::from_str(&applied).unwrap_or_default() })
        })
    }

    pub fn set_imported(&self, kind: &str, local: &str, source_id: &str, source: &Value, applied: &Value) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO imported (kind, local, source_id, source, applied) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![kind, local, source_id, source.to_string(), applied.to_string()],
        )?;
        Ok(())
    }

    pub fn forget_imported(&self, kind: &str, local: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM imported WHERE kind = ?1 AND local = ?2", params![kind, local])?;
        Ok(())
    }

    /// What a deleted post said, while it is kept.
    pub fn deleted_text(&self, id: &str) -> Result<Option<String>> {
        Ok(self.db.lock().query("SELECT content FROM post_deleted_text WHERE post = ?1", params![id], |row| row.get(0))?.into_iter().next())
    }

    /// When the text of a deleted post kept longest is next to be forgotten, kept
    /// `for_seconds`: when to wake a community that sleeps to forget it.
    pub fn next_deleted_text_due(&self, for_seconds: i64) -> Result<Option<i64>> {
        let oldest: Vec<Option<i64>> = self.db.lock().query("SELECT MIN(deleted_at) FROM post_deleted_text", params![], |row| row.get(0))?;
        Ok(oldest.into_iter().flatten().next().map(|at| at + for_seconds))
    }

    /// Forgets what posts deleted before `before` said. How many.
    pub fn forget_deleted_texts(&self, before: i64) -> Result<usize> {
        self.db.lock().execute("DELETE FROM post_deleted_text WHERE deleted_at < ?1", params![before])
    }

    pub fn censored_original(&self, id: &str) -> Result<Option<String>> {
        Ok(self.db.lock().query("SELECT original FROM post_censored WHERE post = ?1", params![id], |row| row.get(0))?.into_iter().next())
    }

    /// When the sender says they sent `id` (see post_sent).
    pub fn set_post_sent(&self, id: &str, sent_at: i64) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO post_sent (post, sent_at) VALUES (?1, ?2)", params![id, sent_at])?;
        Ok(())
    }

    pub fn post_sent(&self, id: &str) -> Result<Option<i64>> {
        Ok(self.db.lock().query("SELECT sent_at FROM post_sent WHERE post = ?1", params![id], |row| row.get(0))?.into_iter().next())
    }

    pub fn set_post_extra(&self, id: &str, extra: &Value) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO post_extra (post, extra) VALUES (?1, ?2)", params![id, extra.to_string()])?;
        Ok(())
    }

    /// Records `did`'s whole vote on the poll `post`; none takes it back. The post
    /// takes the next `rev`.
    pub fn vote(&self, post: &str, did: &str, answers: &[String], now: i64) -> Result<()> {
        let mut db = self.db.lock();
        if answers.is_empty() {
            db.execute("DELETE FROM votes WHERE post = ?1 AND did = ?2", params![post, did])?;
        } else {
            db.execute("INSERT OR REPLACE INTO votes (post, did, answers, at) VALUES (?1, ?2, ?3, ?4)", params![post, did, serde_json::to_string(answers).unwrap_or_default(), now])?;
        }
        Self::touch(&mut db, post)?;
        Ok(())
    }

    /// Polls that ended by `now` that everyone hasn't been told of yet; each is
    /// marked told, and takes the next `rev`.
    pub fn polls_just_ended(&self, now: i64) -> Result<Vec<String>> {
        let mut db = self.db.lock();
        let ended: Vec<(String, String)> = db.query(
            "SELECT post, extra FROM post_extra WHERE extra LIKE '%\"poll\"%' AND post NOT IN (SELECT post FROM polls_ended)",
            params![],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut told = Vec::new();
        for (post, extra) in ended {
            let ends = serde_json::from_str::<Value>(&extra).ok().and_then(|extra| extra["poll"]["ends_at"].as_i64());
            if ends.is_some_and(|ends| ends <= now) {
                db.execute("INSERT OR IGNORE INTO polls_ended (post) VALUES (?1)", params![post])?;
                Self::touch(&mut db, &post)?;
                told.push(post);
            }
        }
        Ok(told)
    }

    /// When the soonest poll still running ends, if one is.
    pub fn next_poll_end(&self) -> Result<Option<i64>> {
        let extras: Vec<String> = self.db.lock().query(
            "SELECT extra FROM post_extra WHERE extra LIKE '%\"poll\"%' AND post NOT IN (SELECT post FROM polls_ended)",
            params![],
            |row| row.get(0),
        )?;
        Ok(extras.iter().filter_map(|extra| serde_json::from_str::<Value>(extra).ok()?["poll"]["ends_at"].as_i64()).min())
    }

    /// Makes `channel` a forum with `tags`, or changes them.
    pub fn set_forum(&self, channel: &str, tags: &Value) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("INSERT OR REPLACE INTO channel_forum (channel, tags) VALUES (?1, ?2)", params![channel, tags.to_string()])?;
        Self::forget_forum_posts(&mut db)?;
        Ok(())
    }

    /// `did`'s choice to be seen online, and as what: `(share, status)`.
    /// Whether `did` shares their presence, and as what. Shared unless they said
    /// otherwise (asked 2026-11-17: seen online by default, as on Discord).
    pub fn presence(&self, did: &str) -> Result<(bool, String)> {
        Ok(self.db.lock().query_row("SELECT share, status FROM presence WHERE did = ?1", params![did], |row| Ok((row.get::<bool>(0)?, row.get::<String>(1)?)))?.unwrap_or((true, "online".into())))
    }

    pub fn set_presence(&self, did: &str, share: bool, status: &str) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO presence (did, share, status) VALUES (?1, ?2, ?3)", params![did, share, status])?;
        Ok(())
    }

    // --- Answering a question --------------------------------------------------

    pub fn answering(&self, id: &str) -> Result<Option<Answering>> {
        self.db.lock().query_row(&format!("SELECT {ANSWERING_COLUMNS} FROM answering WHERE id = ?1"), params![id], answering_from)
    }

    /// What `owner` has under way for the question `post` asks, if anything.
    pub fn answering_of(&self, owner: &str, post: &str) -> Result<Option<Answering>> {
        self.db.lock().query_row(
            &format!("SELECT {ANSWERING_COLUMNS} FROM answering WHERE owner = ?1 AND post = ?2 AND status = 'active'"),
            params![owner, post],
            answering_from,
        )
    }

    pub fn save_answering(&self, answering: &Answering) -> Result<()> {
        let mut db = self.db.lock();
        let answers = Value::from(answering.answers.clone()).to_string();
        let known = db.execute(
            "UPDATE answering SET at = ?2, answers = ?3, status = ?4, updated = ?5 WHERE id = ?1",
            params![answering.id, answering.at as i64, answers, answering.status, answering.updated],
        )?;
        if known == 0 {
            db.execute(
                "INSERT INTO answering (id, owner, post, at, answers, status, updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![answering.id, answering.owner, answering.post, answering.at as i64, answers, answering.status, answering.updated],
            )?;
        }
        Ok(())
    }

    /// Forgets what was started before `before` and never finished.
    pub fn prune_answering(&self, before: i64) -> Result<usize> {
        self.db.lock().execute("DELETE FROM answering WHERE updated < ?1 AND status != 'completed'", params![before])
    }
}

const WIKI_PAGE_COLUMNS: &str = "id, channel, parent, title, position, created_by, created_at, updated_at, deleted";

fn wiki_page_from(row: &Row) -> Result<WikiPage> {
    let deleted: i64 = row.get(8)?;
    Ok(WikiPage { id: row.get(0)?, channel: row.get(1)?, parent: row.get(2)?, title: row.get(3)?, position: row.get(4)?, created_by: row.get(5)?, created_at: row.get(6)?, updated_at: row.get(7)?, deleted: deleted != 0 })
}

const WIKI_COMMENT_COLUMNS: &str = "id, page, start_at, end_at, quote, text, did, at, reply_to, resolved";

fn wiki_comment_from(row: &Row) -> Result<WikiComment> {
    let resolved: i64 = row.get(9)?;
    Ok(WikiComment { id: row.get(0)?, page: row.get(1)?, start: row.get(2)?, end: row.get(3)?, quote: row.get(4)?, text: row.get(5)?, did: row.get(6)?, at: row.get(7)?, reply_to: row.get(8)?, resolved: resolved != 0 })
}

fn form_answer_from(row: &Row) -> Result<FormAnswer> {
    let (data, notes): (String, String) = (row.get(3)?, row.get(5)?);
    Ok(FormAnswer {
        id: row.get(0)?,
        channel: row.get(1)?,
        did: row.get(2)?,
        data: serde_json::from_str(&data).unwrap_or(Value::Null),
        status: row.get(4)?,
        notes: serde_json::from_str(&notes).unwrap_or_default(),
        at: row.get(6)?,
    })
}

fn event_from(row: &Row) -> Result<StoredEvent> {
    let data: String = row.get(1)?;
    let cancelled: i64 = row.get(4)?;
    Ok(StoredEvent { id: row.get(0)?, data: serde_json::from_str(&data).unwrap_or(Value::Null), organizer: row.get(2)?, sequence: row.get(3)?, cancelled: cancelled != 0, created_at: row.get(5)?, updated_at: row.get(6)? })
}

const FOLLOWER_COLUMNS: &str = "id, channel, kind, target, name, secret, created_by, created_at, failures, off_at";

fn follower_from(row: &Row) -> Result<Follower> {
    Ok(Follower {
        id: row.get(0)?,
        channel: row.get(1)?,
        kind: row.get(2)?,
        target: row.get(3)?,
        name: row.get(4)?,
        secret: row.get(5)?,
        created_by: row.get(6)?,
        created_at: row.get(7)?,
        failures: row.get(8)?,
        off_at: row.get(9)?,
    })
}

const FOLLOW_COLUMNS: &str = "id, channel, source, name, channel_name, remote, asked_by, asked_thid, pending, created_at";

fn follow_from(row: &Row) -> Result<Follow> {
    let pending: i64 = row.get(8)?;
    Ok(Follow {
        id: row.get(0)?,
        channel: row.get(1)?,
        source: row.get(2)?,
        name: row.get(3)?,
        channel_name: row.get(4)?,
        remote: row.get(5)?,
        asked_by: row.get(6)?,
        asked_thid: row.get(7)?,
        pending: pending != 0,
        created_at: row.get(9)?,
    })
}

const EVENT_HOOK_COLUMNS: &str = "id, url, secret, events, channel, created_by, created_at, failures, off_at";

fn event_hook_from(row: &Row) -> Result<EventHook> {
    let events: String = row.get(3)?;
    Ok(EventHook {
        id: row.get(0)?,
        url: row.get(1)?,
        secret: row.get(2)?,
        events: serde_json::from_str(&events).unwrap_or_default(),
        channel: row.get(4)?,
        created_by: row.get(5)?,
        created_at: row.get(6)?,
        failures: row.get(7)?,
        off_at: row.get(8)?,
    })
}

fn webhook_from(row: &Row) -> Result<Webhook> {
    Ok(Webhook { id: row.get(0)?, channel: row.get(1)?, name: row.get(2)?, avatar: row.get(3)?, token: row.get(4)?, created_by: row.get(5)?, created_at: row.get(6)? })
}

#[cfg(test)]
mod kept_counts {
    use super::*;

    fn post(store: &Store, author: &Member, id: &str, thread: Option<&str>) {
        store.add_post("board", id, "words", author, None, None, 10).unwrap();
        store.set_written_with(id, None, thread, &[]).unwrap();
    }

    #[test]
    fn counts_are_kept_forgotten_as_posts_change_and_rebuilt_when_wrong() {
        let store = Store::in_memory().unwrap();
        store.set_member("did:example:bea", Role::Member, 1).unwrap();
        let bea = store.member("did:example:bea").unwrap().unwrap();
        store.add_channel("board", "board", "", "did:example:bea", 1).unwrap();
        store.set_forum("board", &serde_json::json!([])).unwrap();
        post(&store, &bea, "t1", None);
        store.add_thread("t1", "board", Some("Help"), &bea.did, 10).unwrap();
        store.set_thread_flags("t1", false, false, Some("help")).unwrap();
        post(&store, &bea, "r1", Some("t1"));
        post(&store, &bea, "r2", Some("t1"));

        let help = |store: &Store| store.section_tallies("board", &["help".to_string()]).unwrap()[1].clone();
        assert_eq!(store.thread("t1").unwrap().unwrap().count, 2);
        assert_eq!(store.forum_post_counts("board").unwrap(), vec![(bea.did.clone(), 3)]);
        assert_eq!((help(&store).threads, help(&store).posts, help(&store).last_thread.as_deref()), (1, 3, Some("t1")));

        // A new post, and one taken back, are counted at once.
        post(&store, &bea, "r3", Some("t1"));
        store.delete_post("r1", None, true).unwrap();
        assert_eq!(store.thread("t1").unwrap().unwrap().count, 3, "a post taken back stays in its thread");
        assert_eq!(store.forum_post_counts("board").unwrap(), vec![(bea.did.clone(), 3)]);
        assert_eq!(help(&store).posts, 4);
        // Moved out of the section, it is counted in none.
        store.set_thread_flags("t1", false, false, None).unwrap();
        assert_eq!((help(&store).threads, store.section_tallies("board", &[]).unwrap()[0].threads), (0, 1));

        // Counts gone wrong (by hand, here) are put right by a rebuild.
        store.thread("t1").unwrap();
        store.db.lock().execute("UPDATE cached_counts SET n = 99", params![]).unwrap();
        assert_eq!(store.thread("t1").unwrap().unwrap().count, 99);
        assert_eq!(store.rebuild_counts().unwrap(), (1, 1, 1));
        assert_eq!(store.thread("t1").unwrap().unwrap().count, 3);
        assert_eq!(store.forum_post_counts("board").unwrap(), vec![(bea.did.clone(), 3)]);

        // A gallery's posts aren't forum posts.
        store.set_gallery("board", true).unwrap();
        assert_eq!(store.forum_post_counts("board").unwrap(), vec![(bea.did.clone(), 0)]);
    }
}

#[cfg(test)]
mod indexes {
    use super::*;

    /// How SQLite would answer `sql`: what each step reads.
    fn plan(store: &Store, sql: &str) -> String {
        store.db.lock().query(&format!("EXPLAIN QUERY PLAN {sql}"), params![], |row| row.get::<String>(3)).unwrap().join("; ")
    }

    #[test]
    fn what_a_big_community_is_asked_most_is_found_without_reading_a_whole_table() {
        // (The plan asked for is SQLite's; MySQL's is told otherwise.)
        if std::env::var("WYVRN_TEST_DATABASE_URL").is_ok_and(|url| !url.is_empty()) {
            return;
        }
        let store = Store::in_memory().unwrap();
        for (sql, index) in [
            ("SELECT COUNT(*) FROM post_state WHERE thread = 't1'", "post_state_by_thread"),
            ("SELECT post FROM post_state WHERE rev > 10 ORDER BY rev LIMIT 100", "post_state_by_rev"),
            ("SELECT post FROM post_state WHERE reply_to = 'p1'", "post_state_by_reply"),
            ("SELECT COUNT(*) FROM posts WHERE author_did = 'did:example:bea' AND posted_at >= 10", "posts_by_author"),
            ("SELECT COUNT(*) FROM posts WHERE posted_at >= 10", "posts_by_time"),
            ("SELECT id FROM threads WHERE channel = 'board'", "threads_by_channel"),
            ("SELECT id FROM form_answers WHERE channel = 'form' ORDER BY at, id", "form_answers_by_channel"),
            ("SELECT follower, post FROM announce_queue WHERE next_at <= 10 ORDER BY next_at, post LIMIT 10", "announce_queue_by_due"),
            ("SELECT id FROM event_queue WHERE next_at <= 10 ORDER BY next_at, id LIMIT 10", "event_queue_by_due"),
        ] {
            let plan = plan(&store, sql);
            assert!(plan.contains(index), "{sql}: {plan}");
        }
    }
}
