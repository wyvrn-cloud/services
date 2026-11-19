//! A community's events (community/1.0's "Events", asked 2026-11-18): the server
//! is each event's organizer and scheduler in calendar/1.0's terms. Members with
//! `create_events` make them and change their own, `manage_events` anyone's;
//! everyone answers with calendar/1.0's `accept`, `tentative` and `decline`, is
//! reminded with its `reminder`, and may subscribe to an iCalendar feed.
//!
//! An event is kept as its fields (`data`), who made it, its `sequence` and
//! whether it was cancelled; what it looks like to one member (how many are
//! going, their own answer, its status and next occurrence) is worked out each
//! time. A recurring one is expanded in its own time zone, so that a weekly
//! stream at 20:00 stays at 20:00 across a change of clocks.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde_json::{json, Map, Value};

use crate::community::{line, refuse, reply_to, Community, Outcome};
use crate::server::AppState;
use crate::store::{Member, StoredEvent};

pub const CREATE_EVENT: &str = "https://wyvrn.app/community/1.0/create-event";
pub const UPDATE_EVENT: &str = "https://wyvrn.app/community/1.0/update-event";
pub const CANCEL_EVENT: &str = "https://wyvrn.app/community/1.0/cancel-event";
pub const EVENT: &str = "https://wyvrn.app/community/1.0/event";
pub const EVENTS_REQUEST: &str = "https://wyvrn.app/community/1.0/events-request";
pub const EVENTS: &str = "https://wyvrn.app/community/1.0/events";
pub const EVENT_RSVPS_REQUEST: &str = "https://wyvrn.app/community/1.0/event-rsvps-request";
pub const EVENT_RSVPS: &str = "https://wyvrn.app/community/1.0/event-rsvps";
pub const CALENDAR_FEED_REQUEST: &str = "https://wyvrn.app/community/1.0/calendar-feed-request";
pub const CALENDAR_FEED: &str = "https://wyvrn.app/community/1.0/calendar-feed";

pub const CALENDAR: &str = "https://didcomm.org/calendar/1.0";
pub const ACCEPT: &str = "https://didcomm.org/calendar/1.0/accept";
pub const TENTATIVE: &str = "https://didcomm.org/calendar/1.0/tentative";
pub const DECLINE: &str = "https://didcomm.org/calendar/1.0/decline";
const C_UPDATE: &str = "https://didcomm.org/calendar/1.0/update";
const C_CANCEL: &str = "https://didcomm.org/calendar/1.0/cancel";
const C_REMINDER: &str = "https://didcomm.org/calendar/1.0/reminder";

const MAX_TITLE: usize = 100;
const MAX_DESCRIPTION: usize = 1000;
const MAX_LOCATION: usize = 200;
const MAX_REMINDERS: usize = 3;
/// The furthest before an event a reminder may be: a week.
const MAX_REMINDER_MINUTES: i64 = 7 * 24 * 60;
/// calendar/1.0's cap on what a recurrence makes.
pub const MAX_OCCURRENCES: usize = 365;
/// How long after it ended an event is still listed.
const LISTED_AFTER: i64 = 7 * 24 * 3600;
/// A reminder this late is not sent at all (the server was down).
const REMINDER_STALE: i64 = 6 * 3600;
/// The longest an event may last: a fortnight.
const MAX_LENGTH: i64 = 14 * 24 * 3600;

fn parse_time(value: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.as_str()?).ok().map(|t| t.with_timezone(&Utc))
}

fn iso(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
}

/// The subset of an RFC 5545 RRULE calendar/1.0's events here take.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    freq: Freq,
    interval: u32,
    count: Option<u32>,
    until: Option<DateTime<Utc>>,
    by_day: Vec<Weekday>,
}

