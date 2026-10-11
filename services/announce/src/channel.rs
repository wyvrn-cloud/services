//! The channel's side of `https://wyvrn.app/announcements/1.0`: what it does with a
//! message, and what it says back. Nothing here touches the network -- the server
//! sends what this decides on (see `server.rs`).

use serde_json::{json, Value};

use crate::store::{Member, Post, Role, Store};

pub const PIURI: &str = "https://wyvrn.app/announcements/1.0";
pub const SUBSCRIBE: &str = "https://wyvrn.app/announcements/1.0/subscribe";
pub const UNSUBSCRIBE: &str = "https://wyvrn.app/announcements/1.0/unsubscribe";
pub const SUBSCRIPTION: &str = "https://wyvrn.app/announcements/1.0/subscription";
pub const POST: &str = "https://wyvrn.app/announcements/1.0/post";
pub const ANNOUNCEMENT: &str = "https://wyvrn.app/announcements/1.0/announcement";
pub const HISTORY_REQUEST: &str = "https://wyvrn.app/announcements/1.0/history-request";
pub const HISTORY: &str = "https://wyvrn.app/announcements/1.0/history";
pub const SUBSCRIBERS_REQUEST: &str = "https://wyvrn.app/announcements/1.0/subscribers-request";
pub const SUBSCRIBERS: &str = "https://wyvrn.app/announcements/1.0/subscribers";
pub const SET_ROLE: &str = "https://wyvrn.app/announcements/1.0/set-role";
/// What the channel tells subscribers of a post's reactions: how many of each, and
/// each subscriber's own. Never who.
pub const REACTIONS: &str = "https://wyvrn.app/announcements/1.0/reactions";

/// How a subscriber reacts to a post: message-reactions/1.0, as in any conversation.
pub const MESSAGE_REACTIONS_PIURI: &str = "https://wyvrn.app/message-reactions/1.0";
pub const MESSAGE_REACTIONS: &str = "https://wyvrn.app/message-reactions/1.0/reactions";
/// How many of an emoji's reactors an admin is named. The count says how many
/// there are; a DID can run to a couple of kilobytes, and a popular post to
/// thousands of reactions.
pub const MAX_NAMED_REACTORS: usize = 50;
/// message-reactions/1.0: a message shows at most this many different emoji.
pub const MAX_DISTINCT_REACTIONS: usize = 20;

pub const USER_PROFILE: &str = "https://didcomm.org/user-profile/1.0";
pub const PROFILE: &str = "https://didcomm.org/user-profile/1.0/profile";
pub const REQUEST_PROFILE: &str = "https://didcomm.org/user-profile/1.0/request-profile";
pub const PROBLEM_REPORT: &str = "https://didcomm.org/report-problem/2.0/problem-report";
/// What a DID change is announced on when there is nothing else to say.
pub const EMPTY: &str = "https://didcomm.org/empty/1.0/empty";

/// How many posts a `history` carries when the request doesn't say, and at most.
pub const DEFAULT_HISTORY_LIMIT: usize = 20;
pub const MAX_HISTORY_LIMIT: usize = 50;
/// How many subscribers a `subscribers` carries when the request doesn't say, and at
/// most: a DID can run to a couple of kilobytes.
pub const DEFAULT_SUBSCRIBERS_LIMIT: usize = 50;
pub const MAX_SUBSCRIBERS_LIMIT: usize = 100;
/// The longest name kept for a subscriber.
const MAX_NAME_LENGTH: usize = 80;

/// What the channel says of itself.
#[derive(Debug, Clone, Default)]
pub struct ChannelInfo {
    pub name: String,
    pub description: String,
    /// Where its web pages are.
    pub url: String,
    /// Its picture: a media type and the bytes, base64.
    pub picture: Option<(String, String)>,
    pub show_authors: bool,
    pub max_post_length: usize,
}

pub struct Channel {
    pub store: Store,
    pub info: ChannelInfo,
}

/// What handling a message came to.
#[derive(Debug, Default)]
pub struct Outcome {
    /// What to answer the sender with, if anything.
    pub reply: Option<Value>,
    /// Messages for others (or for the sender, outside the thread): a DID each.
    pub notices: Vec<(String, Value)>,
    /// Whether there is now something to send to subscribers.
    pub posted: bool,
    /// The post whose reactions changed, which subscribers are to be told of.
    pub reacted: Option<i64>,
}

impl Outcome {
    fn reply(reply: Value) -> Self {
        Self { reply: Some(reply), ..Self::default() }
    }
}

