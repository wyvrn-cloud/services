//! A community run on its own (`wyvrn-community`) brought onto the host: the
//! reverse of [`crate::export`]. Its keys, everything it keeps, its packs and its
//! members' files are copied in, and it is made one of the host's, at
//! `<host>c/<name>/`. Its public address (`community:<name>` at the DID host) comes
//! with it: once the host opens it, the address's document names it here, with
//! the same keys, so members who hold it by its address reach it here unasked.
//! Those who hold it by its own `did:web` don't. Its own server is stopped
//! first, and not started again.

use std::path::Path;

use wyvrn_service::db::Location;

use crate::registry::{State, Storage};
use crate::tenants::Host;

/// Where a community run on its own keeps what it keeps, as its environment says:
/// `COMMUNITY_DATABASE_URL` (or its `[store]` file), and the stickers and files
/// stores, which are SQLite files beside it unless their own database URLs say
/// otherwise.
pub struct Source {
    pub community: Location,
    pub stickers: Option<Location>,
    /// The folder of members' files, with their records in `files.sqlite` there
    /// unless `files_records` says where else.
    pub files: Option<std::path::PathBuf>,
    pub files_records: Option<Location>,
}

impl Host {
    /// Brings the community at `from` in as `name`, made for `owner`, kept as
    /// `storage`. Its keys are kept here as it kept them; nothing at `from` is
    /// changed. Made suspended and served only once everything is in: a copy that
    /// fails leaves nothing behind. Blocks.
    pub fn import(&self, name: &str, title: &str, owner: &str, storage: Storage, from: &Source, now: i64) -> anyhow::Result<String> {
        let record = self.registry.create(name, title, storage, owner, "an import", now)?;
        // Not opened (by a host that is running) before it is all here.
        self.registry.set_state(name, State::Suspended, None)?;
        match self.copy_in(&record, from) {
            Ok(said) => {
                self.registry.set_state(name, State::Active, None)?;
                Ok(said)
            }
            Err(error) => {
                let _ = self.registry.forget(name);
                Err(error)
            }
        }
    }

    fn copy_in(&self, record: &crate::registry::Record, from: &Source) -> anyhow::Result<String> {
        let name = &record.name;
        let target = self.stores(record, record.storage)?;
        let folder = self.folder(name);
        std::fs::create_dir_all(&folder)?;
        let keys = folder.join("identity.json");
        // Its keys: in its database, or (a SQLite store) in the file beside it.
        let beside = match &from.community {
            Location::Sqlite(path) => path.with_file_name("identity.json"),
            _ => folder.join(".no-identity-file"),
        };
        let identity = wyvrn_service::identity::load(&from.community, &beside, "community")?.ok_or_else(|| anyhow::anyhow!("no keys found for the community at {}", from.community))?;
        wyvrn_service::identity::keep(&target.community, &keys, "community", &identity)?;

        wyvrn_community::store::Store::open(&target.community)?;
        wyvrn_service::profile::Kept::open(&target.community, "community")?;
        wyvrn_service::hosted_address::Kept::open(&target.community, "community")?;
        // Only its own tables: its database may hold others'.
        let mut copied = wyvrn_service::db::copy_store_held(&from.community, &target.community, "community", &["wyvrn_identity"])?;
        // Served here, not moving (whatever it was in the middle of where it was).
        wyvrn_community::store::Store::open(&target.community)?.set_setting(wyvrn_community::community::MOVING_SETTING, "0")?;

        if let (Some(source), Some(to)) = (&from.stickers, &target.stickers) {
            wyvrn_stickers::store::Store::open(to)?;
            copied.extend(wyvrn_service::db::copy_store_held(source, to, "stickers", &[])?);
        }
        if let (Some(dir), Some(to)) = (&from.files, &target.files) {
            let records = from.files_records.clone().unwrap_or_else(|| Location::Sqlite(dir.join("files.sqlite")));
            wyvrn_files::store::Store::open(to, &folder.join("files"))?;
            if has_store(&records) {
                copied.extend(wyvrn_service::db::copy_store_held(&records, to, "files", &[])?);
            }
            copy_files(dir, &folder.join("files"))?;
        }
        let rows: usize = copied.iter().map(|(_, rows)| rows).sum();
        tracing::info!(community = %name, tables = copied.len(), rows, "brought a community in");
        Ok(format!(
            "{name} is in, at {}c/{name}/: {} tables, {rows} rows copied. Stop its own server for good; the host opens it and takes up its address.",
            self.config.server.public_url,
            copied.len()
        ))
    }
}

/// Whether there is a store at `location` to copy from: a SQLite file that was
/// never made isn't one.
fn has_store(location: &Location) -> bool {
    match location {
        Location::Sqlite(path) => path.exists(),
        _ => true,
    }
}

/// Members' files, but not the records' own database (copied apart).
fn copy_files(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    if !from.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let file = entry.file_name().to_string_lossy().to_string();
        if entry.file_type()?.is_dir() {
            copy_files(&entry.path(), &target)?;
        } else if file != "files.sqlite" && !file.starts_with("files.sqlite-") {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
