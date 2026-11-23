//! Each hosted community backed up on a schedule, encrypted (asked 2026-11-17).
//!
//! A backup is what an owner moving out is given (export.rs's snapshot: its keys,
//! its store as the single `wyvrn-community` keeps it in SQLite, its packs and its
//! files), packed as a `.tar.gz` and sealed with the host's backup key, kept in
//! `<dir>/<name>/` beside the last few. Off until a key is set: a backup anyone
//! who reaches the disk could read would be the communities' keys for the taking.
//!
//! **The sealed file** (`.wyvrn-backup`), version 2 (security review WYV-47):
//! `WYVRNBK2`, 8 random bytes, the community's name (`u16` big-endian length,
//! then UTF-8), then the archive in pieces of up to 1 MiB, each `u32` (big-endian)
//! length and AES-256-GCM ciphertext. A piece's nonce is the 8 bytes and its
//! number (`u32`, big-endian); its associated data is the header (everything
//! before the first piece), its number, and one byte, 1 for the last piece and 0
//! otherwise: a file cut short doesn't open as a smaller one, and a piece can't be
//! moved to another place, another file, or another community's name. The archive
//! is sealed as it is made, so no unsealed copy of it is ever written.
//! Version 1 (`WYVRNBK1`: no name, the last-piece byte alone as associated data)
//! still opens. `wyvrn-community-host open-backup <file> <out.tar.gz>` opens one.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};

use crate::tenants::Host;

const MAGIC: &[u8; 8] = b"WYVRNBK1";
const MAGIC_V2: &[u8; 8] = b"WYVRNBK2";
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

/// What a v2 piece is bound to: the header, its number, and whether it is the last.
fn bound(header: &[u8], piece: u32, last: bool) -> Vec<u8> {
    let mut aad = header.to_vec();
    aad.extend_from_slice(&piece.to_be_bytes());
    aad.push(u8::from(last));
    aad
}

/// Writes what it is given sealed, piece by piece, into `out` (version 2): the
/// archive goes straight in as it is made. `finish` seals the last piece.
pub struct Sealing<W: Write> {
    cipher: Aes256Gcm,
    out: W,
    prefix: [u8; 8],
    header: Vec<u8>,
    buffer: Vec<u8>,
    piece: u32,
}

