//! What a ban says to whoever it bans, and their appeal against it (community/1.0's
//! "Bans and appeals").
//!
//! Someone banned is told when it was, until when, the reason where the moderator
//! chose to share it, and whether and when they may appeal. That is all they may
//! still ask a community: how their ban stands (`ban-status-request`), and to have
//! it looked at again (`appeal`): one at a time, not before the community's wait
//! after the ban, nor before its cooldown after one refused. Appeals go to whoever
//! holds Ban members, who lift the ban, refuse (saying why, or not), or refuse and
//! close appeals: a permanent ban, with nothing more to ask.

use serde_json::{json, Value};
use wyvrn_service::params;

use crate::community::{refuse, reply_to, Community, Outcome, COMMUNITY};
use crate::store::{Ban, Store};

pub const BAN_STATUS_REQUEST: &str = "https://wyvrn.app/community/1.0/ban-status-request";
pub const BAN_STATUS: &str = "https://wyvrn.app/community/1.0/ban-status";
pub const APPEAL: &str = "https://wyvrn.app/community/1.0/appeal";
pub const APPEALS_REQUEST: &str = "https://wyvrn.app/community/1.0/appeals-request";
pub const APPEALS: &str = "https://wyvrn.app/community/1.0/appeals";
pub const DECIDE_APPEAL: &str = "https://wyvrn.app/community/1.0/decide-appeal";
pub const SET_APPEALS: &str = "https://wyvrn.app/community/1.0/set-appeals";

/// The longest appeal, and a moderator's answer to one.
const MAX_APPEAL: usize = 2000;
const MAX_REPLY: usize = 1000;
/// Unless the community says otherwise: a day after the ban before the first
/// appeal, and a week after one refused before the next.
const WAIT: i64 = 24 * 3600;
const COOLDOWN: i64 = 7 * 24 * 3600;
/// The longest either may be set to: 90 days.
const MAX_SECONDS: i64 = 90 * 24 * 3600;

/// What a ban says beside itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Terms {
    /// The reason is shown to whoever was banned.
    pub shared: bool,
    /// No appeals: a permanent ban.
    pub closed: bool,
    /// Banned by the server itself, holding a banned person's account.
    pub auto: bool,
}

#[derive(Debug, Clone)]
pub struct Appeal {
    pub id: String,
    pub did: String,
    pub ban_at: i64,
    pub text: String,
    pub at: i64,
    /// `open`, `accepted` or `refused`.
    pub state: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<i64>,
    pub reply: Option<String>,
}

const APPEAL_COLUMNS: &str = "id, did, ban_at, text, at, state, decided_by, decided_at, reply";

fn appeal_from(row: &wyvrn_service::db::Row) -> wyvrn_service::db::Result<Appeal> {
    Ok(Appeal {
        id: row.get(0)?,
        did: row.get(1)?,
        ban_at: row.get(2)?,
        text: row.get(3)?,
        at: row.get(4)?,
        state: row.get(5)?,
        decided_by: row.get(6)?,
        decided_at: row.get(7)?,
        reply: row.get(8)?,
    })
}

impl Store {
    pub fn ban_terms(&self, did: &str) -> wyvrn_service::db::Result<Terms> {
        let found = self.db.lock().query_row("SELECT shared, closed, auto FROM ban_terms WHERE did = ?1", params![did], |row| {
            Ok(Terms { shared: row.get::<i64>(0)? != 0, closed: row.get::<i64>(1)? != 0, auto: row.get::<i64>(2)? != 0 })
        })?;
        Ok(found.unwrap_or_default())
    }

