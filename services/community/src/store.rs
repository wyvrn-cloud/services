//! What the server keeps: who is let in, the channels, and what was said in them.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

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
    connection: Mutex<Connection>,
}

type Result<T> = rusqlite::Result<T>;

const MEMBER_COLUMNS: &str = "did, role, name, joined, since, delivered";
// A post's author is called what they are called now, where they are still a
// member; what they were called when they wrote it, otherwise.
const POST_COLUMNS: &str = "serial, channel, seq, id, content, posted_at, author_did, \
    COALESCE((SELECT name FROM members WHERE members.did = posts.author_did), author_name), question, answer, answered_by";
const CHANNEL_COLUMNS: &str =
    "id, name, topic, closed, created_at, created_by, (SELECT COALESCE(MAX(seq), 0) FROM posts WHERE posts.channel = channels.id)";

fn member_from(row: &rusqlite::Row<'_>) -> Result<Member> {
    let role: String = row.get(1)?;
    Ok(Member { did: row.get(0)?, role: Role::parse(&role).unwrap_or(Role::Member), name: row.get(2)?, joined: row.get(3)?, since: row.get(4)?, delivered: row.get(5)? })
}

fn json_from(text: Option<String>) -> Option<Value> {
    text.and_then(|text| serde_json::from_str(&text).ok())
}

fn post_from(row: &rusqlite::Row<'_>) -> Result<Post> {
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
    })
}

fn channel_from(row: &rusqlite::Row<'_>) -> Result<Channel> {
    Ok(Channel { id: row.get(0)?, name: row.get(1)?, topic: row.get(2)?, closed: row.get(3)?, created_at: row.get(4)?, created_by: row.get(5)?, latest_seq: row.get(6)? })
}

