//! Answering `GET`/`HEAD /f/<id>` for a stored file, wherever its bytes are:
//! from disk (Range and all, by tower-http), or from a bucket, sent there or
//! passed on (bucket.rs's `Downloads`). The files service and a community's
//! files both answer through here.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use tower_http::services::ServeFile;

use crate::bucket::{attachment, Downloads};
use crate::store::{FileRecord, Kept, Store};

/// How a stored file is answered: what both callers add to the bytes.
#[derive(Debug, Clone, Copy)]
pub struct Answering {
    /// `Content-Security-Policy: sandbox`, so the bytes are never run as a page
    /// of the host's, even opened in a tab (WYV-52).
    pub sandbox: bool,
}

/// The answer to a request for `file`.
pub async fn stored_file(store: &Store, file: &FileRecord, request: Request, answering: Answering) -> Response {
    // What a fetcher holds, if it holds the file: no need to send it twice.
    let etag = format!("\"{}\"", file.hash);
    if request.headers().get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut response = match store.kept(file) {
        Kept::Disk(path) => match ServeFile::new(path).try_call(request).await {
            Ok(response) => response.into_response(),
            Err(error) => return failed(error),
        },
        Kept::Bucket(bucket, key) => match bucket.downloads() {
            Downloads::Redirect => {
                let link = bucket.link(&key, &file.media_type, &file.name);
                let mut response = StatusCode::FOUND.into_response();
                if let Ok(value) = HeaderValue::from_str(link.as_str()) {
                    response.headers_mut().insert(header::LOCATION, value);
                }
                // Not kept longer than the link is good for.
                let keep = (bucket.link_for().as_secs() / 2).max(10);
                if let Ok(value) = HeaderValue::from_str(&format!("private, max-age={keep}")) {
                    response.headers_mut().insert(header::CACHE_CONTROL, value);
                }
                return response;
            }
            Downloads::Proxy => match passed_on(&bucket, &key, file, request.method() == Method::HEAD, request.headers().get(header::RANGE).and_then(|v| v.to_str().ok()).map(str::to_string)).await {
                Ok(response) => response,
                Err(error) => return failed(error),
            },
        },
        Kept::Lost(tag) => {
            tracing::error!(id = %file.id, %tag, "a file is in a bucket this service isn't given");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    dress(response.headers_mut(), file, &etag, answering);
    response
}

/// What the bucket says of `key`, passed on: `Range` asked for, and the answer's
/// status, length and range, streamed as it comes.
async fn passed_on(bucket: &crate::bucket::Bucket, key: &str, file: &FileRecord, head: bool, range: Option<String>) -> anyhow::Result<Response> {
    if head {
        let mut response = StatusCode::OK.into_response();
        response.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(file.byte_count));
        response.headers_mut().insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        return Ok(response);
    }
    let answer = bucket.fetch(key, range.as_deref()).await?;
    let status = StatusCode::from_u16(answer.status().as_u16())?;
    if !(status.is_success() || status == StatusCode::RANGE_NOT_SATISFIABLE) {
        anyhow::bail!("the bucket answered {status} for {}", file.id);
    }
    let mut response = Response::builder().status(status);
    for name in [header::CONTENT_LENGTH, header::CONTENT_RANGE] {
        if let Some(value) = answer.headers().get(name.as_str()).and_then(|v| HeaderValue::from_bytes(v.as_bytes()).ok()) {
            response = response.header(name, value);
        }
    }
    Ok(response.header(header::ACCEPT_RANGES, "bytes").body(Body::from_stream(answer.bytes_stream()))?)
}

fn dress(headers: &mut HeaderMap, file: &FileRecord, etag: &str, answering: Answering) {
    if let Ok(value) = HeaderValue::from_str(&file.media_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&attachment(&file.name)) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=3600"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if answering.sandbox {
        headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox; default-src 'none'; frame-ancestors 'none'"));
    }
}

fn failed(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "a stored file couldn't be served");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

/// What the store does in the background while a bucket is in use: moves what
/// waits on disk to it, and tells it what was deleted. Every `every`.
pub async fn tend(store: &Store) {
    if store.bucket().is_none() {
        return;
    }
    match store.settle().await {
        Ok(0) => {}
        Ok(moved) => tracing::info!(moved, "moved stored files to the bucket"),
        Err(error) => tracing::warn!(%error, "moving files to the bucket failed; tried again later"),
    }
    match store.drain_doomed().await {
        Ok(0) => {}
        Ok(gone) => tracing::info!(gone, "removed deleted files from the bucket"),
        Err(error) => tracing::warn!(%error, "removing deleted files from the bucket failed; tried again later"),
    }
}
