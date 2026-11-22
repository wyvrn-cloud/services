//! Webhooks (community/1.0's "Webhooks"): a URL that posts into a channel, for
//! other services — a CI's results, a monitor's alerts, a feed — as Discord's
//! do, and taking what Discord's take, so that anything that posts to a Discord
//! webhook posts here given this URL instead:
//!
//! - `POST /api/webhooks/{id}/{token}`: a post, from JSON (`content`, `username`,
//!   `avatar_url`, `embeds`, `thread_name`) or a multipart form (`payload_json`
//!   and files). `?wait=true` answers with the post, as Discord's message object;
//!   otherwise 204. `?thread_id=` posts in a thread.
//! - `GET`, `PATCH` and `DELETE` on `.../messages/{message_id}`: a post the
//!   webhook made, read, changed or taken back.
//! - `GET /api/webhooks/{id}/{token}`: the webhook itself.
//! - `POST .../slack` and `.../github`: Slack's incoming-webhook messages and
//!   GitHub's webhook events, made into the same post (webhook_formats.rs), as
//!   Discord's own endpoints of those names do.
//!
//! A webhook's post is the community's own (its DID, as AutoMod's alerts are),
//! named as the webhook or as the post asks, with `webhook` saying which, and
//! carries `embeds`, checked to Discord's limits. Webhooks are made and managed in
//! the `webhooks` workflow, by whoever holds `manage_webhooks` in the channel.

