//! The host's admin pages (wyvrn_service::admin): every community and how it
//! stands; making one, suspending, removing and restoring it, and what it may hold
//! -- what the `create`, `suspend`, `resume`, `delete`, `restore` and `limits`
//! commands do.

use serde_json::{json, Value};
use wyvrn_service::admin::{button, bytes, parse_bytes, Page, Pages};

use crate::config::Limits;
use crate::registry::{State, Storage};
use crate::server::AppState;

pub const COMMUNITIES: &str = "admin-communities";

/// How long a community removed, or taken over by its own server, is kept before
/// it can be removed for good.
pub const SEALED_FOR: i64 = 30 * 24 * 60 * 60;

fn short(did: &str) -> String {
    if did.chars().count() <= 48 { did.to_string() } else { format!("{}…{}", did.chars().take(28).collect::<String>(), did.chars().rev().take(12).collect::<String>().chars().rev().collect::<String>()) }
}

/// What a community can be kept in here: MySQL only where the host has a server.
fn storage_options(mysql: bool) -> Value {
    let mut options = vec![json!({ "value": "sqlite", "label": "SQLite files of its own" })];
    if mysql {
        options.push(json!({ "value": "mysql-db", "label": "A MySQL database of its own" }));
        options.push(json!({ "value": "mysql-shared", "label": "Tables of its own in the shared MySQL database" }));
    }
    json!(options)
}

/// A form's text field, trimmed.
fn field<'a>(input: &'a Value, name: &str) -> &'a str {
    input[name].as_str().map_or("", str::trim)
}

/// The limits a form gives, over `base`: a number of members and channels, sizes
/// as written ("25 MB").
fn limits_from(input: &Value, base: Limits) -> Result<Limits, String> {
    let count = |name: &str, otherwise: usize| -> Result<usize, String> {
        match field(input, name) {
            "" => Ok(otherwise),
            text => text.parse().map_err(|_| format!("{name}: a number, please.")),
        }
    };
    let size = |name: &str, otherwise: u64| -> Result<u64, String> {
        match field(input, name) {
            "" => Ok(otherwise),
            text => parse_bytes(text).ok_or_else(|| format!("{name}: a size, as in 25 MB.")),
        }
    };
    Ok(Limits {
        members: count("members", base.members)?,
        channels: count("channels", base.channels)?,
        file_bytes: size("file_bytes", base.file_bytes)?,
        files_quota: size("files_quota", base.files_quota)?,
        files_total: size("files_total", base.files_total)?,
    })
}

impl Pages for AppState {
    fn pages(&self) -> Vec<Page> {
        vec![Page {
            id: COMMUNITIES,
            title: "Communities",
            summary: "Every community kept here; make one, set what it may hold, suspend or remove it.",
            events: &["create", "suspend", "resume", "delete", "restore", "limits"],
        }]
    }