/// Reads an RRULE: `FREQ` (`DAILY`, `WEEKLY`, `MONTHLY`), `INTERVAL`, `COUNT`,
/// `UNTIL`, and `BYDAY` (weekly). Anything else is refused, with what.
pub fn parse_rule(text: &str) -> Result<Rule, String> {
    let mut rule = Rule { freq: Freq::Weekly, interval: 1, count: None, until: None, by_day: Vec::new() };
    let mut freq = None;
    for part in text.trim().trim_start_matches("RRULE:").split(';').filter(|p| !p.is_empty()) {
        let (key, value) = part.split_once('=').ok_or_else(|| format!("{part} isn't KEY=VALUE"))?;
        match key.to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match value.to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    other => return Err(format!("FREQ={other} isn't one an event here repeats by (DAILY, WEEKLY, MONTHLY)")),
                })
            }
            "INTERVAL" => rule.interval = value.parse().ok().filter(|n| (1..=366).contains(n)).ok_or("INTERVAL is a number from 1")?,
            "COUNT" => rule.count = Some(value.parse().ok().filter(|n| (1..=MAX_OCCURRENCES as u32).contains(n)).ok_or("COUNT is from 1 to 365")?),
            "UNTIL" => {
                let date = value.trim_end_matches('Z');
                let at = NaiveDateTime::parse_from_str(date, "%Y%m%dT%H%M%S").ok().or_else(|| NaiveDate::parse_from_str(date, "%Y%m%d").ok().and_then(|d| d.and_hms_opt(23, 59, 59)));
                rule.until = Some(at.ok_or("UNTIL is a date (20261231) or a time (20261231T235959Z)")?.and_utc());
            }
            "BYDAY" => {
                for day in value.split(',') {
                    rule.by_day.push(match day.to_ascii_uppercase().as_str() {
                        "MO" => Weekday::Mon,
                        "TU" => Weekday::Tue,
                        "WE" => Weekday::Wed,
                        "TH" => Weekday::Thu,
                        "FR" => Weekday::Fri,
                        "SA" => Weekday::Sat,
                        "SU" => Weekday::Sun,
                        other => return Err(format!("BYDAY={other} isn't a day (MO to SU)")),
                    });
                }
            }
            "WKST" => {}
            other => return Err(format!("{other} isn't taken in a rule here")),
        }
    }
    rule.freq = freq.ok_or("A rule says how often: FREQ")?;
    if !rule.by_day.is_empty() && rule.freq != Freq::Weekly {
        return Err("BYDAY goes with FREQ=WEEKLY here".into());
    }
    Ok(rule)
}

/// A local time in `tz` as UTC; in a gap the clocks skipped, an hour on.
fn in_zone(tz: Tz, local: NaiveDateTime) -> Option<DateTime<Utc>> {
    tz.from_local_datetime(&local).earliest().or_else(|| tz.from_local_datetime(&(local + Duration::hours(1))).earliest()).map(|t| t.with_timezone(&Utc))
}

/// When an event happens, between `from` and `to`: each occurrence's start and
/// end, at most [`MAX_OCCURRENCES`] made in all, those its `exdates` take out left out.
pub fn occurrences(data: &Value, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    let (Some(start), Some(end)) = (parse_time(&data["start"]), parse_time(&data["end"])) else { return Vec::new() };
    let length = end - start;
    let overlaps = |s: DateTime<Utc>| s + length > from && s < to;
    let Some(rule) = data["recurrence"]["rrule"].as_str().and_then(|r| parse_rule(r).ok()) else {
        return if overlaps(start) { vec![(start, end)] } else { Vec::new() };
    };
    let tz: Tz = data["timezone"].as_str().and_then(|z| z.parse().ok()).unwrap_or(Tz::UTC);
    let exdates: Vec<DateTime<Utc>> = data["recurrence"]["exdates"].as_array().into_iter().flatten().filter_map(parse_time).collect();
    let local = start.with_timezone(&tz).naive_local();
    let (date, time) = (local.date(), local.time());
    let mut made = 0usize;
    let mut found = Vec::new();
    // Each candidate local date, in order; `None` past the last one there could be.
    let mut step = 0u32;
    let monday = date - Duration::days(date.weekday().num_days_from_monday() as i64);
    let mut days: Vec<Weekday> = if rule.by_day.is_empty() { vec![date.weekday()] } else { rule.by_day.clone() };
    days.sort_by_key(|d| d.num_days_from_monday());
    'outer: loop {
        let dates: Vec<NaiveDate> = match rule.freq {
            Freq::Daily => vec![date + Duration::days((step * rule.interval) as i64)],
            Freq::Weekly => {
                let week = monday + Duration::days(7 * (step * rule.interval) as i64);
                days.iter().map(|d| week + Duration::days(d.num_days_from_monday() as i64)).filter(|d| *d >= date).collect()
            }
            Freq::Monthly => {
                let months = date.month0() + step * rule.interval;
                let (year, month) = (date.year() + (months / 12) as i32, months % 12 + 1);
                NaiveDate::from_ymd_opt(year, month, date.day()).into_iter().collect()
            }
        };
        step += 1;
        if step > 5000 {
            break;
        }
        for day in dates {
            let Some(at) = in_zone(tz, day.and_time(time)) else { continue };
            if rule.until.is_some_and(|until| at > until) || at >= to || made >= MAX_OCCURRENCES || rule.count.is_some_and(|count| made >= count as usize) {
                break 'outer;
            }
            made += 1;
            if !exdates.contains(&at) && overlaps(at) {
                found.push((at, at + length));
            }
        }
    }
    found
}

