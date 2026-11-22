//! Keeping a community from being overrun (community/1.0's "Bans that hold, and
//! raids"): bans that cover a banned person's verified linked accounts, so a new
//! DID showing one of them is banned on sight; gates that a new account can't
//! simply walk through (a verified linked account to post; no links or files for a
//! member's first hours); and raid mode, which a moderator turns on for a while and
//! the server suggests when people join in a burst: no one new let in, and every
//! channel slow for everyone without a role.

use serde_json::{json, Value};
use wyvrn_service::params;

use crate::community::Community;
use crate::store::{Member, Store};

/// How long a raid's burst of joins is counted over, and how many make one, unless
/// the community says (`raid_threshold`).
pub const BURST_WINDOW: i64 = 10 * 60;
pub const BURST_JOINS: i64 = 10;
/// Seconds between posts, anywhere, in raid mode, for someone with no role.
pub const RAID_SLOW: i64 = 30;
/// The longest raid mode is turned on for at once: a day.
pub const RAID_LONGEST: i64 = 24 * 3600;

impl Store {
    /// The linked accounts a member's profile was last checked to prove:
    /// `(service, account)`, the service by its homepage.
    pub fn member_accounts(&self, did: &str) -> wyvrn_service::db::Result<Vec<(String, String)>> {
        self.db.lock().query("SELECT service, account FROM member_accounts WHERE did = ?1 ORDER BY service", params![did], |row| Ok((row.get(0)?, row.get(1)?)))
    }

    pub fn set_member_accounts(&self, did: &str, accounts: &[(String, String)], now: i64) -> wyvrn_service::db::Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM member_accounts WHERE did = ?1", params![did])?;
        for (service, account) in accounts {
            db.execute("INSERT OR IGNORE INTO member_accounts (did, service, account, at) VALUES (?1, ?2, ?3, ?4)", params![did, service, account, now])?;
        }
        Ok(())
    }

    /// Bans the accounts `did` was checked to hold, for as long as `did` is.
    pub fn ban_accounts_of(&self, did: &str, until: Option<i64>, now: i64) -> wyvrn_service::db::Result<usize> {
        let accounts = self.member_accounts(did)?;
        let mut db = self.db.lock();
        for (service, account) in &accounts {
            db.execute(
                "INSERT OR REPLACE INTO banned_accounts (service, account, banned_did, until, at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![service, account, did, until, now],
            )?;
        }
        Ok(accounts.len())
    }

    pub fn unban_accounts_of(&self, did: &str) -> wyvrn_service::db::Result<()> {
        self.db.lock().execute("DELETE FROM banned_accounts WHERE banned_did = ?1", params![did])?;
        Ok(())
    }

    /// Who was banned holding one of `accounts`, if anyone (a ban still running).
    pub fn banned_holding(&self, accounts: &[(String, String)], now: i64) -> wyvrn_service::db::Result<Option<(String, String, String)>> {
        for (service, account) in accounts {
            let found: Option<String> = self.db.lock().query_row(
                "SELECT banned_did FROM banned_accounts WHERE service = ?1 AND account = ?2 AND (until IS NULL OR until > ?3)",
                params![service, account, now],
                |row| row.get(0),
            )?;
            if let Some(banned) = found {
                return Ok(Some((banned, service.clone(), account.clone())));
            }
        }
        Ok(None)
    }

    /// How many were let in, or asked to be, since `since`.
    pub fn joined_since(&self, since: i64) -> wyvrn_service::db::Result<i64> {
        // (And those asking: a community where an admin lets people in is raided
        // through its waiting list.)
        let mut db = self.db.lock();
        let members: i64 = db.query_value("SELECT COUNT(*) FROM members WHERE since >= ?1", params![since])?;
        let asking: i64 = db.query_value("SELECT COUNT(*) FROM waiting WHERE asked_at >= ?1", params![since])?;
        Ok(members + asking)
    }
}

impl Community {
    /// Raid mode, while on: `{ until, by, at }`.
    pub fn raid(&self, now: i64) -> anyhow::Result<Option<Value>> {
        let raid = self.store.setting("raid")?.and_then(|text| serde_json::from_str::<Value>(&text).ok());
        Ok(raid.filter(|r| r["until"].as_i64().is_some_and(|until| until > now)))
    }

    /// Turns raid mode on for `minutes` (0 turns it off).
    pub fn set_raid(&self, by: &str, minutes: i64, now: i64) -> anyhow::Result<()> {
        if minutes <= 0 {
            self.store.set_setting("raid", "{}")?;
        } else {
            let until = now + (minutes * 60).min(RAID_LONGEST);
            self.store.set_setting("raid", &json!({ "until": until, "by": by, "at": now }).to_string())?;
        }
        Ok(())
    }

    /// People let in at a raid's pace just now, with raid mode off: how many, for
    /// moderators to be asked whether to turn it on.
    pub fn raid_suggested(&self, now: i64) -> anyhow::Result<Option<i64>> {
        if self.raid(now)?.is_some() {
            return Ok(None);
        }
        let threshold = self.gates()?["raid_threshold"].as_i64().filter(|n| *n > 1).unwrap_or(BURST_JOINS);
        let joins = self.store.joined_since(now - BURST_WINDOW)?;
        Ok((joins >= threshold).then_some(joins))
    }

