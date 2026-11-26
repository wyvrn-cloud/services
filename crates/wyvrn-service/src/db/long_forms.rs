//! A `did:peer:4` kept by its short form.
//!
//! A long-form `did:peer:4` (`did:peer:4zQm<hash>:z<document>`) carries its whole
//! document, and an account with many devices and keys has a long one: past what a
//! MySQL key can hold ([`super::MYSQL_DID`]), and longer with each device. Its
//! short form (`did:peer:4zQm<hash>`, everything up to the last `:`) is the hash of
//! that document: one DID's alone, and always the same length.
//!
//! So a store keeps a long-form `did:peer:4` by its short form, wherever one is
//! given as a statement's parameter -- whatever the column, as a key or not -- and
//! keeps the long form beside it, in the store's `wyvrn_long_forms` table. What is
//! read back is the long form again: a value that is the short form of one the
//! store has kept is given back long. A store's callers go on saying, comparing and
//! sending to the DIDs they have, never seeing the short form; a statement finds
//! the same row by either form.
//!
//! Only a long form whose hash is its document's is shortened: the short form
//! names one document, and anything else claiming it is not that DID (and is left
//! as it is, to be refused or not found as any other text would be).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

const PREFIX: &str = "did:peer:4";
/// `did:peer:4zQm`: base58btc (`z`) of a SHA-256 multihash (`0x12 0x20`, `Qm`).
const HASHED: &str = "did:peer:4zQm";
/// How long a short form is: the prefix, and 44 more characters of the hash.
pub(crate) const SHORT_LENGTH: usize = HASHED.len() + 44;

const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

fn is_base58(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| BASE58.contains(&b))
}

/// Where the DID in `text` ends: at a DID URL's path, query or fragment, if any.
fn did_end(text: &str) -> usize {
    text.find(['#', '?', '/']).unwrap_or(text.len())
}

/// The short form of `did` if it is a long-form `did:peer:4` whose hash is its
/// document's (or a DID URL of one: `…#key-1` is kept as `<short>#key-1`), with the
/// long DID itself; `None` for anything else.
pub(crate) fn shortened(text: &str) -> Option<(String, &str)> {
    if !text.starts_with(HASHED) {
        return None;
    }
    let end = did_end(text);
    let did = &text[..end];
    let (hash, document) = did[PREFIX.len()..].split_once(':')?;
    if hash.len() != SHORT_LENGTH - PREFIX.len() || !is_base58(hash) || document.len() < 7 || !document.starts_with('z') || !is_base58(&document[1..]) {
        return None;
    }
    let mut multihash = vec![0x12, 0x20];
    multihash.extend_from_slice(&Sha256::digest(document.as_bytes()));
    if format!("z{}", bs58::encode(multihash).into_string()) != hash {
        return None;
    }
    Some((format!("{PREFIX}{hash}{}", &text[end..]), did))
}

/// The DID a store keeps for `did`: its short form if it is a long-form
/// `did:peer:4`, otherwise itself. What a store does with every parameter of every
/// statement; for a caller that keys something of its own by DID the same way.
pub fn did_key(did: &str) -> Cow<'_, str> {
    match shortened(did) {
        Some((short, _)) => Cow::Owned(short),
        None => Cow::Borrowed(did),
    }
}

/// The short form a value read back is, with what follows it (a DID URL's
/// fragment): `None` for anything that isn't a short-form `did:peer:4`.
pub(crate) fn short_form(value: &[u8]) -> Option<(&str, &str)> {
    if !value.starts_with(HASHED.as_bytes()) || value.len() < SHORT_LENGTH {
        return None;
    }
    let text = std::str::from_utf8(value).ok()?;
    let end = did_end(text);
    (end == SHORT_LENGTH && is_base58(&text[HASHED.len()..end])).then(|| (&text[..end], &text[end..]))
}

/// At most this many long forms are held in memory; past it, all are let go and
/// read from the stores again as they are needed.
const MOST_HELD: usize = 20_000;

/// The long forms this process has seen, by short form: one document's, whichever
/// store it was seen in, so held once for all of them.
pub(crate) fn held() -> &'static Mutex<HashMap<String, String>> {
    static HELD: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    HELD.get_or_init(Default::default)
}

