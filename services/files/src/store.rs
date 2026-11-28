//! What the service keeps: a record per file (whose it is, what it is, how much
//! of it is there, when it goes), and the bytes, one file on disk per record --
//! or, once stored, in a bucket (bucket.rs), as the record's `place` says:
//!
//! - `disk`: in the directory, as every file always was.
//! - `spool`: stored, on disk, waiting to be moved to the store's bucket
//!   ([`Store::settle`] moves it, and then removes it from disk).
//! - `bucket:<tag>`: in the bucket that tag names ([`Bucket::tag`]), at its key
//!   for the file's id.
//!
//! An upload in progress is always on disk. A file in a bucket that is deleted
//! has its key kept in `doomed` until the bucket has been told
//! ([`Store::drain_doomed`]), so that nothing here waits on the network.
//!
//! How much of an upload is there is read from the disk, not the record: a
//! chunk written just before a crash is on the disk and the record may not say
//! so, and the disk is what the next chunk is appended to.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::bucket::Bucket;

use wyvrn_service::db::{self, Db, Location, Row, Schema, MYSQL_DID, MYSQL_TABLE};
use wyvrn_service::params;

use crate::policy::{Limits, Policy, Uploader};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Uploading,
    Stored,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uploading => "uploading",
            Self::Stored => "stored",
        }
    }

    fn parse(text: &str) -> Self {
        if text == "stored" { Self::Stored } else { Self::Uploading }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub id: String,
    /// The DID it was put by, which may change it.
    pub owner: String,
    pub name: String,
    pub media_type: String,
    pub byte_count: u64,
    pub hash: String,
    pub state: State,
    pub created: i64,
    /// An upload not completed by then is dropped.
    pub upload_expires: i64,
    /// A stored file goes then.
    pub expires: i64,
    /// Where its bytes are: `disk`, `spool` or `bucket:<tag>` (see the module).
    pub place: String,
}

impl FileRecord {
    /// Whether the record is still live at `now`: an upload before its deadline,
    /// or a stored file before its expiry.
    pub fn live_at(&self, now: i64) -> bool {
        match self.state {
            State::Uploading => now < self.upload_expires,
            State::Stored => now < self.expires,
        }
    }
}

/// A new record, before it has an id.
pub struct NewFile<'a> {
    pub owner: &'a str,
    pub name: &'a str,
    pub media_type: &'a str,
    pub byte_count: u64,
    pub hash: &'a str,
    pub created: i64,
    pub upload_expires: i64,
    pub expires: i64,
}

const COLUMNS: &str = "id, owner, name, media_type, byte_count, hash, state, created, upload_expires, expires, place";

const SQLITE: &str = r#"CREATE TABLE IF NOT EXISTS files (
        id TEXT PRIMARY KEY, owner TEXT NOT NULL, name TEXT NOT NULL, media_type TEXT NOT NULL,
        byte_count INTEGER NOT NULL, hash TEXT NOT NULL, state TEXT NOT NULL, created INTEGER NOT NULL,
        upload_expires INTEGER NOT NULL, expires INTEGER NOT NULL, place TEXT NOT NULL DEFAULT 'disk');
     CREATE INDEX IF NOT EXISTS files_owner ON files (owner);
     CREATE TABLE IF NOT EXISTS doomed (bucket_key TEXT PRIMARY KEY, place TEXT NOT NULL, at INTEGER NOT NULL);
     CREATE TABLE IF NOT EXISTS settings (name TEXT PRIMARY KEY, value TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS uploaders (
        did TEXT PRIMARY KEY, source TEXT NOT NULL, name TEXT, limits TEXT,
        blocked INTEGER NOT NULL DEFAULT 0, added INTEGER NOT NULL);"#;

fn schema() -> Schema {
    static MYSQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let mysql = MYSQL.get_or_init(|| {
        format!(
            r#"CREATE TABLE IF NOT EXISTS files (
                id VARCHAR(64) NOT NULL PRIMARY KEY, owner {MYSQL_DID} NOT NULL, name VARCHAR(255) NOT NULL, media_type VARCHAR(255) NOT NULL,
                byte_count BIGINT NOT NULL, hash VARCHAR(128) NOT NULL, state VARCHAR(16) NOT NULL, created BIGINT NOT NULL,
                upload_expires BIGINT NOT NULL, expires BIGINT NOT NULL, place VARCHAR(64) NOT NULL DEFAULT 'disk',
                INDEX files_owner (owner)) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS doomed (bucket_key VARCHAR(512) NOT NULL PRIMARY KEY, place VARCHAR(64) NOT NULL, at BIGINT NOT NULL) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS settings (name VARCHAR(64) NOT NULL PRIMARY KEY, value TEXT NOT NULL) {MYSQL_TABLE};
            CREATE TABLE IF NOT EXISTS uploaders (
                did {MYSQL_DID} NOT NULL PRIMARY KEY, source VARCHAR(16) NOT NULL, name VARCHAR(255), limits TEXT,
                blocked INT NOT NULL DEFAULT 0, added BIGINT NOT NULL) {MYSQL_TABLE}"#
        )
    });
    Schema { sqlite: SQLITE, mysql }
}

