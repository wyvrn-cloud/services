//! What the host keeps of its own: which communities there are, how each stands,
//! where its stores are, and what it may hold. Nothing of what is said in them.

use wyvrn_service::db::{Db, Location, Schema, MYSQL_DID, MYSQL_TABLE};
use wyvrn_service::params;

use crate::config::Limits;

const SQLITE: &str = "
CREATE TABLE IF NOT EXISTS communities (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    state TEXT NOT NULL,
    storage TEXT NOT NULL,
    owner TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    created_by TEXT NOT NULL,
    limits TEXT,
    wake_at INTEGER,
    sealed_until INTEGER
);
-- The host's own settings: who may make a community here (admin.rs).
CREATE TABLE IF NOT EXISTS host_settings (
    name TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Codes a host admin hands out, each letting someone make a community.
CREATE TABLE IF NOT EXISTS create_codes (
    code TEXT PRIMARY KEY,
    uses_left INTEGER NOT NULL,
    note TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    created_by TEXT NOT NULL
);";

fn schema() -> Schema {
    static MYSQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let mysql = MYSQL.get_or_init(|| {
        format!(
            "CREATE TABLE IF NOT EXISTS communities (
                id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
                name VARCHAR(64) NOT NULL UNIQUE,
                title TEXT NOT NULL,
                state VARCHAR(16) NOT NULL,
                storage VARCHAR(16) NOT NULL,
                owner {MYSQL_DID} NOT NULL,
                created_at BIGINT NOT NULL,
                created_by {MYSQL_DID} NOT NULL,
                limits TEXT,
                wake_at BIGINT,
                sealed_until BIGINT
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS host_settings (
                name VARCHAR(64) NOT NULL PRIMARY KEY,
                value TEXT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS create_codes (
                code VARCHAR(64) NOT NULL PRIMARY KEY,
                uses_left BIGINT NOT NULL,
                note TEXT NOT NULL,
                created_at BIGINT NOT NULL,
                created_by {MYSQL_DID} NOT NULL
            ) {MYSQL_TABLE}"
        )
    });
    Schema { sqlite: SQLITE, mysql }
}

/// How a community stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Open, and loaded when spoken to.
    Active,
    /// Closed by a host admin: nothing is answered, nothing is lost.
    Suspended,
    /// Read-only, on its way to a server of its owner's own.
    Moving,
    /// Taken over by that server: stopped, and kept until `sealed_until`.
    Moved,
    /// Removed by a host admin: stopped, and kept until `sealed_until`.
    Deleted,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Suspended => "suspended",
            Self::Moving => "moving",
            Self::Moved => "moved",
            Self::Deleted => "deleted",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "active" => Self::Active,
            "suspended" => Self::Suspended,
            "moving" => Self::Moving,
            "moved" => Self::Moved,
            "deleted" => Self::Deleted,
            _ => return None,
        })
    }

    /// Whether it is served: answered, and loaded when spoken to.
    pub fn served(self) -> bool {
        matches!(self, Self::Active | Self::Moving)
    }
}

/// Where a community's stores are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// A folder of SQLite files of its own.
    Sqlite,
    /// A MySQL database of its own.
    MySqlDatabase,
    /// Tables of its own in a MySQL database every such community shares.
    MySqlShared,
}

impl Storage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::MySqlDatabase => "mysql-db",
            Self::MySqlShared => "mysql-shared",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "sqlite" => Self::Sqlite,
            "mysql-db" => Self::MySqlDatabase,
            "mysql-shared" => Self::MySqlShared,
            _ => return None,
        })
    }
}

/// One community, as the host keeps it.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub id: i64,
    /// Its name in the host's addresses: `<public_url>c/<name>/`, `community:<name>`.
    pub name: String,
    /// What it is called, to begin with; its admins change that in the app.
    pub title: String,
    pub state: State,
    pub storage: Storage,
    /// Who it was made for: its first admin, and so its owner.
    pub owner: String,
    pub created_at: i64,
    pub created_by: String,
    /// What an admin set for this community alone, over the host's.
    pub limits: Option<Limits>,
    /// When it is to be opened next, unasked: something it has to do then.
    pub wake_at: Option<i64>,
    pub sealed_until: Option<i64>,
}

/// Whether `name` can be a community's name here: 3 to 32 lowercase letters,
/// digits and `-`, starting with a letter, with no `-` last or doubled: what a DID
/// host takes as a name's label (`community:<name>`).
pub fn valid_name(name: &str) -> bool {
    (3..=32).contains(&name.len())
        && !name.contains("--")
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.ends_with('-')
}

const COLUMNS: &str = "id, name, title, state, storage, owner, created_at, created_by, limits, wake_at, sealed_until";

