//! What the host keeps: for each name, who may claim it, who is claiming it, and
//! the DID published under it.

use wyvrn_service::db::{Db, Location, Row, Schema, MYSQL_DID, MYSQL_TABLE};
use wyvrn_service::params;

/// What is published under a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Published {
    No,
    Yes,
    /// Its holder hasn't answered in the time allowed. Still served.
    Stale,
    /// Its holder ended it. Still served.
    Deactivated,
}

impl Published {
    fn as_str(self) -> &'static str {
        match self {
            Self::No => "no",
            Self::Yes => "yes",
            Self::Stale => "stale",
            Self::Deactivated => "deactivated",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "yes" => Self::Yes,
            "stale" => Self::Stale,
            "deactivated" => Self::Deactivated,
            _ => Self::No,
        }
    }
}

/// Everything about one name.
#[derive(Debug, Clone)]
pub struct Record {
    pub name: String,
    pub published: Published,
    /// An admin set this name aside (beyond what configuration reserves).
    pub reserved: bool,
    /// The DID an admin said may claim it.
    pub assigned: Option<String>,
    /// The key-agreement keys of whoever it is held for, and until when.
    pub hold_keys: Vec<String>,
    pub hold_expires: i64,
    pub did: Option<String>,
    pub version_id: Option<String>,
    pub log: Option<String>,
    pub web_document: Option<String>,
    /// The published document's key-agreement keys: who its holder is.
    pub holder_keys: Vec<String>,
    pub whois: Option<Vec<u8>>,
    /// When the holder was last heard from, and when it was last asked.
    pub heard_at: i64,
    pub asked_at: Option<i64>,
}

pub struct Store {
    db: Db,
}

const COLUMNS: &str = "name, published, reserved, assigned, hold_keys, hold_expires, did, version_id, log, web_document, holder_keys, whois, heard_at, asked_at";

/// `keys` and `key` are words of MySQL's own, so they are always written quoted.
const SQLITE: &str = r#"CREATE TABLE IF NOT EXISTS names (
        name TEXT PRIMARY KEY,
        published TEXT NOT NULL DEFAULT 'no',
        reserved INTEGER NOT NULL DEFAULT 0,
        assigned TEXT,
        hold_keys TEXT NOT NULL DEFAULT '[]',
        hold_expires INTEGER NOT NULL DEFAULT 0,
        did TEXT,
        version_id TEXT,
        log TEXT,
        web_document TEXT,
        holder_keys TEXT NOT NULL DEFAULT '[]',
        whois BLOB,
        heard_at INTEGER NOT NULL DEFAULT 0,
        asked_at INTEGER
    );
    -- Every key any version of a hosted document has had, by name; and
    -- whether the name's holder has shown it holds it (a message
    -- authenticated with it) rather than only listed it.
    CREATE TABLE IF NOT EXISTS "keys" (
        "key" TEXT NOT NULL,
        name TEXT NOT NULL,
        proven INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY ("key", name)
    );
    CREATE INDEX IF NOT EXISTS keys_by_name ON "keys" (name);"#;

fn schema() -> Schema {
    static MYSQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let mysql = MYSQL.get_or_init(|| {
        format!(
            r#"CREATE TABLE IF NOT EXISTS names (
                name VARCHAR(255) NOT NULL PRIMARY KEY,
                published VARCHAR(16) NOT NULL DEFAULT 'no',
                reserved BIGINT NOT NULL DEFAULT 0,
                assigned {MYSQL_DID},
                hold_keys TEXT NOT NULL,
                hold_expires BIGINT NOT NULL DEFAULT 0,
                did {MYSQL_DID},
                version_id VARCHAR(255),
                log LONGTEXT,
                web_document LONGTEXT,
                holder_keys TEXT NOT NULL,
                whois MEDIUMBLOB,
                heard_at BIGINT NOT NULL DEFAULT 0,
                asked_at BIGINT
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS "keys" (
                "key" VARCHAR(255) NOT NULL,
                name VARCHAR(255) NOT NULL,
                proven BIGINT NOT NULL DEFAULT 0,
                PRIMARY KEY ("key", name),
                INDEX keys_by_name (name)
            ) {MYSQL_TABLE}"#
        )
    });
    Schema { sqlite: SQLITE, mysql }
}

impl Store {
    pub fn open(location: &Location) -> anyhow::Result<Self> {
        Ok(Self { db: Db::open(location, &schema(), "did-host")? })
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        Ok(Self { db: Db::scratch(&schema())? })
    }

    fn read(row: &Row) -> wyvrn_service::db::Result<Record> {
        let list = |text: String| serde_json::from_str::<Vec<String>>(&text).unwrap_or_default();
        Ok(Record {
            name: row.get(0)?,
            published: Published::parse(&row.get::<String>(1)?),
            reserved: row.get(2)?,
            assigned: row.get(3)?,
            hold_keys: list(row.get(4)?),
            hold_expires: row.get(5)?,
            did: row.get(6)?,
            version_id: row.get(7)?,
            log: row.get(8)?,
            web_document: row.get(9)?,
            holder_keys: list(row.get(10)?),
            whois: row.get(11)?,
            heard_at: row.get(12)?,
            asked_at: row.get(13)?,
        })
    }

    pub fn get(&self, name: &str) -> anyhow::Result<Option<Record>> {
        Ok(self.db.lock().query_row(&format!("SELECT {COLUMNS} FROM names WHERE name = ?1"), params![name], Self::read)?)
    }

    pub fn all(&self) -> anyhow::Result<Vec<Record>> {
        Ok(self.db.lock().query(&format!("SELECT {COLUMNS} FROM names ORDER BY name"), params![], Self::read)?)
    }

