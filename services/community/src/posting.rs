//! What a post may carry beyond its text, and what is done with posts beyond
//! placing them (community/1.0's "Files, stickers…", "Polls", "Forum channels",
//! "Searching and reading back" and "The member list"): media and attachments, a
//! poll and its votes, a forum thread's title and tags, search, history from a
//! time, and who is seen online.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use crate::community::{line, refuse, reply_to, Community, Outcome, CHANGED, COMMUNITY, HISTORY};
use crate::store::{Channel, Member, Post};

pub const VOTE: &str = "https://wyvrn.app/community/1.0/vote";
pub const SEARCH_REQUEST: &str = "https://wyvrn.app/community/1.0/search-request";
pub const SEARCH_RESULTS: &str = "https://wyvrn.app/community/1.0/search-results";
pub const SET_PRESENCE: &str = "https://wyvrn.app/community/1.0/set-presence";
pub const MEMBER_PRESENCE: &str = "https://wyvrn.app/community/1.0/member-presence";
pub const WATCH: &str = "https://wyvrn.app/community/1.0/watch";
pub const SET_NOTIFICATIONS: &str = "https://wyvrn.app/community/1.0/set-notifications";

/// What reaches a member through the mediator while they aren't watching, and
/// what a member's app is taken to have chosen once it has spoken of it.
pub const LEVELS: &[&str] = &["all", "mentions", "nothing"];
pub const DEFAULT_LEVEL: &str = "mentions";

const MAX_ATTACHMENTS: usize = 10;
const MAX_POLL_ANSWERS: usize = 10;
const MAX_POLL_ANSWER: usize = 55;
const MAX_POLL_QUESTION: usize = 300;
const POLL_SHORTEST: i64 = 3600;
const POLL_LONGEST: i64 = 32 * 24 * 3600;
const MAX_TITLE: usize = 100;
const MAX_TAGS: usize = 20;
const MAX_TAG_NAME: usize = 20;
const MAX_POST_TAGS: usize = 5;
const STATUSES: &[&str] = &["online", "idle", "dnd"];
const MAX_PREVIEWS: usize = 5;
const MAX_COPY_MEDIA: usize = 4;
const MAX_COPY_TEXT: usize = 4000;
const MAX_COPY_NAME: usize = 64;
const MAX_PREVIEW_TITLE: usize = 300;
const MAX_PREVIEW_DESCRIPTION: usize = 1000;
const MAX_PREVIEW_NAME: usize = 100;
const MAX_ID: usize = 256;

/// What a `post` carries beyond its text, checked: `media` and `attachments`, a
/// `poll`, a forum's `title` and `tags`. `Err` says what is wrong, as
/// `(code, comment)`.
pub fn read_extra(body: &Value, channel: &Channel, in_thread: bool, now: i64) -> Result<Option<Value>, (&'static str, String)> {
    let mut extra = Map::new();
    if let Some(media) = body.get("media").filter(|v| !v.is_null()) {
        let (Some(media), Some(attachments)) = (media.as_array(), body["attachments"].as_array()) else {
            return Err(("e.p.msg.invalid-media", "Media are a list, each naming one of the attachments.".into()));
        };
        if media.is_empty() || media.len() > MAX_ATTACHMENTS || attachments.len() > MAX_ATTACHMENTS {
            return Err(("e.p.msg.invalid-media", "A post carries one to ten things.".into()));
        }
        check_attachments(attachments)?;
        let ids: Vec<&str> = attachments.iter().filter_map(|a| a["id"].as_str()).collect();
        if !media.iter().all(|item| item["attachment"].as_str().is_some_and(|id| ids.contains(&id))) {
            return Err(("e.p.msg.invalid-media", "Each media item names one of the attachments.".into()));
        }
        extra.insert("media".into(), json!(media));
        extra.insert("attachments".into(), json!(attachments));
    }
    if let Some(given) = body.get("previews").filter(|v| !v.is_null()) {
        let (previews, attachments) = read_previews(given, body)?;
        if !previews.is_empty() {
            extra.insert("previews".into(), json!(previews));
            with_attachments(&mut extra, attachments);
        }
    }
    if let Some(poll) = body.get("poll").filter(|v| !v.is_null()) {
        extra.insert("poll".into(), read_poll(poll, now).map_err(|why| ("e.p.msg.invalid-poll", why))?);
    }
    if let Some(tags) = &channel.forum {
        if !in_thread {
            // A gallery's post is a picture or video, its title optional.
            if channel.gallery && !extra.contains_key("media") {
                return Err(("e.p.msg.media-needed", "A post in a gallery has a picture or a video.".to_string()));
            }
            let title = match line(&body["title"], MAX_TITLE) {
                Some(title) => Some(title),
                None if channel.gallery => None,
                None => return Err(("e.p.msg.title-needed", "A post in a forum begins a thread, and needs a title.".to_string())),
            };
            let offered: Vec<&str> = tags.as_array().into_iter().flatten().filter_map(|t| t["id"].as_str()).collect();
            let chosen: Vec<String> = body["tags"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|t| offered.contains(t)).take(MAX_POST_TAGS).map(str::to_string).collect();
            if let Some(title) = title {
                extra.insert("title".into(), json!(title));
            }
            if !chosen.is_empty() {
                extra.insert("tags".into(), json!(chosen));
            }
        }
    }
    Ok((!extra.is_empty()).then_some(Value::Object(extra)))
}