/// Whether `text` could be one emoji. What a reaction arrives as is a stranger's
/// text, shown to everyone: nothing with a letter, a space or a control character
/// in it, nothing long, and something that is at least a symbol. Apps check again
/// before showing it, and the web pages escape it.
pub fn is_emoji(text: &str) -> bool {
    let keycap = text.contains('\u{20E3}');
    !text.is_empty()
        && text.len() <= 64
        && text.chars().count() <= 16
        && text.chars().all(|c| {
            !c.is_alphabetic() && !c.is_whitespace() && !c.is_control() && (!c.is_ascii() || (keycap && "#*0123456789".contains(c)))
        })
        && text.chars().any(|c| c as u32 >= 0xA9)
}

fn thread_of(message: &Value) -> Option<&str> {
    message["thid"].as_str().or(message["id"].as_str())
}

fn reply_to(message: &Value, message_type: &str, body: Value) -> Value {
    let mut reply = json!({"type": message_type, "body": body});
    if let Some(thid) = thread_of(message) {
        reply["thid"] = json!(thid);
    }
    reply
}

/// A report-problem/2.0 `problem-report` about `message`.
pub fn problem(message: &Value, code: &str, comment: &str) -> Value {
    let mut report = json!({"type": PROBLEM_REPORT, "body": {"code": code, "comment": comment}});
    if let Some(thid) = thread_of(message) {
        report["pthid"] = json!(thid);
    }
    if let Some(id) = message["id"].as_str() {
        report["ack"] = json!([id]);
    }
    report
}

impl Channel {
    /// The channel's `subscription` for `member` (or for someone who isn't one).
    pub fn subscription(&self, member: Option<&Member>) -> anyhow::Result<Value> {
        Ok(json!({
            "subscribed": member.is_some_and(|m| m.subscribed),
            "role": member.map_or(Role::Subscriber, |m| m.role).as_str(),
            "channel": {
                "name": self.info.name,
                "description": self.info.description,
                "url": self.info.url,
            },
            "latest_seq": self.store.latest_seq()?,
        }))
    }

    /// One post as subscribers are sent it. `id` is the same for everyone, so that a
    /// copy arriving twice (to two devices that then compare notes, or from the log
    /// after it was pushed) is recognized.
    pub fn announcement(&self, post: &Post) -> Value {
        json!({"id": post.id, "type": ANNOUNCEMENT, "body": self.announcement_body(post)})
    }

    fn announcement_body(&self, post: &Post) -> Value {
        let mut body = json!({"seq": post.seq, "content": post.content, "posted_at": post.posted_at});
        if let Some(author) = self.author_of(post) {
            body["author"] = json!(author);
        }
        body
    }

    /// How many members have each emoji on a post, as an object.
    pub fn reaction_counts(&self, seq: i64) -> anyhow::Result<Value> {
        Ok(Value::Object(self.store.reaction_counts(seq)?.into_iter().map(|(emoji, count)| (emoji, json!(count))).collect()))
    }

    /// A post's reactions as one member is told of them: the counts everyone gets,
    /// and what that member has on it themselves. An admin is also told who: an
    /// admin can already list the channel's subscribers, so it tells them nothing
    /// about who is here that they couldn't find out.
    fn reactions_body(&self, post: &Post, member: &str, now: i64) -> anyhow::Result<Value> {
        let mut body = json!({
            "id": post.id,
            "seq": post.seq,
            "counts": self.reaction_counts(post.seq)?,
            "yours": self.store.reactions_of(post.seq, member)?,
            "at": now,
        });
        if self.store.member(member)?.is_some_and(|m| m.role == Role::Admin) {
            body["who"] = Value::Object(
                self.store
                    .reactors(post.seq)?
                    .into_iter()
                    .map(|(emoji, names)| (emoji, json!(names.into_iter().take(MAX_NAMED_REACTORS).collect::<Vec<_>>())))
                    .collect(),
            );
        }
        Ok(body)
    }

    /// The `reactions` message telling `member` where post `seq`'s reactions stand.
    pub fn reactions_notice(&self, seq: i64, member: &str, now: i64) -> anyhow::Result<Option<Value>> {
        let Some(post) = self.store.post(seq)? else { return Ok(None) };
        Ok(Some(json!({"type": REACTIONS, "body": self.reactions_body(&post, member, now)?})))
    }