    /// The lists are written out: MySQL gives a text column no default.
    const ENSURE: &'static str = "INSERT OR IGNORE INTO names (name, hold_keys, holder_keys) VALUES (?1, '[]', '[]')";

    /// Holds `name` for whoever has `keys`, until `expires`. They hold nothing else.
    pub fn hold(&self, name: &str, keys: &[String], expires: i64) -> anyhow::Result<()> {
        let mut db = self.db.lock();
        let keys = serde_json::to_string(keys)?;
        db.execute("UPDATE names SET hold_keys = '[]', hold_expires = 0 WHERE hold_keys = ?1", params![keys])?;
        db.execute(Self::ENSURE, params![name])?;
        db.execute("UPDATE names SET hold_keys = ?2, hold_expires = ?3 WHERE name = ?1", params![name, keys, expires])?;
        Ok(())
    }

    /// A name, other than `except`, whose holder has shown it holds `key`.
    pub fn name_proven_with(&self, key: &str, except: &str) -> anyhow::Result<Option<String>> {
        Ok(self.db.lock().query_row(r#"SELECT name FROM "keys" WHERE "key" = ?1 AND name <> ?2 AND proven = 1 LIMIT 1"#, params![key, except], |row| row.get(0))?)
    }

    /// The names whose holder has shown it holds `key`.
    pub fn names_proven_with(&self, key: &str) -> anyhow::Result<Vec<String>> {
        Ok(self.db.lock().query(r#"SELECT name FROM "keys" WHERE "key" = ?1 AND proven = 1"#, params![key], |row| row.get(0))?)
    }

    /// The names with `key` in any version of their document.
    pub fn names_with_key(&self, key: &str) -> anyhow::Result<Vec<String>> {
        Ok(self.db.lock().query(r#"SELECT name FROM "keys" WHERE "key" = ?1"#, params![key], |row| row.get(0))?)
    }

    /// `name`'s holder has authenticated with `key`.
    pub fn prove(&self, name: &str, key: &str) -> anyhow::Result<()> {
        self.db.lock().execute(r#"UPDATE "keys" SET proven = 1 WHERE "key" = ?1 AND name = ?2"#, params![key, name])?;
        Ok(())
    }

    /// Puts a checked log under `name`. `keys` is every key any version of its
    /// document has had: they are added to what is remembered, never taken away.
    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        &self,
        name: &str,
        published: Published,
        did: &str,
        version_id: &str,
        log: &str,
        web_document: &str,
        holder_keys: &[String],
        keys: &[String],
        now: i64,
    ) -> anyhow::Result<()> {
        let holder_keys = serde_json::to_string(holder_keys)?;
        self.db.lock().transaction(|tx| {
            tx.execute(Self::ENSURE, params![name])?;
            tx.execute(
                "UPDATE names SET published = ?2, did = ?3, version_id = ?4, log = ?5, web_document = ?6, holder_keys = ?7,
                        hold_keys = '[]', hold_expires = 0, heard_at = ?8, asked_at = NULL
                 WHERE name = ?1",
                params![name, published.as_str(), did, version_id, log, web_document, holder_keys, now],
            )?;
            for key in keys {
                tx.execute(r#"INSERT OR IGNORE INTO "keys" ("key", name) VALUES (?1, ?2)"#, params![key, name])?;
            }
            Ok::<_, anyhow::Error>(())
        })
    }

    /// Sets or removes a DID's whois, with the `did:web` document that says so.
    pub fn set_whois(&self, name: &str, whois: Option<&[u8]>, web_document: &str) -> anyhow::Result<()> {
        self.db.lock().execute("UPDATE names SET whois = ?2, web_document = ?3 WHERE name = ?1", params![name, whois, web_document])?;
        Ok(())
    }

    /// An admin's word on who may claim a name: `Some(did)`, or nobody.
    pub fn assign(&self, name: &str, holder: Option<&str>) -> anyhow::Result<()> {
        let mut db = self.db.lock();
        db.execute(Self::ENSURE, params![name])?;
        db.execute("UPDATE names SET reserved = 1, assigned = ?2, hold_keys = '[]', hold_expires = 0 WHERE name = ?1", params![name, holder])?;
        Ok(())
    }

    /// Ends whatever is published under `name`, and forgets who it was for.
    pub fn free(&self, name: &str) -> anyhow::Result<()> {
        self.db.lock().transaction(|tx| {
            tx.execute(r#"DELETE FROM "keys" WHERE name = ?1"#, params![name])?;
            tx.execute("DELETE FROM names WHERE name = ?1", params![name])?;
            Ok::<_, anyhow::Error>(())
        })
    }

    /// The holder of `name` was heard from: it is not stale, and has answered.
    pub fn heard(&self, name: &str, now: i64) -> anyhow::Result<()> {
        self.db.lock().execute(
            "UPDATE names SET heard_at = ?2, asked_at = NULL, published = CASE published WHEN 'stale' THEN 'yes' ELSE published END WHERE name = ?1",
            params![name, now],
        )?;
        Ok(())
    }

    pub fn asked(&self, name: &str, now: i64) -> anyhow::Result<()> {
        self.db.lock().execute("UPDATE names SET asked_at = ?2 WHERE name = ?1", params![name, now])?;
        Ok(())
    }

    pub fn mark_stale(&self, name: &str) -> anyhow::Result<()> {
        self.db.lock().execute("UPDATE names SET published = 'stale' WHERE name = ?1 AND published = 'yes'", params![name])?;
        Ok(())
    }
}
