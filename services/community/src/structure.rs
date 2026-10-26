//! How a community is laid out and taken in (community/1.0's "Categories and
//! order", "Onboarding" and "Insights"). The order and categories are set, and
//! onboarding set up and gone through, by workflows (admin_flows.rs); this is what
//! they rest on, and insights.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::community::{refuse, reply_to, Community, Outcome};
use crate::store::Member;

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
        if message_type != INSIGHTS_REQUEST {
            return Ok(None);
        }
        if !self.powers(&member.did)?.has("view_insights") {
            return refuse(message, "e.p.trust.not-authorized", "Insights take Read insights.").map(Some);
        }
        Ok(Some(Outcome::reply(reply_to(message, INSIGHTS, self.insights(now)?))))
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