/// An event's status at `now`, and its next occurrence (one under way, or the
/// next to begin).
/// An occurrence: its start and end.
type Occurrence = (DateTime<Utc>, DateTime<Utc>);

fn standing(event: &StoredEvent, now: DateTime<Utc>) -> (&'static str, Option<Occurrence>) {
    if event.cancelled {
        return ("cancelled", None);
    }
    let next = occurrences(&event.data, now, now + Duration::days(4000)).into_iter().next();
    match next {
        Some((start, end)) if start <= now && now < end => ("active", Some((start, end))),
        Some(next) => ("scheduled", Some(next)),
        None => ("completed", None),
    }
}

/// When it last ends (for one that doesn't repeat for ever), to stop listing it a
/// week after.
fn ended_long_ago(event: &StoredEvent, now: DateTime<Utc>) -> bool {
    let since = now - Duration::seconds(LISTED_AFTER);
    let cancelled_long_ago = event.cancelled && event.updated_at < since.timestamp();
    cancelled_long_ago || (!event.cancelled && occurrences(&event.data, since, now + Duration::days(4000)).is_empty())
}

impl Community {
    /// Whether `did` sees `event`: one in a channel they can't see isn't there for them.
    fn sees_event(&self, did: &str, event: &StoredEvent) -> anyhow::Result<bool> {
        match event.data["channel"].as_str() {
            Some(channel) => self.sees_id(did, channel),
            None => Ok(true),
        }
    }

    /// Every event still listed (not over a week since), each with when it (next)
    /// happens, in the order the app lists them: by that start.
    pub fn events_listed(&self, now: i64) -> anyhow::Result<Vec<(i64, i64, StoredEvent)>> {
        let at = DateTime::from_timestamp(now, 0).unwrap_or_default();
        let mut listed = Vec::new();
        for event in self.store.events()? {
            if ended_long_ago(&event, at) {
                continue;
            }
            let (_, next) = standing(&event, at);
            let (start, end) = match next {
                Some((start, end)) => (start.timestamp(), end.timestamp()),
                None => match (parse_time(&event.data["start"]), parse_time(&event.data["end"])) {
                    (Some(start), Some(end)) => (start.timestamp(), end.timestamp()),
                    _ => continue,
                },
            };
            listed.push((start, end, event));
        }
        listed.sort_by_key(|(start, _, _)| *start);
        Ok(listed)
    }

    /// `event` as `did` is shown it.
    pub fn event_body(&self, event: &StoredEvent, did: &str, now: i64) -> anyhow::Result<Value> {
        let mut body = event.data.clone();
        let at = DateTime::from_timestamp(now, 0).unwrap_or_default();
        let (status, next) = standing(event, at);
        body["event_id"] = json!(event.id);
        body["organizer"] = json!(self.store.member_id(&event.organizer, now)?);
        body["sequence"] = json!(event.sequence);
        body["status"] = json!(status);
        if body["recurrence"].is_object() {
            if let Some((start, end)) = next {
                body["next"] = json!({ "start": iso(start), "end": iso(end) });
            }
        }
        let rsvps = self.store.event_rsvps(&event.id)?;
        let count = |status: &str| rsvps.iter().filter(|(_, s)| s == status).count();
        body["going"] = json!({ "accepted": count("accepted"), "tentative": count("tentative"), "declined": count("declined") });
        if let Some((_, mine)) = rsvps.iter().find(|(who, _)| who == did) {
            body["mine"] = json!(mine);
        }
        body["created_at"] = json!(event.created_at);
        body["updated_at"] = json!(event.updated_at);
        Ok(body)
    }

    /// `event` sent to every joined member who sees it, but `except`.
    fn event_notices(&self, event: &StoredEvent, except: &str, now: i64) -> anyhow::Result<Vec<(String, Value)>> {
        let mut notices = Vec::new();
        for member in self.store.joined()?.into_iter().filter(|m| m.did != except) {
            if self.sees_event(&member.did, event)? {
                notices.push((member.did.clone(), json!({ "type": EVENT, "id": uuid::Uuid::new_v4().to_string(), "body": { "event": self.event_body(event, &member.did, now)? } })));
            }
        }
        Ok(notices)
    }

