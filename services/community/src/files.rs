//! Files members attach to posts, kept by the community itself (community/1.0's
//! "Files, stickers…"): files/1.0's host, at the community's own DID, for members
//! who may `attach_files` somewhere. The host is wyvrn-files' own; what is here is
//! who may use it, how much, and its HTTP side (`/u/<id>` to put, `/f/<id>` to fetch).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use tower_http::services::ServeFile;
use wyvrn_files::host::{Fetch, Host, Put, PIURI};
use wyvrn_files::policy::{Limits, Policy};

use crate::config::FilesConfig;
use crate::server::AppState;

/// "For as long as the post that carries it is": a hundred years.
const KEEP: i64 = 100 * 365 * 24 * 3600;

/// The community's files: the host, and a lock for each upload in progress.
pub struct Files {
    pub host: Host,
    config: FilesConfig,
    uploads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Files {
    pub fn open(config: &FilesConfig, public_url: &str) -> anyhow::Result<Self> {
        let location = wyvrn_service::db::Location::from_env("COMMUNITY_FILES", &config.dir.join("files.sqlite"))?;
        let store = wyvrn_files::store::Store::open(&location, &config.dir)?;
        let service = wyvrn_files::config::ServiceConfig {
            max_file_bytes: config.max_file_bytes,
            quota_bytes: config.quota_bytes,
            total_bytes: config.total_bytes,
            default_keep_seconds: KEEP,
            max_keep_seconds: KEEP,
            ..Default::default()
        };
        Ok(Self { host: Host::new(store, service, public_url), config: config.clone(), uploads: Mutex::default() })
    }

    /// For tests: records in memory, bytes in a directory of their own.
    pub fn scratch(config: &FilesConfig, public_url: &str) -> anyhow::Result<Self> {
        let service = wyvrn_files::config::ServiceConfig {
            max_file_bytes: config.max_file_bytes,
            quota_bytes: config.quota_bytes,
            default_keep_seconds: KEEP,
            max_keep_seconds: KEEP,
            ..Default::default()
        };
        Ok(Self { host: Host::new(wyvrn_files::store::Store::scratch()?, service, public_url), config: config.clone(), uploads: Mutex::default() })
    }

    pub fn max_file_bytes(&self) -> u64 {
        self.config.max_file_bytes
    }

    fn limits(&self) -> Limits {
        Limits { quota_bytes: self.config.quota_bytes, max_file_bytes: self.config.max_file_bytes, max_keep_seconds: KEEP, max_uploads_in_progress: 8 }
    }

    fn policy(&self) -> Policy {
        let limits = self.limits();
        Policy { open: true, locked: false, require_profile: false, total_bytes: self.config.total_bytes, default_keep_seconds: KEEP, everyone: limits, admins: limits }
    }

    /// Answers a files/1.0 message from a member who may attach files.
    pub fn handle(&self, sender: &str, message: &Value, now: i64) -> Option<(String, Value)> {
        let answer = self.host.handle(sender, &self.limits(), &self.policy(), message, now)?;
        Some(match answer {
            Ok((kind, body)) => (format!("{PIURI}/{kind}"), body),
            Err(refusal) => (format!("{PIURI}/problem-report"), json!({ "code": refusal.code, "comment": refusal.comment, "args": refusal.args })),
        })
    }

    fn upload_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut uploads = self.uploads.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        uploads.retain(|_, lock| Arc::strong_count(lock) > 1);
        uploads.entry(id.to_string()).or_default().clone()
    }
}

/// Whether `message` is files/1.0's.
pub fn is_files(message: &Value) -> bool {
    message["type"].as_str().is_some_and(|t| t.starts_with(PIURI))
}

fn with_offset(status: StatusCode, offset: u64) -> Response {
    let mut response = status.into_response();
    if let Ok(value) = HeaderValue::from_str(&offset.to_string()) {
        response.headers_mut().insert("upload-offset", value);
    }
    response
}

fn fetch_response(error: Fetch) -> Response {
    match error {
        Fetch::NotFound => StatusCode::NOT_FOUND.into_response(),
        Fetch::Gone => StatusCode::GONE.into_response(),
        Fetch::Failed(error) => {
            tracing::error!(%error, "a file couldn't be read");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `HEAD /u/<id>`: where an upload stands.
pub async fn upload_standing(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(files) = &state.community.files else { return StatusCode::NOT_FOUND.into_response() };
    match files.host.upload_in_progress(&id, wyvrn_service::now()) {
        Ok(file) => {
            let mut response = with_offset(StatusCode::OK, files.host.store.received(&id));
            if let Ok(value) = HeaderValue::from_str(&file.byte_count.to_string()) {
                response.headers_mut().insert("upload-length", value);
            }
            response
        }
        Err(error) => fetch_response(error),
    }
}

/// `PUT /u/<id>` with `Upload-Offset`: one chunk, appended if it is the next.
pub async fn put_chunk(State(state): State<Arc<AppState>>, Path(id): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    if state.community.files.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(offset) = headers.get("upload-offset").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok()) else {
        return (StatusCode::BAD_REQUEST, "Upload-Offset is missing or not a number").into_response();
    };
    let lock = state.community.files.as_ref().expect("checked").upload_lock(&id);
    let _held = lock.lock().await;
    let writer = state.clone();
    let outcome = tokio::task::spawn_blocking(move || writer.community.files.as_ref().expect("checked").host.put_chunk(&id, offset, &body, wyvrn_service::now())).await;
    match outcome {
        Ok(Ok(held)) => with_offset(StatusCode::NO_CONTENT, held),
        Ok(Err(Put::WrongOffset(held))) => with_offset(StatusCode::CONFLICT, held),
        Ok(Err(Put::TooLong)) => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Ok(Err(Put::Fetch(error))) => fetch_response(error),
        Err(error) => {
            tracing::error!(%error, "a chunk couldn't be written");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `GET`/`HEAD /f/<id>`: a stored file, whole or by `Range`.
pub async fn fetch(State(state): State<Arc<AppState>>, Path(id): Path<String>, request: Request) -> Response {
    let Some(files) = &state.community.files else { return StatusCode::NOT_FOUND.into_response() };
    let file = match files.host.stored(&id, wyvrn_service::now()) {
        Ok(file) => file,
        Err(error) => return fetch_response(error),
    };
    let tag = format!("\"{}\"", file.hash);
    if request.headers().get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(tag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut response = match ServeFile::new(files.host.store.path(&id)).try_call(request).await {
        Ok(response) => response.into_response(),
        Err(error) => {
            tracing::error!(%error, "a file couldn't be served");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&file.media_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", file.name.replace(['"', '\\'], "_"))) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    if let Ok(value) = HeaderValue::from_str(&tag) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=3600"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}
