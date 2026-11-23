//! Events sent out (community/1.0's "Webhooks", "Sending events out"; asked
//! 2026-11-17: Discord-compatible in, our own format out). A receiver an admin
//! names is sent, as they happen, the events it takes:
//!
//! - `post.created`: a post, in a channel the receiver is for;
//! - `post.changed`: a post as it now stands (edited, taken back, reacted to, its
//!   thread grown), as community/1.0's `changed` is;
//! - `member.joined`, `member.left`;
//! - `ping`, sent when the admin asks, to try a receiver.
//!
//! Each is `POST`ed as `{ id, type, community, at, data }`, members named by
//! member id as everywhere else (never a DID), signed with the receiver's secret:
//! `Wyvrn-Signature: t=<seconds>,v1=<hex HMAC-SHA256 of "<t>.<body>">`. A receiver
//! answers 2xx; anything else is tried again later, and one that keeps failing is
//! turned off, which the audit log says.
//!
//! Where events go is checked as a link preview's fetch is: https only, and never
//! an address on this server or its network (looked up, then connected to as
//! looked up, and no redirect followed), unless the server's config allows that
//! (`community.outgoing_private`, for a receiver beside it).

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::community::Community;
use crate::server::AppState;
use crate::store::{Channel, EventHook, Post};

/// What a receiver may take.
pub const EVENTS: &[(&str, &str)] = &[
    ("post.created", "New posts"),
    ("post.changed", "Posts edited, taken back or reacted to"),
    ("member.joined", "Members joining"),
    ("member.left", "Members leaving"),
];
/// When an event its receiver didn't take is tried again, in seconds after each
/// try; past the last, it is dropped.
pub(crate) const AGAIN_AFTER: &[i64] = &[60, 300, 1800, 7200, 21600];
/// Tries in a row a receiver may fail before it is turned off.
pub(crate) const OFF_AFTER: i64 = 20;
/// How long a receiver has to answer.
const ANSWER_WITHIN: Duration = Duration::from_secs(10);
/// The setting that says how far new posts have been made events of.
const CURSOR: &str = "outgoing_serial";

impl Community {
    /// Queues `kind` for every receiver that takes it, of `channel` (or of none:
    /// a member's coming and going), whose maker still sees that channel.
    pub fn emit_event(&self, kind: &str, channel: Option<&Channel>, data: Value, now: i64) {
        let hooks = match self.store.event_hooks() {
            Ok(hooks) => hooks,
            Err(error) => return tracing::warn!(%error, "couldn't read where events go"),
        };
        for hook in hooks.into_iter().filter(|hook| hook.off_at.is_none() && hook.events.iter().any(|e| e == kind)) {
            let fits = match (channel, &hook.channel) {
                (Some(channel), Some(only)) => only == &channel.id,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => true,
            };
            let sees = channel.is_none_or(|channel| self.powers(&hook.created_by).and_then(|powers| self.sees(&hook.created_by, &powers, channel)).unwrap_or(false));
            if fits && sees {
                self.queue_for(&hook, kind, data.clone(), now);
            }
        }
    }

    /// Queues one event for `hook`.
    pub fn queue_for(&self, hook: &EventHook, kind: &str, data: Value, now: i64) {
        let id = uuid::Uuid::new_v4().to_string();
        let body = json!({ "id": id, "type": kind, "community": self.own.clone().unwrap_or_default(), "at": now, "data": data });
        if let Err(error) = self.store.queue_event(&id, &hook.id, &body.to_string(), now) {
            tracing::warn!(%error, "couldn't queue an event");
        }
    }

    /// Who someone is, in an event: their member id and name.
    fn member_in_event(&self, did: &str, now: i64) -> Value {
        let mut who = json!({ "member": self.store.member_id(did, now).unwrap_or_default() });
        if let Some(name) = self.store.member(did).ok().flatten().and_then(|m| m.name) {
            who["name"] = json!(name);
        }
        who
    }

