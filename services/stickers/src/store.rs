//! What the service keeps: packs, the stickers in them, and the workflows that
//! are under way -- each with the stickers it has gathered so far.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::images::Image;

/// What a pack's pictures are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Sticker,
    /// Custom emoji: small, and each known by a shortcode to be typed.
    Emoji,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sticker => "sticker",
            Self::Emoji => "emoji",
        }
    }

    fn parse(text: &str) -> Self {
        if text == "emoji" { Self::Emoji } else { Self::Sticker }
    }
}

#[derive(Debug, Clone)]
pub struct Pack {
    pub name: String,
    pub kind: Kind,
    pub title: String,
    /// The DID of whoever made it, and may change it.
    pub owner: String,
    /// Whether people other than its maker may add it and send from it.
    pub reuse: bool,
    /// The id of the sticker that stands for the pack, if one was chosen.
    pub icon: Option<String>,
    /// Counts every change, so a copy can be told to be out of date.
    pub version: i64,
    pub created: i64,
    pub updated: i64,
    /// Taken down: its name stays taken, and nothing of it is served.
    pub removed: bool,
}

/// One picture of a pack, or of a pack being made.
#[derive(Debug, Clone)]
pub struct Sticker {
    pub id: String,
    pub media_type: String,
    pub extension: String,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
    pub animated: bool,
    pub bytes: usize,
    /// The emoji it stands for. A custom emoji may have none.
    pub emoji: String,
    /// What a custom emoji is typed as, between colons.
    pub shortcode: Option<String>,
}

impl Sticker {
    pub fn file(&self) -> String {
        format!("{}.{}", self.id, self.extension)
    }
}

/// A workflow under way, or finished: `workflow/1.0`'s instance.
#[derive(Debug, Clone)]
pub struct Instance {
    pub id: String,
    pub template: String,
    /// The DID it was started by: the connection it is bound to.
    pub owner: String,
    pub state: String,
    /// `active`, `paused`, `canceled` or `completed`.
    pub status: String,
    pub context: Value,
    pub updated: i64,
}

pub struct Store {
    db: Mutex<Connection>,
}