    /// Bans `did`, who holds `service`'s `account` that `banned` was banned holding:
    /// a ban coming back under another DID, for as long as that one runs.
    pub fn ban_evading(&self, did: &str, banned: &str, service: &str, account: &str, now: i64) -> anyhow::Result<crate::community::Outcome> {
        let until = self.store.banned(banned, now)?.and_then(|ban| ban.until);
        let name = self.store.member(did)?.and_then(|m| m.name).or(self.store.waiting_one(did)?.and_then(|w| w.name));
        let reason = format!("Holds the {service} account of someone banned here");
        self.store.ban(&crate::store::Ban { did: did.to_string(), name, by: self.own.clone().unwrap_or_default(), at: now, until, reason: Some(reason.clone()) })?;
        self.store.ban_accounts_of(did, until, now)?;
        let was_waiting = self.store.remove_waiting(did)?;
        let was_member = self.store.member(did)?.is_some();
        if was_member {
            self.store.remove_member(did)?;
        }
        let mut outcome = crate::community::Outcome::default();
        if was_member || was_waiting {
            outcome.notices.push((did.to_string(), json!({ "type": crate::community::REMOVED, "body": { "was": "banned" } })));
        }
        if was_waiting {
            outcome.notices.extend(self.admins_told()?);
        }
        self.audit(now, self.own.as_deref().unwrap_or_default(), "member.ban-evasion", Some(did), Some(json!({ "as": banned, "service": service, "account": account, "until": until })));
        Ok(outcome)
    }

    /// Whether `member` is held back as new: no role, and joined within the gates'
    /// `new_member_hours`.
    pub fn is_new_member(&self, member: &Member, now: i64) -> anyhow::Result<bool> {
        let Some(hours) = self.gates()?["new_member_hours"].as_i64().filter(|h| *h > 0) else { return Ok(false) };
        let powers = self.powers(&member.did)?;
        Ok(powers.roles.is_empty() && !powers.all() && member.since + hours * 3600 > now)
    }

    /// Why what `member` posts may not be posted, of what the protections say: a
    /// verified linked account to post at all; no links, files or `everyone` for a
    /// new member; raid mode's slowness.
    pub fn protection_refused(&self, member: &Member, post: &Value, now: i64) -> anyhow::Result<Option<crate::moderation::Refused>> {
        let refused = |code: &'static str, comment: &str, args: Value| Ok(Some(crate::moderation::Refused { code, comment: comment.into(), args }));
        let powers = self.powers(&member.did)?;
        if powers.all() || !powers.roles.is_empty() {
            return Ok(None);
        }
        let gates = self.gates()?;
        if gates["linked_account"] == true && self.store.member_accounts(&member.did)?.is_empty() {
            return refused("e.p.msg.needs-linked-account", "This community asks for a verified linked account to post: show one in your profile here.", json!({}));
        }
        if self.is_new_member(member, now)? {
            let content = post["content"].as_str().unwrap_or_default();
            let links = content.contains("http://") || content.contains("https://") || content.contains("www.");
            let files = post.get("media").is_some_and(|m| m.as_array().is_some_and(|a| !a.is_empty()));
            let everyone = post["mentions"].as_array().into_iter().flatten().any(|m| m["who"] == "everyone" || m["who"].as_str().is_some_and(|w| w.starts_with("role:")));
            if links || files || everyone {
                let hours = gates["new_member_hours"].as_i64().unwrap_or(0);
                return refused(
                    "e.p.msg.too-new",
                    "New members post links, files and mentions of everyone after a while here.",
                    json!({ "until": member.since + hours * 3600 }),
                );
            }
        }
        if self.raid(now)?.is_some() {
            if let Some(last) = self.store.last_post_by(&member.did)? {
                if last + RAID_SLOW > now {
                    return refused("e.p.msg.slow-mode", "This community is slowed down for now.", json!({ "retry_after": last + RAID_SLOW - now }));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ban_covers_the_accounts_it_was_checked_to_hold_until_it_ends() {
        let store = Store::in_memory().unwrap();
        let twitch = ("https://twitch.tv".to_string(), "4242".to_string());
        store.set_member_accounts("did:example:troll", std::slice::from_ref(&twitch), 1).unwrap();
        assert_eq!(store.banned_holding(std::slice::from_ref(&twitch), 10).unwrap(), None);
        store.ban_accounts_of("did:example:troll", Some(100), 10).unwrap();
        assert_eq!(store.banned_holding(std::slice::from_ref(&twitch), 50).unwrap().map(|b| b.0), Some("did:example:troll".to_string()));
        assert_eq!(store.banned_holding(std::slice::from_ref(&twitch), 100).unwrap(), None, "over when the ban is");
        store.unban_accounts_of("did:example:troll").unwrap();
        store.ban_accounts_of("did:example:troll", None, 10).unwrap();
        store.unban_accounts_of("did:example:troll").unwrap();
        assert_eq!(store.banned_holding(&[twitch], 50).unwrap(), None);
    }
}
