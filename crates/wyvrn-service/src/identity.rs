//! A service's own keys, kept where the rest of what it keeps is: in a file beside
//! a SQLite store, or in the database for one that has a database ([`crate::db`]).
//!
//! With a database nothing is left on the machine itself, so a service can be
//! moved or its container remade with nothing carried over but its settings.
//!
//! **Sealed at rest**, where the process is given a key ([`seal_with`]; the
//! community host's `COMMUNITY_HOST_SEAL_KEY`): kept as AES-256-GCM of the keys,
//! under a key derived from it (HKDF-SHA256) and bound to where they are kept, so
//! a stolen database, file or backup alone doesn't make anyone the service. Keys
//! found unsealed are sealed the first time they are read. Without one, they are
//! kept as the file holds them: whoever can read the database can be the service.
//!
//! **Rotated** by giving the new key and, beside it, the old ones
//! ([`opens_with_previous`]): keys sealed with an old one still open, and are
//! sealed again with the new one as they are read. With old keys and no new one,
//! they are unsealed as they are read: sealing turned off.

use std::path::Path;

use anyhow::Context;
use didcomm_agent::Identity;

use crate::db::{Db, Location, Schema, MYSQL_TABLE};
use crate::params;

/// The key keys are sealed with at rest, once the process is given one.
static SEAL: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();

/// Keys sealed with before, which still open what they sealed (to be sealed again).
static PREVIOUS: std::sync::OnceLock<Vec<[u8; 32]>> = std::sync::OnceLock::new();

fn key_from(text: &str) -> anyhow::Result<[u8; 32]> {
    use base64::Engine;
    let text = text.trim();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text.trim_end_matches('=')))
        .context("the seal key is base64 of 32 bytes")?;
    bytes.try_into().map_err(|_| anyhow::anyhow!("the seal key is base64 of 32 bytes"))
}

/// Seals every identity this process keeps, from now on, with `key` (32 bytes, or
/// base64 of them). Once: a second key is refused.
pub fn seal_with(key: &str) -> anyhow::Result<()> {
    let key = key_from(key)?;
    anyhow::ensure!(SEAL.get().is_none_or(|held| *held == key), "a different seal key is set already");
    let _ = SEAL.set(key);
    Ok(())
}

/// Keys sealed with before (base64, separated by commas, spaces or lines): what
/// they sealed still opens, and is sealed again with the key given to
/// [`seal_with`] as it is read, or unsealed where none is. Once.
pub fn opens_with_previous(keys: &str) -> anyhow::Result<()> {
    let keys: Vec<[u8; 32]> = keys.split(|c: char| c == ',' || c.is_whitespace()).filter(|k| !k.is_empty()).map(key_from).collect::<anyhow::Result<_>>()?;
    anyhow::ensure!(PREVIOUS.get().is_none_or(|held| *held == keys), "other previous seal keys are set already");
    let _ = PREVIOUS.set(keys);
    Ok(())
}

const SEALED: &str = "wyvrn-sealed-identity/1";

/// The seal key now, once the environment has been read for one.
fn current() -> Option<&'static [u8; 32]> {
    from_env();
    SEAL.get()
}

/// Keys sealed with before, once the environment has been read for them.
fn previous() -> &'static [[u8; 32]] {
    from_env();
    PREVIOUS.get().map(Vec::as_slice).unwrap_or_default()
}

