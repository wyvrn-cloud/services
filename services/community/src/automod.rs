//! AutoMod (community/1.0's "Moderation"): rules that check each post, and each
//! edit, before it is placed. What a rule finds is filters.rs's: words however
//! they are disguised, filters by kind and how bad, patterns, links by where they
//! go, too many mentions. A rule that finds something refuses the post, holds it
//! for a moderator, or censors it (words as dots, links as `<UNSAFE URL BLOCKED>`),
//! and may tell a channel of it and time its author out. Rules are set through the
//! `automod` workflow (admin_flows.rs) and kept as JSON, one a row:
//!
//! `{ id, name, enabled, words, allowed_words, filters, patterns, links,
//!    allowed_domains, blocked_domains, domain_lists, max_mentions, block, hold,
//!    censor, alert_channel, timeout_seconds, exempt_roles, exempt_channels }`
//!
//! Rules are checked in order. One that refuses or holds the post is the last; one
//! that censors it passes on what is left of it to the next.

use serde_json::{json, Value};

pub use crate::filters::compile;
use crate::filters::{censor, inspect, strings};

use crate::community::{problem, refuse, reply_to, Community, Outcome, CHANGED};
use crate::store::{Channel, Held, Member};

pub const HELD_REQUEST: &str = "https://wyvrn.app/community/1.0/held-request";
pub const HELD: &str = "https://wyvrn.app/community/1.0/held";
pub const RELEASE_HELD: &str = "https://wyvrn.app/community/1.0/release-held";
pub const REJECT_HELD: &str = "https://wyvrn.app/community/1.0/reject-held";
/// A moderator asking what a censored post said, and the answer.
pub const ORIGINAL_REQUEST: &str = "https://wyvrn.app/community/1.0/original-request";
pub const ORIGINAL: &str = "https://wyvrn.app/community/1.0/original";

/// What AutoMod posts in an alert channel is signed as.
pub const AUTOMOD_NAME: &str = "AutoMod";

/// What a rule may do with a post it finds something in, as the rule editor says it.
pub const ACTIONS: [(&str, &str); 4] = [
    ("block", "Refuse the post"),
    ("censor", "Censor the post"),
    ("hold", "Hold it for a moderator"),
    ("note", "Let it through (and tell a channel, or time out, as below)"),
];

/// What `rule` does, as one of `ACTIONS`.
pub fn action_of(rule: &Value) -> &'static str {
    if rule["block"] == true {
        "block"
    } else if rule["hold"] == true {
        "hold"
    } else if rule["censor"] == true {
        "censor"
    } else {
        "note"
    }
}

/// What `rule` does, said short, for the list of rules.
pub fn action_label(rule: &Value) -> &'static str {
    match action_of(rule) {
        "block" => "refuses",
        "hold" => "holds",
        "censor" => "censors",
        _ => "tells",
    }
}

/// The ready-made rules: key, name, what it does, and the rule (without id, name
/// and whether it is on).
pub type Preset = (&'static str, &'static str, &'static str, fn() -> Value);
pub const PRESETS: [Preset; 4] = [
    // Refused outright (asked 2026-11-17): censoring is a choice on the rule.
    ("family", "Family friendly", "refuses a post with swearing, sexual words, slurs, insults or links to adult sites", || {
        json!({
            "filters": { "profanity": "mild", "sexual": "mild", "slurs": "mild", "insults": "moderate" },
            "words": [], "allowed_words": ["shiitake"], "patterns": [],
            "links": false, "domain_lists": ["adult"], "allowed_domains": [], "blocked_domains": [],
            "block": true, "hold": false, "censor": false, "exempt_roles": [], "exempt_channels": [],
        })
    }),
    ("hate", "No slurs or hate", "refuses a post with a slur in it", || {
        json!({
            "filters": { "slurs": "mild" }, "words": [], "allowed_words": [], "patterns": [],
            "links": false, "domain_lists": [], "allowed_domains": [], "blocked_domains": [],
            "block": true, "hold": false, "censor": false, "exempt_roles": [], "exempt_channels": [],
        })
    }),
    ("unsafe-links", "No unsafe links", "censors links to adult sites and link shorteners", || {
        json!({
            "filters": {}, "words": [], "allowed_words": [], "patterns": [],
            "links": false, "domain_lists": ["adult", "shorteners"], "allowed_domains": [], "blocked_domains": [],
            "block": false, "hold": false, "censor": true, "exempt_roles": [], "exempt_channels": [],
        })
    }),
    ("links", "Links to approved sites only", "holds a post with a link for a moderator, unless it is to a site you allow", || {
        json!({
            "filters": {}, "words": [], "allowed_words": [], "patterns": [],
            "links": true, "domain_lists": [], "allowed_domains": [], "blocked_domains": [],
            "block": false, "hold": true, "censor": false, "exempt_roles": [], "exempt_channels": [],
        })
    }),
];

/// What a rule finds in `content`, said for an alert, if anything.
pub fn triggered(rule: &Value, content: &str, mentions: usize) -> Option<String> {
    inspect(rule, content, mentions).map(|found| found.what)
}

/// A post AutoMod censored: what it says now, and the rules that changed it.
pub struct Censored {
    pub content: String,
    pub rules: Vec<String>,
}

/// What AutoMod is checking.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Placing {
    Post,
    Edit,
    Webhook,
}

