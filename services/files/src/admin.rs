//! The service's admin pages (wyvrn_service::admin): what is held and by whom,
//! every file the largest first, the uploaders, and the rules
//! ([`crate::policy`]). An admin's app draws them; only admins are let in.

use serde_json::{json, Value};
use wyvrn_service::admin::{button, bytes, line, parse_bytes, Page, Pages};

use crate::policy::{Limits, Uploader};
use crate::server::AppState;
use crate::store::State;

pub const OVERVIEW: &str = "admin-files-overview";
pub const FILES: &str = "admin-files-files";
pub const UPLOADERS: &str = "admin-files-uploaders";
pub const SETTINGS: &str = "admin-files-settings";

/// How many files the files page lists.
const LISTED: usize = 100;
const DAY: i64 = 24 * 60 * 60;

fn short(did: &str) -> String {
    if did.chars().count() <= 48 { did.to_string() } else { format!("{}…{}", did.chars().take(28).collect::<String>(), did.chars().rev().take(12).collect::<String>().chars().rev().collect::<String>()) }
}

fn days(seconds: i64) -> String {
    let days = seconds as f64 / DAY as f64;
    if (days - days.round()).abs() < 0.01 { format!("{}", days.round() as i64) } else { format!("{days:.2}") }
}

fn when(time: i64, now: i64) -> String {
    let left = time - now;
    if left <= 0 { "now".into() } else if left < DAY { format!("in {} h", (left + 3599) / 3600) } else { format!("in {} days", left / DAY) }
}

/// A size as a field shows it: exactly, in the largest unit it is a whole number
/// of, so that saving a form unchanged changes nothing.
fn exact(n: u64) -> String {
    for (unit, name) in [(1u64 << 40, "TB"), (1 << 30, "GB"), (1 << 20, "MB"), (1 << 10, "KB")] {
        if n >= unit && n % unit == 0 {
            return format!("{} {name}", n / unit);
        }
    }
    format!("{n} bytes")
}

fn text_field(id: &str, label: &str, value: impl Into<String>) -> Value {
    json!({ "type": "text-input", "form-id": id, "label": label, "value": value.into() })
}

fn check(id: &str, label: &str, on: bool) -> Value {
    json!({ "type": "check-box", "form-id": id, "label": label, "value": on })
}

fn limits_fields(prefix: &str, limits: &Limits) -> Vec<Value> {
    vec![
        text_field(&format!("{prefix}quota"), "May keep in all", exact(limits.quota_bytes)),
        text_field(&format!("{prefix}max_file"), "Largest file", exact(limits.max_file_bytes)),
        text_field(&format!("{prefix}max_keep"), "Longest kept (days)", days(limits.max_keep_seconds)),
        text_field(&format!("{prefix}uploads"), "Uploads at once", limits.max_uploads_in_progress.to_string()),
    ]
}

fn field<'a>(input: &'a Value, id: &str) -> &'a str {
    input[id].as_str().map_or("", str::trim)
}

fn read_bytes(input: &Value, id: &str, what: &str) -> Result<u64, String> {
    parse_bytes(field(input, id)).ok_or_else(|| format!("{what}: say a size, like 500 MB or 4 GB."))
}

fn read_days(input: &Value, id: &str, what: &str) -> Result<i64, String> {
    field(input, id).parse::<f64>().ok().filter(|d| *d > 0.0 && d.is_finite()).map(|d| (d * DAY as f64).round() as i64).ok_or_else(|| format!("{what}: say a number of days, more than 0."))
}

fn read_limits(input: &Value, prefix: &str) -> Result<Limits, String> {
    Ok(Limits {
        quota_bytes: read_bytes(input, &format!("{prefix}quota"), "May keep in all")?,
        max_file_bytes: read_bytes(input, &format!("{prefix}max_file"), "Largest file")?,
        max_keep_seconds: read_days(input, &format!("{prefix}max_keep"), "Longest kept")?,
        max_uploads_in_progress: field(input, &format!("{prefix}uploads")).parse().ok().filter(|n| *n > 0).ok_or("Uploads at once: say a number, 1 or more.")?,
    })
}

fn did_of(input: &Value) -> Result<String, String> {
    let did = field(input, "did");
    if did.starts_with("did:") && did.len() <= 2048 && !did.contains(char::is_whitespace) { Ok(did.to_string()) } else { Err("That isn't a DID.".into()) }
}

impl AppState {
    fn uploader_or_mistake(&self, did: &str) -> Result<Uploader, String> {
        self.host.store.uploader(did).map_err(failed)?.ok_or_else(|| "There's no such uploader here.".to_string())
    }