    /// An event's fields as a member gives them, over what it had (`held`):
    /// checked, and with nothing in them this doesn't name. `Err` says what is wrong.
    fn read_event(&self, given: &Value, held: Option<&Value>) -> Result<Value, String> {
        let mut data = held.cloned().unwrap_or_else(|| json!({}));
        let set = |data: &mut Value, key: &str, value: Option<Value>| match value {
            Some(value) => data[key] = value,
            None => {
                data.as_object_mut().map(|d| d.remove(key));
            }
        };
        let said = |key: &str| given.get(key);
        if held.is_none() || said("title").is_some() {
            set(&mut data, "title", Some(json!(line(&given["title"], MAX_TITLE).ok_or("An event needs a title.")?)));
        }
        if let Some(value) = said("description") {
            let text = value.as_str().map(str::trim).unwrap_or_default();
            if text.chars().count() > MAX_DESCRIPTION {
                return Err("A description is up to 1,000 characters.".into());
            }
            set(&mut data, "description", (!text.is_empty()).then(|| json!(text)));
        }
        if held.is_none() || said("timezone").is_some() {
            let zone = given["timezone"].as_str().unwrap_or("UTC");
            if zone.parse::<Tz>().is_err() {
                return Err(format!("{zone} isn't a time zone (an IANA name, such as Europe/London)."));
            }
            set(&mut data, "timezone", Some(json!(zone)));
        }
        if held.is_none() || said("start").is_some() || said("end").is_some() {
            let start = if said("start").is_some() || held.is_none() { parse_time(&given["start"]).ok_or("An event starts at a time: ISO 8601, such as 2026-12-31T21:00:00Z.")? } else { parse_time(&data["start"]).ok_or("Its start is unreadable.")? };
            let end = match said("end") {
                Some(Value::Null) | None if held.is_none() || said("start").is_some() && said("end").is_none() => {
                    // Kept as long as it was; a new one, an hour.
                    let length = held.and_then(|h| Some(parse_time(&h["end"])? - parse_time(&h["start"])?)).unwrap_or(Duration::hours(1));
                    start + length
                }
                Some(end) => parse_time(end).ok_or("An end is a time: ISO 8601.")?,
                None => parse_time(&data["end"]).ok_or("Its end is unreadable.")?,
            };
            if end <= start {
                return Err("An event ends after it starts.".into());
            }
            if (end - start).num_seconds() > MAX_LENGTH {
                return Err("An event lasts a fortnight at most.".into());
            }
            set(&mut data, "start", Some(json!(iso(start))));
            set(&mut data, "end", Some(json!(iso(end))));
        }
        if let Some(value) = said("channel") {
            match value.as_str() {
                Some(id) => {
                    let channel = self.store.channel(id).map_err(|e| e.to_string())?.filter(|c| c.voice.is_some()).ok_or("An event's channel is a voice channel or a stage.")?;
                    set(&mut data, "channel", Some(json!(channel.id)));
                    data.as_object_mut().map(|d| d.remove("location"));
                }
                None => set(&mut data, "channel", None),
            }
        }
        if let Some(value) = said("location") {
            let text = value.as_str().map(str::trim).unwrap_or_default();
            if text.chars().count() > MAX_LOCATION || text.contains('\n') {
                return Err("A place is one line of up to 200 characters.".into());
            }
            set(&mut data, "location", (!text.is_empty()).then(|| json!(text)));
            if !text.is_empty() {
                data.as_object_mut().map(|d| d.remove("channel"));
            }
        }
        data["type"] = json!(if data.get("channel").is_some() { "call" } else { "meeting" });
        if let Some(value) = said("image") {
            if value.is_null() {
                set(&mut data, "image", None);
            } else {
                let media = value["media"].as_array().filter(|m| m.len() == 1).ok_or("A cover is one picture.")?;
                let attachments = value["attachments"].as_array().filter(|a| a.len() == 1).ok_or("A cover is one picture.")?;
                let attachment = &attachments[0];
                let by_reference = attachment["data"]["links"].as_array().is_some_and(|links| !links.is_empty() && links.iter().all(|l| l.as_str().is_some_and(|l| l.starts_with("https://") || l.starts_with("http://"))));
                if !by_reference || attachment["data"]["hash"].as_str().is_none() || media[0]["attachment"] != attachment["id"] || !attachment["media_type"].as_str().is_some_and(|t| t.starts_with("image/")) {
                    return Err("A cover is a picture, by reference.".into());
                }
                set(&mut data, "image", Some(json!({ "media": media, "attachments": attachments })));
            }
        }
        if let Some(value) = said("recurrence") {
            if value.is_null() {
                set(&mut data, "recurrence", None);
            } else {
                let rrule = value["rrule"].as_str().ok_or("A recurrence is { rrule }.")?;
                parse_rule(rrule)?;
                let exdates: Vec<String> = value["exdates"].as_array().into_iter().flatten().filter_map(parse_time).map(iso).take(MAX_OCCURRENCES).collect();
                let mut recurrence = json!({ "rrule": rrule.trim() });
                if !exdates.is_empty() {
                    recurrence["exdates"] = json!(exdates);
                }
                set(&mut data, "recurrence", Some(recurrence));
            }
        }
        if held.is_none() || said("reminders").is_some() {
            let mut reminders = Vec::new();
            for reminder in given["reminders"].as_array().into_iter().flatten() {
                let offset = reminder["offset_minutes"].as_i64().filter(|m| (-MAX_REMINDER_MINUTES..=0).contains(m)).ok_or("A reminder is at the start or before it, a week at most.")?;
                if !reminders.contains(&offset) {
                    reminders.push(offset);
                }
            }
            if reminders.len() > MAX_REMINDERS {
                return Err("Three reminders at most.".into());
            }
            set(&mut data, "reminders", Some(json!(reminders.iter().map(|m| json!({ "offset_minutes": m })).collect::<Vec<_>>())));
        }
        let keep: Map<String, Value> = data.as_object().cloned().unwrap_or_default().into_iter().filter(|(k, _)| ["title", "description", "timezone", "start", "end", "channel", "location", "type", "image", "recurrence", "reminders"].contains(&k.as_str())).collect();
        Ok(Value::Object(keep))
    }