    fn view(&self, _page: &str, _admin: &str, now: i64) -> anyhow::Result<Vec<Value>> {
        let host = &self.host;
        let defaults = host.config.limits;
        let mut items = vec![json!({ "type": "form", "children": [
            { "type": "text", "text": "Make a community. Its name is its address here (c/<name>/) and at the DID host (community:<name>); its owner is its first admin, who can then hand it on." },
            { "type": "text-input", "form-id": "name", "label": "Name (lowercase letters, digits and -)", "value": "" },
            { "type": "text-input", "form-id": "title", "label": "What it is called", "value": "" },
            { "type": "text-input", "form-id": "owner", "label": "Its owner (a DID)", "value": "" },
            { "type": "drop-down", "form-id": "storage", "label": "Kept in", "value": host.config.storage.default, "options": storage_options(!host.config.storage.mysql.is_empty()) },
            button("Make it", "create", json!({})),
        ]})];
        items.push(json!({ "type": "text", "text": format!(
            "Each may hold, unless set for it: {} members, {} channels, files up to {} ({} a member, {} in all). {} open now.",
            defaults.members, defaults.channels, bytes(defaults.file_bytes), bytes(defaults.files_quota), bytes(defaults.files_total), host.loaded_count()
        ) }));
        let records = host.registry.all()?;
        if records.is_empty() {
            items.push(json!({ "type": "text", "text": "No communities yet." }));
        }
        for record in records {
            let mut what = format!("{} · {} · {} · {} · owner {}", record.name, record.title, record.state.as_str(), record.storage.as_str(), short(&record.owner));
            if let Some(until) = record.sealed_until {
                what.push_str(&format!(" · kept {} more days", ((until - now).max(0) + 86_399) / 86_400));
            }
            let mut row = vec![json!({ "type": "text", "text": what })];
            let named = json!({ "name": record.name });
            match record.state {
                State::Active | State::Moving => {
                    row.push(button("Suspend", "suspend", named.clone()));
                    row.push(button("Remove", "delete", named.clone()));
                }
                State::Suspended => {
                    row.push(button("Resume", "resume", named.clone()));
                    row.push(button("Remove", "delete", named.clone()));
                }
                State::Deleted | State::Moved => row.push(button("Restore", "restore", named.clone())),
            }
            items.push(json!({ "type": "row", "children": row }));
            let limits = record.limits.unwrap_or(defaults);
            items.push(json!({ "type": "form", "children": [
                { "type": "text", "text": format!("What {} may hold", record.name) },
                { "type": "text-input", "form-id": "members", "label": "Members", "value": limits.members.to_string() },
                { "type": "text-input", "form-id": "channels", "label": "Channels", "value": limits.channels.to_string() },
                { "type": "text-input", "form-id": "file_bytes", "label": "Largest file", "value": bytes(limits.file_bytes) },
                { "type": "text-input", "form-id": "files_quota", "label": "Files, a member", "value": bytes(limits.files_quota) },
                { "type": "text-input", "form-id": "files_total", "label": "Files, in all", "value": bytes(limits.files_total) },
                button("Set", "limits", named),
            ]}));
        }
        Ok(items)
    }

    fn act(&self, _page: &str, event: &str, input: &Value, admin: &str, now: i64) -> Result<Option<String>, String> {
        let failed = |error: anyhow::Error| {
            tracing::error!(error = %format!("{error:#}"), "an admin page's call failed");
            "Something went wrong here; try again.".to_string()
        };
        let host = &self.host;
        let name = field(input, "name").to_lowercase();
        if event == "create" {
            let owner = field(input, "owner");
            if !owner.starts_with("did:") {
                return Err("Its owner: a DID.".into());
            }
            // (An app that draws no drop-down sends none: the host's own choice, then.)
            let storage = match field(input, "storage") {
                "" => host.config.storage.default.as_str(),
                kind => kind,
            };
            let storage = Storage::parse(storage).ok_or("Kept in: sqlite, mysql-db or mysql-shared.")?;
            if storage != Storage::Sqlite && host.config.storage.mysql.is_empty() {
                return Err("This host has no MySQL server set; keep it in sqlite.".into());
            }
            let title = match field(input, "title") {
                "" => name.clone(),
                title => title.to_string(),
            };
            let record = host.registry.create(&name, &title, storage, owner, admin, now).map_err(|error| error.to_string())?;
            tracing::info!(community = %record.name, owner = %record.owner, by = %admin, "made a community from an admin page");
            return Ok(Some(format!("{} is made, at {}c/{}/.", record.name, host.config.server.public_url, record.name)));
        }
        let record = host.registry.get(&name).map_err(failed)?.ok_or_else(|| format!("There is no community called {name:?}."))?;
        let said = match event {
            "suspend" => {
                host.set_state(&name, State::Suspended, None).map_err(failed)?;
                format!("{name} is suspended: nothing is answered until it is resumed.")
            }
            "resume" if record.state == State::Suspended => {
                host.set_state(&name, State::Active, None).map_err(failed)?;
                format!("{name} is open again.")
            }
            "delete" => {
                host.set_state(&name, State::Deleted, Some(now + SEALED_FOR)).map_err(failed)?;
                format!("{name} is removed. It is kept for 30 days, and can be restored until then.")
            }
            "restore" if matches!(record.state, State::Deleted | State::Moved) => {
                host.set_state(&name, State::Active, None).map_err(failed)?;
                format!("{name} is open again.")
            }
            "limits" => {
                let limits = limits_from(input, record.limits.unwrap_or(host.config.limits))?;
                host.registry.set_limits(&name, Some(&limits)).map_err(failed)?;
                // Taken up the next time it is opened.
                host.unload(&name);
                format!("{name} may hold {} members and {} channels.", limits.members, limits.channels)
            }
            _ => return Err("That can't be done to it now.".into()),
        };
        tracing::info!(community = %name, event, by = %admin, "an admin page changed a community");
        Ok(Some(said))
    }
}
