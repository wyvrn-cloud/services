//! What the service keeps: a record per file (whose it is, what it is, how much
//! of it is there, when it goes), and the bytes, one file on disk per record.
//!
//! How much of an upload is there is read from the disk, not the record: a
//! chunk written just before a crash is on the disk and the record may not say
//! so, and the disk is what the next chunk is appended to.

use std::path::{Path, PathBuf};

use wyvrn_service::db::{self, Db, Location, Row, Schema, MYSQL_DID, MYSQL_TABLE};
use wyvrn_service::params;

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

const COLUMNS: &str = "id, owner, name, media_type, byte_count, hash, state, created, upload_expires, expires";

const SQLITE: &str = r#"CREATE TABLE IF NOT EXISTS files (
        id TEXT PRIMARY KEY, owner TEXT NOT NULL, name TEXT NOT NULL, media_type TEXT NOT NULL,
        byte_count INTEGER NOT NULL, hash TEXT NOT NULL, state TEXT NOT NULL, created INTEGER NOT NULL,
        upload_expires INTEGER NOT NULL, expires INTEGER NOT NULL);
     CREATE INDEX IF NOT EXISTS files_owner ON files (owner);"#;

fn schema() -> Schema {
    static MYSQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let mysql = MYSQL.get_or_init(|| {
        format!(
            r#"CREATE TABLE IF NOT EXISTS files (
                id VARCHAR(64) NOT NULL PRIMARY KEY, owner {MYSQL_DID} NOT NULL, name VARCHAR(255) NOT NULL, media_type VARCHAR(255) NOT NULL,
                byte_count BIGINT NOT NULL, hash VARCHAR(128) NOT NULL, state VARCHAR(16) NOT NULL, created BIGINT NOT NULL,
                upload_expires BIGINT NOT NULL, expires BIGINT NOT NULL,
                INDEX files_owner (owner)) {MYSQL_TABLE}"#
        )
    });
    Schema { sqlite: SQLITE, mysql }
}

pub struct Store {
    db: Db,
    dir: PathBuf,
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
        Ok(Self { db: Db::open(location, &schema(), "files")?, dir: dir.to_path_buf() })
    }

    /// For tests: records in memory, bytes in a directory of their own under the
    /// system's temporary directory.
    pub fn scratch() -> anyhow::Result<Self> {
        let dir = std::env::temp_dir().join(format!("wyvrn-files-test-{}-{}", std::process::id(), new_id()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self { db: Db::scratch(&schema())?, dir })
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
        };
        std::fs::File::create(self.path(&record.id))?;
        self.db.lock().execute(
            &format!("INSERT INTO files ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"),
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
                record.expires
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

    /// Marks `id` stored.
    pub fn mark_stored(&self, id: &str) -> anyhow::Result<()> {
        self.db.lock().execute("UPDATE files SET state = 'stored' WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn set_expires(&self, id: &str, expires: i64) -> anyhow::Result<()> {
        self.db.lock().execute("UPDATE files SET expires = ?2 WHERE id = ?1", params![id, expires])?;
        Ok(())
    }

    /// Removes the record and the bytes. Whether there was one.
    pub fn delete(&self, id: &str) -> anyhow::Result<bool> {
        let removed = self.db.lock().execute("DELETE FROM files WHERE id = ?1", params![id])?;
        match std::fs::remove_file(self.path(id)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(removed > 0)
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

    /// An uploader's change of DID: their files go with them.
    pub fn rotate(&self, prior: &str, new: &str) -> anyhow::Result<bool> {
        Ok(self.db.lock().execute("UPDATE files SET owner = ?2 WHERE owner = ?1", params![prior, new])? > 0)
    }
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
}
