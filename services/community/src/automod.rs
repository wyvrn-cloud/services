//! AutoMod (community/1.0's "Moderation"): rules that check each post before it
//! is placed. A rule is triggered by words or patterns, by links, or by too many
//! mentions, and refuses the post, holds it for a moderator, tells a channel of
//! it, or times its author out -- any of those. Rules are set through the
//! `automod` workflow (admin_flows.rs) and kept as JSON, one a row:
//!
//! `{ id, name, enabled, words, patterns, links, allowed_domains, max_mentions,
//!    block, hold, alert_channel, timeout_seconds, exempt_roles, exempt_channels }`

use serde_json::{json, Value};

use crate::community::{problem, refuse, reply_to, Community, Outcome, CHANGED};
use crate::store::{Channel, Held, Member};

pub const HELD_REQUEST: &str = "https://wyvrn.app/community/1.0/held-request";
pub const HELD: &str = "https://wyvrn.app/community/1.0/held";
pub const RELEASE_HELD: &str = "https://wyvrn.app/community/1.0/release-held";
pub const REJECT_HELD: &str = "https://wyvrn.app/community/1.0/reject-held";

/// What AutoMod posts in an alert channel is signed as.
pub const AUTOMOD_NAME: &str = "AutoMod";
/// The largest compiled pattern a rule may have (Unicode classes such as `\w` take
/// most of it).
const PATTERN_SIZE: usize = 1 << 21;

/// A rule's words, `*` standing for any letters, as one pattern that finds any of
/// them as a whole word whatever its case; the word found is its group `w`.
fn words_pattern(words: &[String]) -> String {
    let each: Vec<String> = words.iter().map(|word| word.split('*').map(regex::escape).collect::<Vec<_>>().join(r"\w*")).collect();
    format!(r"(?i)(?:^|[^\w])(?P<w>{})(?:$|[^\w])", each.join("|"))
}

