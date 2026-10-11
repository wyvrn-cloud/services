//! Checking a `did:webvh` log, and what it comes to.

use didwebvh_rs::DIDWebVHState;
use serde_json::Value;

use wyvrn_hosted_did::keys;
use crate::names::Name;

/// A log that verifies, and what it says.
#[derive(Debug, Clone)]
pub struct Checked {
    /// `did:webvh:<SCID>:<host>:<name>`.
    pub did: String,
    /// The parallel `did:web:<host>:<name>`.
    pub did_web: String,
    /// The `versionId` of the last entry.
    pub version_id: String,
    /// The document the last entry gives.
    pub document: Value,
    /// The same as a `did:web` document, as `did:webvh` derives it.
    pub web_document: Value,
    pub deactivated: bool,
    pub portable: bool,
}

impl Checked {
    pub fn key_agreement(&self) -> Vec<String> {
        keys::keys_for(&self.document, "keyAgreement")
    }

    pub fn authentication(&self) -> Vec<String> {
        keys::keys_for(&self.document, "authentication")
    }
}

/// `did:web:<host>:<name>`.
pub fn did_web_for(host: &str, name: &Name) -> String {
    format!("did:web:{host}:{}", name.as_did_path())
}

/// The SCID a log declares, in its first entry.
fn scid_of(log: &str) -> Option<String> {
    let first: Value = serde_json::from_str(log.lines().next()?).ok()?;
    first["parameters"]["scid"].as_str().map(str::to_string)
}

/// Verifies `log` as the whole history of the DID that `name` at `host` would
/// be, and that its document is that DID's and names its parallel `did:web`.
/// The error says what is wrong, for the holder to read.
/// How far ahead of this host's clock an entry may be dated and still be taken:
/// the clocks of the device that wrote it and of this host are never quite the
/// same. (`did:webvh` allows a resolver "a small, implementation-defined
/// tolerance" here; the library that checks a log allows none.)
pub const CLOCK_TOLERANCE_SECONDS: i64 = 30;

/// When the log's newest entry says it was written, in milliseconds since the epoch.
fn newest_version_time(log: &str) -> Option<i64> {
    let entry: serde_json::Value = serde_json::from_str(log.lines().last()?).ok()?;
    Some(chrono::DateTime::parse_from_rfc3339(entry["versionTime"].as_str()?).ok()?.timestamp_millis())
}

/// An entry is refused if it is dated in the future. One dated a little ahead of
/// this host's clock was written by a device whose clock runs a little fast: it is
/// waited out, so that by the time it is checked it is no longer ahead. One dated
/// far ahead is refused, saying so.
async fn wait_out_clock_skew(log: &str) -> Result<(), String> {
    let Some(written) = newest_version_time(log) else { return Ok(()) };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    let ahead = written - now;
    if ahead > CLOCK_TOLERANCE_SECONDS * 1000 {
        return Err(format!("the newest entry is dated {} seconds from now: check the clock of the device that wrote it", ahead / 1000));
    }
    if ahead > 0 {
        tokio::time::sleep(std::time::Duration::from_millis((ahead + 200) as u64)).await;
    }
    Ok(())
}

pub async fn check(host: &str, name: &Name, log: &str) -> Result<Checked, String> {
    let scid = scid_of(log).ok_or("the log's first entry has no parameters.scid")?;
    let did = format!("did:webvh:{scid}:{host}:{}", name.as_did_path());
    let did_web = did_web_for(host, name);

    wait_out_clock_skew(log).await?;

    // The SCID against the first entry, each entry against the one before it,
    // and every proof against the keys allowed to update at that point.
    let mut state = DIDWebVHState::default();
    let (_, metadata) = state.resolve_log(&did, log, None).await.map_err(|e| format!("the log doesn't verify: {e}"))?;
    let last = state.log_entries().last().ok_or("the log is empty")?;
    let document = last.get_did_document().map_err(|e| format!("the log gives no document: {e}"))?;

    if document["id"].as_str() != Some(did.as_str()) {
        return Err(format!("the document's id is not {did}"));
    }
    let also_known_as: Vec<&str> = document["alsoKnownAs"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if !also_known_as.contains(&did_web.as_str()) {
        return Err(format!("the document's alsoKnownAs doesn't include {did_web}"));
    }

    let mut web_document = last.to_web_did().map_err(|e| format!("no did:web document can be made of it: {e}"))?;
    // The did:web names the did:webvh it is the shadow of: that is what lets a
    // resolver move from the one to the other.
    let names_it = web_document["alsoKnownAs"].as_array().is_some_and(|aka| aka.iter().any(|v| v == &Value::String(did.clone())));
    if !names_it {
        let mut aka = web_document["alsoKnownAs"].as_array().cloned().unwrap_or_default();
        aka.retain(|v| v != &Value::String(did_web.clone()));
        aka.push(Value::String(did.clone()));
        web_document["alsoKnownAs"] = Value::Array(aka);
    }

    Ok(Checked {
        did,
        did_web,
        version_id: metadata.version_id,
        document,
        web_document,
        deactivated: metadata.deactivated,
        portable: metadata.portable,
    })
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    fn dated(offset_ms: i64) -> String {
        let at = chrono::DateTime::from_timestamp_millis(chrono::Utc::now().timestamp_millis() + offset_ms).unwrap();
        // As an entry is: to the second.
        format!(r#"{{"versionId":"1-Qm","versionTime":"{}"}}"#, at.format("%Y-%m-%dT%H:%M:%SZ"))
    }

    #[tokio::test]
    async fn an_entry_dated_a_little_ahead_is_waited_out_and_one_far_ahead_refused() {
        // Written by a device whose clock is a few seconds fast: by the time this
        // returns, the entry is no longer in the future.
        let log = dated(2500);
        let written = newest_version_time(&log).unwrap();
        wait_out_clock_skew(&log).await.unwrap();
        assert!(chrono::Utc::now().timestamp_millis() >= written);

        // One from the past costs nothing.
        let started = std::time::Instant::now();
        wait_out_clock_skew(&dated(-60_000)).await.unwrap();
        assert!(started.elapsed().as_millis() < 100);

        // A clock that is simply wrong is said to be.
        let refused = wait_out_clock_skew(&dated(120_000)).await.unwrap_err();
        assert!(refused.contains("check the clock"), "{refused}");
        // Something that isn't a log is for the real check to refuse.
        assert!(wait_out_clock_skew("not json").await.is_ok());
    }
}