// (Each step answers a request that fails with the response that says why.)
#![allow(clippy::result_large_err)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use axum::body::Bytes;
use axum::extract::{FromRequest, Multipart, Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::community::{line, Community};
use crate::questions::{failed, Refusal};
use crate::server::AppState;
use crate::store::{Channel, Flow, Mention, Post, Webhook};

pub const WEBHOOKS_FLOW: &str = "webhooks";
/// How long what a webhook says is, as Discord's limits are.
const MAX_CONTENT: usize = 2000;
const MAX_USERNAME: usize = 80;
const MAX_EMBEDS: usize = 10;
const MAX_FIELDS: usize = 25;
const MAX_EMBED_TOTAL: usize = 6000;
const MAX_FILES: usize = 10;
const MAX_NAME: usize = 80;
/// As many posts as one webhook makes in [`RATE_WINDOW_MS`], as Discord allows.
const RATE_POSTS: usize = 5;
const RATE_WINDOW_MS: i64 = 2000;

fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// A webhook's URL, as whoever manages it copies it.
pub fn url_of(public_url: &str, hook: &Webhook) -> String {
    format!("{}/api/webhooks/{}/{}", public_url.trim_end_matches('/'), hook.id, hook.token)
}

/// An `http(s)` address, as an embed or avatar may give one; `attachment://name`
/// is one of the files sent with it, where `files` has it.
fn link(value: &Value, files: &HashMap<String, String>) -> Option<String> {
    let text = value.as_str()?.trim();
    if let Some(name) = text.strip_prefix("attachment://") {
        return files.get(name).cloned();
    }
    (text.len() <= 2048 && (text.starts_with("https://") || text.starts_with("http://")) && !text.chars().any(char::is_whitespace)).then(|| text.to_string())
}

/// Text up to `max` characters, as it is (line breaks kept), or none if empty.
fn text(value: &Value, max: usize) -> Option<String> {
    let text = value.as_str()?.trim();
    (!text.is_empty()).then(|| text.chars().take(max).collect())
}

/// Embeds as Discord's webhooks take them, checked: at most ten, each field to its
/// limit, 6000 characters in all. `files` are those sent with them, by name.
pub fn read_embeds(given: &Value, files: &HashMap<String, String>) -> Result<Vec<Value>, String> {
    let Some(list) = given.as_array() else { return Ok(Vec::new()) };
    if list.len() > MAX_EMBEDS {
        return Err("A message has at most 10 embeds.".into());
    }
    let mut total = 0;
    let mut embeds = Vec::new();
    for embed in list {
        let mut out = Map::new();
        let mut count = |value: &Option<String>| total += value.as_ref().map_or(0, |v| v.chars().count());
        let title = text(&embed["title"], 256);
        let description = text(&embed["description"], 4096);
        count(&title);
        count(&description);
        if let Some(title) = title {
            out.insert("title".into(), json!(title));
        }
        if let Some(description) = description {
            out.insert("description".into(), json!(description));
        }
        if let Some(url) = link(&embed["url"], files) {
            out.insert("url".into(), json!(url));
        }
        if let Some(color) = embed["color"].as_u64().filter(|c| *c <= 0xFF_FFFF) {
            out.insert("color".into(), json!(color));
        }
        if let Some(stamp) = embed["timestamp"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()) {
            out.insert("timestamp".into(), json!(stamp.to_rfc3339()));
        }
        if let Some(name) = text(&embed["author"]["name"], 256) {
            total += name.chars().count();
            let mut author = json!({ "name": name });
            if let Some(url) = link(&embed["author"]["url"], files) {
                author["url"] = json!(url);
            }
            if let Some(icon) = link(&embed["author"]["icon_url"], files) {
                author["icon_url"] = json!(icon);
            }
            out.insert("author".into(), author);
        }
        if let Some(footer) = text(&embed["footer"]["text"], 2048) {
            total += footer.chars().count();
            let mut said = json!({ "text": footer });
            if let Some(icon) = link(&embed["footer"]["icon_url"], files) {
                said["icon_url"] = json!(icon);
            }
            out.insert("footer".into(), said);
        }
        for picture in ["image", "thumbnail"] {
            if let Some(url) = link(&embed[picture]["url"], files) {
                out.insert(picture.into(), json!({ "url": url }));
            }
        }
        let fields = embed["fields"].as_array().map(Vec::as_slice).unwrap_or_default();
        if fields.len() > MAX_FIELDS {
            return Err("An embed has at most 25 fields.".into());
        }
        let mut kept = Vec::new();
        for field in fields {
            let (Some(name), Some(value)) = (text(&field["name"], 256), text(&field["value"], 1024)) else {
                return Err("Each embed field has a name and a value.".into());
            };
            total += name.chars().count() + value.chars().count();
            kept.push(json!({ "name": name, "value": value, "inline": field["inline"] == true }));
        }
        if !kept.is_empty() {
            out.insert("fields".into(), json!(kept));
        }
        if out.is_empty() {
            return Err("An embed needs something in it.".into());
        }
        embeds.push(Value::Object(out));
    }
    if total > MAX_EMBED_TOTAL {
        return Err("A message's embeds hold at most 6000 characters in all.".into());
    }
    Ok(embeds)
}

/// A Discord-style error: `{ message, code }` with its HTTP status.
fn problem(status: StatusCode, code: u32, message: &str) -> Response {
    (status, axum::Json(json!({ "message": message, "code": code }))).into_response()
}

fn unknown() -> Response {
    problem(StatusCode::NOT_FOUND, 10015, "Unknown Webhook")
}

/// Posts made by each webhook lately, for its rate limit, in milliseconds.
static RECENT: LazyLock<Mutex<HashMap<String, VecDeque<i64>>>> = LazyLock::new(Default::default);

/// Whether `id` may post now; if not, how long until it may, in seconds.
fn allowed_now(id: &str, now_ms: i64) -> Result<(), f64> {
    let mut recent = RECENT.lock().unwrap_or_else(|p| p.into_inner());
    let times = recent.entry(id.to_string()).or_default();
    while times.front().is_some_and(|t| now_ms - t >= RATE_WINDOW_MS) {
        times.pop_front();
    }
    if times.len() >= RATE_POSTS {
        let wait = RATE_WINDOW_MS - (now_ms - times.front().copied().unwrap_or(now_ms));
        return Err(wait.max(1) as f64 / 1000.0);
    }
    times.push_back(now_ms);
    Ok(())
}

/// The webhook `id`, if `token` is its own.
fn hook_for(community: &Community, id: &str, token: &str) -> Result<Webhook, Response> {
    let Some(hook) = community.store.webhook(id).ok().flatten() else { return Err(unknown()) };
    // (Compared in full whatever differs first: nothing is learned from how long it takes.)
    let same = hook.token.len() == token.len() && hook.token.bytes().zip(token.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
    if !same {
        return Err(problem(StatusCode::UNAUTHORIZED, 50027, "Invalid Webhook Token"));
    }
    Ok(hook)
}

/// What was sent: the JSON payload, and the files with it (`(field, name, type, bytes)`).
struct Sent {
    payload: Value,
    files: Vec<(String, String, String, Bytes)>,
}

async fn read_sent(state: &Arc<AppState>, request: Request) -> Result<Sent, Response> {
    let content_type = request.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    if content_type.starts_with("multipart/form-data") {
        let mut form = Multipart::from_request(request, state).await.map_err(|_| problem(StatusCode::BAD_REQUEST, 50109, "The request body is not a form."))?;
        let mut sent = Sent { payload: json!({}), files: Vec::new() };
        while let Some(field) = form.next_field().await.map_err(|_| problem(StatusCode::BAD_REQUEST, 50109, "The form couldn't be read."))? {
            let name = field.name().unwrap_or_default().to_string();
            if name == "payload_json" {
                let text = field.text().await.unwrap_or_default();
                sent.payload = serde_json::from_str(&text).map_err(|_| problem(StatusCode::BAD_REQUEST, 50109, "payload_json is not JSON."))?;
            } else if let Some(filename) = field.file_name().map(str::to_string) {
                let media_type = field.content_type().unwrap_or("application/octet-stream").to_string();
                let bytes = field.bytes().await.map_err(|_| problem(StatusCode::PAYLOAD_TOO_LARGE, 40005, "Request entity too large"))?;
                sent.files.push((name, filename, media_type, bytes));
            } else {
                // A form's plain fields, as Discord also takes them (content, username).
                let value = field.text().await.unwrap_or_default();
                sent.payload[name] = json!(value);
            }
        }
        if sent.files.len() > MAX_FILES {
            return Err(problem(StatusCode::BAD_REQUEST, 50035, "A message carries at most 10 files."));
        }
        return Ok(sent);
    }
    let bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024).await.map_err(|_| problem(StatusCode::PAYLOAD_TOO_LARGE, 40005, "Request entity too large"))?;
    let payload: Value = serde_json::from_slice(&bytes).map_err(|_| problem(StatusCode::BAD_REQUEST, 50109, "The request body is not JSON."))?;
    if !payload.is_object() {
        return Err(problem(StatusCode::BAD_REQUEST, 50109, "The request body is not a JSON object."));
    }
    Ok(Sent { payload, files: Vec::new() })
}

/// Keeps what was sent as files; the attachments that name them, and each name's
/// address (for `attachment://` in embeds).
fn keep_files(community: &Community, hook: &Webhook, sent: &Sent, now: i64) -> Result<(Vec<Value>, HashMap<String, String>), Response> {
    let mut attachments = Vec::new();
    let mut by_name = HashMap::new();
    if sent.files.is_empty() {
        return Ok((attachments, by_name));
    }
    let Some(files) = &community.files else {
        return Err(problem(StatusCode::BAD_REQUEST, 50035, "This community keeps no files."));
    };
    // Nothing more where the community has used all its host gives it (WYV-20).
    let room = community.config.max_store_bytes;
    let adding: u64 = sent.files.iter().map(|(_, _, _, bytes)| bytes.len() as u64).sum();
    if room > 0 && community.used_bytes.load(std::sync::atomic::Ordering::Relaxed) + adding > room {
        return Err(problem(StatusCode::PAYLOAD_TOO_LARGE, 40005, "This community has used all the room its host gives it."));
    }
    // What each was said to be: `attachments: [{ id, description }]`, `id` the n of `files[n]`.
    let described: Vec<&Value> = sent.payload["attachments"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    for (field, name, media_type, bytes) in &sent.files {
        let filename: String = name.replace(['/', '\\'], "_").chars().take(255).collect();
        let (id, url, hash) = files.keep_bytes(&format!("webhook:{}", hook.id), &filename, media_type, bytes, now).map_err(|_| problem(StatusCode::PAYLOAD_TOO_LARGE, 40005, "Request entity too large"))?;
        let n = field.trim_start_matches("files[").trim_end_matches(']');
        let description = described.iter().find(|d| d["id"].to_string().trim_matches('"') == n).and_then(|d| text(&d["description"], 1000));
        let mut attachment = json!({ "id": id, "media_type": media_type, "filename": filename, "byte_count": bytes.len(), "data": { "links": [url.clone()], "hash": hash } });
        if let Some(description) = description {
            attachment["description"] = json!(description);
        }
        by_name.insert(filename, url);
        attachments.push(attachment);
    }
    Ok((attachments, by_name))
}

/// A post of a webhook's, as Discord says a message.
fn as_discord(post: &Post, hook: &Webhook) -> Value {
    let extra = post.extra.clone().unwrap_or_default();
    let stamp = |secs: i64| chrono::DateTime::from_timestamp(secs, 0).map(|t| t.to_rfc3339()).unwrap_or_default();
    let attachments: Vec<Value> = extra["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| json!({ "id": a["id"], "filename": a["filename"], "size": a["byte_count"], "url": a["data"]["links"][0], "proxy_url": a["data"]["links"][0], "content_type": a["media_type"] }))
        .collect();
    json!({
        "id": post.id,
        "type": 0,
        "channel_id": post.thread.clone().unwrap_or_else(|| post.channel.clone()),
        "webhook_id": hook.id,
        "author": { "id": hook.id, "username": extra["webhook"]["name"].as_str().unwrap_or(&hook.name), "avatar": Value::Null, "bot": true, "discriminator": "0000" },
        "content": post.content,
        "embeds": extra.get("embeds").cloned().unwrap_or_else(|| json!([])),
        "attachments": attachments,
        "mentions": [],
        "mention_roles": [],
        "mention_everyone": false,
        "pinned": post.pinned,
        "tts": false,
        "flags": 0,
        "timestamp": stamp(post.posted_at),
        "edited_timestamp": post.edited_at.map(stamp),
    })
}

#[derive(Deserialize, Default)]
pub struct ExecuteQuery {
    wait: Option<String>,
    thread_id: Option<String>,
}

/// The webhook a request is for, if its token is right and it isn't posting too fast.
fn hook_now(community: &Community, id: &str, token: &str) -> Result<Webhook, Response> {
    let hook = hook_for(community, id, token)?;
    if let Err(wait) = allowed_now(&hook.id, chrono::Utc::now().timestamp_millis()) {
        return Err((StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({ "message": "You are being rate limited.", "retry_after": wait, "global": false }))).into_response());
    }
    Ok(hook)
}

/// `POST /api/webhooks/{id}/{token}`: a post, from whatever holds the URL.
pub async fn execute(State(state): State<Arc<AppState>>, Path((id, token)): Path<(String, String)>, Query(query): Query<ExecuteQuery>, request: Request) -> Response {
    let hook = match hook_now(&state.community, &id, &token) {
        Ok(hook) => hook,
        Err(response) => return response,
    };
    let sent = match read_sent(&state, request).await {
        Ok(sent) => sent,
        Err(response) => return response,
    };
    post_sent(&state, &hook, sent, &query)
}

/// A body that is JSON, or a form whose `payload` field is (as Slack's and
/// GitHub's may be sent).
async fn json_or_payload_form(request: Request) -> Result<Value, Response> {
    let form = request.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("application/x-www-form-urlencoded"));
    let bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024).await.map_err(|_| problem(StatusCode::PAYLOAD_TOO_LARGE, 40005, "Request entity too large"))?;
    let text = if form {
        url::form_urlencoded::parse(&bytes).find(|(name, _)| name == "payload").map(|(_, value)| value.into_owned()).unwrap_or_default().into_bytes()
    } else {
        bytes.to_vec()
    };
    let body: Value = serde_json::from_slice(&text).map_err(|_| problem(StatusCode::BAD_REQUEST, 50109, "The request body is not JSON."))?;
    if !body.is_object() {
        return Err(problem(StatusCode::BAD_REQUEST, 50109, "The request body is not a JSON object."));
    }
    Ok(body)
}

