//! Reading a hosted DID one file at a time, for a caller that does its own
//! fetching and can't be called back into: an app's interface thread, a web
//! worker, anything on the far side of a boundary.
//!
//! The caller asks with the files it has. The answer is either what the DID
//! comes to, or the one file to fetch next. It fetches that, adds it, and asks
//! again. Each ask is the whole of [`resolve`](crate::resolve) and
//! [`whois_of`](crate::whois_of) run over what there is, so the order and the
//! checks are theirs: nothing is decided differently here.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::resolve::{self, Fetch, Resolved, Standing};
use crate::verify;
use crate::whois::Profile;

/// What was fetched so far: by URL, its text, or `None` for "nothing there" (404).
pub type Files = HashMap<String, Option<String>>;

/// A hosted DID, read.
#[derive(Debug, Clone)]
pub struct Read {
    pub resolved: Resolved,
    /// Who it says it is, if it publishes that at its own host and it checks out.
    pub profile: Option<Profile>,
    /// Why there is no profile, if it published one that didn't check out.
    pub profile_problem: Option<String>,
    /// A `did:peer:4` the DID names as itself, that says so back (it names the
    /// DID's short name, and shares a key with it): where its holder writes from.
    pub peer: Option<String>,
}

pub enum Step {
    /// Fetch this URL (text; "nothing there" is an answer too), then ask again.
    Need(String),
    Done(Box<Read>),
    Failed(String),
}

struct Have<'a> {
    files: &'a Files,
    missing: Mutex<Option<String>>,
}

impl Fetch for Have<'_> {
    async fn get(&self, url: &str) -> Result<Option<Vec<u8>>, String> {
        match self.files.get(url) {
            Some(file) => Ok(file.as_ref().map(|text| text.as_bytes().to_vec())),
            None => {
                // The first file found wanting is the one to fetch: whatever is asked
                // for after it was asked for without it.
                self.missing.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get_or_insert_with(|| url.to_string());
                Err(format!("{url} has not been fetched"))
            }
        }
    }
}

/// Reads `did` (a `did:web` or `did:webvh`) from `files`.
pub async fn read(did: &str, files: &Files) -> Step {
    let have = Have { files, missing: Mutex::new(None) };
    let take_missing = |have: &Have<'_>| have.missing.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();

    let resolved = resolve::resolve(did, &have).await;
    // A file not fetched yet is not a verdict on anything: fetch it and ask again.
    if let Some(url) = take_missing(&have) {
        return Step::Need(url);
    }
    let resolved = match resolved {
        Ok(resolved) => resolved,
        Err(why) => return Step::Failed(why),
    };

    let whois = resolve::whois_of(&resolved, &have).await;
    if let Some(url) = take_missing(&have) {
        return Step::Need(url);
    }
    let (profile, profile_problem) = match whois {
        Ok(profile) => (profile, None),
        Err(why) => (None, Some(why)),
    };

    // Only a DID whose history verified can vouch for where its holder writes from.
    let peer = match (&resolved.did_web, resolved.standing) {
        (Some(did_web), Standing::Verified) => verify::also_known_as(&resolved.document)
            .into_iter()
            .filter(|aka| aka.starts_with("did:peer:"))
            .find(|peer| verify::peer4_document(peer).is_some_and(|document| verify::peer_is_holder(&document, did_web, &resolved.document)))
            .map(str::to_string),
        _ => None,
    };
    Step::Done(Box::new(Read { resolved, profile, profile_problem, peer }))
}