    fn overview(&self, now: i64) -> anyhow::Result<Vec<Value>> {
        let policy = self.policy()?;
        let used = self.host.store.used_total()?;
        let total = if policy.total_bytes == 0 { "no limit but the disk".to_string() } else { bytes(policy.total_bytes) };
        let who = match (policy.open, policy.locked) {
            (_, true) => "Locked down: nobody new becomes an uploader.",
            (true, false) => "Open: anyone who authenticates becomes an uploader by uploading.",
            (false, false) => "By invitation: the uploaders the settings name, and those added here.",
        };
        let _ = now;
        Ok(vec![
            line("Held", format!("{} of {total}", bytes(used))),
            line("Files", self.host.store.file_count()?),
            line("Uploaders", self.host.store.uploaders()?.len()),
            json!({ "type": "text", "text": who }),
            json!({ "type": "text", "text": if policy.require_profile { "Uploaders it has no profile for are refused." } else { "Anyone let in may upload, profile or not." } }),
            line("Admins", self.host.config.admins.len()),
            json!({ "type": "divider" }),
            json!({ "type": "text", "text": "Files past their time are removed every ten minutes." }),
            button("Remove what is past its time now", "prune", json!({})),
        ])
    }

    fn files_page(&self, now: i64) -> anyhow::Result<Vec<Value>> {
        let files = self.host.store.largest(LISTED)?;
        if files.is_empty() {
            return Ok(vec![json!({ "type": "text", "text": "Nothing is held." })]);
        }
        let names: std::collections::HashMap<String, String> = self.host.store.uploaders()?.into_iter().filter_map(|u| u.name.map(|name| (u.did, name))).collect();
        let mut items = vec![json!({ "type": "text", "text": format!("The {} largest, largest first.", files.len().min(LISTED)) })];
        for file in files {
            let owner = names.get(&file.owner).cloned().unwrap_or_else(|| short(&file.owner));
            let state = match file.state {
                State::Stored if file.live_at(now) => format!("goes {}", when(file.expires, now)),
                State::Stored => "past its time".to_string(),
                State::Uploading => format!("{} of it uploaded", bytes(self.host.store.received(&file.id))),
            };
            items.push(json!({ "type": "row", "children": [
                { "type": "text", "text": format!("{} · {} · {owner} · {state}", file.name, bytes(file.byte_count)) },
                button("Remove", "remove", json!({ "file": file.id })),
            ]}));
        }
        Ok(items)
    }

    fn uploaders_page(&self) -> anyhow::Result<Vec<Value>> {
        let policy = self.policy()?;
        let mut items = vec![json!({ "type": "form", "children": [
            text_field("did", "Add an uploader by DID", ""),
            button("Add", "add", json!({})),
        ]})];
        let uploaders = self.host.store.uploaders()?;
        if uploaders.is_empty() {
            items.push(json!({ "type": "text", "text": "Nobody has uploaded yet." }));
        }
        for uploader in uploaders {
            let admin = self.host.config.admins.contains(&uploader.did);
            let limits = policy.limits_for(admin, uploader.limits);
            let who = uploader.name.clone().unwrap_or_else(|| "(no profile)".into());
            let whose = if uploader.limits.is_some() { "their own" } else if admin { "the admins'" } else { "everyone's" };
            items.push(json!({ "type": "divider" }));
            items.push(json!({ "type": "text", "text": format!("{who}{}{}", if admin { " · admin" } else { "" }, if uploader.blocked { " · blocked" } else { "" }) }));
            items.push(json!({ "type": "text", "text": short(&uploader.did) }));
            items.push(line("Keeps", format!("{} of {} ({whose} limits) · came by {}", bytes(self.host.store.used_by(&uploader.did)?), bytes(limits.quota_bytes), uploader.source)));
            let did = json!({ "did": uploader.did });
            items.push(json!({ "type": "row", "children": [
                if uploader.blocked { button("Unblock", "unblock", did.clone()) } else { button("Block", "block", did.clone()) },
                button("Remove", "remove", did.clone()),
            ]}));
            let mut form = limits_fields("", &limits);
            form.push(json!({ "type": "row", "children": [
                button("Give them these limits", "set-limits", did.clone()),
                button("Back to their tier's", "clear-limits", did),
            ]}));
            items.push(json!({ "type": "form", "children": form }));
        }
        Ok(items)
    }

    fn settings_page(&self) -> anyhow::Result<Vec<Value>> {
        let policy = self.policy()?;
        let mut form = vec![
            json!({ "type": "text", "text": "Who may upload" }),
            check("open", "Anyone who authenticates may become an uploader", policy.open),
            check("locked", "Locked down: nobody new, whatever else is set", policy.locked),
            check("require_profile", "Refuse uploaders this service has no profile for (they are asked for one)", policy.require_profile),
            json!({ "type": "divider" }),
            text_field("total", "The most held of everyone's (0 for no limit)", if policy.total_bytes == 0 { "0".to_string() } else { exact(policy.total_bytes) }),
            text_field("default_keep", "Kept when the uploader doesn't say (days)", days(policy.default_keep_seconds)),
            json!({ "type": "divider" }),
            json!({ "type": "text", "text": "Everyone's limits" }),
        ];
        form.extend(limits_fields("everyone_", &policy.everyone));
        form.push(json!({ "type": "divider" }));
        form.push(json!({ "type": "text", "text": "Admins' limits" }));
        form.extend(limits_fields("admins_", &policy.admins));
        form.push(button("Save", "save", json!({})));
        Ok(vec![json!({ "type": "form", "children": form })])
    }

