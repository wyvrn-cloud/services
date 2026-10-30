//! `wyvrn_hosted_did::api`, for a web app: built with
//! `wasm-pack build --target web --out-dir pkg-web --release`. Every function takes
//! and gives JSON text (see that module); a failure rejects with words for a person.

use wasm_bindgen::prelude::*;
use wyvrn_hosted_did::api;

fn rejected(why: String) -> JsValue {
    js_sys::Error::new(&why).into()
}

/// Reads a hosted DID from the files fetched so far: `{need}`, `{failed}` or `{done}`.
#[wasm_bindgen(js_name = hostedRead)]
pub async fn hosted_read(did: String, files_json: String) -> Result<String, JsValue> {
    api::read(&did, &files_json).await.map_err(rejected)
}

/// Writes a DID's first log entry: `{did, entry, log, versionId}`.
#[wasm_bindgen(js_name = hostedCreate)]
pub async fn hosted_create(document_json: String, update_secret_jwk: String, update_keys_json: String) -> Result<String, JsValue> {
    api::create(&document_json, &update_secret_jwk, &update_keys_json).await.map_err(rejected)
}

/// Appends a log entry giving the DID a new document.
#[wasm_bindgen(js_name = hostedAppend)]
pub async fn hosted_append(did: String, log: String, document_json: String, update_secret_jwk: String, update_keys_json: String) -> Result<String, JsValue> {
    api::append(&did, &log, &document_json, &update_secret_jwk, &update_keys_json).await.map_err(rejected)
}

/// Appends the entry that ends the DID.
#[wasm_bindgen(js_name = hostedDeactivate)]
pub async fn hosted_deactivate(did: String, log: String, update_secret_jwk: String) -> Result<String, JsValue> {
    api::deactivate(&did, &log, &update_secret_jwk).await.map_err(rejected)
}

/// The `jws` of a signed attachment, over `bytes`.
#[wasm_bindgen(js_name = hostedSignAttachment)]
pub fn hosted_sign_attachment(bytes: &[u8], authentication_secret_jwk: String) -> Result<String, JsValue> {
    api::sign_attachment(bytes, &authentication_secret_jwk).map_err(rejected)
}

/// Signs `payload` under the header `header_json` (alg EdDSA): a compact JWS.
#[wasm_bindgen(js_name = hostedSignCompact)]
pub fn hosted_sign_compact(header_json: String, payload: &[u8], authentication_secret_jwk: String) -> Result<String, JsValue> {
    api::sign_compact(&header_json, payload, &authentication_secret_jwk).map_err(rejected)
}

/// A Verifiable Presentation proved for one verifier (`challenge`, `domain`).
#[wasm_bindgen(js_name = hostedSignPresentation)]
pub fn hosted_sign_presentation(presentation_json: String, secret_jwk: String, verification_method: String, challenge: String, domain: String, created: String) -> Result<String, JsValue> {
    api::sign_presentation(&presentation_json, &secret_jwk, &verification_method, &challenge, &domain, &created).map_err(rejected)
}

/// Throws unless a presentation was proved for `challenge` and `domain` by a key the
/// holder's document lists under `authentication`.
#[wasm_bindgen(js_name = hostedCheckPresentation)]
pub fn hosted_check_presentation(signed_json: String, document_json: String, challenge: String, domain: String) -> Result<(), JsValue> {
    api::check_presentation(&signed_json, &document_json, &challenge, &domain).map_err(rejected)
}

#[wasm_bindgen(js_name = hostedSignedBy)]
pub fn hosted_signed_by(bytes: &[u8], jws_json: String, keys_json: String) -> Result<String, JsValue> {
    api::signed_by(bytes, &jws_json, &keys_json).map_err(rejected)
}

/// A signed whois saying who the DID is.
#[wasm_bindgen(js_name = hostedWhois)]
pub async fn hosted_whois(did: String, profile_json: String, authentication_secret_jwk: String, verification_method: String, valid_from: String) -> Result<String, JsValue> {
    api::whois(&did, &profile_json, &authentication_secret_jwk, &verification_method, &valid_from).await.map_err(rejected)
}

/// The multikey of an Ed25519 key held as a JWK.
#[wasm_bindgen(js_name = hostedPublicMultikey)]
pub fn hosted_public_multikey(secret_jwk: String) -> Result<String, JsValue> {
    api::public_multikey(&secret_jwk).map_err(rejected)
}

/// The `did-configuration.json` a website serves to say it is the DID's holder's.
#[wasm_bindgen(js_name = hostedLinkageConfiguration)]
pub async fn hosted_linkage_configuration(origin: String, dids_json: String, secret_jwk: String, issued: String, expires: String) -> Result<String, JsValue> {
    api::linkage_configuration(&origin, &dids_json, &secret_jwk, &issued, &expires).await.map_err(rejected)
}

/// Whether a website and a DID name each other: `true`, or why not.
#[wasm_bindgen(js_name = hostedCheckLinkage)]
pub fn hosted_check_linkage(configuration: String, origin: String, did: String, document_json: String, now: String) -> Result<String, JsValue> {
    api::check_linkage(&configuration, &origin, &did, &document_json, &now).map_err(rejected)
}

/// A website as typed, as the HTTPS origin it is.
#[wasm_bindgen(js_name = hostedLinkageOrigin)]
pub fn hosted_linkage_origin(text: String) -> Result<String, JsValue> {
    api::linkage_origin(&text).map_err(rejected)
}

/// Whether a linked-account credential checks out against its issuer's document
/// and status list (`""` when there is none to read): `true`, or why not.
#[wasm_bindgen(js_name = hostedCheckLinkedAccount)]
pub fn hosted_check_linked_account(credential_json: String, issuer_document_json: String, status_list_json: String, now: String) -> Result<bool, JsValue> {
    api::check_linked_account(&credential_json, &issuer_document_json, &status_list_json, &now).map(|()| true).map_err(rejected)
}
