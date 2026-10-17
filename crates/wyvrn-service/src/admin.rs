//! A service's admin pages, as `https://didcomm.org/workflow/1.0` workflows that
//! it runs for its admins and nobody else.
//!
//! An admin's app is the workflow's Coordinator and the service its Processor,
//! as for any workflow (the sticker service's are the same protocol). What makes
//! these admin pages is only who is let in: `discover` lists them to an admin
//! alone, and every other message about one is refused to anyone who isn't, at
//! the time they send it -- an admin taken off the list mid-way is out at once.
//!
//! A page is one screen that stays put: a state `view` that each of its events
//! leaves and comes back to, having done something (removed a file, saved a
//! setting), and `done` to close it. So a service writes no state machine, only
//! [`Pages`]: what a page shows, and what each event does. The template is built
//! from that here. What was last done is said at the top of the page, as a note
//! or as an error.
//!
//! Instances are kept in memory. A page open when the service restarts is gone,
//! and the app opens it again; nothing an admin page holds is worth more than
//! that.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const PIURI: &str = "https://didcomm.org/workflow/1.0";
const P: &str = "https://didcomm.org/workflow/1.0/";
/// What an admin page's template id starts with, so that a service with other
/// workflows of its own can tell which messages are about these.
pub const PREFIX: &str = "admin-";
pub const VERSION: &str = "1.0.0";

/// How long an admin page nobody has touched is kept.
const FORGET_AFTER_SECONDS: i64 = 60 * 60;

/// One admin page: its template id (starting with [`PREFIX`]), its title, and
/// the events it takes. `done`, which closes it, is added.
#[derive(Debug, Clone)]
pub struct Page {
    pub id: &'static str,
    pub title: &'static str,
    /// One line on what it is for, for a list of a service's admin pages.
    pub summary: &'static str,
    pub events: &'static [&'static str],
}

/// A service's admin pages: what each shows, and what each event does.
pub trait Pages: Send + Sync {
    fn pages(&self) -> Vec<Page>;

    /// The Workflow UI 1.0 items `page` shows now, for `admin`.
    fn view(&self, page: &str, admin: &str, now: i64) -> anyhow::Result<Vec<Value>>;

    /// Does `event` on `page`, with what the admin's form held. What to say it
    /// did, if anything; or what was wrong, said so it can be put right.
    fn act(&self, page: &str, event: &str, input: &Value, admin: &str, now: i64) -> Result<Option<String>, String>;
}

/// A `problem-report`: one of the workflow protocol's codes.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub code: &'static str,
    pub comment: String,
}

impl Refusal {
    fn new(code: &'static str, comment: impl Into<String>) -> Self {
        Self { code, comment: comment.into() }
    }

    /// The body of the `problem-report` that says it.
    pub fn body(&self) -> Value {
        json!({ "code": self.code, "comment": self.comment, "args": {} })
    }
}

/// An answer: the full message type, and its body.
pub type Answer = Result<(String, Value), Refusal>;

#[derive(Debug, Clone)]
struct Instance {
    page: String,
    owner: String,
    done: bool,
    /// What the last event said: (`information` or `error`, text).
    said: Option<(&'static str, String)>,
    seen_keys: HashSet<String>,
    touched: i64,
}

#[derive(Default)]
pub struct Admin {
    instances: Mutex<HashMap<String, Instance>>,
}

/// The workflow template for `page`.
pub fn template(page: &Page) -> Value {
    let mut transitions = serde_json::Map::new();
    let mut actions = serde_json::Map::new();
    for event in page.events {
        transitions.insert(event.to_string(), json!({ "from": "view", "to": "view", "guard": null, "action": event }));
        actions.insert(event.to_string(), json!({ "typeURI": format!("https://wyvrn.app/admin/1.0/actions/{event}@1") }));
    }
    transitions.insert("done".into(), json!({ "from": "view", "to": "done", "guard": null, "action": null }));
    json!({
        "id": page.id,
        "version": VERSION,
        "name": page.title,
        "description": page.summary,
        "instance_policy": { "mode": "multi_per_connection" },
        "states": { "view": { "final": false }, "done": { "final": true } },
        "transitions": transitions,
        "actions": actions,
        "catalog": {},
        // What each step shows is drawn by the service as things stand (`status`'s `ui`).
        "display_hints": { "ui_version": "1.0", "states": {} },
    })
}

fn hash(template: &Value) -> String {
    format!("{:x}", Sha256::digest(template.to_string().as_bytes()))
}

impl Admin {
    pub fn new() -> Self {
        Self::default()
    }