/// Each of a post's attachments by reference: an id, web links and a hash.
fn check_attachments(attachments: &[Value]) -> Result<(), (&'static str, String)> {
    for attachment in attachments {
        // (http as well as https: what is fetched is checked against the hash.)
        let web = |link: &str| link.starts_with("https://") || link.starts_with("http://");
        let by_reference = attachment["data"]["links"].as_array().is_some_and(|links| !links.is_empty() && links.iter().all(|l| l.as_str().is_some_and(web)));
        if attachment["id"].as_str().is_none() || !by_reference || attachment["data"]["hash"].as_str().is_none() {
            return Err(("e.p.msg.invalid-media", "Each attachment is by reference: an id, web links and a hash.".into()));
        }
    }
    Ok(())
}

/// `attachments` among a post's extras, beside any its media named, each once.
fn with_attachments(extra: &mut Map<String, Value>, more: Vec<Value>) {
    if more.is_empty() {
        return;
    }
    let mut held: Vec<Value> = extra.get("attachments").and_then(Value::as_array).cloned().unwrap_or_default();
    for attachment in more {
        if !held.iter().any(|a| a["id"] == attachment["id"]) {
            held.push(attachment);
        }
    }
    extra.insert("attachments".into(), json!(held));
}

/// A post's previews of its links, as its author built them (community/1.0's
/// `previews`, chat-message/1.0's link previews), checked and with nothing in
/// them but what is named: each for a link `content` has, and a copy of a post
/// (`post`) as the spec gives it. With them, the attachments they name, out of
/// `body`'s. Shown only as the author's say: nothing in them is the server's.
pub fn read_previews(given: &Value, body: &Value) -> Result<(Vec<Value>, Vec<Value>), (&'static str, String)> {
    let bad = |why: &str| ("e.p.msg.invalid-preview", why.to_string());
    let given = given.as_array().ok_or_else(|| bad("Previews are a list."))?;
    if given.len() > MAX_PREVIEWS {
        return Err(bad("A post has at most five previews."));
    }
    let content = body["content"].as_str().unwrap_or_default();
    let offered: Vec<Value> = body["attachments"].as_array().cloned().unwrap_or_default();
    let mut named: Vec<String> = Vec::new();
    let mut name = |id: &Value| -> Result<String, (&'static str, String)> {
        let id = id.as_str().filter(|id| offered.iter().any(|a| a["id"] == *id)).ok_or_else(|| bad("A preview's picture is one of the attachments."))?;
        if !named.iter().any(|n| n == id) {
            named.push(id.to_string());
        }
        Ok(id.to_string())
    };
    let mut previews = Vec::new();
    for preview in given {
        let url = preview["url"].as_str().filter(|url| !url.is_empty() && url.len() <= 2048 && content.contains(*url)).ok_or_else(|| bad("A preview is of a link the post has."))?;
        let mut cleaned = json!({ "url": url });
        for (field, most) in [("title", MAX_PREVIEW_TITLE), ("description", MAX_PREVIEW_DESCRIPTION), ("site_name", MAX_PREVIEW_NAME)] {
            if let Some(text) = line(&preview[field], most) {
                cleaned[field] = json!(text);
            }
        }
        if let Some(image) = preview.get("image").filter(|v| !v.is_null()) {
            cleaned["image"] = json!(name(image)?);
        }
        if let Some(post) = preview.get("post").filter(|v| !v.is_null()) {
            cleaned["post"] = read_copy(post, &mut name).map_err(|why| bad(&why))?;
        }
        previews.push(cleaned);
    }
    let attachments: Vec<Value> = offered.into_iter().filter(|a| a["id"].as_str().is_some_and(|id| named.iter().any(|n| n == id))).collect();
    check_attachments(&attachments)?;
    Ok((previews, attachments))
}