    pub fn set_ban_terms(&self, did: &str, terms: &Terms) -> wyvrn_service::db::Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO ban_terms (did, shared, closed, auto) VALUES (?1, ?2, ?3, ?4)",
            params![did, terms.shared as i64, terms.closed as i64, terms.auto as i64],
        )?;
        Ok(())
    }

    pub fn forget_ban_terms(&self, did: &str) -> wyvrn_service::db::Result<()> {
        self.db.lock().execute("DELETE FROM ban_terms WHERE did = ?1", params![did])?;
        Ok(())
    }

    pub fn add_appeal(&self, appeal: &Appeal) -> wyvrn_service::db::Result<()> {
        self.db.lock().execute(
            &format!("INSERT INTO ban_appeals ({APPEAL_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"),
            params![appeal.id, appeal.did, appeal.ban_at, appeal.text, appeal.at, appeal.state, appeal.decided_by, appeal.decided_at, appeal.reply],
        )?;
        Ok(())
    }

    pub fn appeal(&self, id: &str) -> wyvrn_service::db::Result<Option<Appeal>> {
        self.db.lock().query_row(&format!("SELECT {APPEAL_COLUMNS} FROM ban_appeals WHERE id = ?1"), params![id], appeal_from)
    }

    /// `did`'s appeals, newest first.
    pub fn appeals_of(&self, did: &str) -> wyvrn_service::db::Result<Vec<Appeal>> {
        self.db.lock().query(&format!("SELECT {APPEAL_COLUMNS} FROM ban_appeals WHERE did = ?1 ORDER BY at DESC"), params![did], appeal_from)
    }

    /// Those waiting for a moderator, oldest first; or every one, newest first.
    pub fn appeals(&self, decided_too: bool) -> wyvrn_service::db::Result<Vec<Appeal>> {
        let query = if decided_too {
            format!("SELECT {APPEAL_COLUMNS} FROM ban_appeals ORDER BY at DESC LIMIT 200")
        } else {
            format!("SELECT {APPEAL_COLUMNS} FROM ban_appeals WHERE state = 'open' ORDER BY at")
        };
        self.db.lock().query(&query, params![], appeal_from)
    }

    pub fn decide_appeal(&self, id: &str, state: &str, by: &str, at: i64, reply: Option<&str>) -> wyvrn_service::db::Result<()> {
        self.db.lock().execute(
            "UPDATE ban_appeals SET state = ?2, decided_by = ?3, decided_at = ?4, reply = ?5 WHERE id = ?1",
            params![id, state, by, at, reply],
        )?;
        Ok(())
    }
}

/// A moderator's answer: kept as written, lines and all, up to `max` characters.
fn text_of(value: &Value, max: usize) -> Option<String> {
    let text = value.as_str()?.trim();
    let text: String = text.chars().take(max).collect();
    (!text.is_empty()).then_some(text)
}

impl Community {
    /// Whether appeals are taken, the wait after a ban and the cooldown after one refused.
    pub fn appeal_settings(&self) -> anyhow::Result<(bool, i64, i64)> {
        let number = |name: &str, default: i64| -> anyhow::Result<i64> { Ok(self.store.setting(name)?.and_then(|v| v.parse().ok()).unwrap_or(default)) };
        Ok((self.store.setting("appeals")?.as_deref() != Some("off"), number("appeal_wait", WAIT)?, number("appeal_cooldown", COOLDOWN)?))
    }

    /// Bans `did` saying what the ban says to them: kept beside the ban.
    pub fn keep_ban_terms(&self, did: &str, terms: &Terms) -> anyhow::Result<()> {
        self.store.set_ban_terms(did, terms)?;
        Ok(())
    }

    /// How `ban` stands for whoever it bans: what `ban-status` and `removed` say.
    pub fn ban_status_of(&self, ban: &Ban, now: i64) -> anyhow::Result<Value> {
        let terms = self.store.ban_terms(&ban.did)?;
        let mut status = json!({ "banned": true, "at": ban.at });
        let name = self.changed().name.unwrap_or_else(|| self.config.name.clone());
        if !name.is_empty() {
            status["community"] = json!(name);
        }
        if let Some(until) = ban.until {
            status["until"] = json!(until);
        }
        if let Some(reason) = ban.reason.as_ref().filter(|_| terms.shared || terms.auto) {
            status["reason"] = json!(reason);
        }
        if terms.auto {
            status["auto"] = json!(true);
        }
        let appeals = self.store.appeals_of(&ban.did)?;
        let this_ban: Vec<&crate::appeals::Appeal> = appeals.iter().filter(|a| a.ban_at == ban.at).collect();
        if let Some(latest) = this_ban.first() {
            let mut appeal = json!({ "text": latest.text, "at": latest.at, "state": latest.state });
            if let Some(reply) = &latest.reply {
                appeal["reply"] = json!(reply);
            }
            if let Some(at) = latest.decided_at {
                appeal["decided_at"] = json!(at);
            }
            status["appeal"] = appeal;
        }
        let (taken, wait, cooldown) = self.appeal_settings()?;
        status["appeals"] = if terms.closed {
            json!({ "state": "closed" })
        } else if !taken {
            json!({ "state": "off" })
        } else if this_ban.first().is_some_and(|a| a.state == "open") {
            json!({ "state": "pending" })
        } else {
            let after_refusal = this_ban.iter().filter_map(|a| a.decided_at).max().map(|at| at + cooldown);
            let from = (ban.at + wait).max(after_refusal.unwrap_or(0));
            if from > now { json!({ "state": "wait", "from": from }) } else { json!({ "state": "open" }) }
        };
        Ok(status)
    }

    /// What `did` is told of their ban (or that there is none, now).
    pub fn ban_status(&self, did: &str, now: i64) -> anyhow::Result<Value> {
        match self.store.banned(did, now)? {
            Some(ban) => self.ban_status_of(&ban, now),
            None => Ok(json!({ "banned": false })),
        }
    }

