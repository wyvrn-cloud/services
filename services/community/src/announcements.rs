//! Announcement channels (community/1.0's "Announcement channels", asked
//! 2026-11-18): a text channel whose published posts other channels follow, as
//! Discord's are.
//!
//! - **Publishing** (`publish`): a post goes out once its author, or someone
//!   who manages posts there, publishes it; its edits and its taking back follow
//!   it everywhere it went.
//! - **Another community follows** with a code a member of this one asks for
//!   (`follow-code-request`) and gives their own (`follow`). That community then
//!   writes here itself, server to server (`subscribe`); this one sends it each
//!   published post and each change (`announcement`). The same messages, the
//!   other way round, are this community following another's.
//! - **A Discord webhook, or another service**, is given the posts by the
//!   `announcements` workflow: Discord's way (`?wait=true`, then `PATCH` and
//!   `DELETE` of the message it made), or as a Standard Webhooks event signed
//!   twice, with a secret it shares (`v1`) and with this community's own key
//!   (`v1a`).
//!
//! What is to go waits in `announce_queue`, a post and a follower each, and is
//! sent as the post then stands; `announce_copies` keeps a digest of what each
//! was last sent, so a post reacted to (which changes it, but not what is sent)
//! isn't sent again.

use std::sync::Arc;

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::community::{line, refuse, reply_to, Community, Outcome, COMMUNITY, PROBLEM_REPORT};
use crate::questions::{failed, Refusal};
use crate::server::AppState;
use crate::store::{Channel, Flow, Follow, Follower, Member, Post, Role};

pub const PUBLISH: &str = "https://wyvrn.app/community/1.0/publish";
pub const FOLLOW_CODE_REQUEST: &str = "https://wyvrn.app/community/1.0/follow-code-request";
pub const FOLLOW_CODE: &str = "https://wyvrn.app/community/1.0/follow-code";
pub const FOLLOW: &str = "https://wyvrn.app/community/1.0/follow";
pub const UNFOLLOW: &str = "https://wyvrn.app/community/1.0/unfollow";
pub const SUBSCRIBE: &str = "https://wyvrn.app/community/1.0/subscribe";
pub const SUBSCRIBED: &str = "https://wyvrn.app/community/1.0/subscribed";
pub const UNSUBSCRIBE: &str = "https://wyvrn.app/community/1.0/unsubscribe";
pub const ANNOUNCEMENT: &str = "https://wyvrn.app/community/1.0/announcement";

pub const ANNOUNCEMENTS_FLOW: &str = "announcements";
/// How long a follow waits for the other community to answer before it is given up.
pub const UNANSWERED_FOLLOW: i64 = 7 * 24 * 3600;
/// How long a follow code is good for.
const CODE_FOR: i64 = 600;
/// How many announcement channels one channel follows at most.
pub const MAX_FOLLOWS: usize = 10;
/// The setting that says how far changed posts have been looked at.
const CURSOR: &str = "announce_rev";
/// Where Discord's webhooks are.
const DISCORD_HOSTS: &[&str] = &["discord.com", "discordapp.com", "ptb.discord.com", "canary.discord.com"];

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}

/// Whether `message_type` is one community servers send each other.
pub fn between_servers(message_type: &str) -> bool {
    [SUBSCRIBE, SUBSCRIBED, UNSUBSCRIBE, ANNOUNCEMENT].contains(&message_type)
}