/// A post's extras once an edit says its previews again (`body`'s `previews`,
/// present): those replaced, and its attachments those its media name and the new
/// previews do. `None` where the edit leaves them as they were.
pub fn edited_extra(held: Option<&Value>, body: &Value) -> Result<Option<Value>, (&'static str, String)> {
    let Some(given) = body.get("previews") else { return Ok(None) };
    let mut extra = held.and_then(Value::as_object).cloned().unwrap_or_default();
    let media_ids: Vec<String> = extra.get("media").and_then(Value::as_array).into_iter().flatten().filter_map(|m| m["attachment"].as_str().map(str::to_string)).collect();
    let kept: Vec<Value> = extra.get("attachments").and_then(Value::as_array).into_iter().flatten().filter(|a| a["id"].as_str().is_some_and(|id| media_ids.iter().any(|m| m == id))).cloned().collect();
    extra.remove("previews");
    extra.remove("attachments");
    if !kept.is_empty() {
        extra.insert("attachments".into(), json!(kept));
    }
    if !given.is_null() {
        let (previews, attachments) = read_previews(given, body)?;
        if !previews.is_empty() {
            extra.insert("previews".into(), json!(previews));
            with_attachments(&mut extra, attachments);
        }
    }
    Ok(Some(Value::Object(extra)))
}

