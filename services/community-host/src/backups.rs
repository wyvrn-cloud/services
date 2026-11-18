//! Each hosted community backed up on a schedule, encrypted (asked 2026-11-17).
//!
//! A backup is what an owner moving out is given (export.rs's snapshot: its keys,
//! its store as the single `wyvrn-community` keeps it in SQLite, its packs and its
//! files), packed as a `.tar.gz` and sealed with the host's backup key, kept in
//! `<dir>/<name>/` beside the last few. Off until a key is set: a backup anyone
//! who reaches the disk could read would be the communities' keys for the taking.
//!
//! **The sealed file** (`.wyvrn-backup`): `WYVRNBK1`, then 8 random bytes, then
//! the archive in pieces of up to 1 MiB, each `u32` (big-endian) length and
//! AES-256-GCM ciphertext, its nonce the 8 bytes and the piece's number
//! (`u32`, big-endian), and as associated data one byte: 1 for the last piece, 0
//! otherwise, so that a file cut short doesn't open as a smaller one.
//! `wyvrn-community-host open-backup <file> <out.tar.gz>` opens one.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};

use crate::tenants::Host;

const MAGIC: &[u8; 8] = b"WYVRNBK1";
const PIECE: usize = 1024 * 1024;
/// What a backup's file is called after.
const SUFFIX: &str = ".wyvrn-backup";

/// The host's backup key, from `backups.key` (a file holding 64 hex digits) or
/// `COMMUNITY_HOST_BACKUP_KEY`; none, backups are off.
pub fn key(config: &crate::config::BackupsConfig) -> anyhow::Result<Option<[u8; 32]>> {
    let said = match std::env::var("COMMUNITY_HOST_BACKUP_KEY").ok().filter(|k| !k.trim().is_empty()) {
        Some(key) => key,
        None if !config.key.as_os_str().is_empty() => std::fs::read_to_string(&config.key)?,
        None => return Ok(None),
    };
    let hex: String = said.chars().filter(|c| !c.is_whitespace()).collect();
    anyhow::ensure!(hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()), "the backup key is 64 hex digits (32 bytes)");
    let mut key = [0u8; 32];
    for (n, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[n * 2..n * 2 + 2], 16)?;
    }
    Ok(Some(key))
}

fn nonce(prefix: &[u8; 8], piece: u32) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..8].copy_from_slice(prefix);
    nonce[8..].copy_from_slice(&piece.to_be_bytes());
    nonce
}

