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
/// - `{"done": {did, didWeb, verified, deactivated, host, document, linkedDomains,
///   peer, profile, profileProblem}}`. `linkedDomains` are origins the DID *says*
///   are its holder's: see [`check_linkage`]. `did` is the DID to know the party by (the `did:webvh`
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
                "linkedDomains": crate::linkage::linked_origins(&read.resolved.document),
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

/// Which of `keys_json` (an array of Ed25519 multikeys) signed `bytes`, by the
/// detached signature `jws_json` (`{protected, signature}`, as
/// [`sign_attachment`] makes one): that key, or an empty string if none did.
pub fn signed_by(bytes: &[u8], jws_json: &str, keys_json: &str) -> Result<String, String> {
    let jws: Value = parse("the signature", jws_json)?;
    let keys: Vec<String> = parse("the keys", keys_json)?;
    Ok(crate::keys::signer_among(bytes, &jws, &keys).cloned().unwrap_or_default())
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

/// The file a website serves at `/.well-known/did-configuration.json` to say it is
/// its DID's holder's: one [`linkage::credential`](crate::linkage::credential) for
/// each of `dids_json` (`[{did, verificationMethod}]`: the DID under each of its
/// names), all signed with `secret_jwk`. `issued` and `expires` are RFC 3339.
pub async fn linkage_configuration(origin: &str, dids_json: &str, secret_jwk: &str, issued: &str, expires: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Named {
        did: String,
        verification_method: String,
    }
    let dids: Vec<Named> = parse("the DIDs", dids_json)?;
    let mut credentials = Vec::new();
    for named in dids {
        credentials.push(crate::linkage::credential(&named.did, origin, secret_jwk, &named.verification_method, issued, expires).await?);
    }
    serde_json::to_string_pretty(&crate::linkage::configuration(credentials)).map_err(|e| e.to_string())
}

/// [`linkage::check`](crate::linkage::check): whether `origin` and `did` name each
/// other. `configuration` is what the origin serves at
/// `/.well-known/did-configuration.json`, and `document_json` the DID's document.
/// Answers `true`, or fails with which half didn't hold.
pub fn check_linkage(configuration: &str, origin: &str, did: &str, document_json: &str, now: &str) -> Result<String, String> {
    let document: Value = parse("the document", document_json)?;
    crate::linkage::check(configuration.as_bytes(), origin, did, &document, now).map(|()| "true".to_string())
}

/// [`linkage::origin_of`](crate::linkage::origin_of): a website as typed, as the
/// origin it is. Fails if it isn't a bare HTTPS one.
pub fn linkage_origin(text: &str) -> Result<String, String> {
    crate::linkage::origin_of(text).ok_or_else(|| "That is not a website's address: it should look like example.com".to_string())
}