/// A copy of a post, in a preview: its named fields only, each of its type.
fn read_copy(post: &Value, name: &mut impl FnMut(&Value) -> Result<String, (&'static str, String)>) -> Result<Value, String> {
    let id = |field: &str| post[field].as_str().filter(|v| !v.is_empty() && v.len() <= MAX_ID && !v.chars().any(char::is_control)).map(str::to_string);
    let community = id("community").filter(|did| did.starts_with("did:")).ok_or("A copy names the community by its DID.")?;
    let channel = id("channel").ok_or("A copy names its channel.")?;
    let seq = post["seq"].as_i64().filter(|seq| *seq >= 1).ok_or("A copy has the post's number.")?;
    let author_name = line(&post["author_name"], MAX_COPY_NAME).ok_or("A copy says who wrote it.")?;
    let content = post["content"].as_str().ok_or("A copy has what the post said.")?;
    if content.chars().count() > MAX_COPY_TEXT {
        return Err("A copy says at most 4,000 characters.".into());
    }
    let posted_at = post["posted_at"].as_i64().ok_or("A copy says when it was posted.")?;
    let as_of = post["as_of"].as_i64().ok_or("A copy says when it was read.")?;
    let mut copy = json!({ "community": community, "channel": channel, "seq": seq, "author_name": author_name, "content": content, "posted_at": posted_at, "as_of": as_of });
    for field in ["community_name", "channel_name"] {
        if let Some(text) = line(&post[field], MAX_PREVIEW_NAME) {
            copy[field] = json!(text);
        }
    }
    if let Some(thread) = id("thread") {
        copy["thread"] = json!(thread);
    }
    if let Some(edited_at) = post["edited_at"].as_i64() {
        copy["edited_at"] = json!(edited_at);
    }
    if post["deleted"] == true {
        copy["deleted"] = json!(true);
    }
    if let Some(media) = post.get("media").filter(|v| !v.is_null()) {
        let media = media.as_array().filter(|m| m.len() <= MAX_COPY_MEDIA).ok_or("A copy has at most four media.")?;
        let mut items = Vec::new();
        for item in media {
            let mut item = item.as_object().cloned().ok_or("A copy's media are media items.")?;
            let attachment = name(item.get("attachment").unwrap_or(&Value::Null)).map_err(|(_, why)| why)?;
            item.insert("attachment".into(), json!(attachment));
            items.push(Value::Object(item));
        }
        if !items.is_empty() {
            copy["media"] = json!(items);
        }
    }
    Ok(copy)
}

/// A poll as a post asks it, checked and with nothing in it but what is named.
fn read_poll(poll: &Value, now: i64) -> Result<Value, String> {
    let question = line(&poll["question"], MAX_POLL_QUESTION).ok_or("A poll asks a question.")?;
    let answers = poll["answers"].as_array().ok_or("A poll offers answers.")?;
    if !(2..=MAX_POLL_ANSWERS).contains(&answers.len()) {
        return Err("A poll offers two to ten answers.".into());
    }
    let mut cleaned = Vec::new();
    for (n, answer) in answers.iter().enumerate() {
        let text = line(&answer["text"], MAX_POLL_ANSWER).ok_or("Each answer needs words.")?;
        let id = answer["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 16).map(str::to_string).unwrap_or_else(|| (n + 1).to_string());
        if cleaned.iter().any(|known: &Value| known["id"] == id.as_str()) {
            return Err("Two answers have the same id.".into());
        }
        let mut entry = json!({ "id": id, "text": text });
        if let Some(emoji) = answer["emoji"].as_str().filter(|e| crate::community::is_emoji(e)) {
            entry["emoji"] = json!(emoji);
        }
        cleaned.push(entry);
    }
    let ends_at = poll["ends_at"].as_i64().ok_or("A poll ends at a time.")?;
    if ends_at < now + POLL_SHORTEST || ends_at > now + POLL_LONGEST {
        return Err("A poll lasts from an hour to 32 days.".into());
    }
    Ok(json!({ "question": question, "answers": cleaned, "multiple": poll["multiple"] == true, "ends_at": ends_at }))
}

/// A post's extras as `message` says them: the poll with its results, and
/// whether it has ended.
pub fn extra_body(post: &Post, body: &mut Value, now: i64) {
    let Some(extra) = post.extra.as_ref().and_then(Value::as_object) else { return };
    for (key, value) in extra {
        body[key] = value.clone();
    }
    if let Some(poll) = extra.get("poll") {
        let results: Vec<Value> = poll["answers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|answer| {
                let id = answer["id"].as_str().unwrap_or_default();
                let voters: Vec<&str> = post.votes.iter().filter(|(_, chosen)| chosen.iter().any(|c| c == id)).map(|(did, _)| did.as_str()).collect();
                json!({ "id": id, "voters": voters })
            })
            .collect();
        body["poll"]["results"] = json!(results);
        if poll["ends_at"].as_i64().is_some_and(|ends| ends <= now) {
            body["poll"]["closed"] = json!(true);
        }
    }
}

/// A forum's tags as `update-channel` gives them, checked; a new tag gets an id.
pub fn read_tags(given: &Value, held: Option<&Value>) -> Result<Value, &'static str> {
    let given = given.as_array().ok_or("Tags are a list.")?;
    if given.len() > MAX_TAGS {
        return Err("A forum has at most twenty tags.");
    }
    let known: Vec<&str> = held.and_then(Value::as_array).into_iter().flatten().filter_map(|t| t["id"].as_str()).collect();
    let mut tags = Vec::new();
    for tag in given {
        let name = line(&tag["name"], MAX_TAG_NAME).ok_or("Each tag needs a name.")?;
        let id = tag["id"].as_str().filter(|id| known.contains(id)).map(str::to_string).unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()[..8].to_string());
        let mut entry = json!({ "id": id, "name": name });
        if let Some(emoji) = tag["emoji"].as_str().filter(|e| crate::community::is_emoji(e)) {
            entry["emoji"] = json!(emoji);
        }
        tags.push(entry);
    }
    Ok(Value::Array(tags))
}