fn record(row: &wyvrn_service::db::Row) -> wyvrn_service::db::Result<Record> {
    let state: String = row.get(3)?;
    let storage: String = row.get(4)?;
    let limits: Option<String> = row.get(8)?;
    Ok(Record {
        id: row.get(0)?,
        name: row.get(1)?,
        title: row.get(2)?,
        state: State::parse(&state).unwrap_or(State::Suspended),
        storage: Storage::parse(&storage).unwrap_or(Storage::Sqlite),
        owner: row.get(5)?,
        created_at: row.get(6)?,
        created_by: row.get(7)?,
        limits: limits.and_then(|limits| serde_json::from_str(&limits).ok()),
        wake_at: row.get(9)?,
        sealed_until: row.get(10)?,
    })
}

pub struct Registry {
    db: Db,
}

impl Registry {
    pub fn open(location: &Location) -> anyhow::Result<Self> {
        Ok(Self { db: Db::open(location, &schema(), "community-host")? })
    }

    pub fn scratch() -> anyhow::Result<Self> {
        Ok(Self { db: Db::scratch(&schema())? })
    }

    /// Adds a community, to be opened at once (so that it takes up its address).
    pub fn create(&self, name: &str, title: &str, storage: Storage, owner: &str, by: &str, now: i64) -> anyhow::Result<Record> {
        if !valid_name(name) {
            anyhow::bail!("{name:?} can't be a community's name: 3 to 32 lowercase letters, digits and -, starting with a letter");
        }
        let mut db = self.db.lock();
        let added = db.execute(
            "INSERT OR IGNORE INTO communities (name, title, state, storage, owner, created_at, created_by, wake_at) VALUES (?1, ?2, 'active', ?3, ?4, ?5, ?6, ?5)",
            params![name, title, storage.as_str(), owner, now, by],
        )?;
        if added == 0 {
            anyhow::bail!("there is a community called {name} already");
        }
        drop(db);
        self.get(name)?.ok_or_else(|| anyhow::anyhow!("{name} was added and then not found"))
    }

    pub fn get(&self, name: &str) -> anyhow::Result<Option<Record>> {
        Ok(self.db.lock().query_row(&format!("SELECT {COLUMNS} FROM communities WHERE name = ?1"), params![name], record)?)
    }

    pub fn all(&self) -> anyhow::Result<Vec<Record>> {
        Ok(self.db.lock().query(&format!("SELECT {COLUMNS} FROM communities ORDER BY name"), params![], record)?)
    }

    /// The served communities that are to be opened by now.
    pub fn due(&self, now: i64) -> anyhow::Result<Vec<Record>> {
        Ok(self.db.lock().query(&format!("SELECT {COLUMNS} FROM communities WHERE wake_at IS NOT NULL AND wake_at <= ?1 AND state IN ('active', 'moving') ORDER BY wake_at"), params![now], record)?)
    }

    pub fn set_state(&self, name: &str, state: State, sealed_until: Option<i64>) -> anyhow::Result<bool> {
        Ok(self.db.lock().execute("UPDATE communities SET state = ?2, sealed_until = ?3 WHERE name = ?1", params![name, state.as_str(), sealed_until])? > 0)
    }

    pub fn set_storage(&self, name: &str, storage: Storage) -> anyhow::Result<bool> {
        Ok(self.db.lock().execute("UPDATE communities SET storage = ?2 WHERE name = ?1", params![name, storage.as_str()])? > 0)
    }

    pub fn set_limits(&self, name: &str, limits: Option<&Limits>) -> anyhow::Result<bool> {
        let limits = limits.map(|limits| serde_json::to_string(limits)).transpose()?;
        Ok(self.db.lock().execute("UPDATE communities SET limits = ?2 WHERE name = ?1", params![name, limits])? > 0)
    }

    pub fn set_wake(&self, name: &str, at: Option<i64>) -> anyhow::Result<()> {
        self.db.lock().execute("UPDATE communities SET wake_at = ?2 WHERE name = ?1", params![name, at])?;
        Ok(())
    }

    pub fn setting(&self, name: &str) -> anyhow::Result<Option<String>> {
        Ok(self.db.lock().query_row("SELECT value FROM host_settings WHERE name = ?1", params![name], |row| row.get(0))?)
    }