    /// What `discover` lists of these: every page to an admin, nothing to anyone
    /// else. For a service whose `discover` also lists workflows of its own.
    pub fn listed(&self, pages: &dyn Pages, is_admin: bool, body: &Value) -> Vec<Value> {
        if !is_admin {
            return Vec::new();
        }
        let filters = &body["filters"];
        pages
            .pages()
            .iter()
            .filter(|page| {
                filters["template_id"].as_str().is_none_or(|wanted| wanted == page.id)
                    && filters["version"].as_str().is_none_or(|wanted| wanted == VERSION)
                    && filters["text"].as_str().is_none_or(|text| format!("{} {}", page.id, page.title).to_lowercase().contains(&text.to_lowercase()))
            })
            .map(|page| {
                let mut listed = json!({ "template_id": page.id, "versions": [VERSION], "title": page.title, "description": page.summary });
                if body["include_hash"] == true {
                    listed["hash"] = json!(hash(&template(page)));
                }
                listed
            })
            .collect()
    }

    /// Whether `message` is about an admin page (as against another of the
    /// service's workflows): a template id with [`PREFIX`], or an instance here.
    pub fn concerns(&self, message: &Value) -> bool {
        let body = &message["body"];
        if body["template_id"].as_str().is_some_and(|id| id.starts_with(PREFIX)) {
            return true;
        }
        body["instance_id"].as_str().is_some_and(|id| self.instances.lock().unwrap_or_else(|p| p.into_inner()).contains_key(id))
    }

    /// Answers one workflow message about the admin pages, from `sender`, who is
    /// or isn't an admin now. `None` for a message this doesn't answer: one of
    /// another of the service's workflows, or not this protocol's.
    pub fn handle(&self, pages: &dyn Pages, is_admin: bool, sender: &str, message: &Value, now: i64) -> Option<Answer> {
        let kind = message["type"].as_str()?.strip_prefix(P)?;
        let body = &message["body"];
        if kind == "discover" {
            let workflows = self.listed(pages, is_admin, body);
            return Some(Ok((format!("{P}workflows"), json!({ "paging": { "total": workflows.len(), "next_offset": 0 }, "workflows": workflows }))));
        }
        if !self.concerns(message) {
            return None;
        }
        if !is_admin {
            return Some(Err(Refusal::new("not_authorized", "This is for this service's admins.")));
        }
        Some(match kind {
            "fetch-template" => self.fetch_template(pages, body),
            "start" => self.start(pages, sender, message, now),
            "status" => self.with_mine(sender, body, |_| Ok(())).and_then(|id| self.status(pages, &id, now)),
            "advance" => self.advance(pages, sender, body, now),
            "cancel" => self.with_mine(sender, body, |instance| {
                instance.done = true;
                Ok(())
            })
            .and_then(|id| self.status(pages, &id, now)),
            "pause" | "resume" => self.with_mine(sender, body, |_| Ok(())).and_then(|id| self.status(pages, &id, now)),
            _ => Err(Refusal::new("not_authorized", "Admin pages are this service's own.")),
        })
    }

    fn page(pages: &dyn Pages, id: &str) -> Option<Page> {
        pages.pages().into_iter().find(|page| page.id == id)
    }