    /// A post, in an event.
    pub fn post_in_event(&self, post: &Post, channel: &Channel, now: i64) -> Value {
        let mut author = self.member_in_event(&post.author_did, now);
        if let Some(name) = &post.author_name {
            author["name"] = json!(name);
        }
        let mut said = json!({ "id": post.id, "seq": post.seq, "content": if post.deleted { "" } else { post.content.as_str() }, "author": author, "posted_at": post.posted_at });
        if let Some(thread) = &post.thread {
            said["thread"] = json!(thread);
        }
        if let Some(at) = post.edited_at {
            said["edited_at"] = json!(at);
        }
        if post.deleted {
            said["deleted"] = json!(true);
        }
        if let Some(hook) = post.extra.as_ref().and_then(|extra| extra.get("webhook")) {
            said["webhook"] = json!({ "id": hook["id"], "name": hook["name"] });
        }
        if !post.reactions.is_empty() {
            said["reactions"] = json!(post.reactions.iter().map(|(emoji, by)| json!({ "emoji": emoji, "count": by.len() })).collect::<Vec<_>>());
        }
        json!({ "channel": { "id": channel.id, "name": channel.name }, "post": said })
    }

    /// A post that changed, as an event (`post.changed`).
    pub fn post_changed_event(&self, post: &Post, now: i64) {
        if post.thread.as_deref().is_some_and(|t| self.store.is_private_thread(t).unwrap_or(true)) {
            return;
        }
        if let Ok(Some(channel)) = self.store.channel(&post.channel) {
            self.emit_event("post.changed", Some(&channel), self.post_in_event(post, &channel, now), now);
        }
    }

    /// A member came or went (`member.joined`, `member.left`).
    pub fn member_event(&self, did: &str, joined: bool, now: i64) {
        self.emit_event(if joined { "member.joined" } else { "member.left" }, None, json!({ "member": self.member_in_event(did, now) }), now);
    }

    /// New posts since last time, as `post.created` events. The first time, from now
    /// on: a receiver is sent what happens, not what happened.
    fn new_posts(&self, now: i64) -> anyhow::Result<()> {
        let latest = self.store.latest_serial()?;
        let Some(from) = self.store.setting(CURSOR)?.and_then(|s| s.parse::<i64>().ok()) else {
            return Ok(self.store.set_setting(CURSOR, &latest.to_string())?);
        };
        if from >= latest {
            return Ok(());
        }
        let mut last = from;
        for post in self.store.posts_after(from, 200)? {
            last = last.max(post.serial);
            // (Nor a private thread's: an event's receiver is in no thread.)
            if post.deleted || post.thread.as_deref().is_some_and(|t| self.store.is_private_thread(t).unwrap_or(true)) {
                continue;
            }
            if let Some(channel) = self.store.channel(&post.channel)? {
                self.emit_event("post.created", Some(&channel), self.post_in_event(&post, &channel, now), now);
            }
        }
        self.store.set_setting(CURSOR, &last.to_string())?;
        Ok(())
    }
}

/// `Wyvrn-Signature`'s value for `body`, sent at `at`.
pub fn signature(secret: &str, at: i64, body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(format!("{at}.{body}").as_bytes());
    let digest = mac.finalize().into_bytes();
    format!("t={at},v1={}", digest.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Whether events may go to `ip`: an address out on the internet, unless private
/// ones are allowed.
fn reachable(ip: IpAddr, private: bool) -> bool {
    private || wyvrn_service::guard::is_public(ip)
}

/// Checks a receiver's URL as an admin gives it: what is wrong with it, if anything.
pub fn check_url(url: &str, private: bool) -> Result<url::Url, String> {
    let parsed = url::Url::parse(url.trim()).map_err(|_| "That isn't a web address.".to_string())?;
    match parsed.scheme() {
        "https" => {}
        "http" if private => {}
        _ => return Err("Events go to an https:// address.".into()),
    }
    if parsed.host_str().is_none() || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("That address can't be sent to.".into());
    }
    Ok(parsed)
}

/// A client that reaches `url` and nowhere else: looked up once, out on the
/// internet (unless `private`), following no redirect. With the URL, checked.
pub(crate) async fn client_for(url: &str, private: bool, user_agent: &str) -> Result<(reqwest::Client, url::Url), String> {
    let url = check_url(url, private)?;
    let host = url.host_str().unwrap_or_default().to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), port)).await.map_err(|e| e.to_string())?.collect();
    let Some(address) = addresses.iter().copied().find(|a| reachable(a.ip(), private)) else {
        return Err("its address is on this server's own network".into());
    };
    if addresses.iter().any(|a| !reachable(a.ip(), private)) {
        return Err("its name also points at this server's own network".into());
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(ANSWER_WITHIN)
        .resolve(&host, address)
        .user_agent(user_agent)
        .build()
        .map_err(|e| e.to_string())?;
    Ok((client, url))
}