/// Every service seals its own keys where it is given `WYVRN_SEAL_KEY` (or
/// `WYVRN_SEAL_KEY_FILE`), and opens what an old one sealed with
/// `WYVRN_SEAL_KEY_PREVIOUS` (or `_FILE`), as the community host does with its own
/// names (security review WYV-32). Read once; a key given by the program first
/// ([`seal_with`]) stands. A key that isn't one stops the service: better than
/// keeping keys unsealed the operator meant sealed.
fn from_env() {
    static READ: std::sync::Once = std::sync::Once::new();
    READ.call_once(|| {
        let given = |name: &str| -> Option<String> {
            let direct = std::env::var(name).ok().filter(|v| !v.trim().is_empty());
            direct.or_else(|| std::env::var(format!("{name}_FILE")).ok().filter(|p| !p.trim().is_empty()).map(|path| std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}_FILE {path}: {e}"))))
        };
        if let Some(key) = given("WYVRN_SEAL_KEY") {
            if SEAL.get().is_none() {
                let key = key_from(&key).unwrap_or_else(|e| panic!("WYVRN_SEAL_KEY: {e}"));
                let _ = SEAL.set(key);
            }
        }
        if let Some(keys) = given("WYVRN_SEAL_KEY_PREVIOUS") {
            if PREVIOUS.get().is_none() {
                let keys = keys.split(|c: char| c == ',' || c.is_whitespace()).filter(|k| !k.is_empty()).map(key_from).collect::<anyhow::Result<Vec<_>>>().unwrap_or_else(|e| panic!("WYVRN_SEAL_KEY_PREVIOUS: {e}"));
                let _ = PREVIOUS.set(keys);
            }
        }
    });
}

/// `identity` as it is kept: sealed, where there is a key, else as it is.
fn kept_form(identity: &Identity, at: &str) -> anyhow::Result<String> {
    kept_form_with(identity, at, current())
}

fn kept_form_with(identity: &Identity, at: &str, key: Option<&[u8; 32]>) -> anyhow::Result<String> {
    seal_with_key(&identity.to_json()?, at, key)
}

fn seal_with_key(plain: &str, at: &str, key: Option<&[u8; 32]>) -> anyhow::Result<String> {
    let Some(key) = key else { return Ok(plain.to_string()) };
    use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
    use base64::Engine;
    let cipher = aes_gcm::Aes256Gcm::new_from_slice(&derived(key, at)).expect("a 32-byte key");
    let nonce = aes_gcm::Aes256Gcm::generate_nonce(OsRng);
    let sealed = cipher.encrypt(&nonce, Payload { msg: plain.as_bytes(), aad: at.as_bytes() }).map_err(|_| anyhow::anyhow!("the keys couldn't be sealed"))?;
    let b64 = base64::engine::general_purpose::STANDARD;
    Ok(serde_json::json!({ "sealed": SEALED, "nonce": b64.encode(nonce), "keys": b64.encode(sealed) }).to_string())
}

/// Any secret a service keeps (a key in its settings), as it is kept: sealed with
/// the seal key where it has one, bound to `at` (where it is kept), else as it is
/// (security review WYV-32).
pub fn seal_text(plain: &str, at: &str) -> anyhow::Result<String> {
    seal_with_key(plain, at, current())
}

/// A secret kept by [`seal_text`] read back (sealed or not), and whether it should
/// be kept again (sealed now, sealed again with a new key, or unsealed).
pub fn open_text(kept: &str, at: &str) -> anyhow::Result<(String, bool)> {
    open_with_keys(kept, at, current(), previous())
}

fn open_with_keys(kept: &str, at: &str, current: Option<&[u8; 32]>, previous: &[[u8; 32]]) -> anyhow::Result<(String, bool)> {
    let sealed = serde_json::from_str::<serde_json::Value>(kept).ok().filter(|v| v["sealed"] == SEALED);
    let Some(value) = sealed else { return Ok((kept.to_string(), current.is_some())) };
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let nonce = b64.decode(value["nonce"].as_str().unwrap_or_default()).context("sealed keys' nonce")?;
    let sealed = b64.decode(value["keys"].as_str().unwrap_or_default()).context("sealed keys")?;
    anyhow::ensure!(nonce.len() == 12, "sealed keys' nonce");
    let open = |key: &[u8; 32]| {
        let cipher = aes_gcm::Aes256Gcm::new_from_slice(&derived(key, at)).expect("a 32-byte key");
        cipher.decrypt(aes_gcm::Nonce::from_slice(&nonce), Payload { msg: &sealed, aad: at.as_bytes() }).ok()
    };
    anyhow::ensure!(current.is_some() || !previous.is_empty(), "these keys are sealed: the key they were sealed with is needed (WYVRN_SEAL_KEY, or COMMUNITY_HOST_SEAL_KEY)");
    let (plain, again) = match current.and_then(open) {
        Some(plain) => (plain, false),
        None => (
            previous.iter().find_map(open).ok_or_else(|| anyhow::anyhow!("these keys don't open with this seal key or a previous one (or were moved from where they were sealed)"))?,
            true,
        ),
    };
    Ok((String::from_utf8(plain)?, again))
}

/// Keys as kept (sealed or not) read back; and whether they were unsealed, to be
/// sealed now.
fn read_kept(text: &str, at: &str) -> anyhow::Result<(Identity, bool)> {
    read_kept_with(text, at, current(), previous())
}

fn read_kept_with(text: &str, at: &str, current: Option<&[u8; 32]>, previous: &[[u8; 32]]) -> anyhow::Result<(Identity, bool)> {
    serde_json::from_str::<serde_json::Value>(text).context("the keys aren't JSON")?;
    let (plain, again) = open_with_keys(text, at, current, previous)?;
    Ok((Identity::from_json(&plain)?, again))
}

/// What keys read and kept again became: sealed with the key now, or unsealed.
fn rewritten() -> &'static str {
    if current().is_some() { "sealed" } else { "unsealed" }
}