    fn fetch_template(&self, pages: &dyn Pages, body: &Value) -> Answer {
        let found = Self::page(pages, body["template_id"].as_str().unwrap_or_default())
            .filter(|_| body["template_version"].as_str().is_none_or(|version| version == VERSION))
            .map(|page| template(&page))
            .filter(|template| body["prefer_hash"].as_str().is_none_or(|wanted| wanted == hash(template)));
        match found {
            Some(template) => Ok((format!("{P}template"), json!({ "template": template }))),
            None => Err(Refusal::new("not_found_remote_template", "This service has no such page.")),
        }
    }

    fn start(&self, pages: &dyn Pages, sender: &str, message: &Value, now: i64) -> Answer {
        let body = &message["body"];
        let Some(page) = Self::page(pages, body["template_id"].as_str().unwrap_or_default()) else {
            return Err(Refusal::new("template_not_found", "This service has no such page."));
        };
        let id = body["instance_id"].as_str().or(message["thid"].as_str()).or(message["id"].as_str()).unwrap_or_default().to_string();
        if id.is_empty() {
            return Err(Refusal::new("multiplicity_violation", "A page needs an instance id."));
        }
        {
            let mut instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
            instances.retain(|_, instance| now - instance.touched < FORGET_AFTER_SECONDS);
            match instances.get(&id) {
                // Started again: the same page, as it stands.
                Some(existing) if existing.owner == sender && existing.page == page.id => {}
                Some(_) => return Err(Refusal::new("multiplicity_violation", "That instance id has been used.")),
                None => {
                    instances.insert(id.clone(), Instance { page: page.id.to_string(), owner: sender.to_string(), done: false, said: None, seen_keys: HashSet::new(), touched: now });
                }
            }
        }
        tracing::info!(page = page.id, admin = %sender, "an admin page was opened");
        self.status(pages, &id, now)
    }

    /// The id of `sender`'s instance the body names, after `change` to it.
    fn with_mine(&self, sender: &str, body: &Value, change: impl FnOnce(&mut Instance) -> Result<(), Refusal>) -> Result<String, Refusal> {
        let id = body["instance_id"].as_str().unwrap_or_default();
        let mut instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
        match instances.get_mut(id) {
            Some(instance) if instance.owner == sender => {
                change(instance)?;
                Ok(id.to_string())
            }
            _ => Err(Refusal::new("instance_not_found", "There is no such page open here.")),
        }
    }

    fn advance(&self, pages: &dyn Pages, sender: &str, body: &Value, now: i64) -> Answer {
        let event = body["event"].as_str().unwrap_or_default().to_string();
        let mut repeated = false;
        let mut page = String::new();
        let id = self.with_mine(sender, body, |instance| {
            if instance.done {
                return Err(Refusal::new("guard_failed", "This page is closed."));
            }
            // The same press of a button, arriving twice, is done once.
            if let Some(key) = body["idempotency_key"].as_str() {
                repeated = !instance.seen_keys.insert(key.to_string());
            }
            instance.touched = now;
            page = instance.page.clone();
            Ok(())
        })?;
        if repeated {
            return self.status(pages, &id, now);
        }
        if event == "done" {
            self.with_mine(sender, body, |instance| {
                instance.done = true;
                Ok(())
            })?;
            return Ok((format!("{P}complete"), json!({ "instance_id": id })));
        }
        let Some(found) = Self::page(pages, &page).filter(|found| found.events.contains(&event.as_str())) else {
            return Err(Refusal::new("guard_failed", "That can't be done on this page."));
        };
        let said = match pages.act(found.id, &event, &body["input"], sender, now) {
            Ok(note) => note.map(|note| ("information", note)),
            Err(mistake) => Some(("error", mistake)),
        };
        tracing::info!(page = found.id, %event, admin = %sender, ok = said.as_ref().is_none_or(|(kind, _)| *kind != "error"), "an admin acted");
        self.with_mine(sender, body, |instance| {
            instance.said = said;
            Ok(())
        })?;
        self.status(pages, &id, now)
    }

