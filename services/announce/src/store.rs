//! What the channel keeps: every post, and who is subscribed.

use wyvrn_service::db::{Db, Location, Result, Row, Schema, MYSQL_DID, MYSQL_TABLE};
use wyvrn_service::params;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Subscriber,
    Admin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Subscriber => "subscriber",
            Role::Admin => "admin",
        }
    }

    pub fn parse(role: &str) -> Option<Self> {
        match role {
            "subscriber" => Some(Role::Subscriber),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
}

/// Someone the channel knows: a subscriber, or an admin named ahead of subscribing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub did: String,
    pub role: Role,
    /// Whether posts are sent to them. An admin named in the configuration before
    /// ever subscribing is a member who isn't.
    pub subscribed: bool,
    /// The name their app gave (user-profile/1.0), if it gave one.
    pub name: Option<String>,
    /// Unix seconds.
    pub since: i64,
    /// The last post sent to them.
    pub delivered_seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    /// Its place in the log, from 1.
    pub seq: i64,
    /// The id every copy of it is sent with.
    pub id: String,
    pub content: String,
    /// Unix seconds.
    pub posted_at: i64,
    pub author_did: String,
    pub author_name: Option<String>,
}

pub struct Store {
    db: Db,
}

fn member_from(row: &Row) -> Result<Member> {
    let role: String = row.get(1)?;
    Ok(Member {
        did: row.get(0)?,
        role: Role::parse(&role).unwrap_or(Role::Subscriber),
        subscribed: row.get(2)?,
        name: row.get(3)?,
        since: row.get(4)?,
        delivered_seq: row.get(5)?,
    })
}

fn post_from(row: &Row) -> Result<Post> {
    Ok(Post {
        seq: row.get(0)?,
        id: row.get(1)?,
        content: row.get(2)?,
        posted_at: row.get(3)?,
        author_did: row.get(4)?,
        author_name: row.get(5)?,
    })
}

const MEMBER_COLUMNS: &str = "did, role, subscribed, name, since, delivered_seq";
const POST_COLUMNS: &str = "seq, id, content, posted_at, author_did, author_name";