fn derived(key: &[u8; 32], at: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    hkdf::Hkdf::<sha2::Sha256>::new(Some(b"wyvrn identity at rest"), key).expand(at.as_bytes(), &mut out).expect("32 bytes is a length HKDF gives");
    out
}

/// Where keys are kept, as they are bound to it: the file's name for a file (its
/// folder is the community's own), the service for a database.
fn place_of_file(path: &Path) -> String {
    format!("file:{}", path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or_default())
}

fn file_load(path: &Path) -> anyhow::Result<Identity> {
    let text = std::fs::read_to_string(path)?;
    let at = place_of_file(path);
    let (identity, reseal) = read_kept(&text, &at)?;
    if reseal {
        write_file(path, &kept_form(&identity, &at)?)?;
        tracing::info!(path = %path.display(), "keys {} at rest", rewritten());
    }
    Ok(identity)
}

fn write_file(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, path)?;
    Ok(())
}

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
        Location::Sqlite(_) => {
            if path.exists() {
                return file_load(path).with_context(|| format!("identity file {}", path.display()));
            }
            let identity = Identity::generate()?;
            write_file(path, &kept_form(&identity, &place_of_file(path))?).with_context(|| format!("identity file {}", path.display()))?;
            Ok(identity)
        }
        Location::MySql(_) | Location::Shared { .. } => {
            let db = Db::open(location, &schema(), service).with_context(|| format!("the identity in {location}"))?;
            in_database(&db, path, service)
        }
    }
}

/// The identity kept at `location` (the file at `path` for a SQLite store), if
/// one is: nothing is made.
pub fn load(location: &Location, path: &Path, service: &str) -> anyhow::Result<Option<Identity>> {
    match location {
        Location::Sqlite(_) => Ok(path.exists().then(|| file_load(path)).transpose().with_context(|| format!("identity file {}", path.display()))?),
        Location::MySql(_) | Location::Shared { .. } => {
            let db = Db::open(location, &schema(), service).with_context(|| format!("the identity in {location}"))?;
            let held = db.lock().query_row("SELECT identity FROM wyvrn_identity WHERE id = 1", params![], |row| row.get::<String>(0))?;
            let Some(held) = held else { return Ok(None) };
            let at = format!("db:{service}");
            let (identity, reseal) = read_kept(&held, &at)?;
            if reseal {
                db.lock().execute("UPDATE wyvrn_identity SET identity = ?1 WHERE id = 1", params![kept_form(&identity, &at)?])?;
            }
            Ok(Some(identity))
        }
    }
}

/// Keeps `identity` at `location`, as [`load_or_generate`] would find it there:
/// for a service moved from one store to another with its keys. Refused where
/// other keys are kept already.
pub fn keep(location: &Location, path: &Path, service: &str, identity: &Identity) -> anyhow::Result<()> {
    if let Some(held) = load(location, path, service)? {
        anyhow::ensure!(held.to_json()? == identity.to_json()?, "other keys are kept at {location} already");
        return Ok(());
    }
    match location {
        Location::Sqlite(_) => write_file(path, &kept_form(identity, &place_of_file(path))?).with_context(|| format!("identity file {}", path.display()))?,
        Location::MySql(_) | Location::Shared { .. } => {
            let db = Db::open(location, &schema(), service)?;
            db.lock().execute("INSERT INTO wyvrn_identity (id, identity) VALUES (1, ?1)", params![kept_form(identity, &format!("db:{service}"))?])?;
        }
    }
    Ok(())
}

