//! Sending someone what they are owed, when sending can fail: what a failure
//! means, and how long to wait before trying them again.
//!
//! A service that delivers to the people it serves (a channel's subscribers, a
//! community's members) sends through whatever their DIDs' documents give,
//! typically a mediator. Not every failure is worth trying again a minute later:
//! a mediator that holds no mailbox for the DID (coordinate-mediation 3.1: a
//! forward it refuses is answered `404`, `e.p.msg.unknown-recipient`) will say so
//! every time until its holder registers it again, which may be never. So:
//!
//! - **Gone** ([`Failure::Gone`]): `404` or `410`. Nobody takes mail for the DID.
//!   Delivery to it stops, and resumes when its holder writes again (then it is
//!   plainly there), or at a rare try in case it was registered again meanwhile.
//! - **Too large** ([`Failure::TooLarge`]): `413`. That message will never be
//!   taken; the next one may.
//! - **Later** ([`Failure::Later`]): anything else (`429`, `503`, no answer, a DID
//!   that couldn't be read). Tried again after a wait that doubles each time.

use std::collections::HashMap;

use didcomm_agent::AgentError;

use crate::db::{Db, Result};
use crate::params;

/// What a failed send says about trying again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// Nobody takes mail for the DID: stop, until they are heard from.
    Gone,
    /// The message itself is refused for its size: pass it over.
    TooLarge,
    /// Worth trying again, after a while.
    Later,
}

/// What `error`, from sending to someone, says about trying again.
pub fn failure(error: &anyhow::Error) -> Failure {
    match error.downcast_ref::<AgentError>() {
        Some(AgentError::HttpStatus { status: 404 | 410, .. }) => Failure::Gone,
        Some(AgentError::HttpStatus { status: 413, .. }) => Failure::TooLarge,
        _ => Failure::Later,
    }
}

/// The first wait after a failure worth trying again, in seconds.
pub const LATER_FIRST: i64 = 60;
/// The longest wait between tries of someone who can't be reached for now.
pub const LATER_AT_MOST: i64 = 60 * 60;
/// The first wait after someone was found to be gone, in seconds.
pub const GONE_FIRST: i64 = 60 * 60;
/// The longest wait between tries of someone gone: once a day.
pub const GONE_AT_MOST: i64 = 24 * 60 * 60;

/// How long to wait before trying someone again, in seconds, after `tries`
/// failures in a row (from 1), the last of them `gone` or not: doubling each time,
/// up to a cap.
pub fn retry_in(gone: bool, tries: i64) -> i64 {
    let (first, at_most) = if gone { (GONE_FIRST, GONE_AT_MOST) } else { (LATER_FIRST, LATER_AT_MOST) };
    let doublings = (tries.max(1) - 1).min(20) as u32;
    first.saturating_mul(1 << doublings).min(at_most)
}

/// The table a store keeps those it couldn't deliver to in, for SQLite: one row
/// a DID, from its first failure in a row until a delivery to it goes through or
/// it is heard from. Each store's schema includes it ([`mysql_table`] for MySQL).
pub const SQLITE_TABLE: &str = "CREATE TABLE IF NOT EXISTS undelivered (
        did TEXT PRIMARY KEY,
        since INTEGER NOT NULL,
        tries INTEGER NOT NULL,
        gone INTEGER NOT NULL,
        next_at INTEGER NOT NULL
    )";

/// [`SQLITE_TABLE`] for MySQL, with the store's DID column type and table options.
pub fn mysql_table(did_column: &str, table_options: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS undelivered (
            did {did_column} NOT NULL PRIMARY KEY,
            since BIGINT NOT NULL,
            tries BIGINT NOT NULL,
            gone BIGINT NOT NULL,
            next_at BIGINT NOT NULL
        ) {table_options}"
    )
}

/// Someone a delivery to failed, and when they are tried next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undelivered {
    /// Failures in a row.
    pub tries: i64,
    /// Whether the last said nobody takes their mail ([`Failure::Gone`]).
    pub gone: bool,
    /// Unix seconds: not tried before then.
    pub next_at: i64,
}

impl Undelivered {
    /// Whether they are to be passed over at `now`.
    pub fn held_back(&self, now: i64) -> bool {
        self.next_at > now
    }
}

/// Everyone a delivery has failed to, by DID.
pub fn undelivered(db: &Db) -> Result<HashMap<String, Undelivered>> {
    let rows = db.lock().query("SELECT did, tries, gone, next_at FROM undelivered", params![], |row| {
        Ok((row.get::<String>(0)?, Undelivered { tries: row.get(1)?, gone: row.get(2)?, next_at: row.get(3)? }))
    })?;
    Ok(rows.into_iter().collect())
}

