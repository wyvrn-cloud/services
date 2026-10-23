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
}

/// Someone a post calls on: a member's DID or `everyone`, and the name the post
/// writes them as (`@name`), which is how an app finds them in its text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Mention {
    pub who: String,
    pub name: String,
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

pub struct Store {
    db: Db,
}

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
    (SELECT removed_by FROM post_removals WHERE post_removals.post = posts.id)";
const CHANNEL_COLUMNS: &str =
    "id, name, topic, closed, created_at, created_by, (SELECT COALESCE(MAX(seq), 0) FROM posts WHERE posts.channel = channels.id)";

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
        // Read after, each by a query of its own (`Store::filled`).
        reactions: Vec::new(),
        grew: None,
    })
}

fn channel_from(row: &Row) -> Result<Channel> {
    Ok(Channel { id: row.get(0)?, name: row.get(1)?, topic: row.get(2)?, closed: row.get(3)?, created_at: row.get(4)?, created_by: row.get(5)?, latest_seq: row.get(6)? })
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

const WAITING_COLUMNS: &str = "did, name, asked_at, invite";
const INVITE_COLUMNS: &str = "code, uses_left, expires_at, note, created_at, created_by";

fn waiting_from(row: &Row) -> Result<Waiting> {
    Ok(Waiting { did: row.get(0)?, name: row.get(1)?, asked_at: row.get(2)?, invite: row.get(3)? })
}

fn invite_from(row: &Row) -> Result<Invite> {
    Ok(Invite { code: row.get(0)?, uses_left: row.get(1)?, expires_at: row.get(2)?, note: row.get(3)?, created_at: row.get(4)?, created_by: row.get(5)? })
}

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
    );";

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
                value TEXT NOT NULL
            ) {MYSQL_TABLE}"
        )
    });
    Schema { sqlite: SQLITE, mysql }
}

impl Store {
    pub fn open(location: &Location) -> anyhow::Result<Self> {
        Ok(Self { db: Db::open(location, &schema(), "community")? })
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        Ok(Self { db: Db::scratch(&schema())? })
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

    pub fn admin_count(&self) -> Result<i64> {
        self.db.lock().query_value("SELECT COUNT(*) FROM members WHERE role = 'admin'", params![])
    }

    const ADMIT: &'static str =
        "INSERT OR IGNORE INTO members (did, role, since, delivered) VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(serial), 0) FROM posts))";

    /// Lets `did` in as `role`, or changes the role of someone already in. Someone
    /// new is sent only what is posted from now on.
    pub fn set_member(&self, did: &str, role: Role, now: i64) -> Result<()> {
        let mut db = self.db.lock();
        if db.execute("UPDATE members SET role = ?2 WHERE did = ?1", params![did, role.as_str()])? == 0 {
            db.execute(Self::ADMIT, params![did, role.as_str(), now])?;
        }
        Ok(())
    }

    /// Lets `did` in as `role` unless they are in already, whatever as.
    pub fn admit(&self, did: &str, role: Role, now: i64) -> Result<bool> {
        Ok(self.db.lock().execute(Self::ADMIT, params![did, role.as_str(), now])? > 0)
    }

    pub fn remove_member(&self, did: &str) -> Result<bool> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM member_left WHERE did = ?1", params![did])?;
        Ok(db.execute("DELETE FROM members WHERE did = ?1", params![did])? > 0)
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
        self.db.lock().execute("INSERT OR REPLACE INTO member_left (did, at) VALUES (?1, ?2)", params![did, now])?;
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
                tx.execute("UPDATE answering SET owner = ?2 WHERE owner = ?1", params![prior, new])?;
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

    // --- Channels --------------------------------------------------------------

    pub fn channels(&self) -> Result<Vec<Channel>> {
        self.db.lock().query(&format!("SELECT {CHANNEL_COLUMNS} FROM channels ORDER BY closed, created_at, id"), params![], channel_from)
    }

    pub fn channel(&self, id: &str) -> Result<Option<Channel>> {
        self.db.lock().query_row(&format!("SELECT {CHANNEL_COLUMNS} FROM channels WHERE id = ?1"), params![id], channel_from)
    }

    /// The open channel called `name`, whatever its case.
    pub fn channel_named(&self, name: &str) -> Result<Option<Channel>> {
        self.db.lock().query_row(&format!("SELECT {CHANNEL_COLUMNS} FROM channels WHERE closed = 0 AND LOWER(name) = LOWER(?1)"), params![name], channel_from)
    }

    pub fn add_channel(&self, id: &str, name: &str, topic: &str, by: &str, now: i64) -> Result<()> {
        self.db.lock().execute("INSERT INTO channels (id, name, topic, created_at, created_by) VALUES (?1, ?2, ?3, ?4, ?5)", params![id, name, topic, now, by])?;
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
        drop(db);
        post.grew = self.thread(&post.id)?;
        Ok(post)
    }

    fn all_filled(&self, posts: Vec<Post>) -> Result<Vec<Post>> {
        posts.into_iter().map(|post| self.filled(post)).collect()
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
    pub fn delete_post(&self, id: &str, removed_by: Option<&str>) -> Result<()> {
        let mut db = self.db.lock();
        if let Some(removed_by) = removed_by {
            db.execute("INSERT INTO post_removals (post, removed_by) VALUES (?1, ?2)", params![id, removed_by])?;
        }
        db.execute("UPDATE posts SET content = '', question = NULL, answer = NULL WHERE id = ?1", params![id])?;
        db.execute("DELETE FROM reactions WHERE post = ?1", params![id])?;
        Self::touch(&mut db, id)?;
        db.execute("UPDATE post_state SET deleted = 1, pinned = 0, mentions = NULL WHERE post = ?1", params![id])?;
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
        self.db.lock().query_row(
            "SELECT id, channel, name, created_by, created_at, (SELECT COUNT(*) FROM post_state WHERE post_state.thread = threads.id) FROM threads WHERE id = ?1",
            params![id],
            |row| Ok(Thread { id: row.get(0)?, channel: row.get(1)?, name: row.get(2)?, created_by: row.get(3)?, created_at: row.get(4)?, count: row.get(5)? }),
        )
    }

    /// Grows a thread from the post `id`. `false` if one had grown from it already.
    pub fn add_thread(&self, id: &str, channel: &str, name: Option<&str>, by: &str, now: i64) -> Result<bool> {
        let mut db = self.db.lock();
        let made = db.execute("INSERT OR IGNORE INTO threads (id, channel, name, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5)", params![id, channel, name, by, now])? > 0;
        if made {
            Self::touch(&mut db, id)?;
        }
        Ok(made)
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
        db.query_row(&format!("SELECT {POST_COLUMNS} FROM posts WHERE id = ?1"), params![id], post_from)?
            .ok_or_else(|| wyvrn_service::db::Error::Other("a post just written is not there".into()))
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
        let posts = self.db.lock().query(&format!("SELECT {POST_COLUMNS} FROM posts WHERE serial > ?1 ORDER BY serial LIMIT ?2"), params![serial, limit as i64], post_from)?;
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