    /// What `member` may do to `event`: change and cancel it.
    fn may_change_event(&self, member: &Member, event: &StoredEvent) -> anyhow::Result<bool> {
        let powers = self.powers(&member.did)?;
        Ok(powers.has("manage_events") || (event.organizer == member.did && powers.has("create_events")))
    }

    /// The event `body` names, if `did` sees it.
    fn named_event(&self, did: &str, body: &Value) -> anyhow::Result<Option<StoredEvent>> {
        let Some(id) = body["event_id"].as_str() else { return Ok(None) };
        match self.store.event(id)? {
            Some(event) if self.sees_event(did, &event)? => Ok(Some(event)),
            _ => Ok(None),
        }
    }

    /// The messages about events, from a member: `None` for any other.
    pub fn handle_event_message(&self, member: &Member, message: &Value, now: i64) -> anyhow::Result<Option<Outcome>> {
        let message_type = message["type"].as_str().unwrap_or_default();
        let body = &message["body"];
        let did = member.did.as_str();
        let answered = |event: &StoredEvent| -> anyhow::Result<Value> { Ok(reply_to(message, EVENT, json!({ "event": self.event_body(event, did, now)? }))) };
        let outcome = match message_type {
            CREATE_EVENT => {
                let powers = self.powers(did)?;
                if !(powers.has("create_events") || powers.has("manage_events")) {
                    return refuse(message, "e.p.trust.not-authorized", "Making events takes Create events.").map(Some);
                }
                let data = match self.read_event(body, None) {
                    Ok(data) => data,
                    Err(why) => return refuse(message, "e.p.msg.invalid-event", &why).map(Some),
                };
                if data["channel"].as_str().is_some_and(|c| !self.sees_id(did, c).unwrap_or(false)) {
                    return refuse(message, "e.p.msg.invalid-event", "An event's channel is one you can see.").map(Some);
                }
                let event = StoredEvent { id: format!("evt-{}", uuid::Uuid::new_v4()), data, organizer: did.to_string(), sequence: 0, cancelled: false, created_at: now, updated_at: now };
                self.store.save_event(&event)?;
                self.audit(now, did, "event.create", Some(&event.id), Some(json!({ "title": event.data["title"] })));
                Outcome { reply: Some(answered(&event)?), notices: self.event_notices(&event, did, now)?, ..Outcome::default() }
            }
            UPDATE_EVENT | CANCEL_EVENT => {
                let Some(mut event) = self.named_event(did, body)? else {
                    return refuse(message, "e.p.msg.unknown-event", "There is no such event.").map(Some);
                };
                if !self.may_change_event(member, &event)? {
                    return refuse(message, "e.p.trust.not-authorized", "Changing an event takes Manage events, or having made it.").map(Some);
                }
                if event.cancelled {
                    return refuse(message, "e.p.msg.unknown-event", "That event was cancelled.").map(Some);
                }
                let before = event.data.clone();
                let mut calendar = Vec::new();
                if message_type == UPDATE_EVENT {
                    event.data = match self.read_event(body, Some(&event.data)) {
                        Ok(data) => data,
                        Err(why) => return refuse(message, "e.p.msg.invalid-event", &why).map(Some),
                    };
                    let moved = ["start", "end", "recurrence", "channel", "location"].iter().any(|k| before.get(*k) != event.data.get(*k));
                    event.sequence += 1;
                    if moved {
                        let changes: Map<String, Value> = ["title", "start", "end", "timezone", "location"].iter().filter_map(|k| event.data.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
                        calendar.push(json!({ "type": C_UPDATE, "thid": event.id, "body": { "event_id": event.id, "sequence": event.sequence, "changes": changes, "require_re_rsvp": false } }));
                    }
                    self.audit(now, did, "event.update", Some(&event.id), Some(json!({ "title": event.data["title"] })));
                } else {
                    match body["recurrence_id"].as_str().and_then(|r| DateTime::parse_from_rfc3339(r).ok()) {
                        Some(occurrence) if event.data["recurrence"].is_object() => {
                            let mut exdates = event.data["recurrence"]["exdates"].as_array().cloned().unwrap_or_default();
                            exdates.push(json!(iso(occurrence.with_timezone(&Utc))));
                            event.data["recurrence"]["exdates"] = json!(exdates);
                            calendar.push(json!({ "type": C_CANCEL, "thid": event.id, "body": { "event_id": event.id, "sequence": event.sequence + 1, "cancel_scope": "this", "recurrence_id": iso(occurrence.with_timezone(&Utc)) } }));
                        }
                        Some(_) => return refuse(message, "e.p.msg.invalid-event", "Only a recurring event has occurrences to cancel.").map(Some),
                        None => {
                            event.cancelled = true;
                            calendar.push(json!({ "type": C_CANCEL, "thid": event.id, "body": { "event_id": event.id, "sequence": event.sequence + 1, "cancel_scope": "all", "reason": "organizer-cancelled" } }));
                        }
                    }
                    event.sequence += 1;
                    self.audit(now, did, "event.cancel", Some(&event.id), Some(json!({ "title": event.data["title"], "occurrence": body["recurrence_id"] })));
                }
                event.updated_at = now;
                self.store.save_event(&event)?;
                let mut notices = self.event_notices(&event, did, now)?;
                // What calendar/1.0 says of it, to everyone who answered.
                for (who, _) in self.store.event_rsvps(&event.id)? {
                    if who != did {
                        for said in &calendar {
                            let mut said = said.clone();
                            said["id"] = json!(uuid::Uuid::new_v4().to_string());
                            notices.push((who.clone(), said));
                        }
                    }
                }
                Outcome { reply: Some(answered(&event)?), notices, ..Outcome::default() }
            }
            ACCEPT | TENTATIVE | DECLINE => {
                let named = if body["event_id"].is_string() { body.clone() } else { json!({ "event_id": message["thid"] }) };
                let Some(event) = self.named_event(did, &named)? else {
                    return refuse(message, "event_not_found", "There is no such event.").map(Some);
                };
                if event.cancelled {
                    return refuse(message, "event_not_found", "That event was cancelled.").map(Some);
                }
                if body["sequence"].as_i64().is_some_and(|seen| seen < event.sequence) {
                    return refuse(message, "sequence_stale", "The event has changed since: look again before answering.").map(Some);
                }
                let status = match message_type {
                    ACCEPT => "accepted",
                    TENTATIVE => "tentative",
                    _ => "declined",
                };
                self.store.set_event_rsvp(&event.id, did, status, now)?;
                Outcome::reply(answered(&event)?)
            }
            EVENTS_REQUEST => {
                let at = DateTime::from_timestamp(now, 0).unwrap_or_default();
                let mut listed: Vec<(i64, Value)> = Vec::new();
                for event in self.store.events()? {
                    if self.sees_event(did, &event)? && !ended_long_ago(&event, at) {
                        let body = self.event_body(&event, did, now)?;
                        let when = parse_time(body.get("next").map(|n| &n["start"]).unwrap_or(&body["start"])).map(|t| t.timestamp()).unwrap_or(i64::MAX);
                        listed.push((when, body));
                    }
                }
                listed.sort_by_key(|(when, _)| *when);
                Outcome::reply(reply_to(message, EVENTS, json!({ "events": listed.into_iter().map(|(_, e)| e).collect::<Vec<_>>() })))
            }
            EVENT_RSVPS_REQUEST => {
                let Some(event) = self.named_event(did, body)? else {
                    return refuse(message, "e.p.msg.unknown-event", "There is no such event.").map(Some);
                };
                let mut lists = json!({ "event_id": event.id, "accepted": [], "tentative": [], "declined": [] });
                for (who, status) in self.store.event_rsvps(&event.id)? {
                    if let Some(list) = lists[status.as_str()].as_array_mut() {
                        list.push(json!(self.store.member_id(&who, now)?));
                    }
                }
                Outcome::reply(reply_to(message, EVENT_RSVPS, lists))
            }
            CALENDAR_FEED_REQUEST => {
                let token = match (body["new"].as_bool(), self.store.calendar_feed_of(did)?) {
                    (Some(true), _) | (_, None) => {
                        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
                        self.store.set_calendar_feed(did, &token)?;
                        token
                    }
                    (_, Some(token)) => token,
                };
                let url = format!("{}/calendar/{token}.ics", self.public_url.trim_end_matches('/'));
                Outcome::reply(reply_to(message, CALENDAR_FEED, json!({ "url": url })))
            }
            _ => return Ok(None),
        };
        Ok(Some(outcome))
    }

    /// The events `did` sees, as an iCalendar feed (RFC 5545).
    pub fn ics_for(&self, did: &str, now: i64) -> anyhow::Result<String> {
        let name = self.name_now();
        let host = url::Url::parse(&self.public_url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| "wyvrn.app".into());
        let stamp = DateTime::from_timestamp(now, 0).unwrap_or_default().format("%Y%m%dT%H%M%SZ").to_string();
        let mut out = vec!["BEGIN:VCALENDAR".to_string(), "VERSION:2.0".into(), "PRODID:-//Wyvrn//Community events//EN".into(), "CALSCALE:GREGORIAN".into(), format!("X-WR-CALNAME:{}", ics_text(&name))];
        let channels = self.store.channels()?;
        for event in self.store.events()? {
            if !self.sees_event(did, &event)? {
                continue;
            }
            let data = &event.data;
            let (Some(start), Some(end)) = (parse_time(&data["start"]), parse_time(&data["end"])) else { continue };
            let utc = |t: DateTime<Utc>| t.format("%Y%m%dT%H%M%SZ").to_string();
            out.push("BEGIN:VEVENT".into());
            out.push(format!("UID:{}@{host}", event.id));
            out.push(format!("DTSTAMP:{stamp}"));
            out.push(format!("DTSTART:{}", utc(start)));
            out.push(format!("DTEND:{}", utc(end)));
            out.push(format!("SEQUENCE:{}", event.sequence));
            out.push(format!("SUMMARY:{}", ics_text(data["title"].as_str().unwrap_or_default())));
            if let Some(description) = data["description"].as_str() {
                out.push(format!("DESCRIPTION:{}", ics_text(description)));
            }
            let place = data["location"].as_str().map(str::to_string).or_else(|| data["channel"].as_str().and_then(|c| channels.iter().find(|ch| ch.id == c)).map(|c| format!("🔊 {} ({name})", c.name)));
            if let Some(place) = place {
                out.push(format!("LOCATION:{}", ics_text(&place)));
            }
            if let Some(rrule) = data["recurrence"]["rrule"].as_str() {
                out.push(format!("RRULE:{}", rrule.trim_start_matches("RRULE:")));
                for exdate in data["recurrence"]["exdates"].as_array().into_iter().flatten().filter_map(parse_time) {
                    out.push(format!("EXDATE:{}", utc(exdate)));
                }
            }
            out.push(format!("STATUS:{}", if event.cancelled { "CANCELLED" } else { "CONFIRMED" }));
            out.push("END:VEVENT".into());
        }
        out.push("END:VCALENDAR".into());
        Ok(out.into_iter().map(|l| fold(&l)).collect::<Vec<_>>().join("\r\n") + "\r\n")
    }
}

/// Text in an iCalendar value: `\`, `;`, `,` and line breaks escaped.
fn ics_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace("\r\n", "\\n").replace('\n', "\\n")
}

