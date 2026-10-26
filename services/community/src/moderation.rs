//! Keeping order (community/1.0's "Moderation"): bans, timeouts, slow mode and a
//! server's own rate limit, bulk removal, the audit log and a member's history.
//! Each is a permission's, held to rank where it acts on a member, and each is
//! written in the audit log.

use serde_json::{json, Value};

use crate::community::{line, problem, refuse, reply_to, Community, Outcome, MEMBERS, REMOVED};
use crate::store::{AuditEntry, Ban, Channel, Member};

pub const BAN: &str = "https://wyvrn.app/community/1.0/ban";
pub const UNBAN: &str = "https://wyvrn.app/community/1.0/unban";
pub const BANS_REQUEST: &str = "https://wyvrn.app/community/1.0/bans-request";
pub const BANS: &str = "https://wyvrn.app/community/1.0/bans";
pub const TIMEOUT: &str = "https://wyvrn.app/community/1.0/timeout";
pub const BULK_DELETE: &str = "https://wyvrn.app/community/1.0/bulk-delete";
pub const BULK_DELETED: &str = "https://wyvrn.app/community/1.0/bulk-deleted";
pub const AUDIT_REQUEST: &str = "https://wyvrn.app/community/1.0/audit-request";
pub const AUDIT: &str = "https://wyvrn.app/community/1.0/audit";
pub const MEMBER_HISTORY_REQUEST: &str = "https://wyvrn.app/community/1.0/member-history-request";
pub const MEMBER_HISTORY: &str = "https://wyvrn.app/community/1.0/member-history";

/// The longest timeout: 28 days.
const MAX_TIMEOUT: i64 = 28 * 24 * 3600;
/// The longest slow mode: six hours.
pub const MAX_SLOW_MODE: i64 = 6 * 3600;
/// As far back as a member's posts are removed at once: a week.
const MAX_BULK_SECONDS: i64 = 7 * 24 * 3600;
const MAX_BULK: usize = 100;
const MAX_REASON: usize = 500;
/// The permissions that let someone read a member's history.
const MODERATING: &[&str] = &["manage_reports", "kick_members", "ban_members", "timeout_members"];

/// Why someone may not post now, if they may not: `(code, comment, args)`.
pub struct Refused {
    pub code: &'static str,
    pub comment: String,
    pub args: Value,
}

impl Refused {
    pub fn answer(&self, message: &Value) -> anyhow::Result<Outcome> {
        let mut report = problem(message, self.code, &self.comment);
        report["body"]["args"] = self.args.clone();
        Ok(Outcome::reply(report))
    }
}

impl Community {
    /// Writes in the audit log; a failure to is logged, and doesn't undo what was done.
    pub fn audit(&self, now: i64, actor: &str, action: &str, target: Option<&str>, details: Option<Value>) {
        if let Err(error) = self.store.audit(now, actor, action, target, details.as_ref()) {
            tracing::error!(%error, action, "couldn't write the audit log");
        }
    }

    fn audit_entry(&self, entry: &AuditEntry) -> anyhow::Result<Value> {
        let mut body = json!({ "id": entry.id.to_string(), "at": entry.at, "actor": self.who(&entry.actor)?, "action": entry.action });
        if let Some(target) = &entry.target {
            body["target"] = json!(target);
        }
        if let Some(details) = &entry.details {
            body["details"] = details.clone();
        }
        Ok(body)
    }

    fn bans_body(&self, now: i64) -> anyhow::Result<Value> {
        let mut bans = Vec::new();
        for ban in self.store.bans(now)? {
            bans.push(self.ban_body(&ban)?);
        }
        Ok(json!({ "bans": bans }))
    }

    fn ban_body(&self, ban: &Ban) -> anyhow::Result<Value> {
        // (Someone banned is no member: what they were called is the audit log's to say.)
        let mut body = json!({ "did": ban.did, "by": self.who(&ban.by)?, "at": ban.at });
        if let Some(until) = ban.until {
            body["until"] = json!(until);
        }
        if let Some(reason) = &ban.reason {
            body["reason"] = json!(reason);
        }
        Ok(body)
    }