/// `POST /api/webhooks/{id}/{token}/slack`: a Slack incoming-webhook message,
/// posted as Discord's `/slack` does (webhook_formats.rs). Answers `ok`, as Slack.
pub async fn execute_slack(State(state): State<Arc<AppState>>, Path((id, token)): Path<(String, String)>, Query(query): Query<ExecuteQuery>, request: Request) -> Response {
    let hook = match hook_now(&state.community, &id, &token) {
        Ok(hook) => hook,
        Err(response) => return response,
    };
    let body = match json_or_payload_form(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let sent = Sent { payload: crate::webhook_formats::from_slack(&body), files: Vec::new() };
    let response = post_sent(&state, &hook, sent, &ExecuteQuery { wait: None, thread_id: query.thread_id });
    if response.status().is_success() {
        return (StatusCode::OK, "ok").into_response();
    }
    response
}

/// `POST /api/webhooks/{id}/{token}/github`: one of GitHub's webhook events (its
/// `X-GitHub-Event`), posted as Discord's `/github` does (webhook_formats.rs). An
/// event nothing is shown for (a `ping`, say) is answered 204 and posts nothing.
pub async fn execute_github(State(state): State<Arc<AppState>>, Path((id, token)): Path<(String, String)>, Query(query): Query<ExecuteQuery>, request: Request) -> Response {
    let hook = match hook_now(&state.community, &id, &token) {
        Ok(hook) => hook,
        Err(response) => return response,
    };
    let event = request.headers().get("x-github-event").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let body = match json_or_payload_form(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let Some(payload) = crate::webhook_formats::from_github(&event, &body) else {
        return StatusCode::NO_CONTENT.into_response();
    };
    post_sent(&state, &hook, Sent { payload, files: Vec::new() }, &query)
}

/// Posts what `hook` was sent: checked, kept and posted as any webhook's post.
fn post_sent(state: &Arc<AppState>, hook: &Webhook, sent: Sent, query: &ExecuteQuery) -> Response {
    let community = &state.community;
    let hook = hook.clone();
    let now = crate::gates::now();
    let Some(channel) = community.store.channel(&hook.channel).ok().flatten().filter(|c| !c.closed && c.voice.is_none()) else {
        return problem(StatusCode::NOT_FOUND, 10003, "Unknown Channel");
    };
    // A channel of a kind its host turned off takes nothing new.
    if community.channel_off(&channel) {
        return problem(StatusCode::FORBIDDEN, 50013, "Missing Permissions");
    }
    // Out of the room its host gives it: nothing new, from a webhook either (WYV-20).
    let room = community.config.max_store_bytes;
    if room > 0 && community.used_bytes.load(std::sync::atomic::Ordering::Relaxed) >= room {
        return problem(StatusCode::FORBIDDEN, 50001, "This community has used all the room its host gives it.");
    }
    if community.moving().unwrap_or(false) {
        return problem(StatusCode::FORBIDDEN, 50001, "This community is moving, and takes no posts until it has.");
    }
    let content: String = sent.payload["content"].as_str().unwrap_or_default().trim().chars().collect();
    if content.chars().count() > MAX_CONTENT {
        return problem(StatusCode::BAD_REQUEST, 50035, "Must be 2000 or fewer in length.");
    }
    let (attachments, by_name) = match keep_files(community, &hook, &sent, now) {
        Ok(kept) => kept,
        Err(response) => return response,
    };
    let embeds = match read_embeds(&sent.payload["embeds"], &by_name) {
        Ok(embeds) => embeds,
        Err(why) => return problem(StatusCode::BAD_REQUEST, 50035, &why),
    };
    if content.is_empty() && embeds.is_empty() && attachments.is_empty() {
        return problem(StatusCode::BAD_REQUEST, 50006, "Cannot send an empty message");
    }
    let (content, mentions) = mentions_in(community, &content, &sent.payload);
    let (content, alerted) = match automod(community, &channel, &hook, &content, mentions.len(), now) {
        Ok(checked) => checked,
        Err(response) => return response,
    };
    if alerted {
        state.deliver.notify_one();
    }
    let posted = match post(community, &hook, &channel, &content, &mentions, embeds, attachments, &sent.payload, query.thread_id.as_deref(), now) {
        Ok(posted) => posted,
        Err(response) => return response,
    };
    state.deliver.notify_one();
    for changed in posted.1 {
        crate::server::send_changed_later(state, changed);
    }
    if query.wait.as_deref() == Some("true") {
        return axum::Json(as_discord(&posted.0, &hook)).into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

/// What a webhook's text calls on, as Discord's do: `<@member>` (a member id here,
/// where Discord has a user id), `<@&role>`, `@everyone` and `@here` (this
/// community's everyone), as far as `allowed_mentions` lets it -- everything, where
/// it says nothing, as on Discord. The text says each by name, whether or not it
/// calls on them: one not let through is shown, and notifies nobody.
pub fn mentions_in(community: &Community, content: &str, payload: &Value) -> (String, Vec<Mention>) {
    static TOKEN: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"<@(!|&)?([A-Za-z0-9_-]{1,64})>|@(everyone|here)\b").unwrap());
    let allowed = &payload["allowed_mentions"];
    let said = allowed.is_object();
    let parse: Vec<&str> = allowed["parse"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let listed = |field: &str, id: &str| allowed[field].as_array().is_some_and(|ids| ids.iter().any(|one| one.as_str() == Some(id)));
    let may = |kind: &str, field: &str, id: &str| !said || parse.contains(&kind) || listed(field, id);
    let mut mentions: Vec<Mention> = Vec::new();
    let text = TOKEN.replace_all(content, |found: &regex::Captures| {
        let whole = found[0].to_string();
        if found.get(3).is_some() {
            if may("everyone", "", "") && !mentions.iter().any(|m| m.who == crate::community::EVERYONE) {
                mentions.push(Mention { who: crate::community::EVERYONE.into(), name: crate::community::EVERYONE.into() });
            }
            return "@everyone".to_string();
        }
        let id = &found[2];
        if found.get(1).map(|m| m.as_str()) == Some("&") {
            let Some(role) = community.store.role(id).ok().flatten().filter(|role| role.id != crate::roles::EVERYONE_ROLE) else { return whole };
            let who = format!("role:{}", role.id);
            if may("roles", "roles", id) && !mentions.iter().any(|m| m.who == who) {
                mentions.push(Mention { who, name: role.name.clone() });
            }
            return format!("@{}", role.name);
        }
        let Some(did) = community.store.did_of_member(id).ok().flatten() else { return whole };
        let Some(member) = community.store.member(&did).ok().flatten() else { return whole };
        let name = member.name.clone().unwrap_or_else(|| "someone".into());
        if may("users", "users", id) && !mentions.iter().any(|m| m.who == did) {
            mentions.push(Mention { who: did, name: name.clone() });
        }
        format!("@{name}")
    });
    mentions.truncate(crate::community::MAX_MENTIONS);
    (text.into_owned(), mentions)
}

/// What AutoMod makes of a webhook's post: what it says now (censored, maybe),
/// whether anything was posted besides (an alert), or why it is refused.
fn automod(community: &Community, channel: &Channel, hook: &Webhook, content: &str, mentions: usize, now: i64) -> Result<(String, bool), Response> {
    fn failed<E>(_: E) -> Response {
        problem(StatusCode::INTERNAL_SERVER_ERROR, 0, "500: Internal Server Error")
    }
    let Some(own) = community.own.clone() else { return Ok((content.to_string(), false)) };
    let Some(mut author) = community.store.member(&own).map_err(failed)? else { return Ok((content.to_string(), false)) };
    author.name = Some(hook.name.clone());
    let message = json!({ "type": "webhook", "id": uuid::Uuid::new_v4().to_string(), "body": { "channel": channel.id, "content": content } });
    match community.automod_webhook(&author, channel, &message, content, mentions, now).map_err(failed)? {
        crate::automod::Verdict::Placed { noted, censored } => Ok((censored.map(|c| c.content).unwrap_or_else(|| content.to_string()), noted.is_some_and(|n| n.posted))),
        // Discord's own word for it.
        crate::automod::Verdict::Stopped(_) => Err(problem(StatusCode::BAD_REQUEST, 200000, "Message was blocked by automatic moderation")),
    }
}

/// Makes the post: it, and the posts whose threads changed with it.
#[allow(clippy::too_many_arguments)]
fn post(community: &Community, hook: &Webhook, channel: &Channel, content: &str, mentions: &[Mention], embeds: Vec<Value>, attachments: Vec<Value>, payload: &Value, thread_id: Option<&str>, now: i64) -> Result<(Post, Vec<String>), Response> {
    let failed = |_| problem(StatusCode::INTERNAL_SERVER_ERROR, 0, "500: Internal Server Error");
    let Some(own) = community.own.clone() else { return Err(problem(StatusCode::SERVICE_UNAVAILABLE, 0, "This server posts as nobody.")) };
    community.store.admit(&own, crate::store::Role::Member, now).map_err(failed)?;
    let Some(mut author) = community.store.member(&own).map_err(failed)? else { return Err(problem(StatusCode::INTERNAL_SERVER_ERROR, 0, "500: Internal Server Error")) };
    let name = line(&payload["username"], MAX_USERNAME).unwrap_or_else(|| hook.name.clone());
    author.name = Some(name.clone());
    let thread = match thread_id {
        Some(id) => match community.store.thread(id).map_err(failed)?.filter(|t| t.channel == channel.id) {
            Some(thread) => Some(thread.id),
            None => return Err(problem(StatusCode::BAD_REQUEST, 10003, "Unknown Channel")),
        },
        None => None,
    };
    // A forum's post begins a thread, named as `thread_name` says.
    let title = if channel.forum.is_some() && thread.is_none() {
        match line(&payload["thread_name"], 100) {
            Some(title) => Some(title),
            None => return Err(problem(StatusCode::BAD_REQUEST, 220001, "Webhooks posted to forum channels must have a thread_name or thread_id")),
        }
    } else {
        None
    };
    let mut webhook = json!({ "id": hook.id, "name": name });
    if let Some(avatar) = link(&payload["avatar_url"], &HashMap::new()).or_else(|| hook.avatar.clone()) {
        webhook["avatar_url"] = json!(avatar);
    }
    let mut extra = json!({ "webhook": webhook });
    if !embeds.is_empty() {
        extra["embeds"] = json!(embeds);
    }
    if !attachments.is_empty() {
        extra["media"] = json!(attachments.iter().map(|a| json!({ "attachment": a["id"] })).collect::<Vec<_>>());
        extra["attachments"] = json!(attachments);
    }
    if let Some(title) = &title {
        extra["title"] = json!(title);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let added = community.store.add_post(&channel.id, &id, content, &author, None, None, now).map_err(failed)?;
    community.store.set_written_with(&added.id, None, thread.as_deref(), mentions).map_err(failed)?;
    community.store.set_post_extra(&added.id, &extra).map_err(failed)?;
    let mut changed = Vec::new();
    if let Some(title) = &title {
        community.store.add_thread(&added.id, &channel.id, Some(title), &own, now).map_err(failed)?;
        changed.push(added.id.clone());
    }
    if let Some(thread) = &thread {
        community.store.touch_thread(thread).map_err(failed)?;
        changed.push(thread.clone());
    }
    let post = community.store.post_by_id(&added.id).map_err(failed)?.ok_or_else(|| problem(StatusCode::INTERNAL_SERVER_ERROR, 0, "500: Internal Server Error"))?;
    Ok((post, changed))
}

/// The webhook's own post `message`, if it is one.
fn its_post(community: &Community, hook: &Webhook, message: &str) -> Result<Post, Response> {
    match community.store.post_by_id(message).ok().flatten() {
        Some(post) if !post.deleted && post.extra.as_ref().is_some_and(|e| e["webhook"]["id"] == hook.id.as_str()) => Ok(post),
        _ => Err(problem(StatusCode::NOT_FOUND, 10008, "Unknown Message")),
    }
}

/// `GET /api/webhooks/{id}/{token}`: the webhook, as Discord says one.
pub async fn describe(State(state): State<Arc<AppState>>, Path((id, token)): Path<(String, String)>) -> Response {
    match hook_for(&state.community, &id, &token) {
        Ok(hook) => axum::Json(json!({ "id": hook.id, "type": 1, "channel_id": hook.channel, "name": hook.name, "avatar": Value::Null, "token": hook.token, "application_id": Value::Null })).into_response(),
        Err(response) => response,
    }
}

/// `GET .../messages/{message_id}`.
pub async fn read_message(State(state): State<Arc<AppState>>, Path((id, token, message)): Path<(String, String, String)>) -> Response {
    let community = &state.community;
    let hook = match hook_for(community, &id, &token) {
        Ok(hook) => hook,
        Err(response) => return response,
    };
    match its_post(community, &hook, &message) {
        Ok(post) => axum::Json(as_discord(&post, &hook)).into_response(),
        Err(response) => response,
    }
}

/// `PATCH .../messages/{message_id}`: its content, embeds, and which files it
/// keeps (`attachments`, by id) and adds.
pub async fn edit_message(State(state): State<Arc<AppState>>, Path((id, token, message)): Path<(String, String, String)>, request: Request) -> Response {
    let community = &state.community;
    let hook = match hook_for(community, &id, &token) {
        Ok(hook) => hook,
        Err(response) => return response,
    };
    let post = match its_post(community, &hook, &message) {
        Ok(post) => post,
        Err(response) => return response,
    };
    let now = crate::gates::now();
    let sent = match read_sent(&state, request).await {
        Ok(sent) => sent,
        Err(response) => return response,
    };
    let mut extra = post.extra.clone().unwrap_or_else(|| json!({}));
    let content = match sent.payload.get("content") {
        Some(Value::Null) => String::new(),
        Some(given) => given.as_str().unwrap_or_default().trim().to_string(),
        None => post.content.clone(),
    };
    if content.chars().count() > MAX_CONTENT {
        return problem(StatusCode::BAD_REQUEST, 50035, "Must be 2000 or fewer in length.");
    }
    // What it calls on, and AutoMod, as for a new post (only when the text changes).
    let (content, mentions) = if sent.payload.get("content").is_some() { mentions_in(community, &content, &sent.payload) } else { (content, Vec::new()) };
    let content = if sent.payload.get("content").is_some() {
        let Some(channel) = community.store.channel(&post.channel).ok().flatten() else { return problem(StatusCode::NOT_FOUND, 10003, "Unknown Channel") };
        match automod(community, &channel, &hook, &content, mentions.len(), now) {
            Ok((content, alerted)) => {
                if alerted {
                    state.deliver.notify_one();
                }
                content
            }
            Err(response) => return response,
        }
    } else {
        content
    };
    let (added, by_name) = match keep_files(community, &hook, &sent, now) {
        Ok(kept) => kept,
        Err(response) => return response,
    };
    if sent.payload.get("embeds").is_some() {
        match read_embeds(&sent.payload["embeds"], &by_name) {
            Ok(embeds) if embeds.is_empty() => {
                extra.as_object_mut().map(|e| e.remove("embeds"));
            }
            Ok(embeds) => extra["embeds"] = json!(embeds),
            Err(why) => return problem(StatusCode::BAD_REQUEST, 50035, &why),
        }
    }
    // The files kept: those `attachments` names (all, where it says nothing), and those sent now.
    let mut attachments: Vec<Value> = extra["attachments"].as_array().cloned().unwrap_or_default();
    if let Some(kept) = sent.payload["attachments"].as_array() {
        let ids: Vec<String> = kept.iter().map(|a| a["id"].to_string().trim_matches('"').to_string()).collect();
        attachments.retain(|a| ids.contains(&a["id"].as_str().unwrap_or_default().to_string()));
    }
    attachments.extend(added);
    if attachments.is_empty() {
        extra.as_object_mut().map(|e| {
            e.remove("attachments");
            e.remove("media")
        });
    } else {
        extra["media"] = json!(attachments.iter().map(|a| json!({ "attachment": a["id"] })).collect::<Vec<_>>());
        extra["attachments"] = json!(attachments);
    }
    if content.is_empty() && extra.get("embeds").is_none() && extra.get("attachments").is_none() {
        return problem(StatusCode::BAD_REQUEST, 50006, "Cannot send an empty message");
    }
    let saved = community.store.edit_post(&post.id, &content, &mentions, now).and_then(|_| community.store.set_post_extra(&post.id, &extra));
    if saved.is_err() {
        return problem(StatusCode::INTERNAL_SERVER_ERROR, 0, "500: Internal Server Error");
    }
    crate::server::send_changed_later(&state, post.id.clone());
    match community.store.post_by_id(&post.id).ok().flatten() {
        Some(post) => axum::Json(as_discord(&post, &hook)).into_response(),
        None => problem(StatusCode::NOT_FOUND, 10008, "Unknown Message"),
    }
}

/// `DELETE .../messages/{message_id}`: taken back.
pub async fn delete_message(State(state): State<Arc<AppState>>, Path((id, token, message)): Path<(String, String, String)>) -> Response {
    let community = &state.community;
    let hook = match hook_for(community, &id, &token) {
        Ok(hook) => hook,
        Err(response) => return response,
    };
    let post = match its_post(community, &hook, &message) {
        Ok(post) => post,
        Err(response) => return response,
    };
    if community.store.delete_post(&post.id, None, community.keeps_deleted_text()).is_err() {
        return problem(StatusCode::INTERNAL_SERVER_ERROR, 0, "500: Internal Server Error");
    }
    crate::server::send_changed_later(&state, post.id);
    StatusCode::NO_CONTENT.into_response()
}

impl Community {
    /// What a receiver's form says: where, which events, of which channel (none:
    /// every one, for whoever manages webhooks everywhere).
    fn read_receiver(&self, flow: &Flow, input: &Value, channels: &[Channel]) -> Result<(String, Vec<String>, Option<String>), Refusal> {
        let url = crate::outgoing::check_url(input["url"].as_str().unwrap_or_default(), self.config.outgoing_private).map_err(|why| Refusal::new("guard_failed", &why))?.to_string();
        let events: Vec<String> = crate::outgoing::EVENTS.iter().filter(|(event, _)| input[format!("event-{event}")] == true).map(|(event, _)| event.to_string()).collect();
        if events.is_empty() {
            return Err(Refusal::new("guard_failed", "Choose at least one event."));
        }
        let channel = match input["channel"].as_str().filter(|c| !c.is_empty()) {
            Some(id) if channels.iter().any(|c| c.id == id) => Some(id.to_string()),
            Some(_) => return Err(Refusal::new("guard_failed", "Choose a channel you manage webhooks in.")),
            None if self.powers(&flow.owner).map_err(failed)?.has("manage_webhooks") => None,
            None => return Err(Refusal::new("guard_failed", "Events of every channel take Manage webhooks everywhere; choose a channel.")),
        };
        Ok((url, events, channel))
    }

    fn editable_receiver(&self, id: &str, channels: &[Channel]) -> Result<crate::store::EventHook, Refusal> {
        self.store
            .event_hook(id)
            .map_err(failed)?
            .filter(|h| h.channel.as_deref().is_none_or(|c| channels.iter().any(|ch| ch.id == c)))
            .ok_or_else(|| Refusal::new("instance_not_found", "That receiver is gone."))
    }

    /// The channels `did` manages webhooks in: text channels and forums they see.
    fn webhook_channels(&self, did: &str) -> anyhow::Result<Vec<Channel>> {
        let powers = self.powers(did)?;
        let mut channels = Vec::new();
        for channel in self.store.channels()?.into_iter().filter(|c| c.voice.is_none() && !c.closed) {
            if self.sees(did, &powers, &channel)? && self.channel_permissions(did, &powers, &channel)?.contains("manage_webhooks") {
                channels.push(channel);
            }
        }
        Ok(channels)
    }

    /// Whether `did` manages webhooks anywhere: who may run the workflow.
    pub fn may_manage_webhooks(&self, did: &str) -> anyhow::Result<bool> {
        Ok(!self.webhook_channels(did)?.is_empty())
    }

    pub fn webhooks_ui(&self, flow: &Flow) -> anyhow::Result<(Value, Vec<&'static str>)> {
        let channels = self.webhook_channels(&flow.owner)?;
        let named = |id: &str| channels.iter().find(|c| c.id == id).map(|c| format!("#{}", c.name));
        let options: Vec<Value> = channels.iter().map(|c| json!({ "value": c.id, "label": format!("#{}", c.name) })).collect();
        // Events of every channel only for whoever manages webhooks everywhere.
        let everywhere = self.powers(&flow.owner)?.has("manage_webhooks");
        let mut out_options: Vec<Value> = if everywhere { vec![json!({ "value": "", "label": "Every channel" })] } else { Vec::new() };
        out_options.extend(options.iter().cloned());
        let mut items = vec![json!({ "type": "title", "text": "Webhooks" })];
        if let Some(note) = flow.data["note"].as_str() {
            items.push(json!({ "type": "pass", "text": note }));
        }
        if flow.state == "edit_out" {
            let id = flow.data["event_hook"].as_str().unwrap_or_default();
            let Some(hook) = self.store.event_hook(id)?.filter(|h| h.channel.as_deref().is_none_or(|c| named(c).is_some())) else {
                return Ok((json!([{ "type": "text", "text": "That receiver is gone." }, { "type": "submit-button", "label": "Back", "event": "back", "input": {} }]), vec!["back"]));
            };
            if hook.off_at.is_some() {
                items.push(json!({ "type": "fail", "text": "Turned off: it stopped taking events. Turn it back on once it takes them again." }));
            }
            items.push(json!({ "type": "text", "text": "Each event is a POST of JSON, signed with this receiver's secret (the Wyvrn-Signature header). The secret is shown once, when it is made." }));
            let mut fields = vec![json!({ "type": "text-input", "form-id": "url", "label": "Where they go (https://)", "value": hook.url, "required": true })];
            for (event, label) in crate::outgoing::EVENTS {
                fields.push(json!({ "type": "check-box", "form-id": format!("event-{event}"), "label": label, "value": hook.events.iter().any(|e| e == event) }));
            }
            fields.push(json!({ "type": "drop-down", "form-id": "channel", "label": "Of", "value": hook.channel.clone().unwrap_or_default(), "options": out_options.clone() }));
            fields.push(json!({ "type": "submit-button", "label": "Save", "event": "save_out", "input": {} }));
            items.push(json!({ "type": "form", "children": fields }));
            let mut row = vec![json!({ "type": "submit-button", "label": "Send a test event", "event": "ping_out", "input": {} })];
            if hook.off_at.is_some() {
                row.push(json!({ "type": "submit-button", "label": "Turn back on", "event": "on_out", "input": {} }));
            }
            row.push(json!({ "type": "submit-button", "label": "New secret", "event": "secret_out", "input": {} }));
            row.push(json!({ "type": "submit-button", "label": "Delete", "event": "delete_out", "input": {} }));
            row.push(json!({ "type": "submit-button", "label": "Back", "event": "back", "input": {} }));
            items.push(json!({ "type": "row", "children": row }));
            return Ok((Value::Array(items), vec!["save_out", "ping_out", "on_out", "secret_out", "delete_out", "back"]));
        }
        if flow.state == "edit" {
            let id = flow.data["webhook"].as_str().unwrap_or_default();
            let Some(hook) = self.store.webhook(id)?.filter(|h| named(&h.channel).is_some()) else {
                return Ok((json!([{ "type": "text", "text": "That webhook is gone." }, { "type": "submit-button", "label": "Back", "event": "back", "input": {} }]), vec!["back"]));
            };
            items.push(json!({ "type": "text", "text": "Anything that posts to a Discord webhook posts here, given this URL instead. Keep it secret: whoever has it can post in the channel." }));
            items.push(json!({ "type": "form", "children": [
                { "type": "text-input", "form-id": "url", "label": "Its URL (copy it)", "value": url_of(&self.public_url, &hook) },
                { "type": "text-input", "form-id": "name", "label": "Name", "value": hook.name, "required": true },
                { "type": "text-input", "form-id": "avatar", "label": "Picture (an https:// address, or empty)", "value": hook.avatar.clone().unwrap_or_default() },
                { "type": "drop-down", "form-id": "channel", "label": "Posts in", "value": hook.channel, "options": options },
                { "type": "submit-button", "label": "Save", "event": "save", "input": {} }
            ] }));
            items.push(json!({ "type": "row", "children": [
                { "type": "submit-button", "label": "New URL (the old one stops working)", "event": "new_url", "input": {} },
                { "type": "submit-button", "label": "Delete", "event": "delete", "input": {} },
                { "type": "submit-button", "label": "Back", "event": "back", "input": {} }
            ] }));
            return Ok((Value::Array(items), vec!["save", "new_url", "delete", "back"]));
        }
        items.push(json!({ "type": "text", "text": "A webhook posts into a channel from another service (a build's results, an alert, a feed), as Discord's do: give that service its URL." }));
        let hooks: Vec<Webhook> = self.store.webhooks()?.into_iter().filter(|h| named(&h.channel).is_some()).collect();
        if hooks.is_empty() {
            items.push(json!({ "type": "text", "text": "None yet." }));
        }
        for hook in &hooks {
            items.push(json!({ "type": "row", "children": [
                { "type": "text", "text": format!("{} → {}", hook.name, named(&hook.channel).unwrap_or_default()) },
                { "type": "submit-button", "label": "Edit", "event": "edit", "input": { "webhook": hook.id } }
            ] }));
        }
        items.push(json!({ "type": "divider" }));
        items.push(json!({ "type": "form", "children": [
            { "type": "text-input", "form-id": "name", "label": "New webhook's name", "placeholder": "Build results", "required": true },
            { "type": "drop-down", "form-id": "channel", "label": "Posts in", "value": channels.first().map(|c| c.id.clone()).unwrap_or_default(), "options": options },
            { "type": "submit-button", "label": "Make it", "event": "create", "input": {} }
        ] }));
        // Out: events sent, as they happen, to another service (outgoing.rs).
        items.push(json!({ "type": "divider" }));
        items.push(json!({ "type": "title", "text": "Sending events out" }));
        items.push(json!({ "type": "text", "text": "Tell another service what happens here as it happens: new posts, posts changed, members coming and going, sent to its https:// address as signed JSON." }));
        let receivers: Vec<_> = self.store.event_hooks()?.into_iter().filter(|h| h.channel.as_deref().is_none_or(|c| named(c).is_some())).collect();
        for hook in &receivers {
            let of = hook.channel.as_deref().and_then(named).unwrap_or_else(|| "every channel".into());
            let off = if hook.off_at.is_some() { " (turned off)" } else { "" };
            items.push(json!({ "type": "row", "children": [
                { "type": "text", "text": format!("{} · {} · {}{off}", hook.url, of, hook.events.join(", ")) },
                { "type": "submit-button", "label": "Edit", "event": "edit_out", "input": { "event_hook": hook.id } }
            ] }));
        }
        let mut fields = vec![json!({ "type": "text-input", "form-id": "url", "label": "Where they go", "placeholder": "https://example.com/wyvrn-events", "required": true })];
        for (event, label) in crate::outgoing::EVENTS {
            fields.push(json!({ "type": "check-box", "form-id": format!("event-{event}"), "label": label, "value": *event == "post.created" }));
        }
        fields.push(json!({ "type": "drop-down", "form-id": "channel", "label": "Of", "value": out_options.first().map(|o| o["value"].clone()).unwrap_or_default(), "options": out_options }));
        fields.push(json!({ "type": "submit-button", "label": "Send events there", "event": "create_out", "input": {} }));
        items.push(json!({ "type": "form", "children": fields }));
        items.push(json!({ "type": "submit-button", "label": "Done", "event": "done", "input": {} }));
        Ok((Value::Array(items), vec!["edit", "create", "edit_out", "create_out", "done"]))
    }

    pub fn advance_webhooks(&self, flow: &mut Flow, event: &str, input: &Value, now: i64) -> Result<bool, Refusal> {
        let channels = self.webhook_channels(&flow.owner).map_err(failed)?;
        let channel_of = |given: &Value| -> Result<String, Refusal> {
            given.as_str().filter(|id| channels.iter().any(|c| c.id == *id)).map(str::to_string).ok_or_else(|| Refusal::new("guard_failed", "Choose a channel you manage webhooks in."))
        };
        let editable = |id: &str| -> Result<Webhook, Refusal> {
            self.store.webhook(id).map_err(failed)?.filter(|h| channels.iter().any(|c| c.id == h.channel)).ok_or_else(|| Refusal::new("instance_not_found", "That webhook is gone."))
        };
        match (flow.state.as_str(), event) {
            ("list", "create") => {
                let name = line(&input["name"], MAX_NAME).ok_or_else(|| Refusal::new("guard_failed", "A webhook needs a name."))?;
                let channel = channel_of(&input["channel"])?;
                let hook = Webhook { id: uuid::Uuid::new_v4().simple().to_string()[..16].to_string(), channel, name: name.clone(), avatar: None, token: new_token(), created_by: flow.owner.clone(), created_at: now };
                self.store.save_webhook(&hook).map_err(failed)?;
                self.audit(now, &flow.owner, "webhook.create", Some(&hook.channel), Some(json!({ "name": name })));
                flow.data["webhook"] = json!(hook.id);
                flow.data["note"] = json!(format!("Made {name}. Copy its URL into the service that is to post."));
                flow.state = "edit".into();
                Ok(false)
            }
            ("list", "edit") => {
                let hook = editable(input["webhook"].as_str().unwrap_or_default())?;
                flow.data["webhook"] = json!(hook.id);
                flow.state = "edit".into();
                Ok(false)
            }
            ("list", "done") => {
                flow.status = "completed".into();
                Ok(false)
            }
            ("edit", "save") => {
                let mut hook = editable(flow.data["webhook"].as_str().unwrap_or_default())?;
                hook.name = line(&input["name"], MAX_NAME).ok_or_else(|| Refusal::new("guard_failed", "A webhook needs a name."))?;
                hook.channel = channel_of(&input["channel"])?;
                hook.avatar = match input["avatar"].as_str().map(str::trim).filter(|a| !a.is_empty()) {
                    None => None,
                    Some(given) => Some(link(&json!(given), &HashMap::new()).ok_or_else(|| Refusal::new("guard_failed", "A picture is an https:// address."))?),
                };
                self.store.save_webhook(&hook).map_err(failed)?;
                self.audit(now, &flow.owner, "webhook.update", Some(&hook.channel), Some(json!({ "name": hook.name })));
                flow.data["note"] = json!(format!("Saved {}.", hook.name));
                Ok(false)
            }
            ("edit", "new_url") => {
                let mut hook = editable(flow.data["webhook"].as_str().unwrap_or_default())?;
                hook.token = new_token();
                self.store.save_webhook(&hook).map_err(failed)?;
                self.audit(now, &flow.owner, "webhook.update", Some(&hook.channel), Some(json!({ "name": hook.name, "new_url": true })));
                flow.data["note"] = json!("A new URL: the old one no longer posts.");
                Ok(false)
            }
            ("edit", "delete") => {
                let hook = editable(flow.data["webhook"].as_str().unwrap_or_default())?;
                self.store.delete_webhook(&hook.id).map_err(failed)?;
                self.audit(now, &flow.owner, "webhook.delete", Some(&hook.channel), Some(json!({ "name": hook.name })));
                flow.data["note"] = json!(format!("Deleted {}.", hook.name));
                flow.state = "list".into();
                Ok(false)
            }
            ("edit", "back") | ("edit_out", "back") => {
                flow.state = "list".into();
                Ok(false)
            }
            ("list", "create_out") => {
                let (url, events, channel) = self.read_receiver(flow, input, &channels)?;
                let secret = format!("whsec_{}", new_token());
                let hook = crate::store::EventHook { id: uuid::Uuid::new_v4().simple().to_string()[..16].to_string(), url: url.clone(), secret: secret.clone(), events, channel, created_by: flow.owner.clone(), created_at: now, failures: 0, off_at: None };
                self.store.save_event_hook(&hook).map_err(failed)?;
                self.audit(now, &flow.owner, "event_hook.create", hook.channel.as_deref(), Some(json!({ "url": url })));
                flow.data["event_hook"] = json!(hook.id);
                flow.data["note"] = json!(format!("Events go to {url}. Its signing secret, shown this once: {secret}"));
                flow.state = "edit_out".into();
                Ok(false)
            }
            ("list", "edit_out") => {
                let hook = self.editable_receiver(input["event_hook"].as_str().unwrap_or_default(), &channels)?;
                flow.data["event_hook"] = json!(hook.id);
                flow.data.as_object_mut().map(|d| d.remove("note"));
                flow.state = "edit_out".into();
                Ok(false)
            }
            ("edit_out", "save_out") => {
                let mut hook = self.editable_receiver(flow.data["event_hook"].as_str().unwrap_or_default(), &channels)?;
                let (url, events, channel) = self.read_receiver(flow, input, &channels)?;
                (hook.url, hook.events, hook.channel) = (url, events, channel);
                self.store.save_event_hook(&hook).map_err(failed)?;
                self.audit(now, &flow.owner, "event_hook.update", hook.channel.as_deref(), Some(json!({ "url": hook.url })));
                flow.data["note"] = json!("Saved.");
                Ok(false)
            }
            ("edit_out", "ping_out") => {
                let hook = self.editable_receiver(flow.data["event_hook"].as_str().unwrap_or_default(), &channels)?;
                self.queue_for(&hook, "ping", json!({ "by": self.store.member_id(&flow.owner, now).unwrap_or_default() }), now);
                flow.data["note"] = json!("A test event is on its way.");
                Ok(false)
            }
            ("edit_out", "on_out") => {
                let mut hook = self.editable_receiver(flow.data["event_hook"].as_str().unwrap_or_default(), &channels)?;
                (hook.off_at, hook.failures) = (None, 0);
                self.store.save_event_hook(&hook).map_err(failed)?;
                flow.data["note"] = json!("Turned back on.");
                Ok(false)
            }
            ("edit_out", "secret_out") => {
                let mut hook = self.editable_receiver(flow.data["event_hook"].as_str().unwrap_or_default(), &channels)?;
                hook.secret = format!("whsec_{}", new_token());
                self.store.save_event_hook(&hook).map_err(failed)?;
                self.audit(now, &flow.owner, "event_hook.update", hook.channel.as_deref(), Some(json!({ "url": hook.url, "new_secret": true })));
                flow.data["note"] = json!(format!("A new signing secret, shown this once: {}", hook.secret));
                Ok(false)
            }
            ("edit_out", "delete_out") => {
                let hook = self.editable_receiver(flow.data["event_hook"].as_str().unwrap_or_default(), &channels)?;
                self.store.delete_event_hook(&hook.id).map_err(failed)?;
                self.audit(now, &flow.owner, "event_hook.delete", hook.channel.as_deref(), Some(json!({ "url": hook.url })));
                flow.data["note"] = json!("Events no longer go there.");
                flow.state = "list".into();
                Ok(false)
            }
            _ => Err(Refusal::new("guard_failed", "That can't be done here.")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeds_are_checked_to_discords_limits_and_attachment_links_resolved() {
        let files = HashMap::from([("chart.png".to_string(), "https://c.example/f/abc".to_string())]);
        let embeds = read_embeds(
            &json!([{ "title": "Build #42", "description": "All green", "url": "https://ci.example/42", "color": 3066993, "timestamp": "2026-11-17T10:00:00Z",
                      "author": { "name": "CI", "icon_url": "javascript:alert(1)" }, "footer": { "text": "main" },
                      "image": { "url": "attachment://chart.png" }, "thumbnail": { "url": "ftp://x" },
                      "fields": [{ "name": "Tests", "value": "1203 passed", "inline": true }] }]),
            &files,
        )
        .unwrap();
        assert_eq!(embeds[0]["title"], "Build #42");
        assert_eq!(embeds[0]["image"]["url"], "https://c.example/f/abc");
        assert!(embeds[0].get("thumbnail").is_none());
        assert!(embeds[0]["author"].get("icon_url").is_none());
        assert_eq!(embeds[0]["fields"][0]["inline"], true);
        assert!(read_embeds(&json!(vec![json!({ "title": "x" }); 11]), &files).is_err());
        assert!(read_embeds(&json!([{ "description": "x".repeat(4096) }, { "description": "y".repeat(2000) }]), &files).is_err());
        assert!(read_embeds(&json!([{}]), &files).is_err());
        assert!(read_embeds(&json!([{ "fields": [{ "name": "only a name" }] }]), &files).is_err());
    }

    #[test]
    fn a_webhook_posts_five_times_in_two_seconds_at_most() {
        for n in 0..5 {
            assert!(allowed_now("rate-test", 1000 + n).is_ok());
        }
        let wait = allowed_now("rate-test", 1500).unwrap_err();
        assert!(wait > 1.0 && wait <= 2.0, "{wait}");
        assert!(allowed_now("rate-test", 3001).is_ok());
    }
}