/// A content line folded at 75 octets, as RFC 5545 has it (never inside a character).
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut width = 0;
    for c in line.chars() {
        let size = c.len_utf8();
        if width + size > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += size;
    }
    out
}

/// `GET /calendar/{token}.ics`: a member's feed, from the address only they were given.
pub async fn feed(State(state): State<Arc<AppState>>, Path(file): Path<String>) -> Response {
    let community = &state.community;
    let Some(token) = file.strip_suffix(".ics") else { return StatusCode::NOT_FOUND.into_response() };
    let did = match community.store.calendar_feed_by_token(token) {
        Ok(Some(did)) => did,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if !matches!(community.store.member(&did), Ok(Some(_))) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match community.ics_for(&did, crate::gates::now()) {
        Ok(ics) => ([(header::CONTENT_TYPE, "text/calendar; charset=utf-8"), (header::CACHE_CONTROL, "private, max-age=300")], ics).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Sends the reminders that are due: calendar/1.0's `reminder`, to everyone who
/// said they are going or maybe, at each reminder of each occurrence, once.
pub async fn send_reminders(state: &Arc<AppState>, now: i64) -> anyhow::Result<()> {
    let community = &state.community;
    let at = DateTime::from_timestamp(now, 0).unwrap_or_default();
    let mut due = Vec::new();
    for event in community.store.events()?.into_iter().filter(|e| !e.cancelled) {
        let reminders: Vec<i64> = event.data["reminders"].as_array().into_iter().flatten().filter_map(|r| r["offset_minutes"].as_i64()).collect();
        if reminders.is_empty() {
            continue;
        }
        // Occurrences whose reminders could be due now.
        for (start, _) in occurrences(&event.data, at - Duration::seconds(REMINDER_STALE), at + Duration::minutes(MAX_REMINDER_MINUTES + 1)) {
            for offset in &reminders {
                let fire = start + Duration::minutes(*offset);
                if fire <= at && fire > at - Duration::seconds(REMINDER_STALE) {
                    let key = format!("{}|{}|{offset}", event.id, start.timestamp());
                    if community.store.mark_reminder(&key, now)? {
                        due.push((event.clone(), start, *offset));
                    }
                }
            }
        }
    }
    for (event, start, offset) in due {
        let action = if event.data["channel"].is_string() { "join_call" } else { "acknowledge" };
        for (who, status) in community.store.event_rsvps(&event.id)? {
            if status == "declined" || !community.sees_event(&who, &event).unwrap_or(false) {
                continue;
            }
            let reminder = json!({
                "type": C_REMINDER, "id": uuid::Uuid::new_v4().to_string(), "thid": event.id,
                "body": { "event_id": event.id, "title": event.data["title"], "start": iso(start), "timezone": event.data["timezone"], "offset_minutes": offset, "action": action, "channel": event.data["channel"] }
            });
            if let Err(error) = state.voice.send(&state.agent, &who, &reminder).await {
                tracing::info!(%who, %error, "couldn't send a reminder");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn a_weekly_event_stays_at_its_local_time_across_a_change_of_clocks() {
        // 20:00 in London every Thursday from 2026-10-15; clocks go back on 2026-10-25.
        let data = json!({ "start": "2026-10-15T19:00:00Z", "end": "2026-10-15T21:00:00Z", "timezone": "Europe/London", "recurrence": { "rrule": "FREQ=WEEKLY;COUNT=4", "exdates": ["2026-10-22T19:00:00Z"] } });
        let found: Vec<String> = occurrences(&data, at("2026-01-01T00:00:00Z"), at("2027-01-01T00:00:00Z")).into_iter().map(|(s, _)| iso(s)).collect();
        assert_eq!(found, ["2026-10-15T19:00:00Z", "2026-10-29T20:00:00Z", "2026-11-05T20:00:00Z"]);
    }

    #[test]
    fn rules_by_day_month_and_until_and_only_those() {
        let data = json!({ "start": "2026-11-16T18:00:00Z", "end": "2026-11-16T19:00:00Z", "timezone": "UTC", "recurrence": { "rrule": "FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20261125" } });
        let found: Vec<String> = occurrences(&data, at("2026-01-01T00:00:00Z"), at("2027-01-01T00:00:00Z")).into_iter().map(|(s, _)| iso(s)).collect();
        assert_eq!(found, ["2026-11-16T18:00:00Z", "2026-11-18T18:00:00Z", "2026-11-23T18:00:00Z", "2026-11-25T18:00:00Z"]);
        // The 31st, monthly: only months with one.
        let data = json!({ "start": "2027-01-31T12:00:00Z", "end": "2027-01-31T13:00:00Z", "timezone": "UTC", "recurrence": { "rrule": "FREQ=MONTHLY;COUNT=3" } });
        let found: Vec<String> = occurrences(&data, at("2027-01-01T00:00:00Z"), at("2028-01-01T00:00:00Z")).into_iter().map(|(s, _)| iso(s)).collect();
        assert_eq!(found, ["2027-01-31T12:00:00Z", "2027-03-31T12:00:00Z", "2027-05-31T12:00:00Z"]);
        // For ever, daily: capped at 365.
        let data = json!({ "start": "2027-01-01T12:00:00Z", "end": "2027-01-01T13:00:00Z", "timezone": "UTC", "recurrence": { "rrule": "FREQ=DAILY" } });
        assert_eq!(occurrences(&data, at("2027-01-01T00:00:00Z"), at("2030-01-01T00:00:00Z")).len(), 365);
        for bad in ["FREQ=YEARLY", "FREQ=DAILY;BYDAY=MO", "INTERVAL=2", "FREQ=WEEKLY;COUNT=0", "FREQ=WEEKLY;BYSETPOS=1"] {
            assert!(parse_rule(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_ics_line_is_escaped_and_folded() {
        assert_eq!(ics_text("a, b; c\\d\ne"), "a\\, b\\; c\\\\d\\ne");
        let long = format!("SUMMARY:{}", "x".repeat(100));
        let folded = fold(&long);
        assert!(folded.split("\r\n").all(|l| l.len() <= 75), "{folded}");
        assert_eq!(folded.replace("\r\n ", ""), long);
    }
}