    /// Why `member` may not post in `channel` now, of what moderation and the join
    /// gates say: timed out, too new, slow mode, or posting too fast anywhere.
    pub fn posting_refused(&self, member: &Member, channel: &Channel, now: i64) -> anyhow::Result<Option<Refused>> {
        if let Some(until) = self.store.timed_out_until(&member.did, now)? {
            return Ok(Some(Refused { code: "e.p.msg.timed-out", comment: "You are timed out here for now.".into(), args: json!({ "until": until }) }));
        }
        let powers = self.powers(&member.did)?;
        if let Some(wait) = self.post_wait()?.filter(|_| powers.roles.is_empty() && !powers.all()) {
            let until = member.since + wait * 60;
            if until > now {
                return Ok(Some(Refused { code: "e.p.msg.too-new", comment: "New members wait a little before posting here.".into(), args: json!({ "until": until }) }));
            }
        }
        if channel.slow_mode > 0 && !self.channel_permissions(&member.did, &powers, channel)?.contains("bypass_slow_mode") {
            if let Some(last) = self.store.last_post_in(&channel.id, &member.did)? {
                let next = last + channel.slow_mode;
                if next > now {
                    return Ok(Some(Refused { code: "e.p.msg.slow-mode", comment: "This channel is in slow mode.".into(), args: json!({ "retry_after": next - now }) }));
                }
            }
        }
        let limit = self.config.max_posts_per_minute as i64;
        if limit > 0 && !powers.all() && self.store.posts_since(&member.did, now - 60)? >= limit {
            return Ok(Some(Refused { code: "e.p.req.rate-limited", comment: "That is faster than anyone posts here. Wait a moment.".into(), args: json!({ "retry_after": 60 }) }));
        }
        Ok(None)
    }

    /// Whether `did` is timed out at `now`.
    pub fn timed_out(&self, did: &str, now: i64) -> anyhow::Result<bool> {
        Ok(self.store.timed_out_until(did, now)?.is_some())
    }