    /// The `removed` notice a ban sends: with how it stands.
    pub fn removed_banned(&self, did: &str, now: i64) -> anyhow::Result<Value> {
        Ok(json!({ "type": crate::community::REMOVED, "body": { "was": "banned", "ban": self.ban_status(did, now)? } }))
    }

    /// The two things someone who isn't a member may ask because they were banned.
    pub fn handle_banned(&self, sender: &str, message: &Value, now: i64) -> anyhow::Result<Option<Outcome>> {
        let message_type = message["type"].as_str().unwrap_or_default();
        match message_type {
            BAN_STATUS_REQUEST => Ok(Some(Outcome::reply(reply_to(message, BAN_STATUS, self.ban_status(sender, now)?)))),
            APPEAL => {
                let Some(ban) = self.store.banned(sender, now)? else {
                    return refuse(message, "e.p.msg.unknown-ban", "You aren't banned here.").map(Some);
                };
                let status = self.ban_status_of(&ban, now)?;
                match status["appeals"]["state"].as_str().unwrap_or_default() {
                    "open" => {}
                    "wait" => {
                        let mut refused = crate::community::problem(message, "e.p.msg.appeal-wait", "You can't appeal yet.");
                        refused["body"]["args"] = json!({ "from": status["appeals"]["from"] });
                        return Ok(Some(Outcome::reply(refused)));
                    }
                    "pending" => return refuse(message, "e.p.msg.appeal-pending", "Your appeal is waiting for a moderator.").map(Some),
                    _ => return refuse(message, "e.p.msg.appeal-closed", "This ban can't be appealed.").map(Some),
                }
                let Some(text) = text_of(&message["body"]["text"], MAX_APPEAL) else {
                    return refuse(message, "e.p.msg.invalid-appeal", "Say why the ban should be lifted.").map(Some);
                };
                let id = uuid::Uuid::new_v4().to_string();
                self.store.add_appeal(&Appeal { id: id.clone(), did: sender.into(), ban_at: ban.at, text, at: now, state: "open".into(), decided_by: None, decided_at: None, reply: None })?;
                self.audit(now, sender, "appeal.send", Some(sender), Some(json!({ "appeal": id, "name": ban.name })));
                let mut outcome = Outcome::reply(reply_to(message, BAN_STATUS, self.ban_status_of(&ban, now)?));
                outcome.notices.extend(self.moderators_told()?);
                Ok(Some(outcome))
            }
            _ => Ok(None),
        }
    }

    /// Whoever deals with appeals, told how many wait (in the community's details).
    fn moderators_told(&self) -> anyhow::Result<Vec<(String, Value)>> {
        self.joined_holding("ban_members")?.iter().map(|m| Ok((m.did.clone(), json!({ "type": COMMUNITY, "body": self.community(m)? })))).collect()
    }

    pub fn open_appeals(&self) -> anyhow::Result<usize> {
        Ok(self.store.appeals(false)?.len())
    }

    fn appeal_body(&self, appeal: &Appeal, now: i64) -> anyhow::Result<Value> {
        let mut body = json!({ "id": appeal.id, "did": appeal.did, "text": appeal.text, "at": appeal.at, "state": appeal.state });
        if let Some(by) = &appeal.decided_by {
            body["decided_by"] = self.who(by)?;
        }
        if let Some(at) = appeal.decided_at {
            body["decided_at"] = json!(at);
        }
        if let Some(reply) = &appeal.reply {
            body["reply"] = json!(reply);
        }
        // The ban it is against, as it stands (if it still does), and what came before.
        if let Some(ban) = self.store.banned(&appeal.did, now)?.filter(|ban| ban.at == appeal.ban_at) {
            let terms = self.store.ban_terms(&ban.did)?;
            let mut said = json!({ "at": ban.at, "by": self.who(&ban.by)?, "shared": terms.shared, "auto": terms.auto });
            if let Some(name) = &ban.name {
                body["name"] = json!(name);
            }
            if let Some(until) = ban.until {
                said["until"] = json!(until);
            }
            if let Some(reason) = &ban.reason {
                said["reason"] = json!(reason);
            }
            body["ban"] = said;
        }
        let earlier = self.store.appeals_of(&appeal.did)?.iter().filter(|a| a.id != appeal.id && a.at < appeal.at).count();
        if earlier > 0 {
            body["earlier"] = json!(earlier);
        }
        Ok(body)
    }

    fn appeals_body(&self, decided_too: bool, now: i64) -> anyhow::Result<Value> {
        let appeals = self.store.appeals(decided_too)?.iter().map(|a| self.appeal_body(a, now)).collect::<anyhow::Result<Vec<_>>>()?;
        Ok(json!({ "appeals": appeals }))
    }