    /// Who a post is said to be by, where the channel says.
    pub fn author_of<'a>(&self, post: &'a Post) -> Option<&'a str> {
        post.author_name.as_deref().filter(|_| self.info.show_authors)
    }

    /// The channel's user-profile/1.0 `profile`, in answer to `message`.
    fn profile(&self, message: &Value) -> Value {
        let mut profile = json!({"displayName": self.info.name, "description": self.info.description});
        let mut reply = reply_to(message, PROFILE, json!({}));
        if let Some((media_type, base64)) = &self.info.picture {
            profile["displayPicture"] = json!("#picture");
            reply["attachments"] = json!([{"id": "picture", "media_type": media_type, "data": {"base64": base64}}]);
        }
        reply["body"]["profile"] = profile;
        reply
    }

    fn subscribers_body(&self, request: &Value) -> anyhow::Result<Value> {
        let offset = request["body"]["offset"].as_u64().unwrap_or(0) as usize;
        let limit = (request["body"]["limit"].as_u64().map_or(DEFAULT_SUBSCRIBERS_LIMIT, |l| l as usize))
            .clamp(1, MAX_SUBSCRIBERS_LIMIT);
        let members = self.store.members()?;
        let listed: Vec<Value> = members
            .iter()
            .skip(offset)
            .take(limit)
            .map(|m| {
                json!({
                    "did": m.did,
                    "name": m.name,
                    "role": m.role.as_str(),
                    "subscribed": m.subscribed,
                    "since": m.since,
                })
            })
            .collect();
        Ok(json!({"subscribers": listed, "offset": offset, "total": members.len()}))
    }

    /// Handles one message from `sender`, who is whoever its encryption vouches for.
    pub fn handle(&self, sender: &str, message: &Value, now: i64) -> anyhow::Result<Outcome> {
        let message_type = message["type"].as_str().unwrap_or_default();
        if message_type == PROBLEM_REPORT || message_type == EMPTY {
            return Ok(Outcome::default());
        }
        // A DID a member has left. A device removed from their account still has its
        // keys, and is who would still be using it.
        if self.store.is_superseded(sender)? {
            return Ok(Outcome::reply(problem(
                message,
                "e.p.trust.superseded",
                "This DID has been replaced by another, and is no longer listened to.",
            )));
        }
        let member = self.store.member(sender)?;
        let is_admin = member.as_ref().is_some_and(|m| m.role == Role::Admin);
        let not_admin = || {
            Outcome::reply(problem(message, "e.p.trust.not-authorized", "Only an admin of this channel can do that."))
        };

        match message_type {
            SUBSCRIBE => {
                let member = self.store.subscribe(sender, now)?;
                Ok(Outcome::reply(reply_to(message, SUBSCRIPTION, self.subscription(Some(&member))?)))
            }
            UNSUBSCRIBE => {
                self.store.unsubscribe(sender)?;
                let member = self.store.member(sender)?;
                Ok(Outcome::reply(reply_to(message, SUBSCRIPTION, self.subscription(member.as_ref())?)))
            }
            POST => {
                let Some(author) = member.filter(|_| is_admin) else { return Ok(not_admin()) };
                let content = message["body"]["content"].as_str().unwrap_or_default().trim();
                if content.is_empty() {
                    return Ok(Outcome::reply(problem(message, "e.p.msg.empty", "A post needs some content.")));
                }
                if content.chars().count() > self.info.max_post_length {
                    return Ok(Outcome::reply(problem(
                        message,
                        "e.p.msg.too-long",
                        &format!("A post can be at most {} characters.", self.info.max_post_length),
                    )));
                }
                let post = self.store.add_post(&uuid::Uuid::new_v4().to_string(), content, &author, now)?;
                // Answered with what it became; every subscriber, the author among
                // them, is sent the same announcement under the same id.
                let mut reply = self.announcement(&post);
                if let Some(thid) = thread_of(message) {
                    reply["thid"] = json!(thid);
                }
                Ok(Outcome { reply: Some(reply), posted: true, ..Outcome::default() })
            }
            HISTORY_REQUEST => {
                let limit = (message["body"]["limit"].as_u64().map_or(DEFAULT_HISTORY_LIMIT, |l| l as usize))
                    .clamp(1, MAX_HISTORY_LIMIT);
                let posts = match message["body"]["after_seq"].as_i64() {
                    Some(after) => self.store.posts_after(after, limit)?,
                    None => {
                        let mut posts = self.store.posts_before(message["body"]["before_seq"].as_i64(), limit)?;
                        posts.reverse();
                        posts
                    }
                };
                let announcements: Vec<Value> = posts
                    .iter()
                    .map(|post| {
                        let mut entry = self.announcement_body(post);
                        entry["id"] = json!(post.id);
                        // Only where there are any: most posts have none.
                        if let Ok(reactions) = self.reactions_body(post, sender, now) {
                            if reactions["counts"].as_object().is_some_and(|counts| !counts.is_empty()) {
                                let mut account = json!({"counts": reactions["counts"], "yours": reactions["yours"], "at": now});
                                if let Some(who) = reactions.get("who") {
                                    account["who"] = who.clone();
                                }
                                entry["reactions"] = account;
                            }
                        }
                        entry
                    })
                    .collect();
                Ok(Outcome::reply(reply_to(
                    message,
                    HISTORY,
                    json!({"announcements": announcements, "latest_seq": self.store.latest_seq()?}),
                )))
            }
            MESSAGE_REACTIONS => {
                // A subscriber's whole set of reactions to one post. Reacting is not
                // posting: it is open to every subscriber, and says nothing.
                if !member.as_ref().is_some_and(|m| m.subscribed) {
                    return Ok(Outcome::reply(problem(
                        message,
                        "e.p.trust.not-authorized",
                        "Only a subscriber of this channel can react to its posts.",
                    )));
                }
                // A statement that doesn't say when can't be put in order, and one
                // about something that isn't a post here is about nothing.
                let (Some(stated_at), Some(target)) = (message["created_time"].as_i64(), message["body"]["to"]["id"].as_str())
                else {
                    return Ok(Outcome::default());
                };
                let Some(post) = self.store.post_by_id(target)? else { return Ok(Outcome::default()) };
                let already: Vec<String> = self.store.reaction_counts(post.seq)?.into_iter().map(|(emoji, _)| emoji).collect();
                let mut distinct = already.len();
                let mut emoji: Vec<String> = Vec::new();
                for entry in message["body"]["emoji"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    if !is_emoji(entry) || emoji.iter().any(|e| e == entry) {
                        continue;
                    }
                    // One already on the post can always be joined; a new one only
                    // while the post has room.
                    if !already.iter().any(|e| e == entry) {
                        if distinct >= MAX_DISTINCT_REACTIONS {
                            continue;
                        }
                        distinct += 1;
                    }
                    emoji.push(entry.to_string());
                }
                let changed = self.store.set_reactions(post.seq, sender, &emoji, stated_at)?;
                Ok(Outcome { reacted: changed.then_some(post.seq), ..Outcome::default() })
            }
            SUBSCRIBERS_REQUEST => {
                if !is_admin {
                    return Ok(not_admin());
                }
                Ok(Outcome::reply(reply_to(message, SUBSCRIBERS, self.subscribers_body(message)?)))
            }
            SET_ROLE => {
                if !is_admin {
                    return Ok(not_admin());
                }
                let target = message["body"]["did"].as_str().unwrap_or_default();
                let Some(role) = message["body"]["role"].as_str().and_then(Role::parse) else {
                    return Ok(Outcome::reply(problem(
                        message,
                        "e.p.msg.invalid-role",
                        "A role is \"admin\" or \"subscriber\".",
                    )));
                };
                let Some(before) = self.store.member(target)? else {
                    return Ok(Outcome::reply(problem(
                        message,
                        "e.p.msg.unknown-subscriber",
                        "Nobody with that DID is subscribed to this channel.",
                    )));
                };
                let admins = self.store.members()?.iter().filter(|m| m.role == Role::Admin).count();
                if before.role == Role::Admin && role == Role::Subscriber && admins <= 1 {
                    return Ok(Outcome::reply(problem(
                        message,
                        "e.p.msg.last-admin",
                        "This is the channel's only admin. Make someone else one first.",
                    )));
                }
                self.store.set_role(target, role, now)?;
                let mut outcome = Outcome::reply(reply_to(message, SUBSCRIBERS, self.subscribers_body(message)?));
                // The one whose standing changed is told, wherever it is subscribed.
                if let Some(after) = self.store.member(target)?.filter(|m| m.subscribed && m.role != before.role) {
                    outcome.notices.push((after.did.clone(), json!({"type": SUBSCRIPTION, "body": self.subscription(Some(&after))?})));
                }
                Ok(outcome)
            }
            PROFILE => {
                let profile = &message["body"]["profile"];
                // Absent means no change (user-profile/1.0); present and empty clears.
                if profile.get("displayName").is_some() && member.is_some() {
                    let name: Option<String> = profile["displayName"]
                        .as_str()
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(|name| name.chars().take(MAX_NAME_LENGTH).collect());
                    self.store.set_name(sender, name.as_deref())?;
                }
                Ok(if message["body"]["send_back_yours"] == true {
                    Outcome::reply(self.profile(message))
                } else {
                    Outcome::default()
                })
            }
            REQUEST_PROFILE => Ok(Outcome::reply(self.profile(message))),
            other if other.starts_with("https://didcomm.org/basicmessage/") => Ok(Outcome::reply(problem(
                message,
                "e.p.msg.not-accepted",
                "This is an announcement channel: it doesn't take messages.",
            ))),
            other => Ok(Outcome::reply(problem(message, "e.p.msg.unsupported", &format!("Unsupported message type {other}")))),
        }
    }

    /// A verified `from_prior`: whoever was `prior` is `new` now. Returns what to
    /// tell them, if the channel knew them -- which also shows their app the news
    /// arrived, so it can stop repeating it.
    pub fn rotated(&self, prior: &str, new: &str, now: i64) -> anyhow::Result<Option<Value>> {
        if !self.store.rotate(prior, new, now)? {
            return Ok(None);
        }
        let member = self.store.member(new)?;
        Ok(Some(json!({"type": SUBSCRIPTION, "body": self.subscription(member.as_ref())?})))
    }

    /// Makes the DIDs the configuration names admins, unless they are DIDs a member
    /// has since left.
    pub fn name_admins(&self, dids: &[String], now: i64) -> anyhow::Result<()> {
        for did in dids {
            if self.store.is_superseded(did)? {
                tracing::warn!(%did, "a configured admin DID has been replaced by another, and is left alone");
                continue;
            }
            self.store.set_role(did, Role::Admin, now)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> Channel {
        Channel {
            store: Store::in_memory().unwrap(),
            info: ChannelInfo {
                name: "Wyvrn Updates".into(),
                description: "Release notes".into(),
                url: "https://updates.example/".into(),
                picture: None,
                show_authors: false,
                max_post_length: 100,
            },
        }
    }

    fn message(message_type: &str, body: Value) -> Value {
        json!({"id": "m1", "type": message_type, "body": body})
    }

    fn with_admin(channel: &Channel, did: &str) {
        channel.store.set_role(did, Role::Admin, 1).unwrap();
        channel.store.subscribe(did, 1).unwrap();
    }

    fn code(outcome: &Outcome) -> &str {
        let reply = outcome.reply.as_ref().expect("a reply");
        assert_eq!(reply["type"], PROBLEM_REPORT, "{reply}");
        reply["body"]["code"].as_str().unwrap()
    }

    #[test]
    fn subscribing_is_answered_with_where_the_subscriber_stands() {
        let channel = channel();
        let outcome = channel.handle("did:s", &message(SUBSCRIBE, json!({})), 10).unwrap();
        let reply = outcome.reply.unwrap();
        assert_eq!(reply["type"], SUBSCRIPTION);
        assert_eq!(reply["thid"], "m1");
        assert_eq!(
            reply["body"],
            json!({
                "subscribed": true,
                "role": "subscriber",
                "channel": {"name": "Wyvrn Updates", "description": "Release notes", "url": "https://updates.example/"},
                "latest_seq": 0,
            })
        );
        assert!(!outcome.posted);
    }

    #[test]
    fn only_an_admin_can_post() {
        let channel = channel();
        channel.handle("did:s", &message(SUBSCRIBE, json!({})), 10).unwrap();
        let refused = channel.handle("did:s", &message(POST, json!({"content": "hello"})), 11).unwrap();
        assert_eq!(code(&refused), "e.p.trust.not-authorized");
        assert!(!refused.posted);
        // Nor someone the channel has never heard of.
        assert_eq!(code(&channel.handle("did:x", &message(POST, json!({"content": "hello"})), 11).unwrap()), "e.p.trust.not-authorized");
        assert_eq!(channel.store.latest_seq().unwrap(), 0);
    }

    #[test]
    fn an_admins_post_is_logged_and_answered_with_the_announcement_it_became() {
        let channel = channel();
        with_admin(&channel, "did:a");
        let outcome = channel.handle("did:a", &message(POST, json!({"content": "  Version 1 is out  "})), 20).unwrap();
        assert!(outcome.posted);
        let reply = outcome.reply.unwrap();
        assert_eq!(reply["type"], ANNOUNCEMENT);
        assert_eq!(reply["thid"], "m1");
        assert_eq!(reply["body"], json!({"seq": 1, "content": "Version 1 is out", "posted_at": 20}));
        let post = channel.store.post(1).unwrap().unwrap();
        assert_eq!(reply["id"], post.id.as_str());
        assert_eq!(post.author_did, "did:a");
        // What every subscriber is sent is that same announcement.
        assert_eq!(channel.announcement(&post)["id"], reply["id"]);
    }

    #[test]
    fn a_post_needs_content_of_a_bearable_length() {
        let channel = channel();
        with_admin(&channel, "did:a");
        assert_eq!(code(&channel.handle("did:a", &message(POST, json!({"content": "   "})), 20).unwrap()), "e.p.msg.empty");
        assert_eq!(code(&channel.handle("did:a", &message(POST, json!({})), 20).unwrap()), "e.p.msg.empty");
        let long = "x".repeat(101);
        assert_eq!(code(&channel.handle("did:a", &message(POST, json!({"content": long})), 20).unwrap()), "e.p.msg.too-long");
        assert_eq!(channel.store.latest_seq().unwrap(), 0);
    }

    #[test]
    fn a_post_says_who_wrote_it_only_where_the_channel_does() {
        let mut channel = channel();
        with_admin(&channel, "did:a");
        channel.store.set_name("did:a", Some("Ada")).unwrap();
        let hidden = channel.handle("did:a", &message(POST, json!({"content": "one"})), 20).unwrap().reply.unwrap();
        assert!(hidden["body"].get("author").is_none());
        channel.info.show_authors = true;
        let shown = channel.handle("did:a", &message(POST, json!({"content": "two"})), 21).unwrap().reply.unwrap();
        assert_eq!(shown["body"]["author"], "Ada");
    }

    #[test]
    fn nobody_can_talk_back() {
        let channel = channel();
        with_admin(&channel, "did:a");
        channel.handle("did:s", &message(SUBSCRIBE, json!({})), 10).unwrap();
        for sender in ["did:s", "did:a"] {
            let outcome = channel
                .handle(sender, &message("https://didcomm.org/basicmessage/2.0/message", json!({"content": "hi"})), 11)
                .unwrap();
            assert_eq!(code(&outcome), "e.p.msg.not-accepted");
            assert!(!outcome.posted);
        }
        assert_eq!(channel.store.latest_seq().unwrap(), 0);
    }

    #[test]
    fn the_log_is_there_for_the_asking_oldest_first() {
        let channel = channel();
        with_admin(&channel, "did:a");
        for text in ["one", "two", "three"] {
            channel.handle("did:a", &message(POST, json!({"content": text})), 20).unwrap();
        }
        let seqs = |body: Value| -> Vec<i64> {
            let reply = channel.handle("did:anyone", &message(HISTORY_REQUEST, body), 30).unwrap().reply.unwrap();
            assert_eq!(reply["type"], HISTORY);
            assert_eq!(reply["body"]["latest_seq"], 3);
            reply["body"]["announcements"].as_array().unwrap().iter().map(|a| a["seq"].as_i64().unwrap()).collect()
        };
        assert_eq!(seqs(json!({})), [1, 2, 3]);
        assert_eq!(seqs(json!({"limit": 2})), [2, 3]);
        assert_eq!(seqs(json!({"before_seq": 3, "limit": 1})), [2]);
        assert_eq!(seqs(json!({"after_seq": 1})), [2, 3]);
        let reply = channel.handle("did:anyone", &message(HISTORY_REQUEST, json!({"limit": 1})), 30).unwrap().reply.unwrap();
        let entry = &reply["body"]["announcements"][0];
        assert_eq!(entry["id"], channel.store.post(3).unwrap().unwrap().id.as_str());
        assert_eq!(entry["content"], "three");
    }

    #[test]
    fn an_admin_makes_another_and_the_one_made_is_told() {
        let channel = channel();
        with_admin(&channel, "did:a");
        channel.handle("did:s", &message(SUBSCRIBE, json!({})), 10).unwrap();
        // Not something a subscriber can do, to themselves or anyone.
        let refused = channel.handle("did:s", &message(SET_ROLE, json!({"did": "did:s", "role": "admin"})), 11).unwrap();
        assert_eq!(code(&refused), "e.p.trust.not-authorized");
        assert_eq!(code(&channel.handle("did:s", &message(SUBSCRIBERS_REQUEST, json!({})), 11).unwrap()), "e.p.trust.not-authorized");

        let outcome = channel.handle("did:a", &message(SET_ROLE, json!({"did": "did:s", "role": "admin"})), 12).unwrap();
        let reply = outcome.reply.unwrap();
        assert_eq!(reply["type"], SUBSCRIBERS);
        assert_eq!(reply["body"]["total"], 2);
        assert_eq!(outcome.notices.len(), 1);
        assert_eq!(outcome.notices[0].0, "did:s");
        assert_eq!(outcome.notices[0].1["type"], SUBSCRIPTION);
        assert_eq!(outcome.notices[0].1["body"]["role"], "admin");
        assert!(channel.handle("did:s", &message(POST, json!({"content": "now I can"})), 13).unwrap().posted);
    }

    #[test]
    fn the_last_admin_cannot_be_unmade() {
        let channel = channel();
        with_admin(&channel, "did:a");
        let refused = channel.handle("did:a", &message(SET_ROLE, json!({"did": "did:a", "role": "subscriber"})), 12).unwrap();
        assert_eq!(code(&refused), "e.p.msg.last-admin");
        with_admin(&channel, "did:b");
        let outcome = channel.handle("did:a", &message(SET_ROLE, json!({"did": "did:a", "role": "subscriber"})), 13).unwrap();
        assert_eq!(outcome.reply.unwrap()["type"], SUBSCRIBERS);
        assert_eq!(code(&channel.handle("did:a", &message(POST, json!({"content": "x"})), 14).unwrap()), "e.p.trust.not-authorized");
    }

    #[test]
    fn a_role_is_only_given_to_someone_the_channel_knows() {
        let channel = channel();
        with_admin(&channel, "did:a");
        assert_eq!(
            code(&channel.handle("did:a", &message(SET_ROLE, json!({"did": "did:nobody", "role": "admin"})), 12).unwrap()),
            "e.p.msg.unknown-subscriber"
        );
        assert_eq!(
            code(&channel.handle("did:a", &message(SET_ROLE, json!({"did": "did:a", "role": "owner"})), 12).unwrap()),
            "e.p.msg.invalid-role"
        );
    }

    #[test]
    fn an_admin_who_changes_did_is_still_one_and_the_old_did_is_nobody() {
        let channel = channel();
        with_admin(&channel, "did:old");
        let told = channel.rotated("did:old", "did:new", 30).unwrap().unwrap();
        assert_eq!(told["type"], SUBSCRIPTION);
        assert_eq!(told["body"]["role"], "admin");
        assert!(channel.handle("did:new", &message(POST, json!({"content": "still me"})), 31).unwrap().posted);
        // The DID left behind: a device since removed from the account would use it.
        for (message_type, body) in [(POST, json!({"content": "it's me"})), (SUBSCRIBE, json!({}))] {
            assert_eq!(code(&channel.handle("did:old", &message(message_type, body), 32).unwrap()), "e.p.trust.superseded");
        }
        assert_eq!(channel.store.latest_seq().unwrap(), 1);
        // And naming it in the configuration again doesn't bring it back.
        channel.name_admins(&["did:old".to_string()], 33).unwrap();
        assert!(channel.store.member("did:old").unwrap().is_none());
    }

    #[test]
    fn a_change_of_did_by_a_stranger_makes_them_nothing() {
        let channel = channel();
        with_admin(&channel, "did:a");
        assert!(channel.rotated("did:stranger", "did:other", 30).unwrap().is_none());
        assert_eq!(code(&channel.handle("did:other", &message(POST, json!({"content": "x"})), 31).unwrap()), "e.p.trust.not-authorized");
    }

    #[test]
    fn the_channel_has_a_profile_and_keeps_a_subscribers_name() {
        let mut channel = channel();
        channel.info.picture = Some(("image/png".into(), "aGk=".into()));
        channel.handle("did:s", &message(SUBSCRIBE, json!({})), 10).unwrap();
        let outcome = channel
            .handle("did:s", &message(PROFILE, json!({"profile": {"displayName": " Sam "}, "send_back_yours": true})), 11)
            .unwrap();
        let reply = outcome.reply.unwrap();
        assert_eq!(reply["type"], PROFILE);
        assert_eq!(reply["body"]["profile"]["displayName"], "Wyvrn Updates");
        assert_eq!(reply["body"]["profile"]["displayPicture"], "#picture");
        assert_eq!(reply["attachments"][0]["data"]["base64"], "aGk=");
        assert_eq!(channel.store.member("did:s").unwrap().unwrap().name.as_deref(), Some("Sam"));
        // A profile that doesn't ask for one back gets nothing.
        assert!(channel.handle("did:s", &message(PROFILE, json!({"profile": {}})), 12).unwrap().reply.is_none());
        assert_eq!(channel.store.member("did:s").unwrap().unwrap().name.as_deref(), Some("Sam"));
    }

    fn react(channel: &Channel, did: &str, post_id: &str, emoji: Value, at: i64) -> Outcome {
        let statement = json!({
            "id": "r", "type": MESSAGE_REACTIONS, "created_time": at,
            "body": {"to": {"id": post_id, "author": "did:channel"}, "emoji": emoji},
        });
        channel.handle(did, &statement, at).unwrap()
    }

    fn posted(channel: &Channel) -> Post {
        with_admin(channel, "did:a");
        channel.handle("did:a", &message(POST, json!({"content": "Version 1"})), 20).unwrap();
        channel.store.post(1).unwrap().unwrap()
    }

    #[test]
    fn subscribers_react_and_everyone_is_told_how_many_never_who() {
        let channel = channel();
        let post = posted(&channel);
        channel.handle("did:s", &message(SUBSCRIBE, json!({})), 21).unwrap();
        assert_eq!(react(&channel, "did:s", &post.id, json!(["👍", "❤️"]), 30).reacted, Some(1));
        assert_eq!(react(&channel, "did:a", &post.id.to_uppercase(), json!(["👍"]), 31).reacted, Some(1));

        let told = channel.reactions_notice(1, "did:s", 40).unwrap().unwrap();
        assert_eq!(told["type"], REACTIONS);
        assert_eq!(told["body"], json!({"id": post.id, "seq": 1, "counts": {"👍": 2, "❤️": 1}, "yours": ["👍", "❤️"], "at": 40}));
        assert!(!told.to_string().contains("did:a"));
        // An admin, who can list the subscribers anyway, is told who as well: by
        // name where they gave one.
        channel.store.set_name("did:s", Some("Sam")).unwrap();
        let admin = channel.reactions_notice(1, "did:a", 40).unwrap().unwrap();
        assert_eq!(admin["body"]["yours"], json!(["👍"]));
        assert_eq!(admin["body"]["who"], json!({"👍": ["Sam", "did:a"], "❤️": ["Sam"]}));
        let asked = channel.handle("did:a", &message(HISTORY_REQUEST, json!({})), 41).unwrap().reply.unwrap();
        assert_eq!(asked["body"]["announcements"][0]["reactions"]["who"]["❤️"], json!(["Sam"]));

        // The log says the same to whoever asks for it.
        let history = channel.handle("did:s", &message(HISTORY_REQUEST, json!({})), 41).unwrap().reply.unwrap();
        assert_eq!(history["body"]["announcements"][0]["reactions"]["counts"], json!({"👍": 2, "❤️": 1}));
        assert_eq!(history["body"]["announcements"][0]["reactions"]["yours"], json!(["👍", "❤️"]));
        assert!(history["body"]["announcements"][0]["reactions"].get("who").is_none());
    }

    #[test]
    fn a_statement_replaces_the_last_unless_it_is_older_and_an_empty_one_removes() {
        let channel = channel();
        let post = posted(&channel);
        channel.handle("did:s", &message(SUBSCRIBE, json!({})), 21).unwrap();
        react(&channel, "did:s", &post.id, json!(["👍"]), 30);
        assert_eq!(react(&channel, "did:s", &post.id, json!(["❤️"]), 40).reacted, Some(1));
        assert_eq!(react(&channel, "did:s", &post.id, json!(["👍"]), 35).reacted, None);
        assert_eq!(react(&channel, "did:s", &post.id, json!(["❤️"]), 45).reacted, None, "saying the same again is no change");
        assert_eq!(channel.reaction_counts(1).unwrap(), json!({"❤️": 1}));
        assert_eq!(react(&channel, "did:s", &post.id, json!([]), 50).reacted, Some(1));
        assert_eq!(channel.reaction_counts(1).unwrap(), json!({}));
        assert_eq!(react(&channel, "did:s", &post.id, json!(["👍"]), 48).reacted, None);
    }

    #[test]
    fn only_a_subscriber_reacts_only_to_a_post_and_only_with_emoji() {
        let channel = channel();
        let post = posted(&channel);
        assert_eq!(code(&react(&channel, "did:stranger", &post.id, json!(["👍"]), 30)), "e.p.trust.not-authorized");
        channel.handle("did:s", &message(SUBSCRIBE, json!({})), 21).unwrap();
        let nothing = react(&channel, "did:s", "no-such-post", json!(["👍"]), 30);
        assert!(nothing.reply.is_none() && nothing.reacted.is_none());
        react(&channel, "did:s", &post.id, json!(["<b>hi</b>", "ok", "👍", "👍", " ", "1️⃣", "7", 5]), 31);
        assert_eq!(channel.store.reactions_of(1, "did:s").unwrap(), ["👍", "1️⃣"]);
        // Reacting is not a way to post.
        assert_eq!(channel.store.latest_seq().unwrap(), 1);
    }

    #[test]
    fn a_post_takes_twenty_different_emoji_and_no_more() {
        let channel = channel();
        let post = posted(&channel);
        channel.handle("did:s", &message(SUBSCRIBE, json!({})), 21).unwrap();
        let many: Vec<String> = (0..25).map(|i| char::from_u32(0x1F600 + i).unwrap().to_string()).collect();
        react(&channel, "did:a", &post.id, json!(many[..20]), 30);
        // A twenty-first isn't taken, though one already there can be joined.
        react(&channel, "did:s", &post.id, json!([many[22], many[0]]), 31);
        assert_eq!(channel.store.reactions_of(1, "did:s").unwrap(), [many[0].clone()]);
        assert_eq!(channel.store.reaction_counts(1).unwrap().len(), MAX_DISTINCT_REACTIONS);
    }

    #[test]
    fn reactions_go_with_a_member_who_changes_did() {
        let channel = channel();
        let post = posted(&channel);
        channel.handle("did:s", &message(SUBSCRIBE, json!({})), 21).unwrap();
        react(&channel, "did:s", &post.id, json!(["👍"]), 30);
        channel.rotated("did:s", "did:s2", 40).unwrap();
        assert_eq!(channel.store.reactions_of(1, "did:s2").unwrap(), ["👍"]);
        react(&channel, "did:s2", &post.id, json!([]), 50);
        assert_eq!(channel.reaction_counts(1).unwrap(), json!({}));
    }

    #[test]
    fn a_problem_report_or_an_empty_message_is_never_answered() {
        let channel = channel();
        for message_type in [PROBLEM_REPORT, EMPTY] {
            let outcome = channel.handle("did:s", &message(message_type, json!({})), 10).unwrap();
            assert!(outcome.reply.is_none() && outcome.notices.is_empty());
        }
    }
}