impl Community {
    /// Who is seen online now, by DID: those who chose to be, with a live connection.
    pub fn seen_online(&self) -> anyhow::Result<HashMap<String, String>> {
        let live: Vec<String> = self.live.read().unwrap_or_else(|p| p.into_inner()).iter().filter(|(_, n)| **n > 0).map(|(did, _)| did.clone()).collect();
        let mut seen = HashMap::new();
        for did in live {
            let (share, status) = self.store.presence(&did)?;
            if share {
                seen.insert(did, status);
            }
        }
        Ok(seen)
    }

    /// A member's live connection opened (`true`) or closed: what to tell every
    /// live connection, if who is seen online changed.
    pub fn live_changed(&self, did: &str, opened: bool) -> anyhow::Result<Option<Value>> {
        let changed = {
            let mut live = self.live.write().unwrap_or_else(|p| p.into_inner());
            let count = live.entry(did.to_string()).or_insert(0);
            let before = *count;
            *count = if opened { before + 1 } else { before.saturating_sub(1) };
            (before == 0) != (*count == 0)
        };
        if !changed {
            return Ok(None);
        }
        let (share, status) = self.store.presence(did)?;
        if !share || self.store.member(did)?.is_none() {
            return Ok(None);
        }
        Ok(Some(self.presence_notice(did, opened.then_some(status))))
    }

    /// `member-presence`, to every live connection: by member id, as everyone in a
    /// community is named to everyone else (community/1.0, "Who members are to each
    /// other"). It said the DID, to everyone watching.
    fn presence_notice(&self, did: &str, status: Option<String>) -> Value {
        let member = self.store.member_id(did, crate::gates::now()).unwrap_or_default();
        let mut body = json!({ "member": member });
        if let Some(status) = status {
            body["presence"] = json!(status);
        }
        json!({ "type": MEMBER_PRESENCE, "id": uuid::Uuid::new_v4().to_string(), "body": body })
    }