    fn status(&self, pages: &dyn Pages, id: &str, now: i64) -> Answer {
        let instance = self.instances.lock().unwrap_or_else(|p| p.into_inner()).get(id).cloned().ok_or_else(|| Refusal::new("instance_not_found", "There is no such page open here."))?;
        let page = Self::page(pages, &instance.page).ok_or_else(|| Refusal::new("instance_not_found", "That page is no longer offered."))?;
        let mut ui = vec![json!({ "type": "title", "text": page.title })];
        if instance.done {
            ui.push(json!({ "type": "information", "text": "Closed." }));
        } else {
            if let Some((kind, text)) = &instance.said {
                ui.push(json!({ "type": kind, "text": text }));
            }
            match pages.view(page.id, &instance.owner, now) {
                Ok(items) => ui.extend(items),
                Err(error) => {
                    tracing::error!(error = %format!("{error:#}"), page = page.id, "an admin page couldn't be drawn");
                    ui.push(json!({ "type": "error", "text": "This page couldn't be drawn; try again." }));
                }
            }
        }
        let allowed: Vec<&str> = if instance.done { Vec::new() } else { page.events.iter().copied().chain(["done"]).collect() };
        Ok((
            format!("{P}status"),
            json!({
                "instance_id": id,
                "state": if instance.done { "done" } else { "view" },
                "status": if instance.done { "completed" } else { "active" },
                "allowed_events": allowed,
                "action_menu": {},
                "artifacts": {},
                "ui": ui,
                "ui_profile": "receiver",
            }),
        ))
    }
}

/// A button that fires `event` with `input`, for a row of a list.
pub fn button(label: &str, event: &str, input: Value) -> Value {
    json!({ "type": "button", "label": label, "event": event, "input": input })
}

/// A labelled line of text.
pub fn line(label: &str, value: impl std::fmt::Display) -> Value {
    json!({ "type": "text", "text": format!("{label}: {value}") })
}

/// A number of bytes as a person reads it.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{n} bytes") } else { format!("{value:.1} {}", UNITS[unit]) }
}