pub struct Store {
    db: Db,
    dir: PathBuf,
    /// Where stored files go from now on, if not on disk.
    bucket: Option<Arc<Bucket>>,
    /// Buckets files may still be in from before (another one moved away from).
    known: Vec<Arc<Bucket>>,
}

/// Where a file's bytes are, as its record says.
#[derive(Debug, Clone)]
pub enum Kept {
    Disk(PathBuf),
    Bucket(Arc<Bucket>, String),
    /// In a bucket this store isn't given: its tag.
    Lost(String),
}

/// What [`Store::move_to_bucket`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Moved {
    pub moved: usize,
    pub bytes: u64,
    /// Files whose bytes weren't what their record says, left where they were.
    pub mismatched: Vec<String>,
    /// Files that couldn't be read where they are, left there.
    pub unreadable: Vec<String>,
}

/// A file's id: 20 random bytes, base64url. Unguessable, and safe in a path.
fn new_id() -> String {
    use base64::Engine;
    let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let mut bytes = [0u8; 20];
    bytes[..16].copy_from_slice(a.as_bytes());
    bytes[16..].copy_from_slice(&b.as_bytes()[..4]);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Whether `id` is one this store could have made: what a path may be built from.
pub fn is_id(id: &str) -> bool {
    id.len() == 27 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Store {
    pub fn open(location: &Location, dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| anyhow::anyhow!("making {}: {e}", dir.display()))?;
        let store = Self { db: Db::open(location, &schema(), "files")?, dir: dir.to_path_buf(), bucket: None, known: Vec::new() };
        store.add_columns()?;
        Ok(store)
    }

    /// Keeps stored files in `bucket` from now on, and reads them from it and
    /// from `known` (buckets moved away from). What is on disk stays readable.
    pub fn with_buckets(mut self, bucket: Option<Bucket>, known: Vec<Bucket>) -> Self {
        self.bucket = bucket.map(Arc::new);
        self.known = known.into_iter().map(Arc::new).collect();
        self
    }

    /// The bucket stored files go to, if there is one.
    pub fn bucket(&self) -> Option<&Arc<Bucket>> {
        self.bucket.as_ref()
    }

    /// `place`, added to a store made before it (neither SQLite nor MySQL 8 has
    /// `ADD COLUMN IF NOT EXISTS`, so "duplicate column" is taken as its being there).
    fn add_columns(&self) -> anyhow::Result<()> {
        match self.db.lock().execute("ALTER TABLE files ADD COLUMN place VARCHAR(64) NOT NULL DEFAULT 'disk'", params![]) {
            Err(error) if error.to_string().to_ascii_lowercase().contains("duplicate column") => Ok(()),
            said => said.map(|_| ()).map_err(Into::into),
        }
    }

    /// For tests: records in memory, bytes in a directory of their own under the
    /// system's temporary directory.
    pub fn scratch() -> anyhow::Result<Self> {
        let dir = std::env::temp_dir().join(format!("wyvrn-files-test-{}-{}", std::process::id(), new_id()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self { db: Db::scratch(&schema())?, dir, bucket: None, known: Vec::new() })
    }

    /// Where `file`'s bytes are.
    pub fn kept(&self, file: &FileRecord) -> Kept {
        match file.place.strip_prefix("bucket:") {
            None => Kept::Disk(self.path(&file.id)),
            Some(tag) => match self.bucket_tagged(tag) {
                Some(bucket) => Kept::Bucket(bucket.clone(), bucket.key(&file.id)),
                None => Kept::Lost(tag.to_string()),
            },
        }
    }

    fn bucket_tagged(&self, tag: &str) -> Option<&Arc<Bucket>> {
        self.bucket.iter().chain(self.known.iter()).find(|b| b.tag() == tag)
    }

    /// The whole of `file`, wherever it is.
    pub async fn read(&self, file: &FileRecord) -> anyhow::Result<Vec<u8>> {
        match self.kept(file) {
            Kept::Disk(path) => Ok(tokio::fs::read(path).await?),
            Kept::Bucket(bucket, key) => bucket.get(&key).await,
            Kept::Lost(tag) => anyhow::bail!("{} is in a bucket ({tag}) this service isn't given", file.id),
        }
    }

    /// The same, for a caller that isn't async: a bucket is read on a thread of
    /// its own, with a runtime of its own.
    pub fn read_now(&self, file: &FileRecord) -> anyhow::Result<Vec<u8>> {
        match self.kept(file) {
            Kept::Disk(path) => Ok(std::fs::read(path)?),
            Kept::Bucket(bucket, key) => std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        let bucket = bucket.detached()?;
                        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(bucket.get(&key))
                    })
                    .join()
                    .map_err(|_| anyhow::anyhow!("reading {} from the bucket panicked", file.id))?
            }),
            Kept::Lost(tag) => anyhow::bail!("{} is in a bucket ({tag}) this service isn't given", file.id),
        }
    }

    /// Where a file's bytes are, or would be.
    pub fn path(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    /// How many bytes of `id` are on disk.
    pub fn received(&self, id: &str) -> u64 {
        std::fs::metadata(self.path(id)).map(|m| m.len()).unwrap_or(0)
    }

    pub fn file(&self, id: &str) -> anyhow::Result<Option<FileRecord>> {
        Ok(self.db.lock().query_row(&format!("SELECT {COLUMNS} FROM files WHERE id = ?1"), params![id], file_row)?)
    }

    /// The file `owner` has with this hash and size, if any: an `upload` asked
    /// twice is one file.
    pub fn file_like(&self, owner: &str, hash: &str, byte_count: u64) -> anyhow::Result<Option<FileRecord>> {
        Ok(self.db.lock().query_row(
            &format!("SELECT {COLUMNS} FROM files WHERE owner = ?1 AND hash = ?2 AND byte_count = ?3 ORDER BY created DESC"),
            params![owner, hash, byte_count as i64],
            file_row,
        )?)
    }

    /// Every file, by id.
    pub fn every_file(&self) -> anyhow::Result<Vec<FileRecord>> {
        Ok(self.db.lock().query(&format!("SELECT {COLUMNS} FROM files ORDER BY id"), params![], file_row)?)
    }

    /// Every file `owner` has, newest first.
    pub fn files_of(&self, owner: &str) -> anyhow::Result<Vec<FileRecord>> {
        Ok(self.db.lock().query(&format!("SELECT {COLUMNS} FROM files WHERE owner = ?1 ORDER BY created DESC, id"), params![owner], file_row)?)
    }

    /// What `owner` keeps here, in bytes, uploads in progress counted whole.
    pub fn used_by(&self, owner: &str) -> anyhow::Result<u64> {
        let used: Option<i64> = self.db.lock().query_value("SELECT SUM(byte_count) FROM files WHERE owner = ?1", params![owner])?;
        Ok(used.unwrap_or(0).max(0) as u64)
    }

    pub fn uploads_in_progress(&self, owner: &str) -> anyhow::Result<usize> {
        let count: i64 = self.db.lock().query_value("SELECT COUNT(*) FROM files WHERE owner = ?1 AND state = 'uploading'", params![owner])?;
        Ok(count.max(0) as usize)
    }

    /// Makes a record and an empty file for it.
    pub fn create(&self, file: NewFile<'_>) -> anyhow::Result<FileRecord> {
        let record = FileRecord {
            id: new_id(),
            owner: file.owner.to_string(),
            name: file.name.to_string(),
            media_type: file.media_type.to_string(),
            byte_count: file.byte_count,
            hash: file.hash.to_string(),
            state: State::Uploading,
            created: file.created,
            upload_expires: file.upload_expires,
            expires: file.expires,
            place: "disk".into(),
        };
        std::fs::File::create(self.path(&record.id))?;
        self.db.lock().execute(
            &format!("INSERT INTO files ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"),
            params![
                record.id,
                record.owner,
                record.name,
                record.media_type,
                record.byte_count as i64,
                record.hash,
                record.state.as_str(),
                record.created,
                record.upload_expires,
                record.expires,
                record.place
            ],
        )?;
        Ok(record)
    }

    /// Appends `bytes` to `id`'s file. The caller has checked they belong there.
    pub fn append(&self, id: &str, bytes: &[u8]) -> std::io::Result<u64> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(self.path(id))?;
        file.write_all(bytes)?;
        file.sync_data()?;
        Ok(file.metadata()?.len())
    }

    /// Marks `id` stored: on disk, to be moved to the bucket if there is one.
    pub fn mark_stored(&self, id: &str) -> anyhow::Result<()> {
        let place = if self.bucket.is_some() { "spool" } else { "disk" };
        self.db.lock().execute("UPDATE files SET state = 'stored', place = ?2 WHERE id = ?1", params![id, place])?;
        Ok(())
    }

    pub fn set_expires(&self, id: &str, expires: i64) -> anyhow::Result<()> {
        self.db.lock().execute("UPDATE files SET expires = ?2 WHERE id = ?1", params![id, expires])?;
        Ok(())
    }

    /// Removes the record and the bytes (or, in a bucket, has them removed:
    /// [`Self::drain_doomed`]). Whether there was one.
    pub fn delete(&self, id: &str) -> anyhow::Result<bool> {
        let place: Option<String> = self.db.lock().query_row("SELECT place FROM files WHERE id = ?1", params![id], |row| row.get(0))?;
        let removed = self.db.lock().transaction(|db| -> anyhow::Result<usize> {
            if let Some(tag) = place.as_deref().and_then(|p| p.strip_prefix("bucket:")) {
                let key = self.bucket_tagged(tag).map(|b| b.key(id)).unwrap_or_else(|| id.to_string());
                db.execute("DELETE FROM doomed WHERE bucket_key = ?1", params![key])?;
                db.execute("INSERT INTO doomed (bucket_key, place, at) VALUES (?1, ?2, ?3)", params![key, place.clone(), wyvrn_service::now()])?;
            }
            Ok(db.execute("DELETE FROM files WHERE id = ?1", params![id])?)
        })?;
        match std::fs::remove_file(self.path(id)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(removed > 0)
    }

    /// Tells the buckets to remove what was deleted here. How many went. A key
    /// in a bucket this store isn't given stays, to be told once it is.
    pub async fn drain_doomed(&self) -> anyhow::Result<usize> {
        let doomed = self.db.lock().query("SELECT bucket_key, place FROM doomed ORDER BY at LIMIT 500", params![], |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)))?;
        let mut gone = 0;
        for (key, place) in doomed {
            let Some(bucket) = place.strip_prefix("bucket:").and_then(|tag| self.bucket_tagged(tag)) else { continue };
            bucket.delete(&key).await?;
            self.db.lock().execute("DELETE FROM doomed WHERE bucket_key = ?1", params![key])?;
            gone += 1;
        }
        Ok(gone)
    }

    /// Moves stored files waiting on disk (`spool`) to the bucket, a few at a
    /// time: put there, its size checked, the record changed, and then removed
    /// from disk. How many moved. A file deleted meanwhile is removed from the
    /// bucket again.
    pub async fn settle(&self) -> anyhow::Result<usize> {
        let Some(bucket) = self.bucket.clone() else { return Ok(0) };
        let waiting = self.db.lock().query(&format!("SELECT {COLUMNS} FROM files WHERE place = 'spool' AND state = 'stored' ORDER BY created LIMIT 20"), params![], file_row)?;
        let mut moved = 0;
        for file in waiting {
            let key = bucket.key(&file.id);
            let path = self.path(&file.id);
            bucket.put_file(&key, &path, file.byte_count).await?;
            let there = bucket.size(&key).await?;
            if there != Some(file.byte_count) {
                anyhow::bail!("{} went to the bucket as {there:?} bytes, not {}", file.id, file.byte_count);
            }
            let place = format!("bucket:{}", bucket.tag());
            let changed = self.db.lock().execute("UPDATE files SET place = ?2 WHERE id = ?1 AND place = 'spool'", params![file.id, place])?;
            if changed == 0 {
                bucket.delete(&key).await?;
                continue;
            }
            let _ = std::fs::remove_file(&path);
            moved += 1;
        }
        Ok(moved)
    }

    /// Moves every stored file that isn't in the bucket there (from disk, or
    /// from a bucket moved away from), checking its hash on the way: before, as
    /// it is read, and after, read back from the bucket. Only then is the record
    /// changed and the old copy removed. A file that doesn't match its record is
    /// left where it is and named.
    pub async fn move_to_bucket(&self, mut progress: impl FnMut(&FileRecord)) -> anyhow::Result<Moved> {
        let Some(bucket) = self.bucket.clone() else { anyhow::bail!("there is no bucket to move files to") };
        let here = format!("bucket:{}", bucket.tag());
        let mut moved = Moved::default();
        let mut done: std::collections::HashSet<String> = Default::default();
        loop {
            let batch = self.db.lock().query(
                &format!("SELECT {COLUMNS} FROM files WHERE state = 'stored' AND place <> ?1 ORDER BY id LIMIT 100"),
                params![here.clone()],
                file_row,
            )?;
            let batch: Vec<_> = batch.into_iter().filter(|f| !done.contains(&f.id)).collect();
            if batch.is_empty() {
                return Ok(moved);
            }
            for file in batch {
                done.insert(file.id.clone());
                progress(&file);
                let key = bucket.key(&file.id);
                let was = self.kept(&file);
                // The bytes on disk, or brought down to disk from the other bucket.
                let (path, fetched) = match &was {
                    Kept::Disk(path) => (path.clone(), false),
                    Kept::Bucket(old, old_key) => {
                        let path = self.dir.join(format!(".moving-{}", file.id));
                        match old.get(old_key).await {
                            Ok(bytes) => std::fs::write(&path, bytes)?,
                            Err(_) => {
                                moved.unreadable.push(file.id.clone());
                                continue;
                            }
                        }
                        (path, true)
                    }
                    Kept::Lost(_) => {
                        moved.unreadable.push(file.id.clone());
                        continue;
                    }
                };
                let tidy = |fetched: bool| {
                    if fetched {
                        let _ = std::fs::remove_file(&path);
                    }
                };
                match crate::hash::of_file(&path) {
                    Ok(hash) if hash == file.hash => {}
                    Ok(_) => {
                        moved.mismatched.push(file.id.clone());
                        tidy(fetched);
                        continue;
                    }
                    Err(_) => {
                        moved.unreadable.push(file.id.clone());
                        tidy(fetched);
                        continue;
                    }
                }
                let size = std::fs::metadata(&path)?.len();
                let put = bucket.put_file(&key, &path, size).await;
                tidy(fetched);
                put?;
                let (digest, there) = bucket.hash(&key).await?;
                if there != size || crate::hash::multihash(&digest.as_slice().try_into()?) != file.hash {
                    bucket.delete(&key).await?;
                    moved.mismatched.push(file.id.clone());
                    continue;
                }
                let changed = self.db.lock().execute("UPDATE files SET place = ?2 WHERE id = ?1 AND place = ?3", params![file.id, here.clone(), file.place.clone()])?;
                if changed == 0 {
                    // Deleted, or moved by the mover, meanwhile.
                    if self.file(&file.id)?.is_none_or(|f| f.place != here) {
                        bucket.delete(&key).await?;
                    }
                    continue;
                }
                match was {
                    Kept::Disk(path) => {
                        let _ = std::fs::remove_file(path);
                    }
                    Kept::Bucket(old, old_key) => {
                        let _ = old.delete(&old_key).await;
                    }
                    Kept::Lost(_) => {}
                }
                moved.moved += 1;
                moved.bytes += size;
            }
        }
    }

    /// How many stored files, and bytes, are in each place.
    pub fn places(&self) -> anyhow::Result<Vec<(String, u64, u64)>> {
        Ok(self.db.lock().query(
            "SELECT place, COUNT(*), SUM(byte_count) FROM files WHERE state = 'stored' GROUP BY place ORDER BY place",
            params![],
            |row| Ok((row.get::<String>(0)?, row.get::<i64>(1)?.max(0) as u64, row.get::<Option<i64>>(2)?.unwrap_or(0).max(0) as u64)),
        )?)
    }

    /// Removes every file that is past its time at `now`. How many went.
    pub fn prune(&self, now: i64) -> anyhow::Result<usize> {
        let gone = self.db.lock().query(
            "SELECT id FROM files WHERE (state = 'uploading' AND upload_expires <= ?1) OR (state = 'stored' AND expires <= ?1)",
            params![now],
            |row| row.get::<String>(0),
        )?;
        for id in &gone {
            self.delete(id)?;
        }
        Ok(gone.len())
    }

    /// An uploader's change of DID: their files go with them, and so does what
    /// the service knows of them, unless it already knows the new DID.
    pub fn rotate(&self, prior: &str, new: &str) -> anyhow::Result<bool> {
        let mut db = self.db.lock();
        let moved = db.execute("UPDATE files SET owner = ?2 WHERE owner = ?1", params![prior, new])? > 0;
        let known: i64 = db.query_value("SELECT COUNT(*) FROM uploaders WHERE did = ?1", params![new])?;
        let carried = known == 0 && db.execute("UPDATE uploaders SET did = ?2 WHERE did = ?1", params![prior, new])? > 0;
        Ok(moved || carried)
    }

    /// Everything held, by everyone, in bytes.
    pub fn used_total(&self) -> anyhow::Result<u64> {
        let used: Option<i64> = self.db.lock().query_value("SELECT SUM(byte_count) FROM files", params![])?;
        Ok(used.unwrap_or(0).max(0) as u64)
    }

    /// Every file, the largest first: what an admin looks through.
    pub fn largest(&self, limit: usize) -> anyhow::Result<Vec<FileRecord>> {
        Ok(self.db.lock().query(&format!("SELECT {COLUMNS} FROM files ORDER BY byte_count DESC, id LIMIT {limit}"), params![], file_row)?)
    }

    pub fn file_count(&self) -> anyhow::Result<u64> {
        let count: i64 = self.db.lock().query_value("SELECT COUNT(*) FROM files", params![])?;
        Ok(count.max(0) as u64)
    }

    // --- The rules, and who is an uploader.

    /// The rules an admin has set, if one has.
    pub fn policy(&self) -> anyhow::Result<Option<Policy>> {
        let text: Option<String> = self.db.lock().query_row("SELECT value FROM settings WHERE name = 'policy'", params![], |row| row.get(0))?;
        text.map(|text| serde_json::from_str(&text).map_err(Into::into)).transpose()
    }

    pub fn set_policy(&self, policy: &Policy) -> anyhow::Result<()> {
        let text = serde_json::to_string(policy)?;
        self.db.lock().transaction(|db| -> anyhow::Result<()> {
            db.execute("DELETE FROM settings WHERE name = 'policy'", params![])?;
            db.execute("INSERT INTO settings (name, value) VALUES ('policy', ?1)", params![text])?;
            Ok(())
        })
    }

    pub fn uploader(&self, did: &str) -> anyhow::Result<Option<Uploader>> {
        Ok(self.db.lock().query_row(&format!("SELECT {UPLOADER_COLUMNS} FROM uploaders WHERE did = ?1"), params![did], uploader_row)?)
    }

    /// Everyone the service knows as an uploader, the most recent first.
    pub fn uploaders(&self) -> anyhow::Result<Vec<Uploader>> {
        Ok(self.db.lock().query(&format!("SELECT {UPLOADER_COLUMNS} FROM uploaders ORDER BY added DESC, did"), params![], uploader_row)?)
    }

    /// Writes what is known of an uploader, in place of what was.
    pub fn put_uploader(&self, uploader: &Uploader) -> anyhow::Result<()> {
        let limits = uploader.limits.map(|limits| serde_json::to_string(&limits)).transpose()?;
        self.db.lock().transaction(|db| -> anyhow::Result<()> {
            db.execute("DELETE FROM uploaders WHERE did = ?1", params![uploader.did])?;
            db.execute(
                &format!("INSERT INTO uploaders ({UPLOADER_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"),
                params![uploader.did, uploader.source, uploader.name, limits, i64::from(uploader.blocked), uploader.added],
            )?;
            Ok(())
        })
    }

    /// Forgets an uploader. Their files stay until they expire or are removed.
    pub fn remove_uploader(&self, did: &str) -> anyhow::Result<bool> {
        Ok(self.db.lock().execute("DELETE FROM uploaders WHERE did = ?1", params![did])? > 0)
    }
}

