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
    db: Db,
}

const MEMBER_COLUMNS: &str = "did, role, name, joined, since, delivered";
// A post's author is called what they are called now, where they are still a
// member; what they were called when they wrote it, otherwise.
const POST_COLUMNS: &str = "serial, channel, seq, id, content, posted_at, author_did, \
    COALESCE((SELECT name FROM members WHERE members.did = posts.author_did), author_name), question, answer, answered_by";
const CHANNEL_COLUMNS: &str =
    "id, name, topic, closed, created_at, created_by, (SELECT COALESCE(MAX(seq), 0) FROM posts WHERE posts.channel = channels.id)";

fn member_from(row: &Row) -> Result<Member> {
    let role: String = row.get(1)?;
    Ok(Member { did: row.get(0)?, role: Role::parse(&role).unwrap_or(Role::Member), name: row.get(2)?, joined: row.get(3)?, since: row.get(4)?, delivered: row.get(5)? })
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
    CREATE TABLE IF NOT EXISTS answering (
        id TEXT PRIMARY KEY,
        owner TEXT NOT NULL,
        post TEXT NOT NULL,
        at INTEGER NOT NULL DEFAULT 0,
        answers TEXT NOT NULL DEFAULT '[]',
        status TEXT NOT NULL,
        updated INTEGER NOT NULL
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
            CREATE TABLE IF NOT EXISTS answering (
                id VARCHAR(255) NOT NULL PRIMARY KEY,
                owner {MYSQL_DID} NOT NULL,
                post VARCHAR(255) NOT NULL,
                at BIGINT NOT NULL DEFAULT 0,
                answers MEDIUMTEXT NOT NULL,
                status VARCHAR(32) NOT NULL,
                updated BIGINT NOT NULL
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
        Ok(self.db.lock().execute("DELETE FROM members WHERE did = ?1", params![did])? > 0)
    }

    /// Forgets a member who was only ever named, and whose app never joined.
    pub fn forget_never_joined(&self, did: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM members WHERE did = ?1 AND joined = 0", params![did])?;
        Ok(())
    }

    pub fn set_joined(&self, did: &str, joined: bool) -> Result<()> {
        self.db.lock().execute("UPDATE members SET joined = ?2 WHERE did = ?1", params![did, joined])?;
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
                // What they wrote, and what they were answering, is theirs under the new DID.
                tx.execute("UPDATE posts SET author_did = ?2 WHERE author_did = ?1", params![prior, new])?;
                tx.execute("UPDATE posts SET answered_by = ?2 WHERE answered_by = ?1", params![prior, new])?;
                tx.execute("UPDATE answering SET owner = ?2 WHERE owner = ?1", params![prior, new])?;
            }
            tx.execute("INSERT OR REPLACE INTO superseded (did, became, at) VALUES (?1, ?2, ?3)", params![prior, new, now])?;
            Ok(old.is_some())
        })
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
        self.db.lock().query_row(&format!("SELECT {POST_COLUMNS} FROM posts WHERE id = ?1"), params![id], post_from)
    }

    /// Records who answered the question `id` asks. `false` if someone already had.
    pub fn set_answered(&self, id: &str, by: &str) -> Result<bool> {
        Ok(self.db.lock().execute("UPDATE posts SET answered_by = ?2 WHERE id = ?1 AND answered_by IS NULL", params![id, by])? > 0)
    }

    /// Every post after `serial`, oldest first: what is still to send to someone.
    pub fn posts_after(&self, serial: i64, limit: usize) -> Result<Vec<Post>> {
        self.db.lock().query(&format!("SELECT {POST_COLUMNS} FROM posts WHERE serial > ?1 ORDER BY serial LIMIT ?2"), params![serial, limit as i64], post_from)
    }

    /// A channel's posts before `before` (its newest, with none given), newest first.
    pub fn channel_posts_before(&self, channel: &str, before: Option<i64>, limit: usize) -> Result<Vec<Post>> {
        self.db.lock().query(
            &format!("SELECT {POST_COLUMNS} FROM posts WHERE channel = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3"),
            params![channel, before.unwrap_or(i64::MAX), limit as i64],
            post_from,
        )
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