/// A number of bytes typed by a person: `500`, `10 MB`, `4GB`, `1.5 gb`.
pub fn parse_bytes(text: &str) -> Option<u64> {
    let text = text.trim().to_ascii_lowercase();
    let split = text.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().ok()?;
    let scale: u64 = match unit.trim() {
        "" | "b" | "bytes" => 1,
        "k" | "kb" => 1 << 10,
        "m" | "mb" => 1 << 20,
        "g" | "gb" => 1 << 30,
        "t" | "tb" => 1 << 40,
        _ => return None,
    };
    (number >= 0.0 && number.is_finite()).then(|| (number * scale as f64).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counter(Mutex<u32>);

    impl Pages for Counter {
        fn pages(&self) -> Vec<Page> {
            vec![Page { id: "admin-count", title: "Count", summary: "A number.", events: &["add"] }]
        }

        fn view(&self, _page: &str, _admin: &str, _now: i64) -> anyhow::Result<Vec<Value>> {
            Ok(vec![line("Count", *self.0.lock().unwrap())])
        }

        fn act(&self, _page: &str, _event: &str, input: &Value, _admin: &str, _now: i64) -> Result<Option<String>, String> {
            let by = input["by"].as_u64().ok_or("Say how many.")? as u32;
            *self.0.lock().unwrap() += by;
            Ok(Some(format!("Added {by}.")))
        }
    }

    fn msg(kind: &str, body: Value) -> Value {
        json!({ "id": "m1", "type": format!("{P}{kind}"), "body": body })
    }

    fn texts(answer: &(String, Value)) -> Vec<String> {
        answer.1["ui"].as_array().unwrap().iter().filter_map(|item| item["text"].as_str().map(str::to_string)).collect()
    }

    #[test]
    fn only_an_admin_sees_or_opens_a_page() {
        let (admin, pages) = (Admin::new(), Counter(Mutex::new(0)));
        let discovered = admin.handle(&pages, false, "did:x", &msg("discover", json!({})), 1).unwrap().unwrap();
        assert_eq!(discovered.1["workflows"], json!([]));
        let discovered = admin.handle(&pages, true, "did:a", &msg("discover", json!({})), 1).unwrap().unwrap();
        assert_eq!(discovered.1["workflows"][0]["template_id"], "admin-count");

        let start = msg("start", json!({ "template_id": "admin-count", "instance_id": "i1" }));
        assert_eq!(admin.handle(&pages, false, "did:x", &start, 1).unwrap().unwrap_err().code, "not_authorized");
        let opened = admin.handle(&pages, true, "did:a", &start, 1).unwrap().unwrap();
        assert_eq!(texts(&opened), ["Count", "Count: 0"]);
        assert_eq!(opened.1["allowed_events"], json!(["add", "done"]));
        // Another admin can't drive this one's page; nor can this one once not an admin.
        let advance = |by: Value, key: &str| msg("advance", json!({ "instance_id": "i1", "event": "add", "input": { "by": by }, "idempotency_key": key }));
        assert_eq!(admin.handle(&pages, true, "did:b", &advance(json!(1), "k0"), 2).unwrap().unwrap_err().code, "instance_not_found");
        assert_eq!(admin.handle(&pages, false, "did:a", &advance(json!(1), "k0"), 2).unwrap().unwrap_err().code, "not_authorized");
        // A message about another workflow isn't these pages' to answer.
        assert!(admin.handle(&pages, true, "did:a", &msg("start", json!({ "template_id": "new-sticker-pack" })), 2).is_none());
    }

    #[test]
    fn a_page_acts_says_so_and_closes() {
        let (admin, pages) = (Admin::new(), Counter(Mutex::new(0)));
        admin.handle(&pages, true, "did:a", &msg("start", json!({ "template_id": "admin-count", "instance_id": "i1" })), 1).unwrap().unwrap();
        let advance = |by: Value, key: &str| msg("advance", json!({ "instance_id": "i1", "event": "add", "input": { "by": by }, "idempotency_key": key }));
        let done = admin.handle(&pages, true, "did:a", &advance(json!(2), "k1"), 2).unwrap().unwrap();
        assert_eq!(texts(&done), ["Count", "Added 2.", "Count: 2"]);
        // The same press twice is done once.
        admin.handle(&pages, true, "did:a", &advance(json!(2), "k1"), 2).unwrap().unwrap();
        assert_eq!(*pages.0.lock().unwrap(), 2);
        let wrong = admin.handle(&pages, true, "did:a", &advance(json!("x"), "k2"), 3).unwrap().unwrap();
        assert_eq!(wrong.1["ui"][1], json!({ "type": "error", "text": "Say how many." }));
        let closed = admin.handle(&pages, true, "did:a", &msg("advance", json!({ "instance_id": "i1", "event": "done" })), 4).unwrap().unwrap();
        assert!(closed.0.ends_with("/complete"));
        assert_eq!(admin.handle(&pages, true, "did:a", &advance(json!(1), "k3"), 5).unwrap().unwrap_err().code, "guard_failed");
    }

    #[test]
    fn bytes_read_and_written() {
        assert_eq!(parse_bytes("10 MB"), Some(10 << 20));
        assert_eq!(parse_bytes("1.5gb"), Some(1_610_612_736));
        assert_eq!(parse_bytes("500"), Some(500));
        assert_eq!(parse_bytes("ten"), None);
        assert_eq!(bytes(10 << 20), "10.0 MB");
        assert_eq!(bytes(12), "12 bytes");
    }
}
