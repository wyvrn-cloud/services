//! Posting, replying and reacting from the web view, for a member signed in to it
//! (community/1.0's "The web view"; asked 2026-11-17), and a channel's page told
//! when there is something new (server-sent events).
//!
//! What a form says is made into the community/1.0 message the app would send,
//! and handled as one: the same permissions, slow mode, timeouts, AutoMod and
//! rate of posting, refused with the same words. Every form carries the session's
//! form token (`csrf`), checked before anything else, and the page answers with
//! the channel again (at the post) or with why it wasn't taken.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::pages::Viewer;
use super::sign_in::form_ok;
use super::{as_page, channel_path, page_error};
use crate::server::AppState;
use crate::store::Channel;

/// The emoji a post can be given from the web view in one tap, besides those it has.
pub const QUICK_REACTIONS: &[&str] = &["👍", "❤️", "😂", "🎉", "😮", "😢"];
/// How often a channel's live page is looked at for what is new.
const LOOK_EVERY: Duration = Duration::from_secs(2);
/// How many looks between asking again who a live page's viewer is.
const RECHECK_EVERY: u32 = 5;
/// Live pages open at once from one address, and for one member.
const LIVE_PER_ADDRESS: usize = 20;
const LIVE_PER_MEMBER: usize = 10;

/// What live pages and connections are open, by address and by member: one map a
/// community. Each `LiveHeld` is one, counted while it lasts.
pub type LiveCounts = Arc<std::sync::Mutex<std::collections::HashMap<String, usize>>>;

pub(crate) struct LiveHeld(LiveCounts, Vec<String>);

impl LiveHeld {
    /// One more open under each key, unless one is at its limit.
    pub(crate) fn take(counts: &LiveCounts, keys: Vec<(String, usize)>) -> Option<Self> {
        let mut open = counts.lock().unwrap_or_else(|p| p.into_inner());
        if keys.iter().any(|(key, most)| open.get(key).copied().unwrap_or(0) >= *most) {
            return None;
        }
        for (key, _) in &keys {
            *open.entry(key.clone()).or_insert(0) += 1;
        }
        Some(Self(counts.clone(), keys.into_iter().map(|(key, _)| key).collect()))
    }
}

impl Drop for LiveHeld {
    fn drop(&mut self) {
        let mut open = self.0.lock().unwrap_or_else(|p| p.into_inner());
        for key in &self.1 {
            if let Some(count) = open.get_mut(key) {
                *count -= 1;
                if *count == 0 {
                    open.remove(key);
                }
            }
        }
    }
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/ch/{id}/post", post(post_form)).route("/ch/{id}/m/{seq}/react", post(react_form)).route("/ch/{id}/live", get(live))
}

#[derive(Deserialize)]
pub struct PostForm {
    csrf: String,
    content: String,
    /// The post (by its place in the channel) this answers.
    #[serde(default)]
    reply: Option<i64>,
}

#[derive(Deserialize)]
pub struct ReactForm {
    csrf: String,
    emoji: String,
}

/// The channel `id`, for a member who may read it from the web, and the page it is at.
fn readable(state: &AppState, member: &crate::store::Member, id: &str) -> Option<(Channel, String)> {
    let viewer = Viewer { member: Some(member.clone()), csrf: None };
    let (channel, _) = super::pages::visible_channel(state, &viewer, id)?;
    let at = channel_path(&channel.id, &channel.name);
    Some((channel, at))
}