/// What AutoMod made of a post.
pub enum Verdict {
    /// It goes through: as it was, or `censored`. `noted` carries anything AutoMod
    /// did besides (an alert posted).
    Placed { noted: Option<Outcome>, censored: Option<Censored> },
    /// Refused or held: this is the whole answer.
    Stopped(Outcome),
}

impl Community {
    /// Checks `content`, as `member` is about to place it in `channel` (in
    /// `message`, a post or an edit of one), against AutoMod's rules, and does what
    /// each rule it triggers says. An edit can't be held: a rule that would hold it
    /// refuses it.
    #[allow(clippy::too_many_arguments)]
    pub fn automod(&self, member: &Member, channel: &Channel, message: &Value, content: &str, mentions: usize, now: i64, editing: bool) -> anyhow::Result<Verdict> {
        let powers = self.powers(&member.did)?;
        if powers.all() || self.channel_permissions(&member.did, &powers, channel)?.contains("manage_messages") {
            return Ok(Verdict::Placed { noted: None, censored: None });
        }
        self.run_rules(member, &powers.roles, channel, message, content, mentions, now, if editing { Placing::Edit } else { Placing::Post })
    }

    /// AutoMod for what a webhook posts (webhooks.rs): the community's own post, so
    /// nothing exempts it as the owner's would be, and a rule that would hold it
    /// refuses it -- a held post is let through as its author's, which a webhook's
    /// name and embeds are not.
    pub fn automod_webhook(&self, member: &Member, channel: &Channel, message: &Value, content: &str, mentions: usize, now: i64) -> anyhow::Result<Verdict> {
        self.run_rules(member, &[], channel, message, content, mentions, now, Placing::Webhook)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_rules(&self, member: &Member, roles: &[String], channel: &Channel, message: &Value, content: &str, mentions: usize, now: i64, placing: Placing) -> anyhow::Result<Verdict> {
        let editing = placing == Placing::Edit;
        let body = &message["body"];
        let mut outcome = Outcome::default();
        let mut noted = false;
        let mut now_says = content.to_string();
        let mut censored_by: Vec<String> = Vec::new();
        for rule in self.store.automod_rules()?.into_iter().filter(|rule| rule["enabled"] == true) {
            let exempt = strings(&rule["exempt_roles"]).iter().any(|role| roles.contains(role)) || strings(&rule["exempt_channels"]).contains(&channel.id);
            if exempt {
                continue;
            }
            let Some(found) = inspect(&rule, &now_says, mentions) else { continue };
            noted = true;
            let rule_name = rule["name"].as_str().unwrap_or("A rule").to_string();
            let blocked = rule["block"] == true || (placing != Placing::Post && rule["hold"] == true);
            let held = !blocked && rule["hold"] == true;
            let censors = !blocked && !held && rule["censor"] == true && !found.spans.is_empty();
            let done = if blocked { "refused" } else if held { "held" } else if censors { "censored" } else { "noticed" };
            let what = &found.what;
            // (What was written, before any rule censored it: the audit log and the
            // alert channel are for moderators.)
            let details = json!({ "channel": channel.id, "rule": rule_name, "content": content.chars().take(500).collect::<String>(), "found": what, "edit": editing });
            if blocked {
                self.audit(now, &member.did, "automod.block", Some(&member.did), Some(details.clone()));
            } else if censors {
                self.audit(now, &member.did, "automod.censor", Some(&member.did), Some(details.clone()));
            }
            if let Some(alert) = rule["alert_channel"].as_str().and_then(|id| self.store.channel(id).ok().flatten()).filter(|c| !c.closed && c.voice.is_none()) {
                let name = member.name.clone().unwrap_or_else(|| "someone".into());
                let quoted: String = content.chars().take(300).collect();
                let kind = match placing {
                    Placing::Post => "a post",
                    Placing::Edit => "an edit",
                    Placing::Webhook => "a webhook's post",
                };
                let text = format!("**{AUTOMOD_NAME}** {done} {kind} by {name} in #{} ({rule_name}: {what}):\n> {}", channel.name, quoted.replace('\n', "\n> "));
                if self.post_as_server(&alert.id, &text, now)? {
                    outcome.posted = true;
                }
            }
            if let Some(seconds) = rule["timeout_seconds"].as_i64().filter(|s| *s >= 60) {
                let until = now + seconds;
                if self.store.timed_out_until(&member.did, now)?.is_none_or(|already| already < until) {
                    self.store.set_timeout(&member.did, Some(until), Some(&format!("AutoMod: {rule_name}")))?;
                    self.audit(now, &member.did, "member.timeout", Some(&member.did), Some(json!({ "until": until, "reason": format!("AutoMod: {rule_name}") })));
                }
            }
            if blocked {
                let mut report = problem(message, "e.p.msg.automod", "This community's AutoMod doesn't let that be posted.");
                report["body"]["args"] = json!({ "rule": rule_name });
                outcome.reply = Some(report);
                return Ok(Verdict::Stopped(outcome));
            }
            if held {
                // Held as its author wrote it: whoever lets it through decides on that.
                let id = body["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 64).map(str::to_string).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let mut kept = body.clone();
                kept["id"] = json!(id);
                self.store.hold(&Held { id: id.clone(), channel: channel.id.clone(), author: member.did.clone(), held_at: now, rule: rule_name.clone(), body: kept })?;
                self.audit(now, &member.did, "automod.hold", Some(&member.did), Some(details));
                outcome.reply = Some(reply_to(message, crate::community::POSTED, json!({ "channel": channel.id, "id": id, "held": true })));
                outcome.notices.extend(self.held_notices(Some(&member.did))?);
                return Ok(Verdict::Stopped(outcome));
            }
            if censors {
                now_says = censor(&now_says, &found.spans);
                censored_by.push(rule_name);
            }
        }
        let censored = (!censored_by.is_empty()).then_some(Censored { content: now_says, rules: censored_by });
        Ok(Verdict::Placed { noted: noted.then_some(outcome), censored })
    }

    /// What a censored post said before AutoMod censored it, for whoever manages
    /// posts in its channel.
    pub fn handle_original(&self, member: &Member, message: &Value) -> anyhow::Result<Outcome> {
        let id = message["body"]["to"]["id"].as_str().or(message["body"]["id"].as_str()).unwrap_or_default();
        let Some(post) = self.store.post_by_id(id)?.filter(|post| self.sees_id(&member.did, &post.channel).unwrap_or(false)) else {
            return refuse(message, "e.p.msg.unknown-post", "There is no such post.");
        };
        let Some(channel) = self.store.channel(&post.channel)? else {
            return refuse(message, "e.p.msg.unknown-post", "There is no such post.");
        };
        if !self.may_in(&member.did, &channel, "manage_messages")? {
            return refuse(message, "e.p.trust.not-authorized", "What a censored post said is for whoever manages posts in its channel.");
        }
        let Some(original) = self.store.censored_original(&post.id)?.filter(|_| !post.deleted) else {
            return refuse(message, "e.p.msg.not-censored", "AutoMod didn't censor that post.");
        };
        Ok(Outcome::reply(reply_to(message, ORIGINAL, json!({ "id": post.id, "channel": post.channel, "content": original, "rules": post.censored.unwrap_or_default() }))))
    }

    /// Posts `text` in `channel` as the server itself, under AutoMod's name: a
    /// member of its own community. False where the server doesn't know its DID.
    fn post_as_server(&self, channel: &str, text: &str, now: i64) -> anyhow::Result<bool> {
        let Some(own) = self.own.clone() else { return Ok(false) };
        self.store.admit(&own, crate::store::Role::Member, now)?;
        if self.store.member(&own)?.and_then(|m| m.name).is_none() {
            self.store.set_name(&own, Some(AUTOMOD_NAME))?;
        }
        let Some(author) = self.store.member(&own)? else { return Ok(false) };
        self.store.add_post(channel, &uuid::Uuid::new_v4().to_string(), text, &author, None, None, now)?;
        Ok(true)
    }

    /// The held posts `did` may see: those in channels where they manage posts.
    fn held_for(&self, did: &str) -> anyhow::Result<Value> {
        let powers = self.powers(did)?;
        let mut posts = Vec::new();
        for held in self.store.held()? {
            let Some(channel) = self.store.channel(&held.channel)? else { continue };
            if !self.sees(did, &powers, &channel)? || !self.channel_permissions(did, &powers, &channel)?.contains("manage_messages") {
                continue;
            }
            posts.push(json!({
                "channel": held.channel,
                "id": held.id,
                "content": held.body["content"],
                "author": self.who(&held.author)?,
                "held_at": held.held_at,
                "rule": held.rule,
            }));
        }
        Ok(json!({ "posts": posts }))
    }

    /// `held` for every joined member who manages posts where something is held.
    fn held_notices(&self, except: Option<&str>) -> anyhow::Result<Vec<(String, Value)>> {
        let mut notices = Vec::new();
        for member in self.store.joined()? {
            if Some(member.did.as_str()) == except {
                continue;
            }
            let held = self.held_for(&member.did)?;
            if held["posts"].as_array().is_some_and(|posts| !posts.is_empty()) {
                notices.push((member.did, json!({ "type": HELD, "id": uuid::Uuid::new_v4().to_string(), "body": held })));
            }
        }
        Ok(notices)
    }

    /// `held-request`, `release-held` and `reject-held`; `None` for anything else.
    pub fn handle_held(&self, member: &Member, message_type: &str, message: &Value, now: i64) -> anyhow::Result<Option<Outcome>> {
        if !matches!(message_type, HELD_REQUEST | RELEASE_HELD | REJECT_HELD) {
            return Ok(None);
        }
        let sender = member.did.as_str();
        if message_type == HELD_REQUEST {
            return Ok(Some(Outcome::reply(reply_to(message, HELD, self.held_for(sender)?))));
        }
        let id = message["body"]["id"].as_str().unwrap_or_default();
        let Some(held) = self.store.held()?.into_iter().find(|held| held.id == id) else {
            return refuse(message, "e.p.msg.unknown-post", "Nothing is held by that id.").map(Some);
        };
        let Some(channel) = self.channel_for(sender, &held.channel)? else {
            return refuse(message, "e.p.msg.unknown-channel", "That channel is gone.").map(Some);
        };
        if !self.may_in(sender, &channel, "manage_messages")? {
            return refuse(message, "e.p.trust.not-authorized", "Held posts are for whoever manages posts in their channel.").map(Some);
        }
        self.store.unhold(id)?;
        let details = json!({ "channel": channel.id, "rule": held.rule, "content": held.body["content"] });
        let mut outcome = Outcome::default();
        let author = self.store.member(&held.author)?.filter(|_| message_type == RELEASE_HELD);
        if let Some(author) = author {
            let placed = self.post(&author, &json!({ "type": crate::community::POST, "body": held.body }), now, false)?;
            outcome.posted = placed.posted;
            outcome.changed = placed.changed;
            outcome.posted_id = placed.posted_id;
            self.audit(now, sender, "automod.release", Some(&held.author), Some(details));
        } else {
            // Gone, and its author told no more than that.
            let gone = json!({ "channel": held.channel, "id": held.id, "seq": 0, "content": "", "posted_at": held.held_at, "author": { "did": held.author }, "deleted": true, "rev": 0 });
            outcome.notices.push((held.author.clone(), json!({ "type": CHANGED, "id": uuid::Uuid::new_v4().to_string(), "body": gone })));
            self.audit(now, sender, "automod.reject", Some(&held.author), Some(details));
        }
        outcome.reply = Some(reply_to(message, HELD, self.held_for(sender)?));
        Ok(Some(outcome))
    }
}