fn in_database(db: &Db, path: &Path, service: &str) -> anyhow::Result<Identity> {
    const HELD: &str = "SELECT identity FROM wyvrn_identity WHERE id = 1";
    let at = format!("db:{service}");
    let mut session = db.lock();
    if let Some(held) = session.query_row(HELD, params![], |row| row.get::<String>(0))? {
        let (identity, reseal) = read_kept(&held, &at)?;
        if reseal {
            session.execute("UPDATE wyvrn_identity SET identity = ?1 WHERE id = 1", params![kept_form(&identity, &at)?])?;
            tracing::info!("this service's keys are {} at rest", rewritten());
        }
        return Ok(identity);
    }
    let (identity, from) = if path.exists() {
        (file_load(path).with_context(|| format!("identity file {}", path.display()))?, "copied from its file")
    } else {
        (Identity::generate()?, "made")
    };
    // Another start of the same service may have got there first: what is held
    // is what both use.
    session.execute("INSERT OR IGNORE INTO wyvrn_identity (id, identity) VALUES (1, ?1)", params![kept_form(&identity, &at)?])?;
    let held: String = session.query_value(HELD, params![])?;
    tracing::info!("this service's keys are kept in its database ({from})");
    Ok(read_kept(&held, &at)?.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(identity: &Identity) -> String {
        identity.to_json().unwrap()
    }

    #[test]
    fn any_secret_is_sealed_as_keys_are_and_bound_to_where_it_is_kept() {
        let key = [7u8; 32];
        let sealed = seal_with_key("the oauth key", "verify:oauth_key", Some(&key)).unwrap();
        assert!(!sealed.contains("oauth key"));
        assert_eq!(open_with_keys(&sealed, "verify:oauth_key", Some(&key), &[]).unwrap(), ("the oauth key".to_string(), false));
        assert!(open_with_keys(&sealed, "verify:other", Some(&key), &[]).is_err(), "moved, it doesn't open");
        assert_eq!(open_with_keys("plain", "verify:oauth_key", Some(&key), &[]).unwrap(), ("plain".to_string(), true), "found unsealed: to be sealed");
    }

    #[test]
    fn keys_sealed_with_an_old_key_open_and_are_sealed_again_with_the_new_one_or_unsealed() {
        let identity = Identity::generate().unwrap();
        let (old, new, other) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let at = "file:den";
        let under_old = kept_form_with(&identity, at, Some(&old)).unwrap();
        // The new key alone doesn't open them; with the old one beside it they open, to be sealed again.
        assert!(read_kept_with(&under_old, at, Some(&new), &[]).is_err());
        let (read, again) = read_kept_with(&under_old, at, Some(&new), &[other, old]).unwrap();
        assert_eq!(keys(&read), keys(&identity));
        assert!(again);
        let under_new = kept_form_with(&read, at, Some(&new)).unwrap();
        assert!(!read_kept_with(&under_new, at, Some(&new), &[old]).unwrap().1, "with the key now: kept as they are");
        // Old keys and no new one: they open, to be kept unsealed.
        let (read, again) = read_kept_with(&under_new, at, None, &[new]).unwrap();
        assert!(again);
        assert!(!kept_form_with(&read, at, None).unwrap().contains(SEALED));
        assert!(read_kept_with(&under_new, at, None, &[]).is_err());
    }

    #[test]
    fn keys_in_a_database_are_made_once_and_a_file_that_was_there_is_taken_in() {
        let nowhere = Path::new("/nonexistent/identity.json");
        let db = Db::scratch(&schema()).unwrap();
        let first = in_database(&db, nowhere, "test").unwrap();
        assert_eq!(keys(&in_database(&db, nowhere, "test").unwrap()), keys(&first));

        let file = std::env::temp_dir().join(format!("wyvrn-identity-test-{}-{:?}.json", std::process::id(), std::thread::current().id()));
        let before = Identity::generate().unwrap();
        before.save(&file).unwrap();
        let moved = Db::scratch(&schema()).unwrap();
        assert_eq!(keys(&in_database(&moved, &file, "test").unwrap()), keys(&before));
        // Held from then on, whatever becomes of the file.
        std::fs::remove_file(&file).unwrap();
        assert_eq!(keys(&in_database(&moved, &file, "test").unwrap()), keys(&before));
        assert_ne!(keys(&before), keys(&first));
    }
}