/// A digest of what was sent, to tell whether anything a follower sees changed.
fn digest(body: &Value) -> String {
    Sha256::digest(body.to_string().as_bytes()).iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// A new secret for a service, as Standard Webhooks writes one.
fn new_secret() -> String {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Standard Webhooks' `webhook-signature` for `body`: `v1,` the HMAC-SHA256
/// with `secret`, and `v1a,` the Ed25519 signature with the community's key,
/// each over `<id>.<timestamp>.<body>`, whichever there is.
pub fn standard_signatures(secret: Option<&str>, seed: Option<&[u8; 32]>, id: &str, timestamp: i64, body: &str) -> String {
    let signed = format!("{id}.{timestamp}.{body}");
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut out = Vec::new();
    if let Some(secret) = secret {
        let raw = secret.strip_prefix("whsec_").unwrap_or(secret);
        let key = b64.decode(raw).unwrap_or_else(|_| raw.as_bytes().to_vec());
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("any key length");
        mac.update(signed.as_bytes());
        out.push(format!("v1,{}", b64.encode(mac.finalize().into_bytes())));
    }
    if let Some(seed) = seed {
        out.push(format!("v1a,{}", b64.encode(SigningKey::from_bytes(seed).sign(signed.as_bytes()).to_bytes())));
    }
    out.join(" ")
}

/// The community's public key as Standard Webhooks writes one.
pub fn public_key(seed: &[u8; 32]) -> String {
    format!("whpk_{}", base64::engine::general_purpose::STANDARD.encode(SigningKey::from_bytes(seed).verifying_key().to_bytes()))
}

/// What Discord takes as a webhook's name: up to 80 characters, never saying
/// "discord" or "clyde".
fn discord_username(name: &str) -> String {
    let mut name = name.to_string();
    for word in ["discord", "clyde"] {
        while let Some(at) = name.to_lowercase().find(word) {
            name.replace_range(at..at + word.len(), "");
        }
    }
    let name: String = name.trim().chars().take(80).collect();
    if name.is_empty() { "Wyvrn".into() } else { name }
}

/// Checks a Discord webhook URL as an admin gives it. Where the server allows
/// receivers on its own network (`private`), one there that answers as Discord
/// does (a stand-in, for trying it) is taken too.
pub fn check_discord_url(given: &str, private: bool) -> Result<url::Url, String> {
    let url = url::Url::parse(given.trim()).map_err(|_| "That isn't a web address.".to_string())?;
    let discord = url.scheme() == "https" && url.host_str().is_some_and(|host| DISCORD_HOSTS.contains(&host));
    let ok = (discord || (private && crate::outgoing::check_url(url.as_str(), true).is_ok())) && url.path().starts_with("/api/webhooks/") && url.path().split('/').filter(|s| !s.is_empty()).count() >= 4;
    if ok { Ok(url) } else { Err("A Discord webhook's URL is https://discord.com/api/webhooks/... (in Discord: the channel's settings, Integrations, Webhooks, Copy Webhook URL).".into()) }
}

impl Community {
    /// What the community is called now.
    pub fn name_now(&self) -> String {
        self.changed().name.unwrap_or_else(|| self.config.name.clone())
    }

    /// A post as followers are sent it (`announcement`'s `post`).
    pub fn announced(post: &Post) -> Value {
        let mut body = json!({ "id": post.id, "content": if post.deleted { "" } else { post.content.as_str() }, "posted_at": post.posted_at });
        if let Some(at) = post.edited_at {
            body["edited_at"] = json!(at);
        }
        if post.deleted {
            body["deleted"] = json!(true);
        } else if let Some(extra) = &post.extra {
            for key in ["media", "attachments"] {
                if let Some(value) = extra.get(key) {
                    body[key] = value.clone();
                }
            }
        }
        body
    }

    /// Queues `post` for every follower of its channel still sent to: not turned
    /// off, and whose maker (who added it, or asked for the code another community
    /// followed with) is still here and not banned. One whose maker left or was
    /// banned is paused, as their webhooks and event receivers are (WYV-99,
    /// WYV-125): it is sent nothing until they are back.
    fn queue_for_followers(&self, post: &Post, now: i64) -> anyhow::Result<()> {
        for follower in self.store.followers_of(&post.channel)?.into_iter().filter(|f| f.off_at.is_none() && self.maker_here(&f.created_by, now)) {
            self.store.queue_announcement(&follower.id, &post.id, now)?;
        }
        Ok(())
    }

    /// `publish`: sends a post of an announcement channel to its followers.
    pub fn publish(&self, member: &Member, message: &Value, now: i64) -> anyhow::Result<Outcome> {
        let body = &message["body"];
        let unknown = || refuse(message, "e.p.msg.unknown-post", "There is no such post.");
        let Some(channel) = body["channel"].as_str().map(|id| self.store.channel(id)).transpose()?.flatten() else { return unknown() };
        if !self.sees_id(&member.did, &channel.id)? {
            return unknown();
        }
        let Some(post) = body["post"].as_str().map(|id| self.store.post_by_id(id)).transpose()?.flatten().filter(|p| p.channel == channel.id && p.thread.is_none()) else {
            return unknown();
        };
        if !channel.announcement {
            return refuse(message, "e.p.msg.not-announcement", "That isn't an announcement channel.");
        }
        if post.deleted {
            return refuse(message, "e.p.msg.deleted", "That post was taken back.");
        }
        if post.published_at.is_some() {
            return refuse(message, "e.p.msg.published", "That post was published already.");
        }
        let may = (post.author_did == member.did && self.may_in(&member.did, &channel, "post")?) || self.may_in(&member.did, &channel, "manage_messages")?;
        if !may {
            return refuse(message, "e.p.trust.not-authorized", "A post is published by its author, or by someone who manages posts in the channel.");
        }
        self.store.set_published(&post.id, &channel.id, now)?;
        self.store.touch_post(&post.id)?;
        if let Some(post) = self.store.post_by_id(&post.id)? {
            self.queue_for_followers(&post, now)?;
        }
        self.audit(now, &member.did, "post.publish", Some(&post.id), Some(json!({ "channel": channel.id })));
        self.changed_answer(message, &post.id)
    }

    /// `follow-code-request`: a code to follow an announcement channel with.
    pub fn follow_code(&self, member: &Member, message: &Value, now: i64) -> anyhow::Result<Outcome> {
        let Some(channel) = message["body"]["channel"].as_str().map(|id| self.store.channel(id)).transpose()?.flatten().filter(|c| !c.closed) else {
            return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.");
        };
        if !self.sees_id(&member.did, &channel.id)? {
            return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.");
        }
        if !channel.announcement {
            return refuse(message, "e.p.msg.not-announcement", "That isn't an announcement channel.");
        }
        let code = format!("{}{}", uuid::Uuid::new_v4().simple(), &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let expires = now + CODE_FOR;
        self.store.add_follow_code(&code, &channel.id, &member.did, expires)?;
        Ok(Outcome::reply(reply_to(message, FOLLOW_CODE, json!({ "channel": channel.id, "code": code, "expires_at": expires }))))
    }

    /// The follows of `channel` as `community` lists them for whoever may manage
    /// it: one asked for and not yet answered says `pending`.
    pub(crate) fn follows_body(follows: &[Follow], channel: &str) -> Vec<Value> {
        follows
            .iter()
            .filter(|f| f.channel == channel)
            .map(|f| match f.pending {
                true => json!({ "id": f.id, "community": f.source, "pending": true }),
                false => json!({ "id": f.id, "community": f.source, "name": f.name, "channel_name": f.channel_name }),
            })
            .collect()
    }

    /// `follow`: a channel here is to follow another community's announcement
    /// channel. Answered at once, the follow pending until that community answers.
    pub fn follow(&self, member: &Member, message: &Value, now: i64) -> anyhow::Result<Outcome> {
        let body = &message["body"];
        let Some(channel) = body["channel"].as_str().map(|id| self.store.channel(id)).transpose()?.flatten().filter(|c| !c.closed) else {
            return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.");
        };
        let powers = self.powers(&member.did)?;
        if !self.sees(&member.did, &powers, &channel)? {
            return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.");
        }
        if !(powers.has("manage_channels") || channel.created_by == member.did) {
            return refuse(message, "e.p.trust.not-authorized", "Following takes Manage channels, or having opened the channel.");
        }
        if channel.voice.is_some() || channel.forum.is_some() {
            return refuse(message, "e.p.msg.invalid-follow", "Only a text channel follows announcement channels.");
        }
        let source = body["source"]["community"].as_str().unwrap_or_default().trim().to_string();
        let code = body["source"]["code"].as_str().unwrap_or_default().trim().to_string();
        if !source.starts_with("did:") || source.len() > 3000 || code.is_empty() || code.len() > 64 {
            return refuse(message, "e.p.msg.invalid-follow", "Following needs the community's DID and a follow code from it.");
        }
        if Some(&source) == self.own.as_ref() || Some(&source) == self.speaks_as.as_ref() {
            return refuse(message, "e.p.msg.invalid-follow", "A channel follows another community's announcement channel, not one of this community's.");
        }
        if self.store.follows()?.iter().filter(|f| f.channel == channel.id).count() >= MAX_FOLLOWS {
            return refuse(message, "e.p.msg.too-many", "A channel follows ten announcement channels at most.");
        }
        let follow = Follow {
            id: new_id(),
            channel: channel.id.clone(),
            source: source.clone(),
            name: String::new(),
            channel_name: String::new(),
            remote: None,
            asked_by: member.did.clone(),
            asked_thid: message["thid"].as_str().or(message["id"].as_str()).map(str::to_string),
            pending: true,
            created_at: now,
        };
        self.store.save_follow(&follow)?;
        let subscribe = json!({ "type": SUBSCRIBE, "id": follow.id, "body": { "code": code, "channel": { "id": channel.id, "name": channel.name }, "name": self.name_now() } });
        let mut answer = self.community(member)?;
        answer["following"] = json!(follow.id);
        Ok(Outcome { reply: Some(reply_to(message, COMMUNITY, answer)), notices: vec![(source, subscribe)], channels_changed: true, ..Outcome::default() })
    }

    /// `unfollow`: a channel here stops following.
    pub fn unfollow(&self, member: &Member, message: &Value, now: i64) -> anyhow::Result<Outcome> {
        let body = &message["body"];
        let Some(follow) = body["follow"].as_str().map(|id| self.store.follow(id)).transpose()?.flatten().filter(|f| body["channel"].as_str().is_none_or(|c| c == f.channel)) else {
            return refuse(message, "e.p.msg.unknown-follow", "That channel doesn't follow that.");
        };
        let Some(channel) = self.store.channel(&follow.channel)? else {
            return refuse(message, "e.p.msg.unknown-follow", "That channel doesn't follow that.");
        };
        let powers = self.powers(&member.did)?;
        if !self.sees(&member.did, &powers, &channel)? || !(powers.has("manage_channels") || channel.created_by == member.did) {
            return refuse(message, "e.p.trust.not-authorized", "Stopping a follow takes Manage channels, or having opened the channel.");
        }
        let notices = self.end_follow(&follow, &member.did, now)?;
        Ok(Outcome { reply: Some(reply_to(message, COMMUNITY, self.community(member)?)), notices, channels_changed: true, ..Outcome::default() })
    }

    /// Follows the other community never answered, a week on: given up, and
    /// whoever asked told, as if it had refused (with the channels as they now are).
    pub fn give_up_unanswered(&self, now: i64) -> anyhow::Result<Vec<(String, Value)>> {
        let mut notices = Vec::new();
        for follow in self.store.follows()?.into_iter().filter(|f| f.pending && f.created_at <= now - UNANSWERED_FOLLOW) {
            self.store.delete_follow(&follow.id)?;
            self.audit(now, &follow.asked_by, "follow.unanswered", Some(&follow.channel), Some(json!({ "community": follow.source })));
            let Some(member) = self.store.member(&follow.asked_by)? else { continue };
            let mut report = json!({ "type": PROBLEM_REPORT, "id": uuid::Uuid::new_v4().to_string(), "body": {
                "code": "e.p.msg.follow-unanswered",
                "comment": "That community didn't answer in a week, so the channel doesn't follow it. Ask again once it is about."
            } });
            let mut answer = json!({ "type": COMMUNITY, "id": uuid::Uuid::new_v4().to_string(), "body": self.community(&member)? });
            if let Some(thid) = &follow.asked_thid {
                report["pthid"] = json!(thid);
                answer["thid"] = json!(thid);
            }
            notices.push((member.did.clone(), report));
            notices.push((member.did, answer));
        }
        Ok(notices)
    }

    /// Ends a follow held here, telling the source: what to send it.
    fn end_follow(&self, follow: &Follow, by: &str, now: i64) -> anyhow::Result<Vec<(String, Value)>> {
        self.store.delete_follow(&follow.id)?;
        self.audit(now, by, "follow.delete", Some(&follow.channel), Some(json!({ "community": follow.source, "name": follow.name })));
        Ok(follow.remote.iter().map(|remote| (follow.source.clone(), json!({ "type": UNSUBSCRIBE, "id": uuid::Uuid::new_v4().to_string(), "body": { "follow": remote } }))).collect())
    }

    /// What another community's server sends this one (`subscribe`,
    /// `subscribed`, `unsubscribe`, `announcement`, and a refusal of a
    /// `subscribe`): `None` for anything else.
    pub fn from_server(&self, sender: &str, message: &Value, now: i64) -> anyhow::Result<Option<Outcome>> {
        let message_type = message["type"].as_str().unwrap_or_default();
        let body = &message["body"];
        match message_type {
            SUBSCRIBE => {
                let code = body["code"].as_str().unwrap_or_default();
                let Some((channel_id, asked_by)) = self.store.take_follow_code(code, now)? else {
                    return refuse(message, "e.p.msg.invalid-code", "That follow code is unknown, used or too old. Ask for a new one.").map(Some);
                };
                let Some(channel) = self.store.channel(&channel_id)?.filter(|c| c.announcement && !c.closed && !self.channel_off(c)) else {
                    return refuse(message, "e.p.msg.not-announcement", "That channel is no longer an announcement channel.").map(Some);
                };
                // Whoever asked for the code still sees the channel.
                if self.store.member(&asked_by)?.is_none() || !self.sees_id(&asked_by, &channel.id)? {
                    return refuse(message, "e.p.msg.invalid-code", "Whoever asked for that code can no longer see the channel.").map(Some);
                }
                let name = line(&body["name"], 100).unwrap_or_else(|| "A community".into());
                let their_channel = line(&body["channel"]["name"], 100).unwrap_or_default();
                let follower = Follower {
                    id: new_id(),
                    channel: channel.id.clone(),
                    kind: "community".into(),
                    target: sender.to_string(),
                    name: if their_channel.is_empty() { name.clone() } else { format!("{name} #{their_channel}") },
                    secret: None,
                    created_by: asked_by.clone(),
                    created_at: now,
                    failures: 0,
                    off_at: None,
                };
                self.store.save_follower(&follower)?;
                self.audit(now, &asked_by, "announcement.follow", Some(&channel.id), Some(json!({ "community": sender, "name": follower.name })));
                Ok(Some(Outcome::reply(reply_to(message, SUBSCRIBED, json!({ "follow": follower.id, "channel": { "id": channel.id, "name": channel.name }, "name": self.name_now() })))))
            }
            SUBSCRIBED => {
                let Some(mut follow) = message["thid"].as_str().map(|id| self.store.follow(id)).transpose()?.flatten().filter(|f| f.pending && f.source == sender) else {
                    return Ok(Some(Outcome::default()));
                };
                follow.remote = body["follow"].as_str().filter(|id| !id.is_empty() && id.len() <= 64).map(str::to_string);
                follow.name = line(&body["name"], 100).unwrap_or_else(|| "A community".into());
                follow.channel_name = line(&body["channel"]["name"], 100).unwrap_or_default();
                follow.pending = false;
                self.store.save_follow(&follow)?;
                self.audit(now, &follow.asked_by, "follow.create", Some(&follow.channel), Some(json!({ "community": sender, "name": follow.name })));
                let mut notices = Vec::new();
                if let Some(member) = self.store.member(&follow.asked_by)? {
                    let mut answer = json!({ "type": COMMUNITY, "id": uuid::Uuid::new_v4().to_string(), "body": self.community(&member)? });
                    if let Some(thid) = &follow.asked_thid {
                        answer["thid"] = json!(thid);
                    }
                    notices.push((member.did, answer));
                }
                Ok(Some(Outcome { notices, channels_changed: true, ..Outcome::default() }))
            }
            PROBLEM_REPORT => {
                // A refused `subscribe`: passed on to whoever asked.
                let about = message["pthid"].as_str().or(message["thid"].as_str()).unwrap_or_default();
                let Some(follow) = self.store.follow(about)?.filter(|f| f.pending && f.source == sender) else { return Ok(None) };
                self.store.delete_follow(&follow.id)?;
                let mut report = json!({ "type": PROBLEM_REPORT, "id": uuid::Uuid::new_v4().to_string(), "body": { "code": body["code"], "comment": body["comment"] } });
                if let Some(thid) = &follow.asked_thid {
                    report["pthid"] = json!(thid);
                }
                Ok(Some(Outcome { notices: vec![(follow.asked_by, report)], channels_changed: true, ..Outcome::default() }))
            }
            UNSUBSCRIBE => {
                let id = body["follow"].as_str().unwrap_or_default();
                // Of this one's followers: that community stopped following.
                if let Some(follower) = self.store.follower(id)?.filter(|f| f.kind == "community" && f.target == sender) {
                    self.store.delete_follower(&follower.id)?;
                    self.audit(now, sender, "announcement.unfollow", Some(&follower.channel), Some(json!({ "name": follower.name })));
                    return Ok(Some(Outcome::default()));
                }
                // Of what this one follows: that community took it off.
                let held = self.store.follows()?.into_iter().find(|f| f.source == sender && f.remote.as_deref() == Some(id));
                match held {
                    Some(follow) => {
                        self.store.delete_follow(&follow.id)?;
                        self.audit(now, sender, "follow.delete", Some(&follow.channel), Some(json!({ "community": sender, "name": follow.name })));
                        Ok(Some(Outcome { channels_changed: true, ..Outcome::default() }))
                    }
                    None => Ok(Some(Outcome::default())),
                }
            }
            ANNOUNCEMENT => self.announcement_in(sender, message, now).map(Some),
            _ => Ok(None),
        }
    }

    /// A post of an announcement channel a channel here follows: copied in,
    /// changed, or taken back.
    fn announcement_in(&self, sender: &str, message: &Value, now: i64) -> anyhow::Result<Outcome> {
        let body = &message["body"];
        let id = body["follow"].as_str().unwrap_or_default();
        let held = self.store.follows()?.into_iter().find(|f| !f.pending && f.source == sender && f.remote.as_deref() == Some(id));
        let Some(follow) = held else {
            return Ok(Outcome::reply(json!({ "type": UNSUBSCRIBE, "id": uuid::Uuid::new_v4().to_string(), "body": { "follow": id } })));
        };
        let Some(channel) = self.store.channel(&follow.channel)? else { return Ok(Outcome::default()) };
        // Following into a channel of a kind its host turned off: nothing comes in.
        if self.channel_off(&channel) {
            return Ok(Outcome::default());
        }
        let post = &body["post"];
        let Some(source_post) = post["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 64) else { return Ok(Outcome::default()) };
        let deleted = post["deleted"] == true;
        let content: String = post["content"].as_str().unwrap_or_default().chars().take(self.config.max_post_length).collect();
        let mut extra = json!({ "followed": { "follow": follow.id, "community": follow.source, "name": follow.name, "channel_name": follow.channel_name, "post": source_post } });
        for key in ["media", "attachments"] {
            if let Some(items) = post[key].as_array().filter(|items| !items.is_empty() && items.len() <= 10) {
                extra[key] = json!(items);
            }
        }
        match self.store.followed_post(&follow.id, source_post)? {
            Some(local) => {
                let Some(held) = self.store.post_by_id(&local)?.filter(|p| !p.deleted) else { return Ok(Outcome::default()) };
                if deleted {
                    self.store.delete_post(&held.id, None, self.keeps_deleted_text())?;
                } else if held.content != content || held.extra.as_ref() != Some(&extra) {
                    self.store.edit_post(&held.id, &content, &[], now)?;
                    self.store.set_post_extra(&held.id, &extra)?;
                } else {
                    return Ok(Outcome::default());
                }
                Ok(Outcome { changed: vec![held.id], ..Outcome::default() })
            }
            None if deleted => Ok(Outcome::default()),
            None => {
                let Some(own) = self.own.clone() else { return Ok(Outcome::default()) };
                if content.trim().is_empty() && extra.get("media").is_none() {
                    return Ok(Outcome::default());
                }
                self.store.admit(&own, Role::Member, now)?;
                let Some(mut author) = self.store.member(&own)? else { return Ok(Outcome::default()) };
                author.name = Some(if follow.channel_name.is_empty() { follow.name.clone() } else { format!("{} #{}", follow.name, follow.channel_name) });
                let added = self.store.add_post(&channel.id, &uuid::Uuid::new_v4().to_string(), &content, &author, None, None, now)?;
                self.store.set_post_extra(&added.id, &extra)?;
                self.store.set_followed_post(&follow.id, source_post, &added.id)?;
                Ok(Outcome { posted: true, ..Outcome::default() })
            }
        }
    }

    /// Queues what changed of published posts since last time (edited, taken
    /// back) for their followers. The first time, from now on.
    pub fn queue_changed_announcements(&self, now: i64) -> anyhow::Result<()> {
        let latest = self.store.latest_rev()?;
        let Some(from) = self.store.setting(CURSOR)?.and_then(|s| s.parse::<i64>().ok()) else {
            return Ok(self.store.set_setting(CURSOR, &latest.to_string())?);
        };
        if from >= latest {
            return Ok(());
        }
        let mut last = from;
        for post in self.store.posts_changed_after(from, 200)? {
            last = last.max(post.rev);
            if post.published_at.is_some() {
                self.queue_for_followers(&post, now)?;
            }
        }
        self.store.set_setting(CURSOR, &last.to_string())?;
        Ok(())
    }

    /// Ends every follow of `channel`, which is no longer an announcement
    /// channel: what to send the communities that followed it.
    pub(crate) fn drop_followers(&self, channel: &str) -> anyhow::Result<Vec<(String, Value)>> {
        let mut notices = Vec::new();
        for follower in self.store.followers_of(channel)? {
            if follower.kind == "community" {
                notices.push((follower.target.clone(), json!({ "type": UNSUBSCRIBE, "id": uuid::Uuid::new_v4().to_string(), "body": { "follow": follower.id } })));
            }
            self.store.delete_follower(&follower.id)?;
        }
        Ok(notices)
    }

    // --- The `announcements` workflow -------------------------------------------

    /// The announcement channels `did` manages webhooks in.
    fn announcement_channels_for(&self, did: &str) -> anyhow::Result<Vec<Channel>> {
        let powers = self.powers(did)?;
        let mut channels = Vec::new();
        for channel in self.store.channels()?.into_iter().filter(|c| c.announcement) {
            if self.sees(did, &powers, &channel)? && self.channel_permissions(did, &powers, &channel)?.contains("manage_webhooks") {
                channels.push(channel);
            }
        }
        Ok(channels)
    }

    /// What the channels `did` may manage follow, with each channel.
    fn follows_for(&self, did: &str) -> anyhow::Result<Vec<(Follow, Channel)>> {
        let powers = self.powers(did)?;
        let mut held = Vec::new();
        for follow in self.store.follows()? {
            if let Some(channel) = self.store.channel(&follow.channel)? {
                if self.sees(did, &powers, &channel)? && (powers.has("manage_channels") || channel.created_by == did) {
                    held.push((follow, channel));
                }
            }
        }
        Ok(held)
    }

    /// Whether `did` may run the `announcements` workflow.
    pub fn may_manage_announcements(&self, did: &str) -> anyhow::Result<bool> {
        Ok(!self.announcement_channels_for(did)?.is_empty() || !self.follows_for(did)?.is_empty())
    }

    pub fn announcements_ui(&self, flow: &Flow) -> anyhow::Result<(Value, Vec<&'static str>)> {
        let channels = self.announcement_channels_for(&flow.owner)?;
        let mut items = vec![json!({ "type": "title", "text": "Announcement channels" })];
        if let Some(note) = flow.data["note"].as_str() {
            items.push(json!({ "type": "pass", "text": note }));
        }
        if !channels.is_empty() {
            items.push(json!({ "type": "text", "text": "Whatever follows an announcement channel is sent each post someone publishes there, and its edits. Other communities follow from the channel itself; Discord and other services are added here." }));
            for channel in &channels {
                items.push(json!({ "type": "text", "text": format!("#{}", channel.name) }));
                let followers = self.store.followers_of(&channel.id)?;
                if followers.is_empty() {
                    items.push(json!({ "type": "text", "text": "Nothing follows it yet." }));
                }
                for follower in followers {
                    let kind = match follower.kind.as_str() {
                        "community" => "a community",
                        "discord" => "Discord",
                        _ => "a service",
                    };
                    let off = if follower.off_at.is_some() {
                        " (turned off: it stopped taking posts)"
                    } else if !self.maker_here(&follower.created_by, crate::gates::now()) {
                        " (paused: whoever added it is no longer in the community)"
                    } else {
                        ""
                    };
                    let mut row = vec![json!({ "type": "text", "text": format!("{} · {kind}{off}", follower.name) })];
                    if follower.off_at.is_some() {
                        row.push(json!({ "type": "submit-button", "label": "Turn back on", "event": "on", "input": { "follower": follower.id } }));
                    }
                    row.push(json!({ "type": "submit-button", "label": "Remove", "event": "remove", "input": { "follower": follower.id } }));
                    items.push(json!({ "type": "row", "children": row }));
                    // Another community names itself; a webhook is called what its adder says.
                    if follower.kind != "community" {
                        items.push(json!({ "type": "form", "children": [
                            { "type": "text-input", "form-id": "name", "label": "Called", "value": follower.name, "required": true },
                            { "type": "submit-button", "label": "Rename", "event": "rename", "input": { "follower": follower.id } }
                        ] }));
                    }
                }
            }
            let options: Vec<Value> = channels.iter().map(|c| json!({ "value": c.id, "label": format!("#{}", c.name) })).collect();
            items.push(json!({ "type": "form", "children": [
                { "type": "drop-down", "form-id": "channel", "label": "Send the posts of", "value": channels[0].id, "options": options },
                { "type": "drop-down", "form-id": "kind", "label": "To", "value": "discord", "options": [
                    { "value": "discord", "label": "A Discord webhook" },
                    { "value": "webhook", "label": "Another service (signed)" }
                ] },
                { "type": "text-input", "form-id": "url", "label": "Its URL", "placeholder": "https://discord.com/api/webhooks/...", "required": true },
                { "type": "text-input", "form-id": "name", "label": "Call it", "placeholder": "Our Discord's #news" },
                { "type": "submit-button", "label": "Send posts there", "event": "add", "input": {} }
            ] }));
            if let Some(seed) = &self.signing_seed {
                items.push(json!({ "type": "text", "text": format!("Another service is sent Standard Webhooks events, signed with the secret it is given (v1) and with this community's own key (v1a): {}", public_key(seed)) }));
            }
        }
        let follows = self.follows_for(&flow.owner)?;
        if !follows.is_empty() {
            items.push(json!({ "type": "divider" }));
            items.push(json!({ "type": "title", "text": "Followed here" }));
            for (follow, channel) in &follows {
                let what = if follow.pending { format!("#{} ← {} (waiting for it to answer; given up after a week)", channel.name, follow.source) } else { format!("#{} ← {} #{}", channel.name, follow.name, follow.channel_name) };
                items.push(json!({ "type": "row", "children": [
                    { "type": "text", "text": what },
                    { "type": "submit-button", "label": "Stop following", "event": "unfollow", "input": { "follow": follow.id } }
                ] }));
            }
        }
        items.push(json!({ "type": "submit-button", "label": "Done", "event": "done", "input": {} }));
        Ok((json!(items), vec!["add", "remove", "on", "rename", "unfollow", "done"]))
    }

    pub fn advance_announcements(&self, flow: &mut Flow, event: &str, input: &Value, now: i64) -> Result<bool, Refusal> {
        let channels = self.announcement_channels_for(&flow.owner).map_err(failed)?;
        let follower_of = |id: &Value| -> Result<Follower, Refusal> {
            self.store
                .follower(id.as_str().unwrap_or_default())
                .map_err(failed)?
                .filter(|f| channels.iter().any(|c| c.id == f.channel))
                .ok_or_else(|| Refusal::new("guard_failed", "That is gone."))
        };
        match event {
            "add" => {
                let channel = input["channel"].as_str().and_then(|id| channels.iter().find(|c| c.id == id)).ok_or_else(|| Refusal::new("guard_failed", "Choose an announcement channel you manage webhooks in."))?;
                let given = input["url"].as_str().unwrap_or_default();
                let (kind, url, name, secret) = if input["kind"] == "webhook" {
                    let url = crate::outgoing::check_url(given, self.config.outgoing_private).map_err(|why| Refusal::new("guard_failed", &why))?;
                    let host = url.host_str().unwrap_or_default().to_string();
                    ("webhook", url.to_string(), host, Some(new_secret()))
                } else {
                    let url = check_discord_url(given, self.config.outgoing_private).map_err(|why| Refusal::new("guard_failed", &why))?;
                    ("discord", url.to_string(), "A Discord webhook".to_string(), None)
                };
                let name = match follower_name(&input["name"])? {
                    Some(given) => given,
                    None => name,
                };
                if self.store.followers_of(&channel.id).map_err(failed)?.iter().any(|f| f.target == url) {
                    return Err(Refusal::new("guard_failed", "The channel's posts go there already."));
                }
                let id = new_id();
                // Kept sealed where the service has a seal key (WYV-32); shown plain, this once.
                let kept = secret.as_deref().map(|secret| self.seal_follower_secret(&id, secret)).transpose().map_err(failed)?;
                let follower = Follower { id, channel: channel.id.clone(), kind: kind.into(), target: url.clone(), name, secret: kept, created_by: flow.owner.clone(), created_at: now, failures: 0, off_at: None };
                self.store.save_follower(&follower).map_err(failed)?;
                self.audit(now, &flow.owner, "announcement.webhook", Some(&channel.id), Some(json!({ "kind": kind })));
                flow.data["note"] = json!(match secret {
                    Some(secret) => format!("#{}'s published posts go to {url}. Its signing secret, shown this once: {secret}", channel.name),
                    None => format!("#{}'s published posts go to that Discord webhook from now on.", channel.name),
                });
                Ok(false)
            }
            "remove" => {
                let follower = follower_of(&input["follower"])?;
                if follower.kind == "community" {
                    self.outbox.lock().unwrap_or_else(|p| p.into_inner()).push((follower.target.clone(), json!({ "type": UNSUBSCRIBE, "id": uuid::Uuid::new_v4().to_string(), "body": { "follow": follower.id } })));
                }
                self.store.delete_follower(&follower.id).map_err(failed)?;
                self.audit(now, &flow.owner, "announcement.remove", Some(&follower.channel), Some(json!({ "name": follower.name, "kind": follower.kind })));
                flow.data["note"] = json!(format!("{} no longer follows it.", follower.name));
                Ok(false)
            }
            "rename" => {
                let mut follower = follower_of(&input["follower"])?;
                if follower.kind == "community" {
                    return Err(Refusal::new("guard_failed", "A community is called what it calls itself."));
                }
                follower.name = follower_name(&input["name"])?.ok_or_else(|| Refusal::new("guard_failed", "Give it a name."))?;
                self.store.save_follower(&follower).map_err(failed)?;
                flow.data["note"] = json!(format!("Now called {}.", follower.name));
                Ok(false)
            }
            "on" => {
                let mut follower = follower_of(&input["follower"])?;
                (follower.off_at, follower.failures) = (None, 0);
                self.store.save_follower(&follower).map_err(failed)?;
                flow.data["note"] = json!("Turned back on.");
                Ok(false)
            }
            "unfollow" => {
                let id = input["follow"].as_str().unwrap_or_default();
                let (follow, _) = self.follows_for(&flow.owner).map_err(failed)?.into_iter().find(|(f, _)| f.id == id).ok_or_else(|| Refusal::new("guard_failed", "That is gone."))?;
                let notices = self.end_follow(&follow, &flow.owner, now).map_err(failed)?;
                self.outbox.lock().unwrap_or_else(|p| p.into_inner()).extend(notices);
                flow.data["note"] = json!("Stopped following.");
                Ok(true)
            }
            "done" => {
                flow.status = "completed".into();
                Ok(false)
            }
            _ => Err(Refusal::new("guard_failed", "That can't be done here.")),
        }
    }
}

/// What a webhook follower is called, as given: one line, 80 characters at most;
/// `None` for nothing given.
fn follower_name(given: &Value) -> Result<Option<String>, Refusal> {
    let name = given.as_str().map(str::trim).unwrap_or_default();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > 80 || name.contains(['\n', '\r']) {
        return Err(Refusal::new("guard_failed", "A name is one line of 80 characters at most."));
    }
    Ok(Some(name.to_string()))
}

/// Sends what waits to go to other servers (the outbox), queues what changed of
/// published posts, and sends each follower what is due.
pub async fn send_due(state: &Arc<AppState>, now: i64) -> anyhow::Result<()> {
    let community = &state.community;
    let given_up = community.give_up_unanswered(now)?;
    community.outbox.lock().unwrap_or_else(|p| p.into_inner()).extend(given_up);
    let out: Vec<(String, Value)> = std::mem::take(&mut *community.outbox.lock().unwrap_or_else(|p| p.into_inner()));
    for (did, message) in out {
        if let Err(error) = state.voice.send(&state.agent, &did, &message).await {
            tracing::warn!(%did, %error, "couldn't send to another server");
        }
    }
    community.queue_changed_announcements(now)?;
    for (follower_id, post_id, attempts) in community.store.due_announcements(now, 50)? {
        let done = || community.store.announcement_done(&follower_id, &post_id);
        // Off, or paused since it was queued (its maker gone or banned, WYV-125):
        // what was waiting for it is dropped, not kept for later.
        let Some(mut follower) = community.store.follower(&follower_id)?.filter(|f| f.off_at.is_none() && community.maker_here(&f.created_by, now)) else {
            done()?;
            continue;
        };
        let Some(post) = community.store.post_by_id(&post_id)? else {
            done()?;
            continue;
        };
        let Some(channel) = community.store.channel(&post.channel)?.filter(|c| c.announcement) else {
            done()?;
            continue;
        };
        let body = Community::announced(&post);
        let sent = digest(&body);
        let copy = community.store.announce_copy(&follower.id, &post.id)?;
        if copy.as_ref().is_some_and(|(_, before)| *before == sent) || (copy.is_none() && post.deleted) {
            done()?;
            continue;
        }
        let remote = copy.as_ref().and_then(|(remote, _)| remote.clone());
        let result = match follower.kind.as_str() {
            "community" => {
                let message = json!({ "type": ANNOUNCEMENT, "id": uuid::Uuid::new_v4().to_string(), "body": { "follow": follower.id, "post": body } });
                state.voice.send(&state.agent, &follower.target, &message).await.map(|_| None).map_err(|e| e.to_string())
            }
            "discord" => to_discord(community, &follower, &channel, &post, remote.clone()).await,
            _ => to_service(community, &follower, &channel, &post, copy.is_some(), now).await.map(|_| None),
        };
        match result {
            Ok(made) => {
                community.store.set_announce_copy(&follower.id, &post.id, made.or(remote).as_deref(), &sent)?;
                done()?;
                if follower.failures != 0 {
                    follower.failures = 0;
                    community.store.save_follower(&follower)?;
                }
            }
            Err(why) => {
                tracing::info!(follower = %follower.id, %why, "an announcement wasn't taken; trying again later");
                match crate::outgoing::AGAIN_AFTER.get(attempts as usize) {
                    Some(after) => community.store.announcement_later(&follower.id, &post.id, attempts + 1, now + after)?,
                    None => done()?,
                }
                follower.failures += 1;
                if follower.failures >= crate::outgoing::OFF_AFTER {
                    follower.off_at = Some(now);
                    community.audit(now, &follower.created_by, "announcement.off", Some(&follower.channel), Some(json!({ "name": follower.name, "why": why })));
                }
                community.store.save_follower(&follower)?;
            }
        }
    }
    Ok(())
}

/// What a post says on Discord: its text, and how many files it has here.
/// What a Discord message says of a post: its words, and how many of its files
/// aren't attached to it (`attached` are).
fn discord_content(post: &Post, attached: usize) -> String {
    let files = post.extra.as_ref().and_then(|e| e["media"].as_array()).map_or(0, Vec::len);
    let note = match (files.saturating_sub(attached), attached) {
        (0, _) => String::new(),
        (1, 0) => "\n📎 1 file, in Wyvrn".to_string(),
        (n, 0) => format!("\n📎 {n} files, in Wyvrn"),
        (1, _) => "\n📎 1 more file, in Wyvrn".to_string(),
        (n, _) => format!("\n📎 {n} more files, in Wyvrn"),
    };
    let room = 2000 - note.chars().count();
    let mut content: String = post.content.chars().take(room).collect();
    content.push_str(&note);
    content
}

/// How many files a Discord message takes, and how much of them in all: under
/// what Discord takes from a webhook in a server that isn't boosted (10 MiB).
const DISCORD_FILES: usize = 10;
const DISCORD_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// The files of `post` to attach to its Discord message: those kept here (its
/// files host), opened, in order, while they fit. Sent to Discord they are public
/// there, as the words are: the channel's posts are published to it.
async fn discord_files(community: &Community, post: &Post) -> Vec<(Vec<u8>, String, String)> {
    let (Some(files), Some(extra)) = (&community.files, &post.extra) else { return Vec::new() };
    let mut attached = Vec::new();
    for n in discord_attachable(community, post) {
        if let crate::files::Carried::Opened { bytes, media_type, name } = files.carried(extra, n, DISCORD_FILE_BYTES).await {
            attached.push((bytes, media_type, name));
        }
    }
    attached
}

/// Which of `post`'s files go to Discord with it, by their sizes as kept here: in
/// order, ten at most, while they fit. The same when the post is first sent and
/// when it is changed, so its note counts the same files.
fn discord_attachable(community: &Community, post: &Post) -> Vec<usize> {
    let (Some(files), Some(extra)) = (&community.files, &post.extra) else { return Vec::new() };
    let mut chosen = Vec::new();
    let mut left = DISCORD_FILE_BYTES;
    for n in 0..extra["media"].as_array().map_or(0, Vec::len) {
        if chosen.len() == DISCORD_FILES {
            break;
        }
        if let Some(size) = files.carried_size(extra, n).filter(|size| *size <= left) {
            left -= size;
            chosen.push(n);
        }
    }
    chosen
}

/// Gives the webhook the community's picture as its own, when that picture isn't
/// what it was last given: Modify Webhook with Token, `avatar` as a data URI, which
/// needs no address Discord can reach (a community that isn't public). Made at
/// most 256 pixels a side and a PNG, JPEG or GIF first, which is what Discord
/// takes (`own_picture_small`). Best effort: a post goes out regardless.
async fn discord_avatar(community: &Community, client: &reqwest::Client, base: &reqwest::Url, follower: &Follower) {
    let Some(picture) = community.changed().picture else { return };
    // Named by a digest of the webhook (a setting's name is at most 64 characters).
    let key = format!("discord_avatar:{}", crate::assistant::member_key(&follower.target));
    let now_said = crate::member_ids::picture_hash(&picture.base64);
    if community.store.setting(&key).ok().flatten().as_deref() == Some(now_said.as_str()) {
        return;
    }
    let Some((media_type, bytes)) = community.own_picture_small() else { return };
    let data = format!("data:{media_type};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes));
    match client.patch(base.as_str()).json(&json!({ "avatar": data })).send().await {
        Ok(answer) if answer.status().is_success() => {
            let _ = community.store.set_setting(&key, &now_said);
        }
        Ok(answer) => tracing::warn!(status = %answer.status(), "Discord didn't take the community's picture for its webhook"),
        Err(error) => tracing::warn!(%error, "couldn't give a Discord webhook the community's picture"),
    }
}

/// Sends `post` to a Discord webhook: made, changed or deleted as it stands.
/// What Discord calls the message it made, when it made one.
async fn to_discord(community: &Community, follower: &Follower, channel: &Channel, post: &Post, remote: Option<String>) -> Result<Option<String>, String> {
    let private = community.config.outgoing_private;
    let base = check_discord_url(&follower.target, private)?;
    let at = |id: Option<&str>, wait: bool| {
        let mut url = base.clone();
        if let Some(id) = id {
            url.path_segments_mut().map(|mut s| {
                s.pop_if_empty().push("messages").push(id);
            }).ok();
        }
        if wait {
            url.query_pairs_mut().append_pair("wait", "true");
        }
        url.to_string()
    };
    // Its files go with it when it is first sent; an edit changes the words and
    // leaves them (Discord keeps a message's attachments unless told otherwise).
    let files = if remote.is_none() && !post.deleted { discord_files(community, post).await } else { Vec::new() };
    let attached = match &remote {
        None => files.len(),
        Some(_) => discord_attachable(community, post).len(),
    };
    let content = discord_content(post, attached);
    let (client, _) = crate::outgoing::client_for(base.as_str(), private, "wyvrn-community (announcements)").await?;
    if remote.is_none() && !post.deleted {
        discord_avatar(community, &client, &base, follower).await;
    }
    let answer = match (&remote, post.deleted) {
        (None, _) => {
            let mut payload = json!({ "content": content, "username": discord_username(&format!("{} #{}", community.name_now(), channel.name)), "allowed_mentions": { "parse": [] } });
            // The community's picture beside it, as Discord shows a webhook's: a
            // link, which Discord fetches (so only from an address it can reach).
            if let Some(picture) = community.own_picture_url().filter(|url| url.starts_with("https://")) {
                payload["avatar_url"] = json!(picture);
            }
            if files.is_empty() {
                client.post(at(None, true)).json(&payload).send().await
            } else {
                payload["attachments"] = json!(files.iter().enumerate().map(|(n, (_, _, name))| json!({ "id": n, "filename": name })).collect::<Vec<_>>());
                let mut form = reqwest::multipart::Form::new().text("payload_json", payload.to_string());
                for (n, (bytes, media_type, name)) in files.into_iter().enumerate() {
                    let part = reqwest::multipart::Part::bytes(bytes).file_name(name);
                    let part = part.mime_str(&media_type).map_err(|e| e.to_string())?;
                    form = form.part(format!("files[{n}]"), part);
                }
                client.post(at(None, true)).multipart(form).send().await
            }
        }
        (Some(id), true) => client.delete(at(Some(id), false)).send().await,
        (Some(id), false) => client.patch(at(Some(id), false)).json(&json!({ "content": content, "allowed_mentions": { "parse": [] } })).send().await,
    }
    .map_err(|e| e.to_string())?;
    let status = answer.status();
    // Deleted there already: as good as deleted.
    if remote.is_some() && status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(format!("answered {status}"));
    }
    if remote.is_some() {
        return Ok(None);
    }
    let made: Value = answer.json().await.map_err(|e| e.to_string())?;
    Ok(made["id"].as_str().map(str::to_string))
}

/// Sends `post` to another service as a Standard Webhooks event.
async fn to_service(community: &Community, follower: &Follower, channel: &Channel, post: &Post, sent_before: bool, now: i64) -> Result<(), String> {
    let kind = if post.deleted {
        "announcement.deleted"
    } else if sent_before {
        "announcement.changed"
    } else {
        "announcement.published"
    };
    let mut data = community.post_in_event(post, channel, now);
    data["community"] = json!(community.own.clone().unwrap_or_default());
    let body = json!({ "type": kind, "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true), "data": data }).to_string();
    let id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    let timestamp = chrono::Utc::now().timestamp();
    let secret = community.follower_secret(follower).map_err(|error| {
        tracing::warn!(follower = %follower.id, %error, "an announcement follower's signing secret doesn't open");
        "its signing secret doesn't open".to_string()
    })?;
    let signature = standard_signatures(secret.as_deref(), community.signing_seed.as_ref(), &id, timestamp, &body);
    let (client, url) = crate::outgoing::client_for(&follower.target, community.config.outgoing_private, "wyvrn-community (announcements)").await?;
    let answer = client
        .post(url)
        .header("content-type", "application/json")
        .header("webhook-id", &id)
        .header("webhook-timestamp", timestamp.to_string())
        .header("webhook-signature", signature)
        .body(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if answer.status().is_success() { Ok(()) } else { Err(format!("answered {}", answer.status())) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier};

    #[test]
    fn standard_webhooks_signatures_check_out() {
        // Standard Webhooks' own example: secret, id, timestamp and payload, and
        // the v1 signature it gives for them.
        let secret = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
        let body = r#"{"test": 2432232314}"#;
        let said = standard_signatures(Some(secret), None, "msg_p5jXN8AQM9LWM0D4loKWxJek", 1614265330, body);
        assert_eq!(said, "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=");
        // And v1a, checked with the public key.
        let seed = [7u8; 32];
        let said = standard_signatures(Some(secret), Some(&seed), "msg_1", 5, "{}");
        let v1a = said.split(' ').find_map(|s| s.strip_prefix("v1a,")).unwrap();
        let bytes: [u8; 64] = base64::engine::general_purpose::STANDARD.decode(v1a).unwrap().try_into().unwrap();
        let key = SigningKey::from_bytes(&seed).verifying_key();
        assert!(key.verify(b"msg_1.5.{}", &Signature::from_bytes(&bytes)).is_ok());
        assert!(public_key(&seed).starts_with("whpk_"));
    }

    #[test]
    fn only_discords_webhook_urls_are_taken_as_discord_webhooks() {
        assert!(check_discord_url("https://discord.com/api/webhooks/123/abc", false).is_ok());
        assert!(check_discord_url("https://canary.discord.com/api/webhooks/123/abc?thread_id=9", false).is_ok());
        assert!(check_discord_url("http://discord.com/api/webhooks/123/abc", false).is_err());
        assert!(check_discord_url("https://evil.example/api/webhooks/123/abc", false).is_err());
        assert!(check_discord_url("https://discord.com/channels/1/2", false).is_err());
        // A stand-in beside a server that allows that.
        assert!(check_discord_url("http://127.0.0.1:9/api/webhooks/1/x", true).is_ok());
        assert!(check_discord_url("http://127.0.0.1:9/api/webhooks/1/x", false).is_err());
    }

    #[test]
    fn a_discord_name_never_says_discord() {
        assert_eq!(discord_username("My Discord fans #news"), "My  fans #news");
        assert_eq!(discord_username("discord"), "Wyvrn");
        assert_eq!(discord_username(&"x".repeat(100)).chars().count(), 80);
    }
}

impl Community {
    /// A new follower's signing secret as it is kept: sealed with the service's
    /// seal key where it has one (WYV-32).
    pub fn seal_follower_secret(&self, follower_id: &str, plain: &str) -> anyhow::Result<String> {
        self.sealing().seal(plain, &format!("follower-{follower_id}"))
    }

    /// A follower's signing secret (a Discord webhook has none), opened; kept again
    /// where it should be (found unsealed, or sealed with a previous key).
    pub fn follower_secret(&self, follower: &Follower) -> anyhow::Result<Option<String>> {
        let Some(kept) = &follower.secret else { return Ok(None) };
        let (sealing, what) = (self.sealing(), format!("follower-{}", follower.id));
        let (plain, again) = sealing.open(kept, &what)?;
        if again {
            let mut resealed = follower.clone();
            resealed.secret = Some(sealing.seal(&plain, &what)?);
            self.store.save_follower(&resealed)?;
        }
        Ok(Some(plain))
    }
}