const SQLITE: &str = "CREATE TABLE IF NOT EXISTS members (
        did TEXT PRIMARY KEY,
        role TEXT NOT NULL,
        subscribed INTEGER NOT NULL,
        name TEXT,
        since INTEGER NOT NULL,
        delivered_seq INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS posts (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        id TEXT NOT NULL UNIQUE,
        content TEXT NOT NULL,
        posted_at INTEGER NOT NULL,
        author_did TEXT NOT NULL,
        author_name TEXT
    );
    -- DIDs a member has left (a verified from_prior). Nothing from one is
    -- acted on again: a device removed from an account still has its keys.
    CREATE TABLE IF NOT EXISTS superseded (
        did TEXT PRIMARY KEY,
        became TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    -- What each member has on each post: their whole set, as last stated.
    -- An emptied set is kept, so an older statement can't bring it back.
    CREATE TABLE IF NOT EXISTS reactions (
        seq INTEGER NOT NULL,
        did TEXT NOT NULL,
        emoji TEXT NOT NULL,
        at INTEGER NOT NULL,
        PRIMARY KEY (seq, did)
    );";

fn schema() -> Schema {
    static MYSQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let mysql = MYSQL.get_or_init(|| {
        format!(
            "CREATE TABLE IF NOT EXISTS members (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                role VARCHAR(16) NOT NULL,
                subscribed BIGINT NOT NULL,
                name TEXT,
                since BIGINT NOT NULL,
                delivered_seq BIGINT NOT NULL DEFAULT 0
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS posts (
                seq BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
                id VARCHAR(255) NOT NULL UNIQUE,
                content MEDIUMTEXT NOT NULL,
                posted_at BIGINT NOT NULL,
                author_did {MYSQL_DID} NOT NULL,
                author_name TEXT
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS superseded (
                did {MYSQL_DID} NOT NULL PRIMARY KEY,
                became {MYSQL_DID} NOT NULL,
                at BIGINT NOT NULL
            ) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS reactions (
                seq BIGINT NOT NULL,
                did {MYSQL_DID} NOT NULL,
                emoji TEXT NOT NULL,
                at BIGINT NOT NULL,
                PRIMARY KEY (seq, did)
            ) {MYSQL_TABLE}"
        )
    });
    Schema { sqlite: SQLITE, mysql }
}

impl Store {
    pub fn open(location: &Location) -> anyhow::Result<Self> {
        Ok(Self { db: Db::open(location, &schema(), "announce")? })
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        Ok(Self { db: Db::scratch(&schema())? })
    }

    pub fn member(&self, did: &str) -> Result<Option<Member>> {
        self.db.lock().query_row(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE did = ?1"), params![did], member_from)
    }

    /// Everyone posts are sent to, oldest subscription first.
    pub fn subscribers(&self) -> Result<Vec<Member>> {
        self.db.lock().query(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE subscribed = 1 ORDER BY since, did"), params![], member_from)
    }

    /// Every member, subscribed or not.
    pub fn members(&self) -> Result<Vec<Member>> {
        self.db.lock().query(&format!("SELECT {MEMBER_COLUMNS} FROM members ORDER BY since, did"), params![], member_from)
    }

    pub fn subscriber_count(&self) -> Result<i64> {
        self.db.lock().query_value("SELECT COUNT(*) FROM members WHERE subscribed = 1", params![])
    }

    /// Subscribes `did`, keeping the role it has if the channel already knows it. A
    /// new subscriber starts at the latest post: the log is theirs to ask for, not
    /// sent at them.
    pub fn subscribe(&self, did: &str, now: i64) -> Result<Member> {
        let mut db = self.db.lock();
        let latest: i64 = db.query_value("SELECT COALESCE(MAX(seq), 0) FROM posts", params![])?;
        // Someone already subscribed is left as they are; a member who wasn't (an
        // admin named ahead) starts from now; anyone else is new.
        let known = db.execute("UPDATE members SET since = ?2, delivered_seq = ?3, subscribed = 1 WHERE did = ?1 AND subscribed = 0", params![did, now, latest])?;
        if known == 0 {
            db.execute("INSERT OR IGNORE INTO members (did, role, subscribed, since, delivered_seq) VALUES (?1, 'subscriber', 1, ?2, ?3)", params![did, now, latest])?;
        }
        db.query_row(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE did = ?1"), params![did], member_from)?
            .ok_or_else(|| wyvrn_service::db::Error::Other("a member just written is not there".into()))
    }

    /// Stops sending to `did`. An admin stays one, for when they come back.
    pub fn unsubscribe(&self, did: &str) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("UPDATE members SET subscribed = 0 WHERE did = ?1 AND role = 'admin'", params![did])?;
        db.execute("DELETE FROM members WHERE did = ?1 AND role <> 'admin'", params![did])?;
        Ok(())
    }

    /// Gives `did` a role, whether or not it has subscribed: an admin can be named
    /// first. Making a member who never subscribed an ordinary one again forgets them.
    pub fn set_role(&self, did: &str, role: Role, now: i64) -> Result<()> {
        let mut db = self.db.lock();
        if db.execute("UPDATE members SET role = ?2 WHERE did = ?1", params![did, role.as_str()])? == 0 {
            db.execute("INSERT INTO members (did, role, subscribed, since) VALUES (?1, ?2, 0, ?3)", params![did, role.as_str(), now])?;
        }
        db.execute("DELETE FROM members WHERE did = ?1 AND role = 'subscriber' AND subscribed = 0", params![did])?;
        Ok(())
    }

    /// Forgets a member who was only ever named, and never subscribed.
    pub fn forget_unsubscribed(&self, did: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM members WHERE did = ?1 AND subscribed = 0", params![did])?;
        Ok(())
    }

    pub fn set_name(&self, did: &str, name: Option<&str>) -> Result<()> {
        self.db.lock().execute("UPDATE members SET name = ?2 WHERE did = ?1", params![did, name])?;
        Ok(())
    }

    pub fn set_delivered(&self, did: &str, seq: i64) -> Result<()> {
        self.db.lock().execute("UPDATE members SET delivered_seq = ?2 WHERE did = ?1 AND delivered_seq < ?2", params![did, seq])?;
        Ok(())
    }

    pub fn is_superseded(&self, did: &str) -> Result<bool> {
        Ok(self.db.lock().query_row("SELECT 1 FROM superseded WHERE did = ?1", params![did], |_| Ok(()))?.is_some())
    }

    /// A member has changed DID: everything the channel holds for `prior` is `new`'s
    /// now, and `prior` is never listened to again. Returns whether `prior` was a
    /// member. Where `new` is one already (it subscribed afresh before saying where it
    /// came from), it keeps the higher role of the two and stays subscribed if either
    /// was.
    pub fn rotate(&self, prior: &str, new: &str, now: i64) -> Result<bool> {
        self.db.lock().transaction(|tx| {
            let old = tx.query_row(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE did = ?1"), params![prior], member_from)?;
            if let Some(old) = &old {
                let existing = tx.query_row(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE did = ?1"), params![new], member_from)?;
                tx.execute("DELETE FROM members WHERE did = ?1", params![prior])?;
                match existing {
                    None => {
                        tx.execute(
                            "INSERT INTO members (did, role, subscribed, name, since, delivered_seq)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            params![new, old.role.as_str(), old.subscribed, old.name, old.since, old.delivered_seq],
                        )?;
                    }
                    Some(existing) => {
                        let role = if old.role == Role::Admin { Role::Admin } else { existing.role };
                        tx.execute(
                            "UPDATE members SET role = ?2, subscribed = ?3, name = ?4, since = ?5, delivered_seq = ?6 WHERE did = ?1",
                            params![
                                new,
                                role.as_str(),
                                old.subscribed || existing.subscribed,
                                existing.name.as_ref().or(old.name.as_ref()),
                                existing.since.min(old.since),
                                existing.delivered_seq.max(old.delivered_seq)
                            ],
                        )?;
                    }
                }
            }
            // Their reactions go with them. Where they had reacted to a post as both,
            // what the new DID said stands.
            tx.execute("UPDATE OR IGNORE reactions SET did = ?2 WHERE did = ?1", params![prior, new])?;
            tx.execute("DELETE FROM reactions WHERE did = ?1", params![prior])?;
            tx.execute("INSERT OR REPLACE INTO superseded (did, became, at) VALUES (?1, ?2, ?3)", params![prior, new, now])?;
            Ok(old.is_some())
        })
    }

    pub fn add_post(&self, id: &str, content: &str, author: &Member, now: i64) -> Result<Post> {
        let mut db = self.db.lock();
        let seq = db.insert(
            "INSERT INTO posts (id, content, posted_at, author_did, author_name) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, content, now, author.did, author.name],
        )?;
        db.query_row(&format!("SELECT {POST_COLUMNS} FROM posts WHERE seq = ?1"), params![seq], post_from)?
            .ok_or_else(|| wyvrn_service::db::Error::Other("a post just written is not there".into()))
    }

    pub fn post(&self, seq: i64) -> Result<Option<Post>> {
        self.db.lock().query_row(&format!("SELECT {POST_COLUMNS} FROM posts WHERE seq = ?1"), params![seq], post_from)
    }

    /// The post sent out under `id`. Ids are compared as DIDComm compares them,
    /// without regard to case.
    pub fn post_by_id(&self, id: &str) -> Result<Option<Post>> {
        self.db.lock().query_row(&format!("SELECT {POST_COLUMNS} FROM posts WHERE lower(id) = lower(?1)"), params![id], post_from)
    }

    /// Makes `emoji` the whole of what `did` has on post `seq`, as stated at `at`.
    /// Returns whether anything changed: a statement no newer than the one held is
    /// ignored, and so is one that says the same.
    pub fn set_reactions(&self, seq: i64, did: &str, emoji: &[String], at: i64) -> Result<bool> {
        let mut db = self.db.lock();
        let held: Option<(String, i64)> =
            db.query_row("SELECT emoji, at FROM reactions WHERE seq = ?1 AND did = ?2", params![seq, did], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let stated = serde_json::to_string(emoji).unwrap_or_else(|_| "[]".into());
        if let Some((held_emoji, held_at)) = &held {
            if *held_at >= at {
                return Ok(false);
            }
            if *held_emoji == stated {
                db.execute("UPDATE reactions SET at = ?3 WHERE seq = ?1 AND did = ?2", params![seq, did, at])?;
                return Ok(false);
            }
        }
        db.execute("INSERT OR REPLACE INTO reactions (seq, did, emoji, at) VALUES (?1, ?2, ?3, ?4)", params![seq, did, stated, at])?;
        Ok(held.is_some() || !emoji.is_empty())
    }

    /// What `did` has on post `seq`.
    pub fn reactions_of(&self, seq: i64, did: &str) -> Result<Vec<String>> {
        let held: Option<String> = self.db.lock().query_row("SELECT emoji FROM reactions WHERE seq = ?1 AND did = ?2", params![seq, did], |row| row.get(0))?;
        Ok(held.and_then(|emoji| serde_json::from_str(&emoji).ok()).unwrap_or_default())
    }

    /// Each emoji on post `seq` with how many members have it there, in the order
    /// the emoji first appeared. Never who.
    pub fn reaction_counts(&self, seq: i64) -> Result<Vec<(String, i64)>> {
        let sets: Vec<String> = self.db.lock().query("SELECT emoji FROM reactions WHERE seq = ?1 ORDER BY at, did", params![seq], |row| row.get(0))?;
        let mut counts: Vec<(String, i64)> = Vec::new();
        for set in sets {
            for emoji in serde_json::from_str::<Vec<String>>(&set).unwrap_or_default() {
                match counts.iter_mut().find(|(known, _)| *known == emoji) {
                    Some((_, count)) => *count += 1,
                    None => counts.push((emoji, 1)),
                }
            }
        }
        Ok(counts)
    }

    /// Who has each emoji on post `seq`: a name where the member gave one, their DID
    /// where they didn't, earliest first. For an admin's eyes only.
    pub fn reactors(&self, seq: i64) -> Result<Vec<(String, Vec<String>)>> {
        let sets: Vec<(String, String)> = self.db.lock().query(
            "SELECT reactions.emoji, COALESCE(members.name, reactions.did)
             FROM reactions LEFT JOIN members ON members.did = reactions.did
             WHERE reactions.seq = ?1 ORDER BY reactions.at, reactions.did",
            params![seq],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut reactors: Vec<(String, Vec<String>)> = Vec::new();
        for (set, who) in sets {
            for emoji in serde_json::from_str::<Vec<String>>(&set).unwrap_or_default() {
                match reactors.iter_mut().find(|(known, _)| *known == emoji) {
                    Some((_, names)) => names.push(who.clone()),
                    None => reactors.push((emoji, vec![who.clone()])),
                }
            }
        }
        Ok(reactors)
    }

    pub fn latest_seq(&self) -> Result<i64> {
        self.db.lock().query_value("SELECT COALESCE(MAX(seq), 0) FROM posts", params![])
    }

    /// Up to `limit` posts older than `before` (or the newest, without one), newest
    /// first.
    pub fn posts_before(&self, before: Option<i64>, limit: usize) -> Result<Vec<Post>> {
        self.db.lock().query(&format!("SELECT {POST_COLUMNS} FROM posts WHERE seq < ?1 ORDER BY seq DESC LIMIT ?2"), params![before.unwrap_or(i64::MAX), limit as i64], post_from)
    }

    /// Up to `limit` posts newer than `after`, oldest first.
    pub fn posts_after(&self, after: i64, limit: usize) -> Result<Vec<Post>> {
        self.db.lock().query(&format!("SELECT {POST_COLUMNS} FROM posts WHERE seq > ?1 ORDER BY seq LIMIT ?2"), params![after, limit as i64], post_from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::in_memory().unwrap()
    }

    fn admin(store: &Store, did: &str) -> Member {
        store.set_role(did, Role::Admin, 1).unwrap();
        store.member(did).unwrap().unwrap()
    }

    #[test]
    fn a_new_subscriber_starts_at_the_latest_post() {
        let store = store();
        let author = admin(&store, "did:a");
        store.add_post("p1", "one", &author, 10).unwrap();
        store.add_post("p2", "two", &author, 11).unwrap();
        let member = store.subscribe("did:s", 20).unwrap();
        assert_eq!((member.role, member.subscribed, member.delivered_seq), (Role::Subscriber, true, 2));
    }

    #[test]
    fn subscribing_again_changes_nothing() {
        let store = store();
        let author = admin(&store, "did:a");
        store.subscribe("did:s", 20).unwrap();
        store.add_post("p1", "one", &author, 30).unwrap();
        let again = store.subscribe("did:s", 40).unwrap();
        assert_eq!((again.since, again.delivered_seq), (20, 0));
        assert_eq!(store.subscriber_count().unwrap(), 1);
    }

    #[test]
    fn an_admin_named_before_subscribing_is_one_when_they_do() {
        let store = store();
        admin(&store, "did:a");
        assert!(store.subscribers().unwrap().is_empty());
        let member = store.subscribe("did:a", 5).unwrap();
        assert_eq!((member.role, member.subscribed), (Role::Admin, true));
    }

    #[test]
    fn unsubscribing_forgets_a_subscriber_and_keeps_an_admin() {
        let store = store();
        admin(&store, "did:a");
        store.subscribe("did:a", 5).unwrap();
        store.subscribe("did:s", 5).unwrap();
        store.unsubscribe("did:a").unwrap();
        store.unsubscribe("did:s").unwrap();
        assert!(store.subscribers().unwrap().is_empty());
        assert_eq!(store.member("did:a").unwrap().unwrap().role, Role::Admin);
        assert!(store.member("did:s").unwrap().is_none());
    }

    #[test]
    fn a_change_of_did_carries_everything_across_and_retires_the_old_one() {
        let store = store();
        admin(&store, "did:old");
        store.subscribe("did:old", 5).unwrap();
        store.set_name("did:old", Some("Ada")).unwrap();
        assert!(store.rotate("did:old", "did:new", 9).unwrap());
        assert!(store.member("did:old").unwrap().is_none());
        let moved = store.member("did:new").unwrap().unwrap();
        assert_eq!((moved.role, moved.subscribed, moved.name.as_deref(), moved.since), (Role::Admin, true, Some("Ada"), 5));
        assert!(store.is_superseded("did:old").unwrap());
        assert!(!store.is_superseded("did:new").unwrap());
    }

    #[test]
    fn a_change_of_did_onto_a_member_keeps_the_higher_role() {
        let store = store();
        admin(&store, "did:old");
        store.subscribe("did:new", 7).unwrap();
        assert!(store.rotate("did:old", "did:new", 9).unwrap());
        let merged = store.member("did:new").unwrap().unwrap();
        assert_eq!((merged.role, merged.subscribed), (Role::Admin, true));
        assert_eq!(store.members().unwrap().len(), 1);
    }

    #[test]
    fn a_change_from_a_did_nobody_knew_is_only_noted() {
        let store = store();
        assert!(!store.rotate("did:stranger", "did:new", 9).unwrap());
        assert!(store.member("did:new").unwrap().is_none());
        assert!(store.is_superseded("did:stranger").unwrap());
    }

    #[test]
    fn posts_are_numbered_from_one_and_read_either_way() {
        let store = store();
        let author = admin(&store, "did:a");
        for (i, text) in ["one", "two", "three"].iter().enumerate() {
            assert_eq!(store.add_post(&format!("p{i}"), text, &author, 10).unwrap().seq, i as i64 + 1);
        }
        let newest: Vec<_> = store.posts_before(None, 2).unwrap().into_iter().map(|p| p.seq).collect();
        assert_eq!(newest, [3, 2]);
        let older: Vec<_> = store.posts_before(Some(2), 10).unwrap().into_iter().map(|p| p.seq).collect();
        assert_eq!(older, [1]);
        let after: Vec<_> = store.posts_after(1, 10).unwrap().into_iter().map(|p| p.seq).collect();
        assert_eq!(after, [2, 3]);
        assert_eq!(store.latest_seq().unwrap(), 3);
    }
}
