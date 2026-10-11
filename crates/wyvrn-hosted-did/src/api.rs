//! Everything an app needs of this crate, as text in and text out: the same
//! functions whether they are reached through WebAssembly or across an IPC
//! boundary, so that neither wrapper decides anything.
//!
//! Arguments and results are JSON. A failure is an `Err` with words for a person.

use serde_json::{json, Value};

use crate::read::{self, Files, Step};
use crate::resolve::Standing;
use crate::whois::{Image, Profile};
use crate::write::{self, ProfileFields, Written};

fn parse<T: serde::de::DeserializeOwned>(what: &str, text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| format!("{what} is not what was expected: {e}"))
}

fn image(image: &Option<Image>) -> Value {
    match image {
        Some(image) => json!({
            "url": image.url, "mediaType": image.media_type, "width": image.width, "height": image.height,
            "sha256": image.sha256, "inline": image.is_inline(),
        }),
        None => Value::Null,
    }
}

fn profile(profile: &Profile) -> Value {
    json!({
        "kind": profile.kind, "name": profile.name, "description": profile.description,
        "image": image(&profile.image), "logo": image(&profile.logo), "banner": image(&profile.banner), "background": image(&profile.background),
    })
}

/// Reads a hosted DID from the files fetched so far (`{"<url>": "<text>" | null}`).
/// Answers one of:
///
/// - `{"need": "<url>"}`: fetch that (as text; a 404 is `null`), add it, ask again.
/// - `{"failed": "<why>"}`.
/// - `{"done": {did, didWeb, verified, deactivated, host, document, peer, profile,
///   profileProblem}}`. `did` is the DID to know the party by (the `did:webvh`
///   where one verified). `verified` is whether its history did, as against being
///   its host's word. `peer` is a `did:peer:4` it names that names it back.
pub async fn read(did: &str, files_json: &str) -> Result<String, String> {
    let files: Files = parse("the files", files_json)?;
    let answer = match read::read(did, &files).await {
        Step::Need(url) => json!({ "need": url }),
        Step::Failed(why) => json!({ "failed": why }),
        Step::Done(read) => {
            let place = crate::urls::Location::of(&read.resolved.did);
            json!({ "done": {
                "did": read.resolved.did,
                "didWeb": read.resolved.did_web,
                "verified": read.resolved.standing == Standing::Verified,
                "deactivated": read.resolved.deactivated,
                "host": place.as_ref().map(|place| place.host.clone()),
                "name": place.map(|place| place.path.join(":")),
                "document": read.resolved.document,
                "peer": read.peer,
                "profile": read.profile.as_ref().map(profile),
                "profileProblem": read.profile_problem,
            }})
        }
    };
    Ok(answer.to_string())
}

fn written(written: Written) -> String {
    json!({ "did": written.did, "entry": written.entry, "log": written.log, "versionId": written.version_id }).to_string()
}

/// [`write::create`]. `update_keys_json` is an array of multikeys. Answers
/// `{did, entry, log, versionId}`.
pub async fn create(document_json: &str, update_secret_jwk: &str, update_keys_json: &str) -> Result<String, String> {
    let document: Value = parse("the document", document_json)?;
    let update_keys: Vec<String> = parse("the update keys", update_keys_json)?;
    write::create(&document, update_secret_jwk, &update_keys).await.map(written)
}

/// [`write::append`]. `update_keys_json` is an array of multikeys, or `null` to
/// leave who may update as it is.
pub async fn append(did: &str, log: &str, document_json: &str, update_secret_jwk: &str, update_keys_json: &str) -> Result<String, String> {
    let document: Value = parse("the document", document_json)?;
    let update_keys: Option<Vec<String>> = parse("the update keys", update_keys_json)?;
    write::append(did, log, &document, update_secret_jwk, update_keys.as_deref()).await.map(written)
}

/// [`write::deactivate`].
pub async fn deactivate(did: &str, log: &str, update_secret_jwk: &str) -> Result<String, String> {
    write::deactivate(did, log, update_secret_jwk).await.map(written)
}

/// [`write::sign_attachment`]: the `jws` object, as JSON.
pub fn sign_attachment(bytes: &[u8], authentication_secret_jwk: &str) -> Result<String, String> {
    write::sign_attachment(bytes, authentication_secret_jwk).map(|jws| jws.to_string())
}

/// [`write::whois`]: the signed presentation, as JSON. `profile_json` is
/// `{kind, name, description, image, logo, banner, background}`, all optional.
pub async fn whois(did: &str, profile_json: &str, authentication_secret_jwk: &str, verification_method: &str, valid_from: &str) -> Result<String, String> {
    let profile: ProfileFields = parse("the profile", profile_json)?;
    write::whois(did, &profile, authentication_secret_jwk, verification_method, valid_from).await.map(|vp| vp.to_string())
}

/// [`write::public_multikey`].
pub fn public_multikey(secret_jwk: &str) -> Result<String, String> {
    write::public_multikey(secret_jwk)
}