/// What handling `message` came to: back to the channel, at `anchor`, or why not.
fn answered(state: &Arc<AppState>, sender: &str, message: &Value, back: &str) -> Response {
    let outcome = match state.community.handle(sender, message, crate::gates::now()) {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::warn!(%error, "a post from the web failed");
            return page_error(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong", "That didn't go through. Try again.");
        }
    };
    let reply = crate::server::carry_out(state, sender, outcome);
    match reply {
        // Refused: as the app would be told.
        Some(reply) if reply["type"].as_str().is_some_and(|t| t.ends_with("/problem-report")) => {
            let said = reply["body"]["comment"].as_str().unwrap_or("That wasn't taken.");
            page_error(StatusCode::BAD_REQUEST, "Not posted", said)
        }
        Some(reply) => {
            let anchor = reply["body"]["seq"].as_i64().map(|seq| format!("#m{seq}")).unwrap_or_default();
            as_page(Redirect::to(&format!("{back}{anchor}")).into_response())
        }
        None => as_page(Redirect::to(back).into_response()),
    }
}

/// `POST ch/{id}/post`: a post, or a reply to one.
async fn post_form(State(state): State<Arc<AppState>>, Path(id): Path<String>, headers: HeaderMap, Form(form): Form<PostForm>) -> Response {
    if state.web.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(member) = form_ok(&state, &headers, &form.csrf) else {
        return page_error(StatusCode::FORBIDDEN, "Sign in again", "Your session ended, or that form wasn't from here.");
    };
    let Some((channel, at)) = readable(&state, &member, &id) else { return page_error(StatusCode::NOT_FOUND, "No such channel", "There's no such channel for you here.") };
    let content = form.content.trim();
    if content.is_empty() {
        return as_page(Redirect::to(&format!("../../{at}")).into_response());
    }
    let mut body = json!({ "channel": channel.id, "id": uuid::Uuid::new_v4().to_string(), "content": content });
    if let Some(answered) = form.reply.and_then(|seq| state.community.store.post_at(&channel.id, seq).ok().flatten()).filter(|p| !p.deleted) {
        body["reply_to"] = json!({ "id": answered.id, "author": answered.author_did, "snippet": super::render::excerpt(&answered.content, 100) });
        // An answer in a thread is in that thread.
        if let Some(thread) = &answered.thread {
            body["thread"] = json!(thread);
        }
    }
    let message = json!({ "type": crate::community::POST, "id": uuid::Uuid::new_v4().to_string(), "body": body });
    answered(&state, &member.did, &message, &format!("../../{at}"))
}

/// `POST ch/{id}/m/{seq}/react`: an emoji put on the post, or taken off if it was
/// this member's already.
async fn react_form(State(state): State<Arc<AppState>>, Path((id, seq)): Path<(String, i64)>, headers: HeaderMap, Form(form): Form<ReactForm>) -> Response {
    if state.web.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(member) = form_ok(&state, &headers, &form.csrf) else {
        return page_error(StatusCode::FORBIDDEN, "Sign in again", "Your session ended, or that form wasn't from here.");
    };
    let Some((channel, at)) = readable(&state, &member, &id) else { return page_error(StatusCode::NOT_FOUND, "No such channel", "There's no such channel for you here.") };
    let Some(post) = state.community.store.post_at(&channel.id, seq).ok().flatten() else { return page_error(StatusCode::NOT_FOUND, "No such post", "That post isn't here.") };
    // Their whole set on it, as message-reactions/1.0 says it: this one in or out.
    let mut theirs: Vec<String> = post.reactions.iter().filter(|(_, by)| by.contains(&member.did)).map(|(emoji, _)| emoji.clone()).collect();
    match theirs.iter().position(|emoji| *emoji == form.emoji) {
        Some(at) => {
            theirs.remove(at);
        }
        None => theirs.push(form.emoji.clone()),
    }
    let message = json!({ "type": crate::community::REACT, "id": uuid::Uuid::new_v4().to_string(), "body": { "to": { "id": post.id, "author": post.author_did }, "emoji": theirs } });
    let back = format!("../../../../{at}");
    match answered(&state, &member.did, &message, &back) {
        // (Back at the post, not the foot of the channel.)
        response if response.status().is_redirection() => as_page(Redirect::to(&format!("{back}#m{seq}")).into_response()),
        response => response,
    }
}