    fn save_settings(&self, input: &Value) -> Result<String, String> {
        let mut policy = self.policy().map_err(failed)?;
        policy.open = input["open"] == true;
        policy.locked = input["locked"] == true;
        policy.require_profile = input["require_profile"] == true;
        policy.total_bytes = read_bytes(input, "total", "The most held")?;
        policy.default_keep_seconds = read_days(input, "default_keep", "Kept when the uploader doesn't say")?;
        policy.everyone = read_limits(input, "everyone_")?;
        policy.admins = read_limits(input, "admins_")?;
        self.host.store.set_policy(&policy).map_err(failed)?;
        Ok("Saved.".into())
    }
}

fn failed(error: anyhow::Error) -> String {
    tracing::error!(error = %format!("{error:#}"), "an admin page's store call failed");
    "Something went wrong here; try again.".into()
}

impl Pages for AppState {
    fn pages(&self) -> Vec<Page> {
        vec![
            Page { id: OVERVIEW, title: "Files: overview", summary: "What is held, by how many, under which rules.", events: &["prune"] },
            Page { id: FILES, title: "Files: every file", summary: "Everything held, the largest first, to remove.", events: &["remove"] },
            Page { id: UPLOADERS, title: "Files: uploaders", summary: "Who may upload: add, block, remove, and their own limits.", events: &["add", "remove", "block", "unblock", "set-limits", "clear-limits"] },
            Page { id: SETTINGS, title: "Files: settings", summary: "Sign-up, lock-down, profiles, and every limit.", events: &["save"] },
        ]
    }

    fn view(&self, page: &str, _admin: &str, now: i64) -> anyhow::Result<Vec<Value>> {
        match page {
            OVERVIEW => self.overview(now),
            FILES => self.files_page(now),
            UPLOADERS => self.uploaders_page(),
            SETTINGS => self.settings_page(),
            _ => Ok(Vec::new()),
        }
    }

    fn act(&self, page: &str, event: &str, input: &Value, admin: &str, now: i64) -> Result<Option<String>, String> {
        let store = &self.host.store;
        let said = match (page, event) {
            (OVERVIEW, "prune") => format!("Removed {}.", store.prune(now).map_err(failed)?),
            (FILES, "remove") => {
                let id = field(input, "file");
                let file = store.file(id).map_err(failed)?.ok_or("That file is already gone.")?;
                store.delete(id).map_err(failed)?;
                tracing::info!(file = %id, owner = %file.owner, %admin, "an admin removed a file");
                format!("Removed {}.", file.name)
            }
            (UPLOADERS, "add") => {
                let did = did_of(input)?;
                if store.uploader(&did).map_err(failed)?.is_some() {
                    return Err("They are an uploader already.".into());
                }
                store.put_uploader(&Uploader { did, source: "admin".into(), name: None, limits: None, blocked: false, added: now }).map_err(failed)?;
                "Added.".into()
            }
            (UPLOADERS, "remove") => {
                let did = did_of(input)?;
                store.remove_uploader(&did).map_err(failed)?;
                if self.host.config.uploaders.contains(&did) {
                    "Removed; but the settings name them, so they come back at their next upload. Block them to keep them out.".into()
                } else {
                    "Removed. Their files stay until they go or are removed.".into()
                }
            }
            (UPLOADERS, "block" | "unblock") => {
                let mut uploader = self.uploader_or_mistake(&did_of(input)?)?;
                uploader.blocked = event == "block";
                store.put_uploader(&uploader).map_err(failed)?;
                if uploader.blocked { "Blocked.".into() } else { "Unblocked.".into() }
            }
            (UPLOADERS, "set-limits") => {
                let mut uploader = self.uploader_or_mistake(&did_of(input)?)?;
                uploader.limits = Some(read_limits(input, "")?);
                store.put_uploader(&uploader).map_err(failed)?;
                "They have limits of their own now.".into()
            }
            (UPLOADERS, "clear-limits") => {
                let mut uploader = self.uploader_or_mistake(&did_of(input)?)?;
                uploader.limits = None;
                store.put_uploader(&uploader).map_err(failed)?;
                "Back to their tier's limits.".into()
            }
            (SETTINGS, "save") => self.save_settings(input)?,
            _ => return Err("That can't be done here.".into()),
        };
        Ok(Some(said))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_days_read_back_what_they_show() {
        let limits = Limits { quota_bytes: 10 << 30, max_file_bytes: 123_456_789, max_keep_seconds: 90 * DAY, max_uploads_in_progress: 16 };
        let mut input = serde_json::Map::new();
        for item in limits_fields("x_", &limits) {
            input.insert(item["form-id"].as_str().unwrap().into(), item["value"].clone());
        }
        assert_eq!(read_limits(&Value::Object(input), "x_").unwrap(), limits);
        assert_eq!(days(DAY / 2), "0.50");
        assert!(read_days(&json!({ "d": "0" }), "d", "Kept").is_err());
    }
}
