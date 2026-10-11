//! What the host keeps: for each name, who may claim it, who is claiming it, and
//! the DID published under it.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

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
    db: Mutex<Connection>,
}

const COLUMNS: &str = "name, published, reserved, assigned, hold_keys, hold_expires, did, version_id, log, web_document, holder_keys, whois, heard_at, asked_at";

impl Store {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        if let Some(parent) = path.as_ref().parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        Self::with(Connection::open(path)?)
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(db: Connection) -> anyhow::Result<Self> {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS names (
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
            -- Every key of every published document, and the one name it is under.
            CREATE TABLE IF NOT EXISTS keys (
                key TEXT PRIMARY KEY,
                name TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS keys_by_name ON keys (name);",
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Record> {
        let list = |text: String| serde_json::from_str::<Vec<String>>(&text).unwrap_or_default();
        Ok(Record {
            name: row.get(0)?,
            published: Published::parse(&row.get::<_, String>(1)?),
            reserved: row.get::<_, i64>(2)? != 0,
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
        Ok(self.db().query_row(&format!("SELECT {COLUMNS} FROM names WHERE name = ?1"), [name], Self::read).optional()?)
    }

    pub fn all(&self) -> anyhow::Result<Vec<Record>> {
        let db = self.db();
        let mut statement = db.prepare(&format!("SELECT {COLUMNS} FROM names ORDER BY name"))?;
        let records = statement.query_map([], Self::read)?.collect::<Result<_, _>>()?;
        Ok(records)
    }

    fn ensure(db: &Connection, name: &str) -> rusqlite::Result<()> {
        db.execute("INSERT OR IGNORE INTO names (name) VALUES (?1)", [name]).map(|_| ())
    }

    /// Holds `name` for whoever has `keys`, until `expires`. They hold nothing else.
    pub fn hold(&self, name: &str, keys: &[String], expires: i64) -> anyhow::Result<()> {
        let db = self.db();
        let keys = serde_json::to_string(keys)?;
        db.execute("UPDATE names SET hold_keys = '[]', hold_expires = 0 WHERE hold_keys = ?1", [&keys])?;
        Self::ensure(&db, name)?;
        db.execute("UPDATE names SET hold_keys = ?2, hold_expires = ?3 WHERE name = ?1", params![name, keys, expires])?;
        Ok(())
    }

    /// The name, other than `except`, that has a published document with `key` in it.
    pub fn name_with_key(&self, key: &str, except: &str) -> anyhow::Result<Option<String>> {
        Ok(self.db().query_row("SELECT name FROM keys WHERE key = ?1 AND name <> ?2", [key, except], |row| row.get(0)).optional()?)
    }

    /// The published name whose holder has all of `keys`: who a sender is, if anyone.
    pub fn name_held_by(&self, keys: &[String]) -> anyhow::Result<Option<String>> {
        let Some(first) = keys.first() else { return Ok(None) };
        let name: Option<String> = self.db().query_row("SELECT name FROM keys WHERE key = ?1", [first], |row| row.get(0)).optional()?;
        let Some(name) = name else { return Ok(None) };
        let holder = self.get(&name)?.map(|record| record.holder_keys).unwrap_or_default();
        Ok(keys.iter().all(|key| holder.contains(key)).then_some(name))
    }

    /// Puts a checked log under `name`. `keys` is every key of its document.
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
        let mut db = self.db();
        let tx = db.transaction()?;
        Self::ensure(&tx, name)?;
        tx.execute(
            "UPDATE names SET published = ?2, did = ?3, version_id = ?4, log = ?5, web_document = ?6, holder_keys = ?7,
                    hold_keys = '[]', hold_expires = 0, heard_at = ?8, asked_at = NULL
             WHERE name = ?1",
            params![name, published.as_str(), did, version_id, log, web_document, serde_json::to_string(holder_keys)?, now],
        )?;
        tx.execute("DELETE FROM keys WHERE name = ?1", [name])?;
        for key in keys {
            tx.execute("INSERT OR REPLACE INTO keys (key, name) VALUES (?1, ?2)", [key.as_str(), name])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Sets or removes a DID's whois, with the `did:web` document that says so.
    pub fn set_whois(&self, name: &str, whois: Option<&[u8]>, web_document: &str) -> anyhow::Result<()> {
        self.db().execute("UPDATE names SET whois = ?2, web_document = ?3 WHERE name = ?1", params![name, whois, web_document])?;
        Ok(())
    }

    /// An admin's word on who may claim a name: `Some(did)`, or nobody.
    pub fn assign(&self, name: &str, holder: Option<&str>) -> anyhow::Result<()> {
        let db = self.db();
        Self::ensure(&db, name)?;
        db.execute("UPDATE names SET reserved = 1, assigned = ?2, hold_keys = '[]', hold_expires = 0 WHERE name = ?1", params![name, holder])?;
        Ok(())
    }

    /// Ends whatever is published under `name`, and forgets who it was for.
    pub fn free(&self, name: &str) -> anyhow::Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute("DELETE FROM keys WHERE name = ?1", [name])?;
        tx.execute("DELETE FROM names WHERE name = ?1", [name])?;
        tx.commit()?;
        Ok(())
    }

    /// The holder of `name` was heard from: it is not stale, and has answered.
    pub fn heard(&self, name: &str, now: i64) -> anyhow::Result<()> {
        self.db().execute(
            "UPDATE names SET heard_at = ?2, asked_at = NULL, published = CASE published WHEN 'stale' THEN 'yes' ELSE published END WHERE name = ?1",
            params![name, now],
        )?;
        Ok(())
    }

    pub fn asked(&self, name: &str, now: i64) -> anyhow::Result<()> {
        self.db().execute("UPDATE names SET asked_at = ?2 WHERE name = ?1", params![name, now])?;
        Ok(())
    }

    pub fn mark_stale(&self, name: &str) -> anyhow::Result<()> {
        self.db().execute("UPDATE names SET published = 'stale' WHERE name = ?1 AND published = 'yes'", [name])?;
        Ok(())
    }
}