fn answering_from(row: &rusqlite::Row<'_>) -> Result<Answering> {
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

impl Store {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        Self::with(Connection::open(path)?)
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(connection: Connection) -> anyhow::Result<Self> {
        // The `members` and `add` commands open the same file while the server has it open.
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS members (
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
            CREATE TABLE IF NOT EXISTS answering (
                id TEXT PRIMARY KEY,
                owner TEXT NOT NULL,
                post TEXT NOT NULL,
                at INTEGER NOT NULL DEFAULT 0,
                answers TEXT NOT NULL DEFAULT '[]',
                status TEXT NOT NULL,
                updated INTEGER NOT NULL
            );",
        )?;
        Ok(Self { connection: Mutex::new(connection) })
    }

    fn connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // --- Members ---------------------------------------------------------------

    pub fn member(&self, did: &str) -> Result<Option<Member>> {
        self.connection().query_row(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE did = ?1"), [did], member_from).optional()
    }

    pub fn members(&self) -> Result<Vec<Member>> {
        let connection = self.connection();
        let mut statement = connection.prepare(&format!("SELECT {MEMBER_COLUMNS} FROM members ORDER BY role, since, did"))?;
        let rows = statement.query_map([], member_from)?;
        rows.collect()
    }

    /// Everyone whose app has asked to be sent what is posted.
    pub fn joined(&self) -> Result<Vec<Member>> {
        Ok(self.members()?.into_iter().filter(|member| member.joined).collect())
    }

    pub fn admin_count(&self) -> Result<i64> {
        self.connection().query_row("SELECT COUNT(*) FROM members WHERE role = 'admin'", [], |row| row.get(0))
    }

    /// Lets `did` in as `role`, or changes the role of someone already in. Someone
    /// new is sent only what is posted from now on.
    pub fn set_member(&self, did: &str, role: Role, now: i64) -> Result<()> {
        self.connection().execute(
            "INSERT INTO members (did, role, since, delivered) VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(serial), 0) FROM posts))
             ON CONFLICT (did) DO UPDATE SET role = ?2",
            params![did, role.as_str(), now],
        )?;
        Ok(())
    }

    /// Lets `did` in as `role` unless they are in already, whatever as.
    pub fn admit(&self, did: &str, role: Role, now: i64) -> Result<bool> {
        let added = self.connection().execute(
            "INSERT OR IGNORE INTO members (did, role, since, delivered) VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(serial), 0) FROM posts))",
            params![did, role.as_str(), now],
        )?;
        Ok(added > 0)
    }

    pub fn remove_member(&self, did: &str) -> Result<bool> {
        Ok(self.connection().execute("DELETE FROM members WHERE did = ?1", [did])? > 0)
    }

    pub fn set_joined(&self, did: &str, joined: bool) -> Result<()> {
        self.connection().execute("UPDATE members SET joined = ?2 WHERE did = ?1", params![did, joined])?;
        Ok(())
    }

    pub fn set_name(&self, did: &str, name: Option<&str>) -> Result<()> {
        self.connection().execute("UPDATE members SET name = ?2 WHERE did = ?1", params![did, name])?;
        Ok(())
    }

    pub fn set_delivered(&self, did: &str, serial: i64) -> Result<()> {
        self.connection().execute("UPDATE members SET delivered = MAX(delivered, ?2) WHERE did = ?1", params![did, serial])?;
        Ok(())
    }

    pub fn is_superseded(&self, did: &str) -> Result<bool> {
        self.connection().query_row("SELECT 1 FROM superseded WHERE did = ?1", [did], |_| Ok(())).optional().map(|found| found.is_some())
    }

    /// A member has changed DID: what the server holds for `prior` is `new`'s now,
    /// and `prior` is never listened to again. Returns whether `prior` was a member.
    /// Where `new` is one already, it keeps the higher role of the two.
    pub fn rotate(&self, prior: &str, new: &str, now: i64) -> Result<bool> {
        let mut connection = self.connection();
        let transaction = connection.transaction()?;
        let old = transaction.query_row(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE did = ?1"), [prior], member_from).optional()?;
        if let Some(old) = &old {
            let existing = transaction.query_row(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE did = ?1"), [new], member_from).optional()?;
            transaction.execute("DELETE FROM members WHERE did = ?1", [prior])?;
            match existing {
                None => {
                    transaction.execute(
                        "INSERT INTO members (did, role, name, joined, since, delivered) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![new, old.role.as_str(), old.name, old.joined, old.since, old.delivered],
                    )?;
                }
                Some(existing) => {
                    let role = if old.role == Role::Admin { Role::Admin } else { existing.role };
                    transaction.execute(
                        "UPDATE members SET role = ?2, joined = ?3, name = COALESCE(name, ?4), since = MIN(since, ?5), delivered = MAX(delivered, ?6) WHERE did = ?1",
                        params![new, role.as_str(), old.joined || existing.joined, old.name, old.since, old.delivered],
                    )?;
                }
            }
            // What they wrote, and what they were answering, is theirs under the new DID.
            transaction.execute("UPDATE posts SET author_did = ?2 WHERE author_did = ?1", params![prior, new])?;
            transaction.execute("UPDATE posts SET answered_by = ?2 WHERE answered_by = ?1", params![prior, new])?;
            transaction.execute("UPDATE answering SET owner = ?2 WHERE owner = ?1", params![prior, new])?;
        }
        transaction.execute("INSERT OR REPLACE INTO superseded (did, became, at) VALUES (?1, ?2, ?3)", params![prior, new, now])?;
        transaction.commit()?;
        Ok(old.is_some())
    }

    // --- Channels --------------------------------------------------------------

    pub fn channels(&self) -> Result<Vec<Channel>> {
        let connection = self.connection();
        let mut statement = connection.prepare(&format!("SELECT {CHANNEL_COLUMNS} FROM channels ORDER BY closed, created_at, id"))?;
        let rows = statement.query_map([], channel_from)?;
        rows.collect()
    }

    pub fn channel(&self, id: &str) -> Result<Option<Channel>> {
        self.connection().query_row(&format!("SELECT {CHANNEL_COLUMNS} FROM channels WHERE id = ?1"), [id], channel_from).optional()
    }

    /// The open channel called `name`, whatever its case.
    pub fn channel_named(&self, name: &str) -> Result<Option<Channel>> {
        self.connection()
            .query_row(&format!("SELECT {CHANNEL_COLUMNS} FROM channels WHERE closed = 0 AND LOWER(name) = LOWER(?1)"), [name], channel_from)
            .optional()
    }

    pub fn add_channel(&self, id: &str, name: &str, topic: &str, by: &str, now: i64) -> Result<()> {
        self.connection().execute("INSERT INTO channels (id, name, topic, created_at, created_by) VALUES (?1, ?2, ?3, ?4, ?5)", params![id, name, topic, now, by])?;
        Ok(())
    }

    pub fn update_channel(&self, id: &str, name: &str, topic: &str, closed: bool) -> Result<()> {
        self.connection().execute("UPDATE channels SET name = ?2, topic = ?3, closed = ?4 WHERE id = ?1", params![id, name, topic, closed])?;
        Ok(())
    }

    // --- Posts -----------------------------------------------------------------

    pub fn latest_serial(&self) -> Result<i64> {
        self.connection().query_row("SELECT COALESCE(MAX(serial), 0) FROM posts", [], |row| row.get(0))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_post(&self, channel: &str, id: &str, content: &str, author: &Member, question: Option<&Value>, answer: Option<&Value>, now: i64) -> Result<Post> {
        let connection = self.connection();
        connection.execute(
            "INSERT INTO posts (channel, seq, id, content, posted_at, author_did, author_name, question, answer)
             VALUES (?1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM posts WHERE channel = ?1), ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![channel, id, content, now, author.did, author.name, question.map(Value::to_string), answer.map(Value::to_string)],
        )?;
        connection.query_row(&format!("SELECT {POST_COLUMNS} FROM posts WHERE id = ?1"), [id], post_from)
    }

    pub fn post_by_id(&self, id: &str) -> Result<Option<Post>> {
        self.connection().query_row(&format!("SELECT {POST_COLUMNS} FROM posts WHERE id = ?1"), [id], post_from).optional()
    }

    /// Records who answered the question `id` asks. `false` if someone already had.
    pub fn set_answered(&self, id: &str, by: &str) -> Result<bool> {
        Ok(self.connection().execute("UPDATE posts SET answered_by = ?2 WHERE id = ?1 AND answered_by IS NULL", params![id, by])? > 0)
    }

    /// Every post after `serial`, oldest first: what is still to send to someone.
    pub fn posts_after(&self, serial: i64, limit: usize) -> Result<Vec<Post>> {
        let connection = self.connection();
        let mut statement = connection.prepare(&format!("SELECT {POST_COLUMNS} FROM posts WHERE serial > ?1 ORDER BY serial LIMIT ?2"))?;
        let rows = statement.query_map(params![serial, limit as i64], post_from)?;
        rows.collect()
    }

    /// A channel's posts before `before` (its newest, with none given), newest first.
    pub fn channel_posts_before(&self, channel: &str, before: Option<i64>, limit: usize) -> Result<Vec<Post>> {
        let connection = self.connection();
        let mut statement =
            connection.prepare(&format!("SELECT {POST_COLUMNS} FROM posts WHERE channel = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3"))?;
        let rows = statement.query_map(params![channel, before.unwrap_or(i64::MAX), limit as i64], post_from)?;
        rows.collect()
    }

    // --- Answering a question --------------------------------------------------

    pub fn answering(&self, id: &str) -> Result<Option<Answering>> {
        self.connection().query_row("SELECT id, owner, post, at, answers, status, updated FROM answering WHERE id = ?1", [id], answering_from).optional()
    }

    /// What `owner` has under way for the question `post` asks, if anything.
    pub fn answering_of(&self, owner: &str, post: &str) -> Result<Option<Answering>> {
        self.connection()
            .query_row(
                "SELECT id, owner, post, at, answers, status, updated FROM answering WHERE owner = ?1 AND post = ?2 AND status = 'active'",
                params![owner, post],
                answering_from,
            )
            .optional()
    }

    pub fn save_answering(&self, answering: &Answering) -> Result<()> {
        self.connection().execute(
            "INSERT INTO answering (id, owner, post, at, answers, status, updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (id) DO UPDATE SET at = ?4, answers = ?5, status = ?6, updated = ?7",
            params![answering.id, answering.owner, answering.post, answering.at as i64, Value::from(answering.answers.clone()).to_string(), answering.status, answering.updated],
        )?;
        Ok(())
    }

    /// Forgets what was started before `before` and never finished.
    pub fn prune_answering(&self, before: i64) -> Result<usize> {
        self.connection().execute("DELETE FROM answering WHERE updated < ?1 AND status != 'completed'", [before])
    }
}
