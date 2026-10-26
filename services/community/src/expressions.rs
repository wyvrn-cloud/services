//! The community's own sticker and custom-emoji packs (community/1.0's "Files,
//! stickers and the community's own emoji"): wyvrn-stickers' processor inside the
//! community service, made and changed through its workflows by members holding
//! `manage_expressions`, served at `/s/<name>`. The packs are the community's, not
//! whoever made them: the processor is told every maker is the community itself,
//! so any of them may change any pack -- and only one pack is being made at a time.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use wyvrn_stickers::processor::Processor;
use wyvrn_stickers::store::{draft_scope, pack_scope, Store};
use wyvrn_stickers::templates::{EDIT_PACK, NEW_EMOJI_PACK, NEW_STICKER_PACK};

use crate::server::AppState;

pub struct Expressions {
    pub processor: Processor,
}

impl Expressions {
    /// Packs kept in `path` (or the database the environment names for them,
    /// `COMMUNITY_STICKERS_DATABASE_URL`), served under `public_url`'s `s/`.
    pub fn open(path: &std::path::Path, public_url: &str) -> anyhow::Result<Self> {
        let location = wyvrn_service::db::Location::from_env("COMMUNITY_STICKERS", path)?;
        Ok(Self::with(Store::open(&location)?, public_url))
    }

    pub fn scratch(public_url: &str) -> anyhow::Result<Self> {
        Ok(Self::with(Store::in_memory()?, public_url))
    }

    fn with(store: Store, public_url: &str) -> Self {
        let base = format!("{}s/", if public_url.ends_with('/') { public_url.to_string() } else { format!("{public_url}/") });
        Self { processor: Processor::new(store, Default::default(), &base) }
    }

    /// Whether a workflow message is about the packs: a start of one of their
    /// templates, or an instance the processor holds.
    pub fn concerns(&self, message: &Value) -> bool {
        let body = &message["body"];
        if message["type"].as_str().is_some_and(|t| t.ends_with("/start") || t.ends_with("/fetch-template")) {
            return body["template_id"].as_str().is_some_and(|id| [NEW_STICKER_PACK, NEW_EMOJI_PACK, EDIT_PACK].contains(&id));
        }
        body["instance_id"].as_str().is_some_and(|id| self.processor.store.instance(id).ok().flatten().is_some())
    }

    /// The community's packs, as `community` lists them: their addresses.
    pub fn packs(&self, owner: &str) -> anyhow::Result<Vec<String>> {
        Ok(self.processor.store.packs(Some(owner))?.into_iter().filter(|pack| !pack.removed).map(|pack| self.processor.link(&pack.name)).collect())
    }
}

fn internal(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "a pack couldn't be served");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

fn picture(bytes: Vec<u8>, media_type: &str, cache: &'static str) -> Response {
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    if let Ok(media_type) = HeaderValue::from_str(media_type) {
        headers.insert(header::CONTENT_TYPE, media_type);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

/// `GET /s/<name>` or `/s/<name>/pack.json`: a pack, as anyone may fetch it.
pub async fn manifest(State(state): State<Arc<AppState>>, Path(name): Path<String>, request_headers: HeaderMap) -> Response {
    let Some(expressions) = &state.community.expressions else { return StatusCode::NOT_FOUND.into_response() };
    let pack = match expressions.processor.store.pack(&name) {
        Ok(Some(pack)) => pack,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal(error),
    };
    if pack.removed {
        return StatusCode::GONE.into_response();
    }
    let body = match expressions.processor.manifest(&pack) {
        Ok(manifest) => manifest.to_string(),
        Err(error) => return internal(error),
    };
    let tag = format!("\"{}-{}\"", pack.version, body.len());
    if request_headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(tag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if let Ok(tag) = HeaderValue::from_str(&tag) {
        headers.insert(header::ETAG, tag);
    }
    response
}

/// `GET /s/<name>/<file>`: one picture of a pack, kept for good (named for what is in it).
pub async fn pack_file(State(state): State<Arc<AppState>>, Path((name, file)): Path<(String, String)>) -> Response {
    let Some(expressions) = &state.community.expressions else { return StatusCode::NOT_FOUND.into_response() };
    if name == "drafts" {
        return StatusCode::NOT_FOUND.into_response();
    }
    match expressions.processor.store.pack(&name) {
        Ok(Some(pack)) if pack.removed => return StatusCode::GONE.into_response(),
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal(error),
    }
    match expressions.processor.store.file(&pack_scope(&name), &file) {
        Ok(Some((bytes, media_type))) => picture(bytes, &media_type, "public, max-age=31536000, immutable"),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal(error),
    }
}

/// `GET /s/drafts/<instance>/<file>`: a picture of a pack being made, for its maker's app.
pub async fn draft_file(State(state): State<Arc<AppState>>, Path((instance, file)): Path<(String, String)>) -> Response {
    let Some(expressions) = &state.community.expressions else { return StatusCode::NOT_FOUND.into_response() };
    match expressions.processor.store.file(&draft_scope(&instance), &file) {
        Ok(Some((bytes, media_type))) => picture(bytes, &media_type, "private, no-store"),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal(error),
    }
}