impl<W: Write> Sealing<W> {
    pub fn new(key: &[u8; 32], name: &str, mut out: W) -> anyhow::Result<Self> {
        // (A v4 UUID's bytes are the system's randomness.)
        let mut prefix = [0u8; 8];
        prefix.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..8]);
        let name_bytes = name.as_bytes();
        anyhow::ensure!(name_bytes.len() <= u16::MAX as usize, "that name is too long");
        let mut header = MAGIC_V2.to_vec();
        header.extend_from_slice(&prefix);
        header.extend_from_slice(&(name_bytes.len() as u16).to_be_bytes());
        header.extend_from_slice(name_bytes);
        out.write_all(&header)?;
        Ok(Self { cipher: Aes256Gcm::new_from_slice(key)?, out, prefix, header, buffer: Vec::with_capacity(PIECE + 1), piece: 0 })
    }

    fn seal_piece(&mut self, plain: &[u8], last: bool) -> std::io::Result<()> {
        let sealed = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce(&self.prefix, self.piece)), Payload { msg: plain, aad: &bound(&self.header, self.piece, last) })
            .map_err(|_| std::io::Error::other("couldn't seal a piece"))?;
        self.out.write_all(&(sealed.len() as u32).to_be_bytes())?;
        self.out.write_all(&sealed)?;
        self.piece = self.piece.checked_add(1).ok_or_else(|| std::io::Error::other("too large to seal"))?;
        Ok(())
    }

    /// Seals what is left as the last piece; what it wrote to.
    pub fn finish(mut self) -> anyhow::Result<W> {
        let rest = std::mem::take(&mut self.buffer);
        self.seal_piece(&rest, true)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

impl<W: Write> Write for Sealing<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(data);
        // A full piece is sealed only once more follows it: the last one is `finish`'s.
        while self.buffer.len() > PIECE {
            let piece: Vec<u8> = self.buffer.drain(..PIECE).collect();
            self.seal_piece(&piece, false)?;
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

/// Seals the file `from` into `to` as `name`'s (version 2).
pub fn seal(key: &[u8; 32], name: &str, from: &Path, to: &Path) -> anyhow::Result<()> {
    let mut sealing = Sealing::new(key, name, std::io::BufWriter::new(std::fs::File::create(to)?))?;
    std::io::copy(&mut std::fs::File::open(from)?, &mut sealing)?;
    sealing.finish()?;
    Ok(())
}

/// Opens a sealed backup into `to` (the `.tar.gz`), refusing one that isn't
/// whole. The community it is of, where it says (version 2).
pub fn open(key: &[u8; 32], from: &Path, to: &Path) -> anyhow::Result<Option<String>> {
    let cipher = Aes256Gcm::new_from_slice(key)?;
    let mut input = std::io::BufReader::new(std::fs::File::open(from)?);
    let mut magic = [0u8; 8];
    input.read_exact(&mut magic)?;
    let v2 = &magic == MAGIC_V2;
    anyhow::ensure!(v2 || &magic == MAGIC, "that isn't a community's backup");
    let mut prefix = [0u8; 8];
    input.read_exact(&mut prefix)?;
    let mut header = magic.to_vec();
    header.extend_from_slice(&prefix);
    let name = if v2 {
        let mut length = [0u8; 2];
        input.read_exact(&mut length)?;
        let mut name = vec![0u8; u16::from_be_bytes(length) as usize];
        input.read_exact(&mut name)?;
        header.extend_from_slice(&length);
        header.extend_from_slice(&name);
        Some(String::from_utf8(name)?)
    } else {
        None
    };
    let mut output = std::io::BufWriter::new(std::fs::File::create(to)?);
    let mut piece: u32 = 0;
    loop {
        let mut length = [0u8; 4];
        input.read_exact(&mut length).map_err(|_| anyhow::anyhow!("the backup was cut short"))?;
        let mut sealed = vec![0u8; u32::from_be_bytes(length) as usize];
        input.read_exact(&mut sealed).map_err(|_| anyhow::anyhow!("the backup was cut short"))?;
        // Its last piece, or one more to come: whichever opens it is what it is.
        let nonce = nonce(&prefix, piece);
        let aad = |last: bool| if v2 { bound(&header, piece, last) } else { vec![u8::from(last)] };
        let (plain, last) = match cipher.decrypt(Nonce::from_slice(&nonce), Payload { msg: &sealed, aad: &aad(false) }) {
            Ok(plain) => (plain, false),
            Err(_) => (cipher.decrypt(Nonce::from_slice(&nonce), Payload { msg: &sealed, aad: &aad(true) }).map_err(|_| anyhow::anyhow!("that backup doesn't open with this key, or was changed"))?, true),
        };
        output.write_all(&plain)?;
        if last {
            break;
        }
        piece += 1;
    }
    output.flush()?;
    Ok(name)
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
        let made = (|| -> anyhow::Result<PathBuf> {
            // The snapshot is SQLite files, so it is written out, for as long as it
            // takes, where only this service's user can read it (WYV-47).
            std::fs::create_dir_all(&staging)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700))?;
            }
            self.snapshot(name, &staging.join("data"))?;
            // Packed and sealed in one go: no unsealed archive is ever written.
            let sealed = folder.join(format!("{name}-{now}{SUFFIX}"));
            let partial = folder.join(format!(".{name}-{now}{SUFFIX}.part"));
            let sealing = Sealing::new(&key, name, std::io::BufWriter::new(std::fs::File::create(&partial)?))?;
            let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(sealing, flate2::Compression::default()));
            archive.append_dir_all(name, &staging)?;
            archive.into_inner()?.finish()?.finish()?;
            std::fs::rename(&partial, &sealed)?;
            Ok(sealed)
        })();
        let _ = std::fs::remove_dir_all(&staging);
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
        seal(&key, "makers", &plain, &sealed).unwrap();
        let back = dir.join("back");
        open(&key, &sealed, &back).unwrap();
        assert_eq!(std::fs::read(&back).unwrap(), bytes);
        assert!(open(&[8u8; 32], &sealed, &back).is_err());
        // Cut after its first piece: refused, not a smaller archive.
        let whole = std::fs::read(&sealed).unwrap();
        let header = 16 + 2 + "makers".len();
        let first = header + 4 + u32::from_be_bytes(whole[header..header + 4].try_into().unwrap()) as usize;
        std::fs::write(&sealed, &whole[..first]).unwrap();
        assert!(open(&key, &sealed, &back).is_err());
        // The name in its header is bound to every piece: another one opens nothing.
        let mut renamed = whole.clone();
        renamed[18..24].copy_from_slice(b"breakr");
        std::fs::write(&sealed, &renamed).unwrap();
        assert!(open(&key, &sealed, &back).is_err());
        std::fs::write(&sealed, &whole).unwrap();
        assert_eq!(open(&key, &sealed, &back).unwrap().as_deref(), Some("makers"));
        // A version 1 backup (the last-piece byte alone bound) still opens.
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let prefix = [3u8; 8];
        let mut v1 = MAGIC.to_vec();
        v1.extend_from_slice(&prefix);
        let piece = cipher.encrypt(Nonce::from_slice(&nonce(&prefix, 0)), Payload { msg: b"old archive", aad: &[1] }).unwrap();
        v1.extend_from_slice(&(piece.len() as u32).to_be_bytes());
        v1.extend_from_slice(&piece);
        std::fs::write(&sealed, &v1).unwrap();
        assert_eq!(open(&key, &sealed, &back).unwrap(), None);
        assert_eq!(std::fs::read(&back).unwrap(), b"old archive");
        // An empty file seals and opens too.
        std::fs::write(&plain, b"").unwrap();
        seal(&key, "makers", &plain, &sealed).unwrap();
        open(&key, &sealed, &back).unwrap();
        assert!(std::fs::read(&back).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