    /// `vote`, `search-request`, `set-presence`, and `history-request` with `at`;
    /// `None` for anything else.
    pub fn handle_posting(&self, member: &Member, message_type: &str, message: &Value, now: i64) -> anyhow::Result<Option<Outcome>> {
        let body = &message["body"];
        let sender = member.did.as_str();
        let outcome = match message_type {
            VOTE => {
                let id = body["to"]["id"].as_str().unwrap_or_default();
                let Some(post) = self.store.post_by_id(id)?.filter(|post| body["to"]["author"].as_str().is_none_or(|author| author == post.author_did)).filter(|post| self.sees_id(sender, &post.channel).unwrap_or(false)) else {
                    return refuse(message, "e.p.msg.unknown-post", "There is no such post.").map(Some);
                };
                let Some(poll) = post.extra.as_ref().and_then(|extra| extra.get("poll")).filter(|_| !post.deleted) else {
                    return refuse(message, "e.p.msg.invalid-vote", "That post isn't a poll.").map(Some);
                };
                if poll["ends_at"].as_i64().is_some_and(|ends| ends <= now) {
                    return refuse(message, "e.p.msg.poll-closed", "That poll has ended.").map(Some);
                }
                if self.timed_out(sender, now)? {
                    return refuse(message, "e.p.msg.timed-out", "You are timed out here for now.").map(Some);
                }
                let offered: Vec<&str> = poll["answers"].as_array().into_iter().flatten().filter_map(|a| a["id"].as_str()).collect();
                let mut chosen: Vec<String> = Vec::new();
                for answer in body["answers"].as_array().into_iter().flatten() {
                    match answer.as_str().filter(|a| offered.contains(a)) {
                        Some(a) if !chosen.iter().any(|c| c == a) => chosen.push(a.to_string()),
                        Some(_) => {}
                        None => return refuse(message, "e.p.msg.invalid-vote", "That isn't one of the poll's answers.").map(Some),
                    }
                }
                if chosen.len() > 1 && poll["multiple"] != true {
                    return refuse(message, "e.p.msg.invalid-vote", "This poll takes one answer.").map(Some);
                }
                self.store.vote(&post.id, sender, &chosen, now)?;
                let post = self.store.post_by_id(&post.id)?.expect("just voted on");
                Outcome { reply: Some(reply_to(message, CHANGED, self.post_body(&post, now))), changed: vec![post.id], ..Outcome::default() }
            }

            SEARCH_REQUEST => {
                let query: String = body["query"].as_str().unwrap_or_default().chars().take(200).collect();
                let words: Vec<String> = query.split_whitespace().take(8).map(str::to_string).collect();
                if query.trim().chars().count() < 2 || words.is_empty() {
                    return refuse(message, "e.p.msg.invalid-query", "Search for at least two letters.").map(Some);
                }
                let limit = body["limit"].as_u64().map_or(20, |limit| (limit as usize).clamp(1, 50));
                let before = body["before"].as_i64().or_else(|| body["before"].as_str().and_then(|b| b.parse().ok()));
                // Only in channels they may see: a channel they may not is unknown.
                let seen = self.seen_by(sender)?;
                if body["channel"].as_str().is_some_and(|channel| !seen.contains(channel)) {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.").map(Some);
                }
                let every = self.store.channels()?.iter().all(|channel| seen.contains(&channel.id));
                let within: Option<Vec<String>> = (!every).then(|| seen.into_iter().collect());
                let mut found = self.store.search(&words, body["channel"].as_str(), body["author"].as_str(), within.as_deref(), before, limit + 1)?;
                let more = found.len() > limit;
                found.truncate(limit);
                let next = found.last().map(|post| post.serial);
                let mut messages = Vec::new();
                for post in &found {
                    // (Not one in a private thread they aren't in.)
                    if post.thread.is_none() || self.reads_post_id(sender, post)? {
                        messages.push(self.post_body(post, now));
                    }
                }
                let mut results = json!({ "messages": messages, "more": more });
                if let Some(next) = next.filter(|_| more) {
                    results["next"] = json!(next);
                }
                Outcome::reply(reply_to(message, SEARCH_RESULTS, results))
            }

            SET_PRESENCE => {
                let (share, status) = self.store.presence(sender)?;
                let share = body["share"].as_bool().unwrap_or(share);
                let status = match body["status"].as_str() {
                    Some(given) if STATUSES.contains(&given) => given.to_string(),
                    Some(_) => return refuse(message, "e.p.msg.invalid-presence", "Presence is online, idle or dnd.").map(Some),
                    None => status,
                };
                self.store.set_presence(sender, share, &status)?;
                let mut outcome = Outcome::reply(reply_to(message, COMMUNITY, self.community(member)?));
                let connected = self.live.read().unwrap_or_else(|p| p.into_inner()).get(sender).is_some_and(|n| *n > 0);
                if connected {
                    outcome.live.push(self.presence_notice(sender, share.then_some(status)));
                }
                outcome
            }

            SET_NOTIFICATIONS => {
                match body["level"].as_str() {
                    Some(level) if LEVELS.contains(&level) => self.store.set_notify_level(sender, level)?,
                    Some(_) => return refuse(message, "e.p.msg.invalid-level", "What reaches you is all, mentions or nothing.").map(Some),
                    None => self.store.settle_notify_level(sender, DEFAULT_LEVEL)?,
                }
                for (field, follow) in [("follow", true), ("unfollow", false)] {
                    for thread in body[field].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        self.store.follow_thread(sender, thread, follow)?;
                    }
                }
                Outcome::reply(reply_to(message, COMMUNITY, self.community(member)?))
            }

            // Only on a live connection, which the server takes before it gets here.
            WATCH => return refuse(message, "e.p.req.live-only", "Watching is asked for on a live connection.").map(Some),

            crate::community::HISTORY_REQUEST if body.get("at").is_some_and(Value::is_i64) => {
                let Some(channel) = self.channel_for(sender, body["channel"].as_str().unwrap_or_default())? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.").map(Some);
                };
                let limit = body["limit"].as_u64().map_or(crate::community::DEFAULT_HISTORY_LIMIT, |l| (l as usize).clamp(2, crate::community::MAX_HISTORY_LIMIT));
                let thread = body["thread"].as_str();
                if let Some(thread) = thread {
                    if !self.reads_thread(sender, &self.powers(sender)?, &channel, thread)? {
                        return refuse(message, "e.p.msg.unknown-thread", "There is no such thread in this channel.").map(Some);
                    }
                }
                let (posts, more, newer) = self.store.channel_posts_around(&channel.id, thread, body["at"].as_i64().unwrap_or(now), limit / 2)?;
                let messages: Vec<Value> = posts.iter().map(|post| self.post_body(post, now)).collect();
                Outcome::reply(reply_to(message, HISTORY, json!({ "channel": channel.id, "messages": messages, "more": more, "newer": newer })))
            }

