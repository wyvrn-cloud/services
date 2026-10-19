//! What the relay keeps: who it has granted. Nothing of what passes through it.

use wyvrn_service::db::{Db, Location, Schema, MYSQL_DID, MYSQL_TABLE};
use wyvrn_service::params;

const SQLITE: &str = "CREATE TABLE IF NOT EXISTS grants (did TEXT PRIMARY KEY, granted INTEGER NOT NULL);";

fn schema() -> Schema {
    static MYSQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let mysql = MYSQL.get_or_init(|| format!("CREATE TABLE IF NOT EXISTS grants (did {MYSQL_DID} NOT NULL PRIMARY KEY, granted BIGINT NOT NULL) {MYSQL_TABLE}"));
    Schema { sqlite: SQLITE, mysql }
}

pub struct Store {
    db: Db,
}

impl Store {
    pub fn open(location: &Location) -> anyhow::Result<Self> {
        Ok(Self { db: Db::open(location, &schema(), "relay")? })
    }

    pub fn scratch() -> anyhow::Result<Self> {
        Ok(Self { db: Db::scratch(&schema())? })
    }

    pub fn grant(&self, did: &str, now: i64) -> anyhow::Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM grants WHERE did = ?", params![did])?;
        db.execute("INSERT INTO grants (did, granted) VALUES (?, ?)", params![did, now])?;
        Ok(())
    }

    pub fn release(&self, did: &str) -> anyhow::Result<bool> {
        Ok(self.db.lock().execute("DELETE FROM grants WHERE did = ?", params![did])? > 0)
    }

    pub fn granted(&self, did: &str) -> anyhow::Result<bool> {
        Ok(self.db.lock().query_row("SELECT granted FROM grants WHERE did = ?", params![did], |row| row.get::<i64>(0))?.is_some())
    }

    /// Every DID granted, and when.
    pub fn grants(&self) -> anyhow::Result<Vec<(String, i64)>> {
        Ok(self.db.lock().query("SELECT did, granted FROM grants ORDER BY granted", params![], |row| Ok((row.get(0)?, row.get(1)?)))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_is_kept_until_released() {
        let store = Store::scratch().unwrap();
        assert!(!store.granted("did:example:a").unwrap());
        store.grant("did:example:a", 10).unwrap();
        store.grant("did:example:a", 20).unwrap();
        assert!(store.granted("did:example:a").unwrap());
        assert_eq!(store.grants().unwrap(), vec![("did:example:a".to_string(), 20)]);
        assert!(store.release("did:example:a").unwrap());
        assert!(!store.granted("did:example:a").unwrap());
    }
}