    /// The messages of moderation, from `member`; `None` for anything else.
    pub fn handle_moderation(&self, member: &Member, message_type: &str, message: &Value, now: i64) -> anyhow::Result<Option<Outcome>> {
        let body = &message["body"];
        let sender = member.did.as_str();
        let outcome = match message_type {
            BAN | UNBAN | BANS_REQUEST => {
                let powers = self.powers(sender)?;
                if !powers.has("ban_members") {
                    return refuse(message, "e.p.trust.not-authorized", "Banning takes Ban members.").map(Some);
                }
                if message_type == BANS_REQUEST {
                    return Ok(Some(Outcome::reply(reply_to(message, BANS, self.bans_body(now)?))));
                }
                let Some(did) = body["did"].as_str().filter(|did| did.starts_with("did:") && did.len() <= 4096) else {
                    return refuse(message, "e.p.msg.invalid-ban", "That isn't a DID.").map(Some);
                };
                if message_type == UNBAN {
                    if !self.store.unban(did)? {
                        return refuse(message, "e.p.msg.unknown-ban", "They aren't banned.").map(Some);
                    }
                    self.audit(now, sender, "member.unban", Some(did), None);
                    return Ok(Some(Outcome::reply(reply_to(message, BANS, self.bans_body(now)?))));
                }
                if did == sender {
                    return refuse(message, "e.p.msg.invalid-ban", "You can't ban yourself.").map(Some);
                }
                let existing = self.store.member(did)?;
                if existing.is_some() {
                    if self.owner()?.as_deref() == Some(did) {
                        return refuse(message, "e.p.msg.owner", "The owner can't be banned.").map(Some);
                    }
                    if !self.outranks(&powers, did)? {
                        return refuse(message, "e.p.trust.not-authorized", "You can only ban someone ranked below you.").map(Some);
                    }
                }
                let until = match body.get("seconds").filter(|v| !v.is_null()) {
                    Some(seconds) => match seconds.as_i64().filter(|s| *s >= 60) {
                        Some(seconds) => Some(now + seconds),
                        None => return refuse(message, "e.p.msg.invalid-ban", "A ban lasts at least a minute.").map(Some),
                    },
                    None => None,
                };
                let remove = match body["remove_posts"].as_i64().unwrap_or(0) {
                    seconds @ (0 | 3600 | 86400) => seconds,
                    _ => return refuse(message, "e.p.msg.invalid-ban", "Posts are removed from the last hour, the last day, or not at all.").map(Some),
                };
                let reason = line(&body["reason"], MAX_REASON);
                self.store.ban(&Ban { did: did.to_string(), by: sender.to_string(), at: now, until, reason: reason.clone() })?;
                let was_waiting = self.store.remove_waiting(did)?;
                let mut outcome = Outcome::reply(reply_to(message, BANS, json!({})));
                let mut removed = Vec::new();
                if remove > 0 {
                    removed = self.store.recent_posts_by(None, did, now - remove, 10_000)?;
                    for id in &removed {
                        self.store.delete_post(id, Some(sender))?;
                    }
                    outcome.changed.extend(removed.iter().cloned());
                }
                if existing.is_some() {
                    self.store.remove_member(did)?;
                }
                if existing.is_some() || was_waiting {
                    outcome.notices.push((did.to_string(), json!({ "type": REMOVED, "body": { "was": "banned" } })));
                }
                if was_waiting {
                    outcome.notices.extend(self.admins_told()?);
                }
                let mut details = json!({ "removed": removed.len() });
                if let Some(until) = until {
                    details["until"] = json!(until);
                }
                if let Some(reason) = &reason {
                    details["reason"] = json!(reason);
                }
                self.audit(now, sender, "member.ban", Some(did), Some(details));
                outcome.reply = Some(reply_to(message, BANS, self.bans_body(now)?));
                outcome
            }

            TIMEOUT => {
                let powers = self.powers(sender)?;
                if !powers.has("timeout_members") {
                    return refuse(message, "e.p.trust.not-authorized", "Timing someone out takes Time members out.").map(Some);
                }
                let did = body["did"].as_str().unwrap_or_default();
                let Some(target) = self.store.member(did)? else {
                    return refuse(message, "e.p.msg.invalid-member", "There is no such member.").map(Some);
                };
                if self.owner()?.as_deref() == Some(did) {
                    return refuse(message, "e.p.msg.owner", "The owner can't be timed out.").map(Some);
                }
                if !self.outranks(&powers, did)? {
                    return refuse(message, "e.p.trust.not-authorized", "You can only time out someone ranked below you.").map(Some);
                }
                let seconds = match body["seconds"].as_i64() {
                    Some(0) => 0,
                    Some(seconds) if (60..=MAX_TIMEOUT).contains(&seconds) => seconds,
                    _ => return refuse(message, "e.p.msg.invalid-timeout", "A timeout lasts from a minute to 28 days, or 0 to end one.").map(Some),
                };
                let reason = line(&body["reason"], MAX_REASON);
                let until = (seconds > 0).then_some(now + seconds);
                self.store.set_timeout(did, until, reason.as_deref())?;
                let mut details = json!({ "until": until.unwrap_or(0) });
                if let Some(reason) = &reason {
                    details["reason"] = json!(reason);
                }
                self.audit(now, sender, "member.timeout", Some(did), Some(details));
                let mut outcome = Outcome::reply(reply_to(message, MEMBERS, self.members_for(member)?));
                if target.joined {
                    outcome.notices.push((did.to_string(), json!({ "type": crate::community::COMMUNITY, "body": self.community(&target)? })));
                }
                outcome
            }

            BULK_DELETE => {
                let Some(channel) = self.store.channel(body["channel"].as_str().unwrap_or_default())? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.").map(Some);
                };
                if !self.may_in(sender, &channel, "manage_messages")? {
                    return refuse(message, "e.p.trust.not-authorized", "Removing posts takes managing posts in the channel.").map(Some);
                }
                let ids: Vec<String> = match (body["posts"].as_array(), body["author"].as_str()) {
                    (Some(posts), None) if !posts.is_empty() && posts.len() <= MAX_BULK => {
                        let mut ids = Vec::new();
                        for id in posts.iter().filter_map(Value::as_str) {
                            if let Some(post) = self.store.post_by_id(id)?.filter(|post| post.channel == channel.id && !post.deleted) {
                                ids.push(post.id);
                            }
                        }
                        ids
                    }
                    (None, Some(author)) => {
                        let seconds = body["seconds"].as_i64().filter(|s| (1..=MAX_BULK_SECONDS).contains(s));
                        let Some(seconds) = seconds else {
                            return refuse(message, "e.p.msg.invalid-bulk", "Posts are removed from at most the last week.").map(Some);
                        };
                        self.store.recent_posts_by(Some(&channel.id), author, now - seconds, MAX_BULK)?
                    }
                    _ => return refuse(message, "e.p.msg.invalid-bulk", "Name up to a hundred posts, or a member and how far back.").map(Some),
                };
                for id in &ids {
                    let author = self.store.post_by_id(id)?.map(|post| post.author_did).unwrap_or_default();
                    self.store.delete_post(id, (author != sender).then_some(sender))?;
                }
                let mut details = json!({ "count": ids.len() });
                if let Some(author) = body["author"].as_str() {
                    details["author"] = json!(author);
                }
                self.audit(now, sender, "post.bulk-remove", Some(&channel.id), Some(details));
                Outcome { reply: Some(reply_to(message, BULK_DELETED, json!({ "removed": ids.len() }))), changed: ids, ..Outcome::default() }
            }

            AUDIT_REQUEST => {
                if !self.powers(sender)?.has("view_audit_log") {
                    return refuse(message, "e.p.trust.not-authorized", "The audit log takes Read the audit log.").map(Some);
                }
                let limit = body["limit"].as_u64().map_or(30, |limit| (limit as usize).clamp(1, 100));
                let before = body["before"].as_str().and_then(|id| id.parse().ok()).or(body["before"].as_i64());
                let mut entries = self.store.audit_entries(before, limit + 1, body["actor"].as_str(), body["target"].as_str(), body["action"].as_str())?;
                let more = entries.len() > limit;
                entries.truncate(limit);
                let entries = entries.iter().map(|entry| self.audit_entry(entry)).collect::<anyhow::Result<Vec<_>>>()?;
                Outcome::reply(reply_to(message, AUDIT, json!({ "entries": entries, "more": more })))
            }

            MEMBER_HISTORY_REQUEST => {
                let powers = self.powers(sender)?;
                if !MODERATING.iter().any(|permission| powers.has(permission)) {
                    return refuse(message, "e.p.trust.not-authorized", "A member's history is for those who keep order.").map(Some);
                }
                let did = body["did"].as_str().unwrap_or_default();
                if did.is_empty() {
                    return refuse(message, "e.p.msg.invalid-member", "Whose?").map(Some);
                }
                let mut reports = Vec::new();
                for report in self.store.reports(true)? {
                    let about = report.member.as_deref() == Some(did) || report.post.as_ref().is_some_and(|post| post["author"]["did"] == did);
                    if about {
                        reports.push(self.report_body(&report)?);
                    }
                }
                let actions = self.store.audit_entries(None, 100, None, Some(did), None)?.iter().map(|entry| self.audit_entry(entry)).collect::<anyhow::Result<Vec<_>>>()?;
                let mut history = json!({ "did": did, "reports": reports, "actions": actions });
                if let Some(member) = self.store.member(did)? {
                    history["since"] = json!(member.since);
                    if let Some(name) = member.name {
                        history["name"] = json!(name);
                    }
                }
                if let Some(ban) = self.store.banned(did, now)? {
                    history["banned"] = self.ban_body(&ban)?;
                }
                if let Some(until) = self.store.timed_out_until(did, now)? {
                    history["timed_out_until"] = json!(until);
                }
                Outcome::reply(reply_to(message, MEMBER_HISTORY, history))
            }

            _ => return Ok(None),
        };
        Ok(Some(outcome))
    }
}