/// Where a sticker belongs: a published pack, or a workflow's gathering of them.
pub fn pack_scope(name: &str) -> String {
    format!("pack:{name}")
}
pub fn draft_scope(instance: &str) -> String {
    format!("draft:{instance}")
}

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
            "CREATE TABLE IF NOT EXISTS packs (
                name TEXT PRIMARY KEY, kind TEXT NOT NULL, title TEXT NOT NULL, owner TEXT NOT NULL,
                reuse INTEGER NOT NULL, icon TEXT, version INTEGER NOT NULL, created INTEGER NOT NULL,
                updated INTEGER NOT NULL, removed INTEGER NOT NULL DEFAULT 0);
             CREATE INDEX IF NOT EXISTS packs_owner ON packs (owner);
             CREATE TABLE IF NOT EXISTS stickers (
                scope TEXT NOT NULL, position INTEGER NOT NULL, id TEXT NOT NULL, media_type TEXT NOT NULL,
                extension TEXT NOT NULL, width INTEGER NOT NULL, height INTEGER NOT NULL, sha256 TEXT NOT NULL,
                animated INTEGER NOT NULL, emoji TEXT NOT NULL, shortcode TEXT, bytes BLOB NOT NULL,
                PRIMARY KEY (scope, id));
             CREATE TABLE IF NOT EXISTS instances (
                id TEXT PRIMARY KEY, template TEXT NOT NULL, owner TEXT NOT NULL, state TEXT NOT NULL,
                status TEXT NOT NULL, context TEXT NOT NULL, updated INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS instances_owner ON instances (owner);
             CREATE TABLE IF NOT EXISTS advances (
                instance TEXT NOT NULL, key TEXT NOT NULL, PRIMARY KEY (instance, key));",
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // --- packs

    pub fn pack(&self, name: &str) -> anyhow::Result<Option<Pack>> {
        Ok(self
            .db()
            .query_row("SELECT name, kind, title, owner, reuse, icon, version, created, updated, removed FROM packs WHERE name = ?1", [name], pack_row)
            .optional()?)
    }

    /// Every pack, or every pack of one maker's, oldest first.
    pub fn packs(&self, owner: Option<&str>) -> anyhow::Result<Vec<Pack>> {
        let db = self.db();
        let mut statement = db.prepare(
            "SELECT name, kind, title, owner, reuse, icon, version, created, updated, removed FROM packs
             WHERE ?1 IS NULL OR owner = ?1 ORDER BY created, name",
        )?;
        let rows = statement.query_map([owner], pack_row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Publishes a pack under `pack.name`, taking the stickers gathered under
    /// `from` as its own. `false` if the name was taken first.
    pub fn publish(&self, pack: &Pack, from: &str) -> anyhow::Result<bool> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let made = tx.execute(
            "INSERT OR IGNORE INTO packs (name, kind, title, owner, reuse, icon, version, created, updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7)",
            params![pack.name, pack.kind.as_str(), pack.title, pack.owner, pack.reuse, pack.icon, pack.created],
        )?;
        if made == 0 {
            return Ok(false);
        }
        tx.execute("UPDATE stickers SET scope = ?1 WHERE scope = ?2", params![pack_scope(&pack.name), from])?;
        tx.commit()?;
        Ok(true)
    }

    /// Changes a pack's own details, and counts the change.
    pub fn update_pack(&self, name: &str, title: &str, reuse: bool, icon: Option<&str>, now: i64) -> anyhow::Result<()> {
        self.db().execute("UPDATE packs SET title = ?2, reuse = ?3, icon = ?4, version = version + 1, updated = ?5 WHERE name = ?1", params![name, title, reuse, icon, now])?;
        Ok(())
    }

    /// Counts a change to a pack's stickers.
    pub fn touch_pack(&self, name: &str, now: i64) -> anyhow::Result<()> {
        self.db().execute("UPDATE packs SET version = version + 1, updated = ?2 WHERE name = ?1", params![name, now])?;
        Ok(())
    }

    /// Takes a pack down: nothing of it is served again, and its pictures are
    /// deleted. The name stays taken, so nobody else can pass for it.
    pub fn remove_pack(&self, name: &str, now: i64) -> anyhow::Result<bool> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let removed = tx.execute("UPDATE packs SET removed = 1, version = version + 1, updated = ?2 WHERE name = ?1 AND removed = 0", params![name, now])?;
        tx.execute("DELETE FROM stickers WHERE scope = ?1", [pack_scope(name)])?;
        tx.commit()?;
        Ok(removed > 0)
    }

    // --- stickers

    pub fn stickers(&self, scope: &str) -> anyhow::Result<Vec<Sticker>> {
        let db = self.db();
        let mut statement = db.prepare(
            "SELECT id, media_type, extension, width, height, sha256, animated, length(bytes), emoji, shortcode FROM stickers WHERE scope = ?1 ORDER BY position",
        )?;
        let rows = statement
            .query_map([scope], |row| {
                Ok(Sticker {
                    id: row.get(0)?,
                    media_type: row.get(1)?,
                    extension: row.get(2)?,
                    width: row.get(3)?,
                    height: row.get(4)?,
                    sha256: row.get(5)?,
                    animated: row.get(6)?,
                    bytes: row.get::<_, i64>(7)? as usize,
                    emoji: row.get(8)?,
                    shortcode: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Adds a picture to the end of `scope`. `false` if that picture is in it already.
    pub fn add_sticker(&self, scope: &str, image: &Image, bytes: &[u8], emoji: &str, shortcode: Option<&str>) -> anyhow::Result<bool> {
        let db = self.db();
        let added = db.execute(
            "INSERT OR IGNORE INTO stickers (scope, position, id, media_type, extension, width, height, sha256, animated, emoji, shortcode, bytes)
             VALUES (?1, (SELECT COALESCE(MAX(position), 0) + 1 FROM stickers WHERE scope = ?1), ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![scope, image.id(), image.media_type, image.extension, image.width, image.height, image.sha256, image.animated, emoji, shortcode, bytes],
        )?;
        Ok(added > 0)
    }

    pub fn remove_sticker(&self, scope: &str, id: &str) -> anyhow::Result<bool> {
        Ok(self.db().execute("DELETE FROM stickers WHERE scope = ?1 AND id = ?2", params![scope, id])? > 0)
    }

    /// A picture's bytes and type, by the file name it is served under.
    pub fn file(&self, scope: &str, file: &str) -> anyhow::Result<Option<(Vec<u8>, String)>> {
        let Some((id, extension)) = file.rsplit_once('.') else { return Ok(None) };
        Ok(self
            .db()
            .query_row("SELECT bytes, media_type FROM stickers WHERE scope = ?1 AND id = ?2 AND extension = ?3", params![scope, id, extension], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?)
    }

    // --- workflows

    pub fn instance(&self, id: &str) -> anyhow::Result<Option<Instance>> {
        Ok(self.db().query_row("SELECT id, template, owner, state, status, context, updated FROM instances WHERE id = ?1", [id], instance_row).optional()?)
    }

    /// The workflow of `template` that `owner` has under way, if there is one.
    pub fn active_instance(&self, owner: &str, template: &str) -> anyhow::Result<Option<Instance>> {
        Ok(self
            .db()
            .query_row(
                "SELECT id, template, owner, state, status, context, updated FROM instances WHERE owner = ?1 AND template = ?2 AND status IN ('active', 'paused') ORDER BY updated DESC LIMIT 1",
                params![owner, template],
                instance_row,
            )
            .optional()?)
    }

    pub fn save_instance(&self, instance: &Instance) -> anyhow::Result<()> {
        self.db().execute(
            "INSERT INTO instances (id, template, owner, state, status, context, updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (id) DO UPDATE SET state = ?4, status = ?5, context = ?6, updated = ?7",
            params![instance.id, instance.template, instance.owner, instance.state, instance.status, instance.context.to_string(), instance.updated],
        )?;
        Ok(())
    }

    /// Notes that `key` advanced `instance`. `false` if it already had: the same
    /// press of a button, arriving twice.
    pub fn first_advance(&self, instance: &str, key: &str) -> anyhow::Result<bool> {
        Ok(self.db().execute("INSERT OR IGNORE INTO advances (instance, key) VALUES (?1, ?2)", params![instance, key])? > 0)
    }

    /// Deletes what a workflow gathered and never published.
    pub fn drop_draft(&self, instance: &str) -> anyhow::Result<()> {
        self.db().execute("DELETE FROM stickers WHERE scope = ?1", [draft_scope(instance)])?;
        Ok(())
    }

    /// Forgets workflows nobody has touched since `before`, and what they gathered.
    pub fn prune(&self, before: i64) -> anyhow::Result<usize> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute("DELETE FROM stickers WHERE scope IN (SELECT 'draft:' || id FROM instances WHERE updated < ?1)", [before])?;
        tx.execute("DELETE FROM advances WHERE instance IN (SELECT id FROM instances WHERE updated < ?1)", [before])?;
        let gone = tx.execute("DELETE FROM instances WHERE updated < ?1", [before])?;
        tx.commit()?;
        Ok(gone)
    }

    /// A maker changed DID (a verified `from_prior`): what was `prior`'s is `new`'s.
    pub fn rotate(&self, prior: &str, new: &str) -> anyhow::Result<bool> {
        let db = self.db();
        let packs = db.execute("UPDATE packs SET owner = ?2 WHERE owner = ?1", params![prior, new])?;
        let instances = db.execute("UPDATE instances SET owner = ?2 WHERE owner = ?1", params![prior, new])?;
        Ok(packs + instances > 0)
    }
}

fn pack_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Pack> {
    Ok(Pack {
        name: row.get(0)?,
        kind: Kind::parse(&row.get::<_, String>(1)?),
        title: row.get(2)?,
        owner: row.get(3)?,
        reuse: row.get(4)?,
        icon: row.get(5)?,
        version: row.get(6)?,
        created: row.get(7)?,
        updated: row.get(8)?,
        removed: row.get(9)?,
    })
}

fn instance_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Instance> {
    Ok(Instance {
        id: row.get(0)?,
        template: row.get(1)?,
        owner: row.get(2)?,
        state: row.get(3)?,
        status: row.get(4)?,
        context: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or(Value::Null),
        updated: row.get(6)?,
    })
}
