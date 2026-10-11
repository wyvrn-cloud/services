//! Checking a `did:webvh` log, and what it comes to.

use didwebvh_rs::DIDWebVHState;
use serde_json::Value;

use crate::keys;
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
pub async fn check(host: &str, name: &Name, log: &str) -> Result<Checked, String> {
    let scid = scid_of(log).ok_or("the log's first entry has no parameters.scid")?;
    let did = format!("did:webvh:{scid}:{host}:{}", name.as_did_path());
    let did_web = did_web_for(host, name);

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