/// Sends one event. `Ok` when the receiver took it.
async fn send(hook: &EventHook, body: &str, kind: &str, id: &str, private: bool) -> Result<(), String> {
    let (client, url) = client_for(&hook.url, private, "wyvrn-community (events)").await?;
    let at = chrono::Utc::now().timestamp();
    let answer = client
        .post(url)
        .header("content-type", "application/json")
        .header("wyvrn-event", kind)
        .header("wyvrn-delivery", id)
        .header("wyvrn-signature", signature(&hook.secret, at, body))
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if answer.status().is_success() { Ok(()) } else { Err(format!("answered {}", answer.status())) }
}

/// Sends what is due, and makes events of what was posted meanwhile, every two
/// seconds.
pub async fn run_outgoing(state: Arc<AppState>) {
    loop {
        let now = crate::gates::now();
        if let Err(error) = state.community.new_posts(now) {
            tracing::warn!(%error, "couldn't make events of new posts");
        }
        if let Err(error) = send_due(&state, now).await {
            tracing::warn!(%error, "couldn't send events");
        }
        // Announcement channels' posts to their followers, and what else waits
        // to go to another server (announcements.rs).
        if let Err(error) = crate::announcements::send_due(&state, now).await {
            tracing::warn!(%error, "couldn't send announcements");
        }
        // Events' reminders (events.rs).
        if let Err(error) = crate::events::send_reminders(&state, now).await {
            tracing::warn!(%error, "couldn't send reminders");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn send_due(state: &Arc<AppState>, now: i64) -> anyhow::Result<()> {
    let community = &state.community;
    let private = community.config.outgoing_private;
    for (id, hook_id, body, attempts) in community.store.due_events(now, 50)? {
        let Some(mut hook) = community.store.event_hook(&hook_id)?.filter(|hook| hook.off_at.is_none()) else {
            community.store.event_done(&id)?;
            continue;
        };
        let kind = serde_json::from_str::<Value>(&body).ok().and_then(|b| b["type"].as_str().map(str::to_string)).unwrap_or_default();
        match send(&hook, &body, &kind, &id, private).await {
            Ok(()) => {
                community.store.event_done(&id)?;
                if hook.failures != 0 {
                    hook.failures = 0;
                    community.store.save_event_hook(&hook)?;
                }
            }
            Err(why) => {
                tracing::info!(hook = %hook.id, %why, "an event wasn't taken; trying again later");
                match AGAIN_AFTER.get(attempts as usize) {
                    Some(after) => community.store.event_later(&id, attempts + 1, now + after)?,
                    None => community.store.event_done(&id)?,
                }
                hook.failures += 1;
                if hook.failures >= OFF_AFTER {
                    hook.off_at = Some(now);
                    community.audit(now, &hook.created_by, "event_hook.off", None, Some(json!({ "url": hook.url, "why": why })));
                }
                community.store.save_event_hook(&hook)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_go_only_to_addresses_out_on_the_internet_unless_private_ones_are_allowed() {
        for ip in ["127.0.0.1", "10.1.2.3", "192.168.0.10", "172.16.5.5", "169.254.1.1", "100.64.0.1", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:127.0.0.1"] {
            assert!(!reachable(ip.parse().unwrap(), false), "{ip}");
            assert!(reachable(ip.parse().unwrap(), true), "{ip}");
        }
        for ip in ["93.184.216.34", "2606:2800:220:1::248"] {
            assert!(reachable(ip.parse().unwrap(), false), "{ip}");
        }
        assert!(check_url("http://example.com/hook", false).is_err());
        assert!(check_url("https://user:pw@example.com/", false).is_err());
        assert!(check_url("https://example.com/hook", false).is_ok());
        assert!(check_url("http://127.0.0.1:9/hook", true).is_ok());
    }

    #[test]
    fn a_signature_is_hmac_sha256_over_the_time_and_the_body() {
        // RFC 4231's case 2, as "<t>.<body>": key "Jefe", data "what do ya want for nothing?".
        let said = signature("Jefe", 0, "what do ya want for nothing?");
        let mut mac = Hmac::<Sha256>::new_from_slice(b"Jefe").unwrap();
        mac.update(b"0.what do ya want for nothing?");
        let expected: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(said, format!("t=0,v1={expected}"));
    }
}