/// Notes that a delivery to `did` failed at `now` (`gone`: [`Failure::Gone`]), and
/// when to try them next. Returns that, and whether they were already known to be
/// gone (a failure logged once already).
pub fn delivery_failed(db: &Db, did: &str, gone: bool, now: i64) -> Result<(Undelivered, bool)> {
    let mut db = db.lock();
    let before: Option<(i64, i64, bool)> =
        db.query_row("SELECT since, tries, gone FROM undelivered WHERE did = ?1", params![did], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    let (since, tries, was_gone) = before.map_or((now, 1, false), |(since, tries, was_gone)| (since, tries + 1, was_gone));
    let next_at = now + retry_in(gone, tries);
    db.execute("INSERT OR REPLACE INTO undelivered (did, since, tries, gone, next_at) VALUES (?1, ?2, ?3, ?4, ?5)", params![did, since, tries, gone, next_at])?;
    Ok((Undelivered { tries, gone, next_at }, was_gone))
}

/// `did` is reachable again (a delivery went through, or they wrote): delivered to
/// as everyone is. Returns whether they had been held back.
pub fn delivery_resumed(db: &Db, did: &str) -> Result<bool> {
    let mut db = db.lock();
    if db.query_row("SELECT 1 FROM undelivered WHERE did = ?1", params![did], |row| row.get::<i64>(0))?.is_none() {
        return Ok(false);
    }
    db.execute("DELETE FROM undelivered WHERE did = ?1", params![did])?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(status: u16) -> anyhow::Error {
        AgentError::HttpStatus { uri: "https://mediator.example".into(), status, body: String::new() }.into()
    }

    #[test]
    fn a_mediator_holding_no_mailbox_means_gone_and_a_busy_one_means_later() {
        assert_eq!(failure(&status(404)), Failure::Gone);
        assert_eq!(failure(&status(410)), Failure::Gone);
        assert_eq!(failure(&status(413)), Failure::TooLarge);
        for code in [429, 500, 502, 503] {
            assert_eq!(failure(&status(code)), Failure::Later, "{code}");
        }
        assert_eq!(failure(&anyhow::anyhow!("connection refused")), Failure::Later);
        // As it arrives: wrapped in the context of whatever sent it.
        assert_eq!(failure(&status(404).context("sending a post")), Failure::Gone);
    }

    #[test]
    fn a_failure_is_kept_until_the_did_is_reachable_again() {
        let mysql: &'static str = Box::leak(mysql_table(crate::db::MYSQL_DID, crate::db::MYSQL_TABLE).into_boxed_str());
        let db = Db::scratch(&crate::db::Schema { sqlite: SQLITE_TABLE, mysql }).unwrap();
        let did = "did:example:gone";
        assert!(undelivered(&db).unwrap().is_empty());
        let (first, was_gone) = delivery_failed(&db, did, true, 1_000).unwrap();
        assert_eq!((first.tries, first.gone, first.next_at, was_gone), (1, true, 1_000 + GONE_FIRST, false));
        let (second, was_gone) = delivery_failed(&db, did, true, first.next_at).unwrap();
        assert_eq!((second.tries, was_gone), (2, true));
        assert_eq!(second.next_at, first.next_at + 2 * GONE_FIRST);
        let held = undelivered(&db).unwrap();
        assert!(held[did].held_back(first.next_at + 1));
        assert!(!held[did].held_back(second.next_at));
        assert!(delivery_resumed(&db, did).unwrap());
        assert!(!delivery_resumed(&db, did).unwrap());
        assert!(undelivered(&db).unwrap().is_empty());
        // Starting over, it waits as little as the first time.
        assert_eq!(delivery_failed(&db, did, false, 5_000).unwrap().0.next_at, 5_000 + LATER_FIRST);
    }

    #[test]
    fn waits_double_up_to_an_hour_or_a_day_for_someone_gone() {
        let later: Vec<i64> = (1..=8).map(|tries| retry_in(false, tries)).collect();
        assert_eq!(later, [60, 120, 240, 480, 960, 1920, 3600, 3600]);
        assert_eq!(retry_in(true, 1), 3600);
        assert_eq!(retry_in(true, 5), 16 * 3600);
        assert_eq!(retry_in(true, 6), 24 * 3600);
        assert_eq!(retry_in(true, 1000), 24 * 3600);
        assert_eq!(retry_in(false, 0), 60);
    }
}