const UPLOADER_COLUMNS: &str = "did, source, name, limits, blocked, added";

fn uploader_row(row: &Row) -> db::Result<Uploader> {
    let limits: Option<String> = row.get(3)?;
    Ok(Uploader {
        did: row.get(0)?,
        source: row.get(1)?,
        name: row.get(2)?,
        // Limits that can't be read are as if none were set: the tier's apply.
        limits: limits.and_then(|text| serde_json::from_str::<Limits>(&text).ok()),
        blocked: row.get::<i64>(4)? != 0,
        added: row.get(5)?,
    })
}

fn file_row(row: &Row) -> db::Result<FileRecord> {
    Ok(FileRecord {
        id: row.get(0)?,
        owner: row.get(1)?,
        name: row.get(2)?,
        media_type: row.get(3)?,
        byte_count: row.get::<i64>(4)?.max(0) as u64,
        hash: row.get(5)?,
        state: State::parse(&row.get::<String>(6)?),
        created: row.get(7)?,
        upload_expires: row.get(8)?,
        expires: row.get(9)?,
        place: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new<'a>(owner: &'a str, hash: &'a str, bytes: u64) -> NewFile<'a> {
        NewFile { owner, name: "a.bin", media_type: "application/octet-stream", byte_count: bytes, hash, created: 100, upload_expires: 200, expires: 1000 }
    }

    #[test]
    fn a_file_is_made_appended_to_stored_and_let_go() {
        let store = Store::scratch().unwrap();
        let record = store.create(new("did:example:alice", "Qm1", 5)).unwrap();
        assert!(is_id(&record.id));
        assert_eq!(store.received(&record.id), 0);
        assert_eq!(store.append(&record.id, b"hel").unwrap(), 3);
        assert_eq!(store.append(&record.id, b"lo").unwrap(), 5);
        assert_eq!(std::fs::read(store.path(&record.id)).unwrap(), b"hello");
        assert_eq!(store.file_like("did:example:alice", "Qm1", 5).unwrap().unwrap().id, record.id);
        assert_eq!(store.file_like("did:example:bob", "Qm1", 5).unwrap(), None);
        assert_eq!(store.used_by("did:example:alice").unwrap(), 5);
        assert_eq!(store.uploads_in_progress("did:example:alice").unwrap(), 1);
        store.mark_stored(&record.id).unwrap();
        assert_eq!(store.file(&record.id).unwrap().unwrap().state, State::Stored);
        assert_eq!(store.uploads_in_progress("did:example:alice").unwrap(), 0);
        store.set_expires(&record.id, 5000).unwrap();
        assert_eq!(store.file(&record.id).unwrap().unwrap().expires, 5000);
        assert!(store.delete(&record.id).unwrap());
        assert!(!store.delete(&record.id).unwrap());
        assert!(!store.path(&record.id).exists());
        assert_eq!(store.used_by("did:example:alice").unwrap(), 0);
    }

    #[test]
    fn pruning_takes_what_is_past_its_time_and_rotation_moves_the_rest() {
        let store = Store::scratch().unwrap();
        let stale_upload = store.create(new("did:example:alice", "Qm1", 1)).unwrap();
        let expired = store.create(new("did:example:alice", "Qm2", 1)).unwrap();
        store.mark_stored(&expired.id).unwrap();
        let kept = store.create(new("did:example:alice", "Qm3", 1)).unwrap();
        store.mark_stored(&kept.id).unwrap();
        store.set_expires(&kept.id, 2000).unwrap();
        assert_eq!(store.prune(150).unwrap(), 0);
        assert_eq!(store.prune(1000).unwrap(), 2);
        assert_eq!(store.files_of("did:example:alice").unwrap().iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), [kept.id.as_str()]);
        assert!(!store.path(&stale_upload.id).exists() && !store.path(&expired.id).exists());
        assert!(store.rotate("did:example:alice", "did:example:alice2").unwrap());
        assert_eq!(store.files_of("did:example:alice2").unwrap().len(), 1);
        assert!(!store.rotate("did:example:nobody", "did:example:x").unwrap());
    }

    #[test]
    fn a_store_from_before_places_is_given_them_its_files_on_disk() {
        let dir = std::env::temp_dir().join(format!("wyvrn-files-places-{}", new_id()));
        let location = Location::Sqlite(dir.join("files.sqlite"));
        let id = {
            let store = Store::open(&location, &dir).unwrap();
            let file = store.create(new("did:example:alice", &crate::hash::of_bytes(b"hi"), 2)).unwrap();
            store.append(&file.id, b"hi").unwrap();
            store.mark_stored(&file.id).unwrap();
            store.db.lock().execute("ALTER TABLE files DROP COLUMN place", params![]).unwrap();
            file.id
        };
        let store = Store::open(&location, &dir).unwrap();
        let file = store.file(&id).unwrap().unwrap();
        assert_eq!(file.place, "disk");
        assert!(matches!(store.kept(&file), Kept::Disk(path) if path == dir.join(&id)));
        assert_eq!(store.read_now(&file).unwrap(), b"hi");
        // Opened again, nothing more to add.
        drop(store);
        Store::open(&location, &dir).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_rules_and_the_uploaders_are_kept() {
        let store = Store::scratch().unwrap();
        assert_eq!(store.policy().unwrap(), None);
        let mut policy = Policy::from_config(&crate::config::ServiceConfig::default());
        policy.locked = true;
        store.set_policy(&policy).unwrap();
        store.set_policy(&policy).unwrap();
        assert_eq!(store.policy().unwrap(), Some(policy.clone()));

        let alice = Uploader { did: "did:example:alice".into(), source: "signup".into(), name: None, limits: None, blocked: false, added: 5 };
        store.put_uploader(&alice).unwrap();
        let named = Uploader { name: Some("Alice".into()), limits: Some(Limits { quota_bytes: 7, ..policy.everyone }), ..alice.clone() };
        store.put_uploader(&named).unwrap();
        assert_eq!(store.uploader("did:example:alice").unwrap(), Some(named.clone()));
        assert_eq!(store.uploaders().unwrap().len(), 1);
        // A change of DID carries them over.
        store.rotate("did:example:alice", "did:example:alice2").unwrap();
        assert_eq!(store.uploader("did:example:alice2").unwrap().unwrap().name.as_deref(), Some("Alice"));
        assert!(store.remove_uploader("did:example:alice2").unwrap());
        assert_eq!(store.uploader("did:example:alice2").unwrap(), None);
    }
}