            _ => return Ok(None),
        };
        Ok(Some(outcome))
    }
}

/// Whether a post goes through the mediator ([`Community::reaches`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Whether or not a connection of theirs is watching: it is for them.
    Always,
    /// Only while no connection of theirs is watching.
    UnlessWatching,
    Never,
}

impl Community {
    /// Whether `post` goes to `member` through the mediator (community/1.0's
    /// "What reaches a member, and how"): what concerns them (a mention, an answer,
    /// a thread they follow) always, unless they chose nothing; the rest, for one
    /// who chose everything, only while no connection of theirs is watching -- the
    /// one watching has it already, and every post would be a push to every device.
    /// A member whose app has never said is sent everything, as before.
    pub fn reaches(&self, member: &Member, post: &Post) -> anyhow::Result<Reach> {
        let level = self.store.notify_level(&member.did)?;
        // Their own post they have from the connection they posted on.
        if level.is_some() && post.author_did == member.did {
            return Ok(Reach::Never);
        }
        match level.as_deref() {
            Some("nothing") => return Ok(Reach::Never),
            _ if self.concerns(member, post)? => return Ok(Reach::Always),
            None | Some("all") => return Ok(Reach::UnlessWatching),
            _ => return Ok(Reach::Never),
        }
    }

    /// Whether `post` is for `member` in particular: it mentions them (by member,
    /// `everyone`, or a role they hold), answers them, or is in a thread they follow.
    fn concerns(&self, member: &Member, post: &Post) -> anyhow::Result<bool> {
        if post.reply_author.as_deref() == Some(member.did.as_str()) {
            return Ok(true);
        }
        let mut roles: Option<Vec<String>> = None;
        for mention in &post.mentions {
            if mention.who == member.did || mention.who == "everyone" {
                return Ok(true);
            }
            if let Some(role) = mention.who.strip_prefix("role:") {
                if roles.is_none() {
                    roles = Some(self.store.member_roles(&member.did)?);
                }
                if roles.as_ref().is_some_and(|held| held.iter().any(|r| r == role)) {
                    return Ok(true);
                }
            }
        }
        if let Some(asked) = post.answer.as_ref().and_then(|answer| answer["to"].as_str()) {
            if self.store.post_by_id(asked)?.is_some_and(|asked| asked.author_did == member.did) {
                return Ok(true);
            }
        }
        if let Some(thread) = &post.thread {
            if self.store.follows_thread(&member.did, thread)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether a `changed` goes to `member` through the mediator: only to one who
    /// chose everything, or whose app has never said.
    pub fn changes_reach(&self, member: &Member) -> anyhow::Result<bool> {
        Ok(matches!(self.store.notify_level(&member.did)?.as_deref(), None | Some("all")))
    }
}