    pub fn set_setting(&self, name: &str, value: &str) -> anyhow::Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO host_settings (name, value) VALUES (?1, ?2)", params![name, value])?;
        Ok(())
    }

    /// A code that lets someone make a community, good for `uses` of them.
    pub fn add_code(&self, code: &str, uses: i64, note: &str, by: &str, now: i64) -> anyhow::Result<()> {
        self.db.lock().execute("INSERT INTO create_codes (code, uses_left, note, created_at, created_by) VALUES (?1, ?2, ?3, ?4, ?5)", params![code, uses, note, now, by])?;
        Ok(())
    }

    /// Every code with uses left: `(code, uses_left, note)`.
    pub fn codes(&self) -> anyhow::Result<Vec<(String, i64, String)>> {
        Ok(self.db.lock().query("SELECT code, uses_left, note FROM create_codes WHERE uses_left > 0 ORDER BY created_at, code", params![], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?)
    }

    /// Uses `code` once, if it has a use left.
    pub fn use_code(&self, code: &str) -> anyhow::Result<bool> {
        Ok(self.db.lock().execute("UPDATE create_codes SET uses_left = uses_left - 1 WHERE code = ?1 AND uses_left > 0", params![code])? > 0)
    }

    /// Gives a use back (the community it was to make wasn't made).
    pub fn unuse_code(&self, code: &str) -> anyhow::Result<()> {
        self.db.lock().execute("UPDATE create_codes SET uses_left = uses_left + 1 WHERE code = ?1", params![code])?;
        Ok(())
    }

    pub fn revoke_code(&self, code: &str) -> anyhow::Result<bool> {
        Ok(self.db.lock().execute("DELETE FROM create_codes WHERE code = ?1", params![code])? > 0)
    }

    /// How many communities `owner` made here that are still kept.
    pub fn made_by(&self, owner: &str) -> anyhow::Result<i64> {
        Ok(self.db.lock().query_value("SELECT COUNT(*) FROM communities WHERE created_by = ?1 AND state IN ('active', 'suspended', 'moving')", params![owner])?)
    }

    /// When `owner` last made one here, if they have.
    pub fn last_made_by(&self, owner: &str) -> anyhow::Result<Option<i64>> {
        Ok(self.db.lock().query_value("SELECT MAX(created_at) FROM communities WHERE created_by = ?1", params![owner])?)
    }

    /// Forgets a community altogether (its stores are the caller's to remove).
    pub fn forget(&self, name: &str) -> anyhow::Result<bool> {
        Ok(self.db.lock().execute("DELETE FROM communities WHERE name = ?1", params![name])? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_community_is_added_once_found_by_name_and_woken_when_due() {
        let registry = Registry::scratch().unwrap();
        let made = registry.create("makers", "The Makers", Storage::Sqlite, "did:example:owner", "did:example:admin", 100).unwrap();
        assert_eq!((made.state, made.storage, made.wake_at), (State::Active, Storage::Sqlite, Some(100)));
        assert!(registry.create("makers", "Again", Storage::Sqlite, "did:example:x", "did:example:admin", 101).is_err());
        assert!(registry.create("Makers!", "Bad", Storage::Sqlite, "did:example:x", "did:example:admin", 101).is_err());

        assert_eq!(registry.due(99).unwrap(), vec![]);
        assert_eq!(registry.due(100).unwrap()[0].name, "makers");
        registry.set_wake("makers", None).unwrap();
        assert!(registry.due(1000).unwrap().is_empty());
        registry.set_wake("makers", Some(500)).unwrap();
        registry.set_state("makers", State::Suspended, None).unwrap();
        assert!(registry.due(1000).unwrap().is_empty(), "a suspended community isn't woken");

        let limits = Limits { members: 5, ..Limits::default() };
        registry.set_limits("makers", Some(&limits)).unwrap();
        assert_eq!(registry.get("makers").unwrap().unwrap().limits, Some(limits));
        assert!(registry.forget("makers").unwrap());
        assert!(registry.get("makers").unwrap().is_none());
    }

    #[test]
    fn settings_and_codes_are_kept_in_mysql_too() {
        let Some(url) = std::env::var("WYVRN_TEST_DATABASE_URL").ok().filter(|url| !url.is_empty()) else { return };
        let server = url.trim_end_matches('/');
        let database = format!("wyvrn_test_host_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        {
            let mut admin = mysql::Conn::new(mysql::Opts::from_url(server).unwrap()).unwrap();
            mysql::prelude::Queryable::query_drop(&mut admin, format!("CREATE DATABASE {database}")).unwrap();
        }
        let registry = Registry::open(&Location::MySql(format!("{server}/{database}"))).unwrap();
        registry.set_setting("creating.mode", "codes").unwrap();
        registry.set_setting("creating.mode", "anyone").unwrap();
        assert_eq!(registry.setting("creating.mode").unwrap().as_deref(), Some("anyone"));
        registry.add_code("ABC", 1, "note", &format!("did:example:{}", "a".repeat(2000)), 1).unwrap();
        assert!(registry.use_code("ABC").unwrap());
        assert!(!registry.use_code("ABC").unwrap());
        registry.unuse_code("ABC").unwrap();
        assert_eq!(registry.codes().unwrap().len(), 1);
        registry.create("makers", "The Makers", Storage::Sqlite, "did:example:o", "did:example:o", 5).unwrap();
        assert_eq!((registry.made_by("did:example:o").unwrap(), registry.last_made_by("did:example:o").unwrap()), (1, Some(5)));
    }

    #[test]
    fn a_name_is_what_a_did_hosts_segment_can_be() {
        for good in ["abc", "makers", "the-workshop", "a12"] {
            assert!(valid_name(good), "{good}");
        }
        for bad in ["ab", "1abc", "-abc", "abc-", "a--b", "Abc", "a_bc", "a.bc", &"a".repeat(33)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
}