/// Whether `did` is a short-form `did:peer:4` (`did:peer:4zQm<hash>`, nothing
/// after): a DID nobody can resolve without having seen its long form.
pub fn is_short_form(did: &str) -> bool {
    short_form(did.as_bytes()).is_some_and(|(_, rest)| rest.is_empty())
}

/// The long form of short-form `did:peer:4` `short` that some store of this
/// process has read or written, if any has.
pub fn long_form_seen(short: &str) -> Option<String> {
    held_long_form(short)
}

pub(crate) fn held_long_form(short: &str) -> Option<String> {
    held().lock().unwrap_or_else(|p| p.into_inner()).get(short).cloned()
}

pub(crate) fn hold(short: &str, long: &str) {
    let mut held = held().lock().unwrap_or_else(|p| p.into_inner());
    if held.len() >= MOST_HELD {
        held.clear();
    }
    held.entry(short.to_string()).or_insert_with(|| long.to_string());
}

/// How long a short form a store had no long form for is not asked about again.
const UNKNOWN_FOR: Duration = Duration::from_secs(60);

/// One store's part: which long forms it has been given already (so each is
/// written to its table once), and which short forms it was asked for and hasn't.
#[derive(Default)]
pub(crate) struct Kept {
    written: Mutex<HashSet<String>>,
    unknown: Mutex<Option<(Instant, HashSet<String>)>>,
    /// What [`super::Db::long_forms_left`] says.
    pub(crate) left: Mutex<Vec<(String, String)>>,
}

impl Kept {
    pub(crate) fn is_written(&self, short: &str) -> bool {
        self.written.lock().unwrap_or_else(|p| p.into_inner()).contains(short)
    }

    pub(crate) fn written(&self, short: &str) {
        let mut written = self.written.lock().unwrap_or_else(|p| p.into_inner());
        if written.len() >= MOST_HELD {
            written.clear();
        }
        written.insert(short.to_string());
    }

    /// A transaction was undone: what it wrote to the table may have gone with it.
    pub(crate) fn forget_written(&self) {
        self.written.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    pub(crate) fn is_unknown(&self, short: &str) -> bool {
        let mut unknown = self.unknown.lock().unwrap_or_else(|p| p.into_inner());
        if unknown.as_ref().is_some_and(|(since, _)| since.elapsed() > UNKNOWN_FOR) {
            *unknown = None;
        }
        unknown.as_ref().is_some_and(|(_, shorts)| shorts.contains(short))
    }

    pub(crate) fn unknown(&self, short: &str) {
        let mut unknown = self.unknown.lock().unwrap_or_else(|p| p.into_inner());
        let (_, shorts) = unknown.get_or_insert_with(|| (Instant::now(), HashSet::new()));
        if shorts.len() < MOST_HELD {
            shorts.insert(short.to_string());
        }
    }

    /// The table has been written to by a long form, perhaps one asked for before.
    pub(crate) fn forget_unknown(&self, short: &str) {
        if let Some((_, shorts)) = self.unknown.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
            shorts.remove(short);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_form_is_kept_short_only_when_its_hash_is_its_documents() {
        let long = didcomm_resolver_peer::peer4::encode(serde_json::json!({ "verificationMethod": [], "service": [{ "id": "#s", "type": "X", "serviceEndpoint": "https://example.com" }] })).unwrap();
        let short = didcomm_resolver_peer::peer4::long_to_short(&long).unwrap();
        assert_eq!(did_key(&long), short);
        assert_eq!(did_key(&format!("{long}#key-1")), format!("{short}#key-1"));
        assert_eq!(short_form(short.as_bytes()), Some((short.as_str(), "")));
        assert_eq!(short_form(format!("{short}#key-2").as_bytes()), Some((short.as_str(), "#key-2")));
        // Already short, another method, or someone else's document under its hash.
        assert_eq!(did_key(&short), short);
        assert_eq!(did_key("did:web:example.com"), "did:web:example.com");
        let other = didcomm_resolver_peer::peer4::encode(serde_json::json!({ "verificationMethod": [] })).unwrap();
        let forged = format!("{short}:{}", other.rsplit(':').next().unwrap());
        assert_eq!(did_key(&forged), forged);
        assert_eq!(short_form(long.as_bytes()), None);
    }
}
