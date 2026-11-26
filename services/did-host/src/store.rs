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

/// An entry held back (did-hosting/1.0, "The host as a witness"): checked, and
/// everything its publication will need, kept until it is witnessed, replaced or
/// withdrawn. One for a name at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub name: String,
    /// The newest entry's `versionId`.
    pub version_id: String,
    /// The witnessed entry it follows.
    pub after: String,
    /// The entries held back, one per line.
    pub entries: String,
    pub did: String,
    /// The whole log, with them.
    pub log: String,
    pub web_document: String,
    pub holder_keys: Vec<String>,
    /// Every key any version of the document has had, with them.
    pub all_keys: Vec<String>,
    /// The keys the publish showed were the holder's (written and signed with).
    pub proven: Vec<String>,
    pub deactivated: bool,
    pub accepted_at: i64,
    pub publish_at: i64,
    /// Whether the holder has been told it is to be published within the hour.
    pub soon_sent: bool,
}

pub struct Store {
    db: Db,
}

const PENDING_COLUMNS: &str =
    "name, version_id, after_version, entries, did, log, web_document, holder_keys, all_keys, proven, deactivated, accepted_at, publish_at, soon_sent";

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
    CREATE INDEX IF NOT EXISTS keys_by_name ON "keys" (name);
    -- The pictures a name's whois refers to (its linked accounts'), by their
    -- digest: served beside it, and gone with it.
    CREATE TABLE IF NOT EXISTS whois_pictures (
        name TEXT NOT NULL,
        picture TEXT NOT NULL,
        media_type TEXT NOT NULL,
        data BLOB NOT NULL,
        PRIMARY KEY (name, picture)
    );
    -- What the host's witness has signed for, by name: the did-witness.json
    -- served beside the log (did-hosting/1.0, "The host as a witness").
    CREATE TABLE IF NOT EXISTS witness_proofs (
        name TEXT PRIMARY KEY,
        proofs TEXT NOT NULL
    );
    -- An entry held back, one for a name at a time (store::Pending).
    CREATE TABLE IF NOT EXISTS pending_entries (
        name TEXT PRIMARY KEY,
        version_id TEXT NOT NULL,
        after_version TEXT NOT NULL,
        entries TEXT NOT NULL,
        did TEXT NOT NULL,
        log TEXT NOT NULL,
        web_document TEXT NOT NULL,
        holder_keys TEXT NOT NULL,
        all_keys TEXT NOT NULL,
        proven TEXT NOT NULL,
        deactivated INTEGER NOT NULL DEFAULT 0,
        accepted_at INTEGER NOT NULL,
        publish_at INTEGER NOT NULL,
        soon_sent INTEGER NOT NULL DEFAULT 0
    );
    -- The host's own secrets beside its identity: its witness key, sealed
    -- where the service seals its keys.
    CREATE TABLE IF NOT EXISTS host_secrets (
        name TEXT PRIMARY KEY,
        secret TEXT NOT NULL
    );"#;

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
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS whois_pictures (
                name VARCHAR(255) NOT NULL,
                picture VARCHAR(64) NOT NULL,
                media_type VARCHAR(32) NOT NULL,
                data BLOB NOT NULL,
                PRIMARY KEY (name, picture)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS witness_proofs (
                name VARCHAR(255) NOT NULL PRIMARY KEY,
                proofs MEDIUMTEXT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS pending_entries (
                name VARCHAR(255) NOT NULL PRIMARY KEY,
                version_id VARCHAR(255) NOT NULL,
                after_version VARCHAR(255) NOT NULL,
                entries LONGTEXT NOT NULL,
                did {MYSQL_DID} NOT NULL,
                log LONGTEXT NOT NULL,
                web_document LONGTEXT NOT NULL,
                holder_keys TEXT NOT NULL,
                all_keys MEDIUMTEXT NOT NULL,
                proven TEXT NOT NULL,
                deactivated BIGINT NOT NULL DEFAULT 0,
                accepted_at BIGINT NOT NULL,
                publish_at BIGINT NOT NULL,
                soon_sent BIGINT NOT NULL DEFAULT 0,
                INDEX pending_by_time (publish_at)
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS host_secrets (
                name VARCHAR(255) NOT NULL PRIMARY KEY,
                secret TEXT NOT NULL
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
    /// `witness_proofs`, where the host witnesses it, is its `did-witness.json`
    /// now: written first (did:webvh 1.0: "The updated did-witness.json file MUST
    /// be published BEFORE the updated DID Log file"), in the same transaction. An
    /// entry pending under the name is gone: what it would have followed has moved on.
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
        witness_proofs: Option<&str>,
        now: i64,
    ) -> anyhow::Result<()> {
        let holder_keys = serde_json::to_string(holder_keys)?;
        self.db.lock().transaction(|tx| {
            if let Some(proofs) = witness_proofs {
                tx.execute("INSERT OR REPLACE INTO witness_proofs (name, proofs) VALUES (?1, ?2)", params![name, proofs])?;
            }
            tx.execute("DELETE FROM pending_entries WHERE name = ?1", params![name])?;
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

    /// Sets or removes a DID's whois, with the `did:web` document that says so, and
    /// the pictures it refers to (`(picture, media type, bytes)`, `picture` its
    /// name): those of a whois before are gone.
    pub fn set_whois(&self, name: &str, whois: Option<&[u8]>, web_document: &str, pictures: &[(String, String, Vec<u8>)]) -> anyhow::Result<()> {
        self.db.lock().transaction(|tx| {
            tx.execute("UPDATE names SET whois = ?2, web_document = ?3 WHERE name = ?1", params![name, whois, web_document])?;
            tx.execute("DELETE FROM whois_pictures WHERE name = ?1", params![name])?;
            for (picture, media_type, bytes) in pictures {
                tx.execute("INSERT OR REPLACE INTO whois_pictures (name, picture, media_type, data) VALUES (?1, ?2, ?3, ?4)", params![name, picture, media_type, bytes])?;
            }
            Ok::<_, anyhow::Error>(())
        })
    }

    /// A picture `name`'s whois refers to, by its name: `(media type, bytes)`.
    pub fn whois_picture(&self, name: &str, picture: &str) -> anyhow::Result<Option<(String, Vec<u8>)>> {
        Ok(self.db.lock().query_row("SELECT media_type, data FROM whois_pictures WHERE name = ?1 AND picture = ?2", params![name, picture], |row| Ok((row.get::<String>(0)?, row.get::<Vec<u8>>(1)?)))?)
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
            tx.execute("DELETE FROM whois_pictures WHERE name = ?1", params![name])?;
            tx.execute("DELETE FROM witness_proofs WHERE name = ?1", params![name])?;
            tx.execute("DELETE FROM pending_entries WHERE name = ?1", params![name])?;
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

    /// `name`'s `did-witness.json`, if its host has witnessed anything of it.
    pub fn witness_proofs(&self, name: &str) -> anyhow::Result<Option<String>> {
        Ok(self.db.lock().query_row("SELECT proofs FROM witness_proofs WHERE name = ?1", params![name], |row| row.get(0))?)
    }

    fn read_pending(row: &Row) -> wyvrn_service::db::Result<Pending> {
        let list = |text: String| serde_json::from_str::<Vec<String>>(&text).unwrap_or_default();
        Ok(Pending {
            name: row.get(0)?,
            version_id: row.get(1)?,
            after: row.get(2)?,
            entries: row.get(3)?,
            did: row.get(4)?,
            log: row.get(5)?,
            web_document: row.get(6)?,
            holder_keys: list(row.get(7)?),
            all_keys: list(row.get(8)?),
            proven: list(row.get(9)?),
            deactivated: row.get(10)?,
            accepted_at: row.get(11)?,
            publish_at: row.get(12)?,
            soon_sent: row.get(13)?,
        })
    }

    /// Holds `pending` back: in place of any entry pending under its name before.
    pub fn hold_back(&self, pending: &Pending) -> anyhow::Result<()> {
        let list = |keys: &[String]| serde_json::to_string(keys);
        self.db.lock().execute(
            &format!("INSERT OR REPLACE INTO pending_entries ({PENDING_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"),
            params![
                pending.name,
                pending.version_id,
                pending.after,
                pending.entries,
                pending.did,
                pending.log,
                pending.web_document,
                list(&pending.holder_keys)?,
                list(&pending.all_keys)?,
                list(&pending.proven)?,
                pending.deactivated,
                pending.accepted_at,
                pending.publish_at,
                pending.soon_sent
            ],
        )?;
        Ok(())
    }

    /// The entry pending under `name`, if there is one.
    pub fn pending(&self, name: &str) -> anyhow::Result<Option<Pending>> {
        Ok(self.db.lock().query_row(&format!("SELECT {PENDING_COLUMNS} FROM pending_entries WHERE name = ?1"), params![name], Self::read_pending)?)
    }

    /// Every entry pending, soonest first.
    pub fn all_pending(&self) -> anyhow::Result<Vec<Pending>> {
        Ok(self.db.lock().query(&format!("SELECT {PENDING_COLUMNS} FROM pending_entries ORDER BY publish_at"), params![], Self::read_pending)?)
    }

    /// Drops the entry pending under `name`: replaced, or withdrawn.
    pub fn drop_pending(&self, name: &str) -> anyhow::Result<()> {
        self.db.lock().execute("DELETE FROM pending_entries WHERE name = ?1", params![name])?;
        Ok(())
    }

    /// The holder of `name` has been told its pending entry is to be published soon.
    pub fn told_soon(&self, name: &str) -> anyhow::Result<()> {
        self.db.lock().execute("UPDATE pending_entries SET soon_sent = 1 WHERE name = ?1", params![name])?;
        Ok(())
    }

    /// One of the host's own secrets, as it is kept (sealed or not).
    pub fn secret(&self, name: &str) -> anyhow::Result<Option<String>> {
        Ok(self.db.lock().query_row("SELECT secret FROM host_secrets WHERE name = ?1", params![name], |row| row.get(0))?)
    }

    pub fn set_secret(&self, name: &str, kept: &str) -> anyhow::Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO host_secrets (name, secret) VALUES (?1, ?2)", params![name, kept])?;
        Ok(())
    }
}