    /// Lifts `did`'s ban: theirs, its accounts' and what it said. Whether there was one.
    pub fn lift_ban(&self, did: &str) -> anyhow::Result<bool> {
        self.store.unban_accounts_of(did)?;
        self.store.forget_ban_terms(did)?;
        Ok(self.store.unban(did)?)
    }

    /// A moderator's side: the appeals, deciding one, and the community's settings for them.
    pub fn handle_appeals(&self, sender: &str, message_type: &str, message: &Value, now: i64) -> anyhow::Result<Option<Outcome>> {
        if ![APPEALS_REQUEST, DECIDE_APPEAL, SET_APPEALS].contains(&message_type) {
            return Ok(None);
        }
        let body = &message["body"];
        let powers = self.powers(sender)?;
        if message_type == SET_APPEALS {
            if !powers.has("manage_community") {
                return refuse(message, "e.p.trust.not-authorized", "That takes Manage community.").map(Some);
            }
            if let Some(on) = body["on"].as_bool() {
                self.store.set_setting("appeals", if on { "on" } else { "off" })?;
            }
            for (key, name) in [("wait", "appeal_wait"), ("cooldown", "appeal_cooldown")] {
                if let Some(seconds) = body.get(key).filter(|v| !v.is_null()) {
                    let Some(seconds) = seconds.as_i64().filter(|s| (0..=MAX_SECONDS).contains(s)) else {
                        return refuse(message, "e.p.msg.invalid-appeals", "Waits are from none to 90 days.").map(Some);
                    };
                    self.store.set_setting(name, &seconds.to_string())?;
                }
            }
            let (on, wait, cooldown) = self.appeal_settings()?;
            self.audit(now, sender, "community.appeals", None, Some(json!({ "on": on, "wait": wait, "cooldown": cooldown })));
            let member = self.store.member(sender)?.expect("a member");
            return Ok(Some(Outcome::reply(reply_to(message, COMMUNITY, self.community(&member)?))));
        }
        if !powers.has("ban_members") {
            return refuse(message, "e.p.trust.not-authorized", "Appeals are for those who may ban.").map(Some);
        }
        if message_type == APPEALS_REQUEST {
            return Ok(Some(Outcome::reply(reply_to(message, APPEALS, self.appeals_body(body["decided"] == true, now)?))));
        }
        let Some(appeal) = self.store.appeal(body["id"].as_str().unwrap_or_default())?.filter(|a| a.state == "open") else {
            return refuse(message, "e.p.msg.unknown-appeal", "There is no such appeal waiting.").map(Some);
        };
        let reply = text_of(&body["reply"], MAX_REPLY);
        let decision = body["decision"].as_str().unwrap_or_default();
        // What they were called, for the audit log: no member, so not otherwise named.
        let name = self.store.banned(&appeal.did, now)?.and_then(|ban| ban.name);
        let mut outcome = Outcome::default();
        match decision {
            "accept" => {
                self.store.decide_appeal(&appeal.id, "accepted", sender, now, reply.as_deref())?;
                self.lift_ban(&appeal.did)?;
                let mut told = json!({ "banned": false, "lifted": true });
                if let Some(reply) = &reply {
                    told["reply"] = json!(reply);
                }
                outcome.notices.push((appeal.did.clone(), json!({ "type": BAN_STATUS, "body": told })));
            }
            "refuse" | "close" => {
                self.store.decide_appeal(&appeal.id, "refused", sender, now, reply.as_deref())?;
                if decision == "close" {
                    // Nothing more to ask: banned for good.
                    if let Some(mut ban) = self.store.banned(&appeal.did, now)? {
                        ban.until = None;
                        self.store.ban(&ban)?;
                    }
                    let terms = Terms { closed: true, ..self.store.ban_terms(&appeal.did)? };
                    self.store.set_ban_terms(&appeal.did, &terms)?;
                }
                outcome.notices.push((appeal.did.clone(), json!({ "type": BAN_STATUS, "body": self.ban_status(&appeal.did, now)? })));
            }
            _ => return refuse(message, "e.p.msg.invalid-appeal", "An appeal is accepted, refused, or refused and closed.").map(Some),
        }
        let action = match decision {
            "accept" => "appeal.accept",
            "refuse" => "appeal.refuse",
            _ => "appeal.close",
        };
        let mut details = json!({ "appeal": appeal.id });
        if let Some(name) = &name {
            details["name"] = json!(name);
        }
        if let Some(reply) = &reply {
            details["reply"] = json!(reply);
        }
        self.audit(now, sender, action, Some(&appeal.did), Some(details));
        outcome.notices.extend(self.moderators_told()?.into_iter().filter(|(did, _)| did != sender));
        outcome.reply = Some(reply_to(message, APPEALS, self.appeals_body(false, now)?));
        Ok(Some(outcome))
    }
}
