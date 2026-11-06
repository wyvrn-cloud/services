//! How a community is laid out and taken in (community/1.0's "Categories and
//! order", "Onboarding" and "Insights"). The order and categories are set, and
//! onboarding set up and gone through, by workflows (admin_flows.rs); this is what
//! they rest on, and insights.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::community::{refuse, reply_to, Community, Outcome};
use crate::store::Member;

pub const DELETE_CHANNEL: &str = "https://wyvrn.app/community/1.0/delete-channel";
pub const RESTORE_CHANNEL: &str = "https://wyvrn.app/community/1.0/restore-channel";
pub const TRASH_REQUEST: &str = "https://wyvrn.app/community/1.0/trash-request";
pub const TRASH: &str = "https://wyvrn.app/community/1.0/trash";
pub const ARRANGE: &str = "https://wyvrn.app/community/1.0/arrange";
/// How long a deleted channel is kept in the trash before it goes for good.
pub const TRASH_KEPT_FOR: i64 = 30 * DAY;
/// As many categories as a community holds, and the longest a category's name is.
const MAX_CATEGORIES: usize = 50;
const MAX_CATEGORY_NAME: usize = 40;
pub const INSIGHTS_REQUEST: &str = "https://wyvrn.app/community/1.0/insights-request";
pub const INSIGHTS: &str = "https://wyvrn.app/community/1.0/insights";
/// How far back insights go.
const INSIGHT_DAYS: i64 = 90;
const DAY: i64 = 86_400;