/// Seals `from` into `to`, a piece at a time.
pub fn seal(key: &[u8; 32], from: &Path, to: &Path) -> anyhow::Result<()> {
    let cipher = Aes256Gcm::new_from_slice(key)?;
    let mut input = std::fs::File::open(from)?;
    let mut output = std::io::BufWriter::new(std::fs::File::create(to)?);
    // (A v4 UUID's bytes are the system's randomness.)
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..8]);
    output.write_all(MAGIC)?;
    output.write_all(&prefix)?;
    let total = input.metadata()?.len();
    let mut done: u64 = 0;
    let mut buffer = vec![0u8; PIECE];
    let mut piece: u32 = 0;
    loop {
        let mut filled = 0;
        while filled < PIECE {
            let n = input.read(&mut buffer[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        done += filled as u64;
        let last = done >= total;
        let sealed = cipher.encrypt(Nonce::from_slice(&nonce(&prefix, piece)), Payload { msg: &buffer[..filled], aad: &[u8::from(last)] }).map_err(|_| anyhow::anyhow!("couldn't seal a piece"))?;
        output.write_all(&(sealed.len() as u32).to_be_bytes())?;
        output.write_all(&sealed)?;
        if last {
            break;
        }
        piece = piece.checked_add(1).ok_or_else(|| anyhow::anyhow!("too large to seal"))?;
    }
    output.flush()?;
    Ok(())
}

/// Opens a sealed backup into `to` (the `.tar.gz`), refusing one that isn't whole.
pub fn open(key: &[u8; 32], from: &Path, to: &Path) -> anyhow::Result<()> {
    let cipher = Aes256Gcm::new_from_slice(key)?;
    let mut input = std::io::BufReader::new(std::fs::File::open(from)?);
    let mut magic = [0u8; 8];
    input.read_exact(&mut magic)?;
    anyhow::ensure!(&magic == MAGIC, "that isn't a community's backup");
    let mut prefix = [0u8; 8];
    input.read_exact(&mut prefix)?;
    let mut output = std::io::BufWriter::new(std::fs::File::create(to)?);
    let mut piece: u32 = 0;
    loop {
        let mut length = [0u8; 4];
        input.read_exact(&mut length).map_err(|_| anyhow::anyhow!("the backup was cut short"))?;
        let mut sealed = vec![0u8; u32::from_be_bytes(length) as usize];
        input.read_exact(&mut sealed).map_err(|_| anyhow::anyhow!("the backup was cut short"))?;
        // Its last piece, or one more to come: whichever opens it is what it is.
        let nonce = nonce(&prefix, piece);
        let (plain, last) = match cipher.decrypt(Nonce::from_slice(&nonce), Payload { msg: &sealed, aad: &[0] }) {
            Ok(plain) => (plain, false),
            Err(_) => (cipher.decrypt(Nonce::from_slice(&nonce), Payload { msg: &sealed, aad: &[1] }).map_err(|_| anyhow::anyhow!("that backup doesn't open with this key, or was changed"))?, true),
        };
        output.write_all(&plain)?;
        if last {
            break;
        }
        piece += 1;
    }
    output.flush()?;
    Ok(())
}

impl Host {
    /// Where `name`'s backups are kept.
    pub fn backups_of(&self, name: &str) -> PathBuf {
        self.config.backups.dir_or(&self.config.storage.dir).join(name)
    }

    /// `name`'s backups, newest first: file name, and when made (seconds).
    pub fn backups(&self, name: &str) -> Vec<(String, i64)> {
        let mut found: Vec<(String, i64)> = std::fs::read_dir(self.backups_of(name))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let file = entry.file_name().to_str()?.to_string();
                let at = file.strip_suffix(SUFFIX)?.rsplit('-').next()?.parse().ok()?;
                Some((file, at))
            })
            .collect();
        found.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
        found
    }

    /// Backs `name` up now: its snapshot packed and sealed, the oldest past
    /// `backups.keep` removed. Blocks. What it made.
    pub fn back_up(&self, name: &str, now: i64) -> anyhow::Result<PathBuf> {
        let Some(key) = key(&self.config.backups)? else { anyhow::bail!("backups are off: set backups.key, or COMMUNITY_HOST_BACKUP_KEY") };
        let folder = self.backups_of(name);
        std::fs::create_dir_all(&folder)?;
        let staging = folder.join(format!(".staging-{now}"));
        let packed = folder.join(format!(".{now}.tar.gz"));
        let made = (|| -> anyhow::Result<PathBuf> {
            self.snapshot(name, &staging.join("data"))?;
            let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(std::fs::File::create(&packed)?, flate2::Compression::default()));
            archive.append_dir_all(name, &staging)?;
            archive.into_inner()?.finish()?.flush()?;
            let sealed = folder.join(format!("{name}-{now}{SUFFIX}"));
            seal(&key, &packed, &sealed)?;
            Ok(sealed)
        })();
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_file(&packed);
        let made = made?;
        for (old, _) in self.backups(name).into_iter().skip(self.config.backups.keep.max(1)) {
            let _ = std::fs::remove_file(folder.join(old));
        }
        tracing::info!(community = %name, file = %made.display(), "backed a community up");
        Ok(made)
    }

    /// Backs up every served community whose last backup is older than
    /// `backups.every_hours`, one at a time. Nothing, with backups off.
    pub fn back_up_due(&self, now: i64) {
        // One round at a time: a large community can take longer than the next look.
        static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                RUNNING.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let _done = Done;
        match key(&self.config.backups) {
            Ok(Some(_)) => {}
            Ok(None) => return,
            Err(error) => return tracing::error!(%error, "the backup key can't be read: no backups"),
        }
        let every = self.config.backups.every_hours.max(1) * 3600;
        let records = match self.registry.all() {
            Ok(records) => records,
            Err(error) => return tracing::warn!(%error, "couldn't read which communities to back up"),
        };
        for record in records.into_iter().filter(|r| r.state.served()) {
            let last = self.backups(&record.name).first().map(|(_, at)| *at).unwrap_or(0);
            if now - last < every {
                continue;
            }
            if let Err(error) = self.back_up(&record.name, now) {
                tracing::error!(community = %record.name, error = %format!("{error:#}"), "couldn't back a community up");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backup_opens_with_its_key_alone_and_not_once_cut_short_or_changed() {
        let dir = std::env::temp_dir().join(format!("wyvrn-backup-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("plain");
        // More than two pieces, the last a short one.
        let bytes: Vec<u8> = (0..(PIECE * 2 + 1234)).map(|n| (n % 251) as u8).collect();
        std::fs::write(&plain, &bytes).unwrap();
        let key = [7u8; 32];
        let sealed = dir.join("sealed");
        seal(&key, &plain, &sealed).unwrap();
        let back = dir.join("back");
        open(&key, &sealed, &back).unwrap();
        assert_eq!(std::fs::read(&back).unwrap(), bytes);
        assert!(open(&[8u8; 32], &sealed, &back).is_err());
        // Cut after its first piece: refused, not a smaller archive.
        let whole = std::fs::read(&sealed).unwrap();
        let first = 16 + 4 + u32::from_be_bytes(whole[16..20].try_into().unwrap()) as usize;
        std::fs::write(&sealed, &whole[..first]).unwrap();
        assert!(open(&key, &sealed, &back).is_err());
        // An empty file seals and opens too.
        std::fs::write(&plain, b"").unwrap();
        seal(&key, &plain, &sealed).unwrap();
        open(&key, &sealed, &back).unwrap();
        assert!(std::fs::read(&back).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