/// A pattern as a rule gives it, compiled with a size limit, or why it can't be.
pub fn compile(pattern: &str) -> Result<regex::Regex, String> {
    regex::RegexBuilder::new(pattern).case_insensitive(true).size_limit(PATTERN_SIZE).build().map_err(|error| error.to_string())
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

/// What in `content` a rule finds, if anything: a word, a pattern's match, a link
/// to a domain not allowed, or too many mentions.
pub fn triggered(rule: &Value, content: &str, mentions: usize) -> Option<String> {
    let words: Vec<String> = strings(&rule["words"]).iter().map(|word| word.trim().to_string()).filter(|word| !word.is_empty()).collect();
    if !words.is_empty() {
        if let Some(found) = compile(&words_pattern(&words)).ok().and_then(|re| re.captures(content).map(|c| c["w"].to_string())) {
            return Some(format!("the word “{found}”"));
        }
    }
    for pattern in strings(&rule["patterns"]) {
        if let Some(found) = compile(&pattern).ok().and_then(|re| re.find(content).map(|m| m.as_str().to_string())) {
            return Some(format!("“{found}”"));
        }
    }
    if rule["links"] == true {
        static LINK: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)\b(?:https?://|www\.)([^\s/?#<>()\]]+)").expect("a valid pattern"));
        let allowed = strings(&rule["allowed_domains"]);
        for found in LINK.captures_iter(content) {
            let host = found[1].to_lowercase();
            let host = host.trim_start_matches("www.").split(':').next().unwrap_or_default().to_string();
            let fine = allowed.iter().any(|domain| {
                let domain = domain.trim().trim_start_matches("www.").to_lowercase();
                !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
            });
            if !fine {
                return Some(format!("a link to {host}"));
            }
        }
    }
    if rule["max_mentions"].as_u64().is_some_and(|most| most > 0 && mentions as u64 > most) {
        return Some(format!("{mentions} mentions"));
    }
    None
}

/// What AutoMod made of a post.
pub enum Verdict {
    /// It goes through.
    Placed,
    /// It goes through, and AutoMod also alerted or timed out (`outcome` carries
    /// any alert to post).
    PlacedNoted(Outcome),
    /// Refused or held: this is the whole answer.
    Stopped(Outcome),
}

impl Community {
    /// Checks a post against AutoMod's rules, as `member` is about to place it in
    /// `channel`, and does what the first rule it triggers says.
    pub fn automod(&self, member: &Member, channel: &Channel, message: &Value, mentions: usize, now: i64) -> anyhow::Result<Verdict> {
        let powers = self.powers(&member.did)?;
        if powers.all() || self.channel_permissions(&member.did, &powers, channel)?.contains("manage_messages") {
            return Ok(Verdict::Placed);
        }
        let body = &message["body"];
        let content = body["content"].as_str().unwrap_or_default();
        let found = self.store.automod_rules()?.into_iter().filter(|rule| rule["enabled"] == true).find_map(|rule| {
            let exempt = strings(&rule["exempt_roles"]).iter().any(|role| powers.roles.contains(role)) || strings(&rule["exempt_channels"]).contains(&channel.id);
            if exempt {
                return None;
            }
            triggered(&rule, content, mentions).map(|what| (rule, what))
        });
        let Some((rule, what)) = found else { return Ok(Verdict::Placed) };
        let rule_name = rule["name"].as_str().unwrap_or("A rule").to_string();
        let mut outcome = Outcome::default();
        let blocked = rule["block"] == true;
        let held = !blocked && rule["hold"] == true;
        let done = if blocked { "refused" } else if held { "held" } else { "noticed" };
        let details = json!({ "channel": channel.id, "rule": rule_name, "content": content.chars().take(500).collect::<String>(), "found": what });
        if blocked {
            self.audit(now, &member.did, "automod.block", Some(&member.did), Some(details.clone()));
        }
        if let Some(alert) = rule["alert_channel"].as_str().and_then(|id| self.store.channel(id).ok().flatten()).filter(|c| !c.closed && c.voice.is_none()) {
            let name = member.name.clone().unwrap_or_else(|| "someone".into());
            let quoted: String = content.chars().take(300).collect();
            let text = format!("**{AUTOMOD_NAME}** {done} a post by {name} in #{} ({rule_name}: {what}):\n> {}", channel.name, quoted.replace('\n', "\n> "));
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
            let id = body["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 64).map(str::to_string).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let mut kept = body.clone();
            kept["id"] = json!(id);
            self.store.hold(&Held { id: id.clone(), channel: channel.id.clone(), author: member.did.clone(), held_at: now, rule: rule_name.clone(), body: kept })?;
            self.audit(now, &member.did, "automod.hold", Some(&member.did), Some(details));
            outcome.reply = Some(reply_to(message, crate::community::POSTED, json!({ "channel": channel.id, "id": id, "held": true })));
            outcome.notices.extend(self.held_notices(Some(&member.did))?);
            return Ok(Verdict::Stopped(outcome));
        }
        Ok(Verdict::PlacedNoted(outcome))
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
            if !self.channel_permissions(did, &powers, &channel)?.contains("manage_messages") {
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
        let Some(channel) = self.store.channel(&held.channel)? else {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_links_patterns_and_mentions_trigger_a_rule() {
        let rule = json!({ "words": ["free*", "scam"], "patterns": [r"\d{4}-\d{4}"], "links": true, "allowed_domains": ["wyvrn.app"], "max_mentions": 3 });
        assert!(triggered(&rule, "Get FREEBIES now", 0).is_some());
        assert!(triggered(&rule, "is this a scam?", 0).is_some());
        assert!(triggered(&rule, "scampi for dinner", 0).is_none());
        assert!(triggered(&rule, "call 1234-5678", 0).is_some());
        assert!(triggered(&rule, "see https://docs.wyvrn.app/x and www.wyvrn.app", 0).is_none());
        assert_eq!(triggered(&rule, "see https://evil.example/x", 0).as_deref(), Some("a link to evil.example"));
        assert!(triggered(&rule, "hi all", 4).is_some());
        assert!(triggered(&rule, "hi all", 3).is_none());
        assert!(compile("(unclosed").is_err());
    }
}
