//! A service's own keys, kept where the rest of what it keeps is: in a file beside
//! a SQLite store, or in the database for one that has a database ([`crate::db`]).
//!
//! With a database nothing is left on the machine itself, so a service can be
//! moved or its container remade with nothing carried over but its settings. The
//! keys are stored as the file holds them: unencrypted. Whoever can read the
//! database can be the service, as whoever could read the file could.

use std::path::Path;

use anyhow::Context;
use didcomm_agent::Identity;

use crate::db::{Db, Location, Schema, MYSQL_TABLE};
use crate::params;

fn schema() -> Schema {
    static MYSQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let mysql = MYSQL.get_or_init(|| format!("CREATE TABLE IF NOT EXISTS wyvrn_identity (id BIGINT NOT NULL PRIMARY KEY, identity MEDIUMTEXT NOT NULL) {MYSQL_TABLE}"));
    Schema { sqlite: "CREATE TABLE IF NOT EXISTS wyvrn_identity (id INTEGER PRIMARY KEY, identity TEXT NOT NULL)", mysql }
}

/// The identity of the service called `service`, whose store is at `location`:
/// made the first time, and the same one ever after.
///
/// For a SQLite store it is the file at `path`, as it always was. For a database
/// it is in the database; a service that had a file at `path` before it had a
/// database keeps those keys, which are copied in (the file is left where it is,
/// and not read again).
pub fn load_or_generate(location: &Location, path: &Path, service: &str) -> anyhow::Result<Identity> {
    match location {
        Location::Sqlite(_) => Identity::load_or_generate(path).with_context(|| format!("identity file {}", path.display())),
        Location::MySql(_) => {
            let db = Db::open(location, &schema(), service).with_context(|| format!("the identity in {location}"))?;
            in_database(&db, path)
        }
    }
}

fn in_database(db: &Db, path: &Path) -> anyhow::Result<Identity> {
    const HELD: &str = "SELECT identity FROM wyvrn_identity WHERE id = 1";
    let mut session = db.lock();
    if let Some(held) = session.query_row(HELD, params![], |row| row.get::<String>(0))? {
        return Ok(Identity::from_json(&held)?);
    }
    let (identity, from) = if path.exists() {
        (Identity::load(path).with_context(|| format!("identity file {}", path.display()))?, "copied from its file")
    } else {
        (Identity::generate()?, "made")
    };
    // Another start of the same service may have got there first: what is held
    // is what both use.
    session.execute("INSERT OR IGNORE INTO wyvrn_identity (id, identity) VALUES (1, ?1)", params![identity.to_json()?])?;
    let held: String = session.query_value(HELD, params![])?;
    tracing::info!("this service's keys are kept in its database ({from})");
    Ok(Identity::from_json(&held)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(identity: &Identity) -> String {
        identity.to_json().unwrap()
    }

    #[test]
    fn keys_in_a_database_are_made_once_and_a_file_that_was_there_is_taken_in() {
        let nowhere = Path::new("/nonexistent/identity.json");
        let db = Db::scratch(&schema()).unwrap();
        let first = in_database(&db, nowhere).unwrap();
        assert_eq!(keys(&in_database(&db, nowhere).unwrap()), keys(&first));

        let file = std::env::temp_dir().join(format!("wyvrn-identity-test-{}-{:?}.json", std::process::id(), std::thread::current().id()));
        let before = Identity::generate().unwrap();
        before.save(&file).unwrap();
        let moved = Db::scratch(&schema()).unwrap();
        assert_eq!(keys(&in_database(&moved, &file).unwrap()), keys(&before));
        // Held from then on, whatever becomes of the file.
        std::fs::remove_file(&file).unwrap();
        assert_eq!(keys(&in_database(&moved, &file).unwrap()), keys(&before));
        assert_ne!(keys(&before), keys(&first));
    }
}