/// A day, as insights name it: `YYYY-MM-DD`, UTC, from the second it starts.
fn day_name(start: i64) -> String {
    // Days since 1970 to a civil date (Howard Hinnant's algorithm).
    let z = start.div_euclid(DAY) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

impl Community {
    /// The onboarding as set up: `{ welcome?, suggested: [channel ids] }`, `{}` with none.
    pub fn onboarding(&self) -> anyhow::Result<Value> {
        Ok(self.store.setting("onboarding")?.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_else(|| json!({})))
    }

    /// Whether the community has onboarding to go through: a welcome, roles to pick,
    /// or channels suggested.
    pub fn has_onboarding(&self) -> anyhow::Result<bool> {
        let set = self.onboarding()?;
        let welcome = set["welcome"].as_str().is_some_and(|w| !w.trim().is_empty());
        let suggested = set["suggested"].as_array().is_some_and(|s| !s.is_empty());
        Ok(welcome || suggested || self.store.roles()?.iter().any(|role| role.self_assignable))
    }

    fn insights(&self, now: i64) -> anyhow::Result<Value> {
        let today = now - now.rem_euclid(DAY);
        let since = today - (INSIGHT_DAYS - 1) * DAY;
        let mut days: BTreeMap<i64, Value> = (0..INSIGHT_DAYS)
            .map(|n| {
                let start = since + n * DAY;
                (start, json!({ "day": day_name(start), "posts": 0, "by_channel": {}, "joined": 0, "left": 0, "active": 0 }))
            })
            .collect();
        for (day, channel, count) in self.store.posts_by_day(since)? {
            if let Some(entry) = days.get_mut(&day) {
                entry["posts"] = json!(entry["posts"].as_i64().unwrap_or(0) + count);
                entry["by_channel"][channel] = json!(count);
            }
        }
        for (day, count) in self.store.posters_by_day(since)? {
            if let Some(entry) = days.get_mut(&day) {
                entry["active"] = json!(count);
            }
        }
        for (day, kind, count) in self.store.member_events_by_day(since)? {
            if let Some(entry) = days.get_mut(&day).filter(|_| kind == "joined" || kind == "left") {
                entry[kind.as_str()] = json!(count);
            }
        }
        Ok(json!({ "members": self.store.members()?.len(), "days": days.into_values().collect::<Vec<_>>() }))
    }

    /// `insights-request`; `None` for anything else.
    pub fn handle_structure(&self, member: &Member, message_type: &str, message: &Value, now: i64) -> anyhow::Result<Option<Outcome>> {
        match message_type {
            DELETE_CHANNEL | RESTORE_CHANNEL | TRASH_REQUEST => return self.handle_trash(member, message_type, message, now).map(Some),
            ARRANGE => return self.arrange(member, message, now).map(Some),
            _ => {}
        }
        if message_type != INSIGHTS_REQUEST {
            return Ok(None);
        }
        if !self.powers(&member.did)?.has("view_insights") {
            return refuse(message, "e.p.trust.not-authorized", "Insights take Read insights.").map(Some);
        }
        Ok(Some(Outcome::reply(reply_to(message, INSIGHTS, self.insights(now)?))))
    }

    /// A channel deleted, into the trash for [`TRASH_KEPT_FOR`] (by whoever may
    /// change it); one brought back out; and what is in it, for those who manage
    /// channels.
    fn handle_trash(&self, member: &Member, message_type: &str, message: &Value, now: i64) -> anyhow::Result<Outcome> {
        let sender = member.did.as_str();
        let manages = self.powers(sender)?.has("manage_channels");
        let id = message["body"]["channel"].as_str().unwrap_or_default();
        match message_type {
            DELETE_CHANNEL => {
                let Some(channel) = self.store.channel(id)? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.");
                };
                if channel.created_by != sender && !manages {
                    return refuse(message, "e.p.trust.not-authorized", "Only whoever opened a channel, or someone who manages channels, can delete it.");
                }
                self.store.trash_channel(&channel.id, sender, now)?;
                self.audit(now, sender, "channel.delete", Some(&channel.id), Some(json!({ "name": channel.name })));
                Ok(Outcome { reply: Some(reply_to(message, crate::community::COMMUNITY, self.community(member)?)), channels_changed: true, ..Outcome::default() })
            }
            RESTORE_CHANNEL => {
                if !manages {
                    return refuse(message, "e.p.trust.not-authorized", "The trash is for those who manage channels.");
                }
                let Some((channel, _, _)) = self.store.trashed_channels()?.into_iter().find(|(channel, _, _)| channel.id == id) else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such channel in the trash.");
                };
                if !channel.closed && self.store.channel_named_among(&channel.name, channel.voice.is_some())?.is_some() {
                    return refuse(message, "e.p.msg.name-taken", "Another channel has its name now: rename that one first.");
                }
                self.store.restore_channel(&channel.id)?;
                self.audit(now, sender, "channel.restore", Some(&channel.id), Some(json!({ "name": channel.name })));
                Ok(Outcome { reply: Some(reply_to(message, TRASH, self.trash_body()?)), channels_changed: true, ..Outcome::default() })
            }
            _ => {
                if !manages {
                    return refuse(message, "e.p.trust.not-authorized", "The trash is for those who manage channels.");
                }
                Ok(Outcome::reply(reply_to(message, TRASH, self.trash_body()?)))
            }
        }
    }

    fn trash_body(&self) -> anyhow::Result<Value> {
        let channels: Vec<Value> = self
            .store
            .trashed_channels()?
            .into_iter()
            .map(|(channel, at, by)| {
                let kind = if channel.voice.is_some() { "voice" } else if channel.forum.is_some() { "forum" } else { "text" };
                json!({ "id": channel.id, "name": channel.name, "kind": kind, "deleted_at": at, "by": by, "purge_at": at + TRASH_KEPT_FOR })
            })
            .collect();
        Ok(json!({ "channels": channels }))
    }

    /// The order of the categories and channels, and which category each channel
    /// is in, all at once (`manage_channels`): what a list dragged into a new order
    /// sends. A category not listed is deleted, its channels going to none; one
    /// listed with an id not known yet is made, with that id.
    fn arrange(&self, member: &Member, message: &Value, now: i64) -> anyhow::Result<Outcome> {
        let sender = member.did.as_str();
        if !self.powers(sender)?.has("manage_channels") {
            return refuse(message, "e.p.trust.not-authorized", "Arranging channels takes Manage channels.");
        }
        let body = &message["body"];
        let mut categories = Vec::new();
        for (position, given) in body["categories"].as_array().map(Vec::as_slice).unwrap_or_default().iter().enumerate() {
            let id = given["id"].as_str().unwrap_or_default();
            if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return refuse(message, "e.p.msg.invalid-category", "A category's id is 1 to 32 letters, digits, - and _.");
            }
            let Some(name) = crate::community::line(&given["name"], MAX_CATEGORY_NAME) else {
                return refuse(message, "e.p.msg.empty", "A category needs a name.");
            };
            if categories.iter().any(|c: &crate::store::Category| c.id == id) {
                return refuse(message, "e.p.msg.invalid-category", "A category is listed once.");
            }
            categories.push(crate::store::Category { id: id.to_string(), name, position: position as i64 });
        }
        if categories.len() > MAX_CATEGORIES {
            return refuse(message, "e.p.msg.too-many", "That is as many categories as a community holds.");
        }
        let held = self.store.channels()?;
        let mut order: Vec<(String, Option<String>)> = Vec::new();
        for given in body["channels"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let id = given["id"].as_str().unwrap_or_default();
            if !held.iter().any(|channel| channel.id == id) || order.iter().any(|(listed, _)| listed == id) {
                return refuse(message, "e.p.msg.unknown-channel", "Each channel is listed once, and only channels there are.");
            }
            let category = match given["category"].as_str() {
                Some(category) if categories.iter().any(|c| c.id == category) => Some(category.to_string()),
                Some(_) => return refuse(message, "e.p.msg.invalid-category", "A channel's category is one of those listed."),
                None => None,
            };
            order.push((id.to_string(), category));
        }
        // Channels not listed keep their order, after those that are, in the category they were in where it stays.
        for channel in &held {
            if !order.iter().any(|(id, _)| *id == channel.id) {
                order.push((channel.id.clone(), channel.category.clone().filter(|id| categories.iter().any(|c| c.id == *id))));
            }
        }
        for gone in self.store.categories()?.into_iter().filter(|old| !categories.iter().any(|c| c.id == old.id)) {
            self.store.delete_category(&gone.id)?;
        }
        for category in &categories {
            self.store.save_category(category)?;
        }
        self.store.order_channels(&order)?;
        self.audit(now, sender, "channel.update", None, Some(json!({ "arranged": true })));
        Ok(Outcome { reply: Some(reply_to(message, crate::community::COMMUNITY, self.community(member)?)), channels_changed: true, ..Outcome::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_are_named_as_dates() {
        assert_eq!(day_name(0), "1970-01-01");
        assert_eq!(day_name(951_782_400), "2000-02-29");
        assert_eq!(day_name(1_792_972_800), "2026-10-26");
    }
}