/// `GET ch/{id}/live`: server-sent events saying, as it changes, how far the
/// channel has got (`seq`, its latest post) and when a post in it last changed
/// (`rev`), as `channel`; and who is in voice as the side of the page shows it, as
/// `voice` (a fingerprint, only where the page shows any). The page fetches itself
/// again when either moves. Only for a channel the viewer may read, signed in or not.
async fn live(State(state): State<Arc<AppState>>, Path(id): Path<String>, request: axum::extract::Request) -> Response {
    if state.web.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let headers = request.headers().clone();
    let viewer = Viewer::of(&state, &headers);
    if super::pages::visible_channel(&state, &viewer, &id).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    // So many open at once from one address, and from one member (WYV-51).
    let mut keys = Vec::new();
    // Every client but a trusted proxy's own (WYV-74, WYV-98): the trusted proxies
    // give the client, so a private address is a client on a private network.
    if let Some(client) = state.clients.of(&request) {
        keys.push((format!("ip:{client}"), LIVE_PER_ADDRESS));
    }
    if let Some((member, _)) = super::sign_in::session_of(&state, &headers) {
        keys.push((format!("member:{}", member.did), LIVE_PER_MEMBER));
    }
    let Some(held) = LiveHeld::take(&state.live_open, keys) else {
        return (StatusCode::TOO_MANY_REQUESTS, "Too many pages of this community are open at once.").into_response();
    };
    // Each item waits until the channel or its voice has moved on (or the channel
    // is gone), then says so.
    struct Seen {
        channel: Option<(i64, i64)>,
        voice: Option<String>,
        ticks: u32,
        _held: LiveHeld,
    }
    let seen = Seen { channel: None, voice: None, ticks: 0, _held: held };
    let stream = futures_util::stream::unfold((state, id, viewer, seen, headers), |(state, id, mut viewer, mut seen, headers)| async move {
        loop {
            // Every few looks, who this is is asked again: one signed out, taken
            // out or banned since sees what anyone may, or nothing (WYV-51).
            seen.ticks += 1;
            if seen.ticks % RECHECK_EVERY == 0 {
                viewer = Viewer::of(&state, &headers);
                super::pages::visible_channel(&state, &viewer, &id)?;
            }
            let store = &state.community.store;
            let now = store.channel(&id).ok().flatten().filter(|c| !c.closed).map(|c| (c.latest_seq, store.channel_rev(&c.id).unwrap_or(0)))?;
            if seen.channel != Some(now) {
                seen.channel = Some(now);
                let event = Event::default().event("channel").data(json!({ "seq": now.0, "rev": now.1 }).to_string());
                return Some((Ok::<Event, Infallible>(event), (state, id, viewer, seen, headers)));
            }
            let voice = super::pages::voice_shown(&state, &viewer);
            if seen.voice.as_deref() != Some(voice.as_str()) {
                let first = seen.voice.is_none();
                seen.voice = Some(voice.clone());
                // Nothing of voice on this page to begin with: nothing to say.
                if !(first && voice.is_empty()) {
                    let event = Event::default().event("voice").data(voice);
                    return Some((Ok::<Event, Infallible>(event), (state, id, viewer, seen, headers)));
                }
            }
            tokio::time::sleep(LOOK_EVERY).await;
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

#[cfg(test)]
mod live_tests {
    #[test]
    fn so_many_live_pages_at_once_and_each_counted_until_it_closes() {
        let counts = super::LiveCounts::default();
        let keys = || vec![("ip:test-live".to_string(), 2)];
        let (first, second) = (super::LiveHeld::take(&counts, keys()).unwrap(), super::LiveHeld::take(&counts, keys()).unwrap());
        assert!(super::LiveHeld::take(&counts, keys()).is_none());
        drop(first);
        let third = super::LiveHeld::take(&counts, keys()).unwrap();
        drop((second, third));
        assert!(counts.lock().unwrap().get("ip:test-live").is_none());
    }
}
