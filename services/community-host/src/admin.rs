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

/// The kinds of channel a host may turn off, as its admin page names them.
const KINDS: &[(&str, &str)] = &[
    ("announcement", "Announcement channels"),
    ("voice", "Voice channels"),
    ("stage", "Stages"),
    ("forum", "Forums"),
    ("gallery", "Galleries"),
    ("calendar", "Calendars"),
    ("form", "Forms"),
    ("wiki", "Wikis"),
];

/// The kinds of channel a form turns off: each `allow_<kind>` box left unticked.
/// One the form doesn't have (an app that draws no check box) stays as `base`
/// has it. The same as the host's, `None`: it follows the host's from then on.
fn kinds_from(input: &Value, base: &[String], host: &[String]) -> Option<Vec<String>> {
    let off: Vec<String> = KINDS
        .iter()
        .filter(|(kind, _)| match input[format!("allow_{kind}")].as_bool() {
            Some(allowed) => !allowed,
            None => base.iter().any(|off| off == kind),
        })
        .map(|(kind, _)| kind.to_string())
        .collect();
    (off != host).then_some(off)
}

/// The limits a form gives, over `base`: a number of members and channels, sizes
/// as written ("25 MB"), and the kinds of channel allowed (over the `host`'s).
fn limits_from(input: &Value, base: Limits, host: &Limits) -> Result<Limits, String> {
    let host_kinds = host.disabled_kinds(host);
    let disabled_channel_kinds = kinds_from(input, &base.disabled_kinds(host), &host_kinds);
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
        roles: count("roles", base.roles)?.clamp(1, 250),
        posts_per_minute: count("posts_per_minute", base.posts_per_minute)?,
        // Left empty, as it was; 0, no limit.
        store_bytes: match field(input, "store_bytes") {
            "" => base.store_bytes,
            "0" => 0,
            text => parse_bytes(text).ok_or("Its store: a size, as in 2 GB, or 0 for no limit.")?,
        },
        sticker_packs: count("sticker_packs", base.sticker_packs)?,
        stickers_per_pack: count("stickers_per_pack", base.stickers_per_pack)?.max(1),
        emoji_packs: count("emoji_packs", base.emoji_packs)?,
        emoji_per_pack: count("emoji_per_pack", base.emoji_per_pack)?.max(1),
        deleted_text_days: u32::try_from(count("deleted_text_days", base.deleted_text_days as usize)?).unwrap_or(u32::MAX),
        disabled_channel_kinds,
    })
}

impl Pages for AppState {
    fn pages(&self) -> Vec<Page> {
        vec![Page {
            id: COMMUNITIES,
            title: "Communities",
            summary: "Every community kept here; make one, set what it may hold, suspend or remove it.",
            events: &["create", "suspend", "resume", "delete", "restore", "limits", "move", "handed", "creating", "code", "revoke_code", "backup"],
        }]
    }

    fn view(&self, _page: &str, _admin: &str, now: i64) -> anyhow::Result<Vec<Value>> {
        let host = &self.host;
        let defaults = &host.config.limits;
        let mut items = vec![json!({ "type": "form", "children": [
            { "type": "text", "text": "Make a community. Its name is its address here (c/<name>/) and at the DID host (community:<name>); its owner is its first admin, who can then hand it on." },
            { "type": "text-input", "form-id": "name", "label": "Name (lowercase letters, digits and -)", "value": "" },
            { "type": "text-input", "form-id": "title", "label": "What it is called", "value": "" },
            { "type": "text-input", "form-id": "owner", "label": "Its owner (a DID)", "value": "" },
            { "type": "drop-down", "form-id": "storage", "label": "Kept in", "value": host.config.storage.default, "options": storage_options(!host.config.storage.mysql.is_empty()) },
            button("Make it", "create", json!({})),
        ]})];
        // Making a community from the app (creating.rs): who may, and the codes.
        let mode = self.creating_mode();
        items.push(json!({ "type": "form", "children": [
            { "type": "text", "text": format!("Who may make a community here from their app, as its owner (each up to {}).", host.config.creating.per_owner) },
            { "type": "drop-down", "form-id": "mode", "label": "Who may make one", "value": mode.as_str(), "options": [
                { "value": "closed", "label": "Only you, here" },
                { "value": "codes", "label": "Whoever has a code you hand out" },
                { "value": "anyone", "label": "Anyone" }
            ] },
            button("Save", "creating", json!({})),
        ]}));
        if mode == crate::creating::Mode::Codes {
            items.push(json!({ "type": "form", "children": [
                { "type": "text-input", "form-id": "uses", "label": "A new code, good for how many communities", "value": "1" },
                { "type": "text-input", "form-id": "note", "label": "For whom (only you see this)", "value": "" },
                button("Make a code", "code", json!({})),
            ]}));
            for (code, uses, note) in host.registry.codes()? {
                let said = if note.is_empty() { format!("{code} · {uses} left") } else { format!("{code} · {uses} left · {note}") };
                items.push(json!({ "type": "row", "children": [{ "type": "text", "text": said }, button("Revoke", "revoke_code", json!({ "code": code }))] }));
            }
        }
        let off: Vec<&str> = KINDS.iter().filter(|(kind, _)| defaults.disabled_kinds(defaults).iter().any(|o| o == kind)).map(|(_, label)| *label).collect();
        let off = if off.is_empty() { String::new() } else { format!(" Turned off, unless set for it: {}.", off.join(", ").to_lowercase()) };
        items.push(json!({ "type": "text", "text": format!(
            "Each may hold, unless set for it: {} members, {} channels, files up to {} ({} a member, {} in all).{off} {} open now.",
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
            if let Some((_, at)) = host.backups(&record.name).first() {
                let hours = (now - at).max(0) / 3600;
                what.push_str(&if hours < 1 { " · backed up within the hour".to_string() } else { format!(" · backed up {hours} h ago") });
            }
            let mut row = vec![json!({ "type": "text", "text": what })];
            let named = json!({ "name": record.name });
            if record.state.served() && crate::backups::key(&host.config.backups).ok().flatten().is_some() {
                row.push(button("Back up now", "backup", named.clone()));
            }
            match record.state {
                State::Active => {
                    row.push(button("Suspend", "suspend", named.clone()));
                    row.push(button("Remove", "delete", named.clone()));
                }
                // Its owner is taking it to their own server: stopped by itself once
                // that holds its address, or here (with no DID host to tell).
                State::Moving => {
                    row.push(button("Handed over", "handed", named.clone()));
                    row.push(button("Suspend", "suspend", named.clone()));
                }
                State::Suspended => {
                    row.push(button("Resume", "resume", named.clone()));
                    row.push(button("Remove", "delete", named.clone()));
                }
                State::Deleted | State::Moved => row.push(button("Restore", "restore", named.clone())),
            }
            items.push(json!({ "type": "row", "children": row }));
            // Its DIDs, to copy (asked 2026-11-18: only the URL was shown, and
            // the DID had to be worked out from it). `copyable` is Wyvrn's own:
            // another app shows the text.
            let at = format!("{}c/{}/", host.config.server.public_url, record.name);
            if host.config.server.did_method == wyvrn_service::address::DidMethod::Web {
                if let Ok(did) = wyvrn_service::address::did_web_for(&at) {
                    items.push(json!({ "type": "text", "text": did, "copyable": true, "label": "Its DID" }));
                }
            }
            if let Ok(Some(address)) = host.address(&record.name) {
                items.push(json!({ "type": "text", "text": address.did_web, "copyable": true, "label": "Its public address" }));
            }
            let limits = record.limits.clone().unwrap_or_else(|| defaults.clone());
            let off = limits.disabled_kinds(defaults);
            let mut kinds = vec![json!({ "type": "text", "text": "Kinds of channel it may open (text channels always). One it has of a kind turned off is read-only." })];
            for (kind, label) in KINDS {
                kinds.push(json!({ "type": "check-box", "form-id": format!("allow_{kind}"), "label": label, "value": !off.iter().any(|o| o == kind) }));
            }
            let mut form = json!({ "type": "form", "children": [
                { "type": "text", "text": format!("What {} may hold", record.name) },
                { "type": "text-input", "form-id": "members", "label": "Members", "value": limits.members.to_string() },
                { "type": "text-input", "form-id": "channels", "label": "Channels", "value": limits.channels.to_string() },
                { "type": "text-input", "form-id": "file_bytes", "label": "Largest file", "value": bytes(limits.file_bytes) },
                { "type": "text-input", "form-id": "files_quota", "label": "Files, a member", "value": bytes(limits.files_quota) },
                { "type": "text-input", "form-id": "files_total", "label": "Files, in all", "value": bytes(limits.files_total) },
                { "type": "text-input", "form-id": "roles", "label": "Roles", "value": limits.roles.to_string() },
                { "type": "text-input", "form-id": "posts_per_minute", "label": "Posts a minute, from everyone (0: no limit)", "value": limits.posts_per_minute.to_string() },
                { "type": "text-input", "form-id": "store_bytes", "label": "Its store (0: no limit)", "value": if limits.store_bytes == 0 { "0".to_string() } else { bytes(limits.store_bytes) } },
                { "type": "text-input", "form-id": "sticker_packs", "label": "Sticker packs", "value": limits.sticker_packs.to_string() },
                { "type": "text-input", "form-id": "stickers_per_pack", "label": "Stickers in a pack", "value": limits.stickers_per_pack.to_string() },
                { "type": "text-input", "form-id": "emoji_packs", "label": "Emoji packs", "value": limits.emoji_packs.to_string() },
                { "type": "text-input", "form-id": "emoji_per_pack", "label": "Emoji in a pack", "value": limits.emoji_per_pack.to_string() },
                { "type": "text-input", "form-id": "deleted_text_days", "label": "Days moderators may see what a deleted post said (0: none)", "value": limits.deleted_text_days.to_string() },
            ]});
            if let Some(children) = form["children"].as_array_mut() {
                children.extend(kinds);
                children.push(button("Set", "limits", named.clone()));
            }
            items.push(form);
            if record.state.served() {
                let others: Vec<Value> = storage_options(!host.config.storage.mysql.is_empty())
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|option| option["value"] != record.storage.as_str())
                    .cloned()
                    .collect();
                if !others.is_empty() {
                    items.push(json!({ "type": "form", "children": [
                        { "type": "drop-down", "form-id": "to", "label": format!("Move {} to", record.name), "options": others },
                        button("Move", "move", named),
                    ]}));
                }
            }
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
        match event {
            "creating" => {
                let mode = crate::creating::Mode::parse(field(input, "mode")).ok_or("Who may make one: choose.")?;
                self.set_creating_mode(mode).map_err(failed)?;
                tracing::info!(mode = mode.as_str(), by = %admin, "who may make a community here changed");
                return Ok(Some(match mode {
                    crate::creating::Mode::Closed => "Only host admins make communities here now.".to_string(),
                    crate::creating::Mode::Codes => "Whoever has a code may make a community now. Make codes below.".to_string(),
                    crate::creating::Mode::Anyone => "Anyone may make a community here now.".to_string(),
                }));
            }
            "code" => {
                let uses: i64 = field(input, "uses").parse().ok().filter(|n| (1..=1000).contains(n)).ok_or("Good for: a number from 1 to 1000.")?;
                // Ten letters and digits from a fresh UUID's randomness: easy to read out, hard to guess.
                let code: String = uuid::Uuid::new_v4().simple().to_string().chars().filter(|c| c.is_ascii_alphanumeric()).take(10).collect::<String>().to_uppercase();
                host.registry.add_code(&code, uses, field(input, "note"), admin, now).map_err(failed)?;
                return Ok(Some(format!("A code: {code}. Hand it to whoever is to make a community.")));
            }
            "revoke_code" => {
                host.registry.revoke_code(field(input, "code")).map_err(failed)?;
                return Ok(Some("That code no longer works.".into()));
            }
            _ => {}
        }
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
            "move" => {
                let to = Storage::parse(field(input, "to")).ok_or("Move it to: choose where.")?;
                tokio::task::block_in_place(|| host.move_storage(&name, to)).map_err(|error| {
                    tracing::error!(community = %name, error = %format!("{error:#}"), "couldn't move a community's storage");
                    format!("It couldn't be moved, and is kept where it was: {error}")
                })?
            }
            "backup" => {
                let made = tokio::task::block_in_place(|| host.back_up(&name, now)).map_err(|error| format!("It couldn't be backed up: {error}"))?;
                format!("{name} is backed up: {}", made.display())
            }
            "handed" if record.state == State::Moving => {
                host.exports.forget(&name);
                host.set_state(&name, State::Moved, Some(now + SEALED_FOR)).map_err(failed)?;
                format!("{name} is stopped here, and kept for 30 days.")
            }
            "limits" => {
                let limits = limits_from(input, record.limits.clone().unwrap_or_else(|| host.config.limits.clone()), &host.config.limits)?;
                host.change_limits(&name, record.limits.as_ref(), &limits).map_err(failed)?;
                format!("{name} may hold {} members and {} channels.", limits.members, limits.channels)
            }
            _ => return Err("That can't be done to it now.".into()),
        };
        tracing::info!(community = %name, event, by = %admin, "an admin page changed a community");
        Ok(Some(said))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_turns_kinds_of_channel_off_and_back_to_the_hosts() {
        let host = Limits { disabled_channel_kinds: Some(vec!["wiki".into()]), ..Limits::default() };
        let every_box = |off: &[&str]| -> Value { KINDS.iter().map(|(kind, _)| (format!("allow_{kind}"), json!(!off.contains(kind)))).collect::<serde_json::Map<_, _>>().into() };
        // Forms and wikis off for this one.
        let set = limits_from(&every_box(&["form", "wiki"]), host.clone(), &host).unwrap();
        assert_eq!(set.disabled_channel_kinds, Some(vec!["form".to_string(), "wiki".to_string()]));
        assert_eq!(set.disabled_kinds(&host), ["form", "wiki"]);
        // Ticked as the host has them: it follows the host's again.
        assert_eq!(limits_from(&every_box(&["wiki"]), set.clone(), &host).unwrap().disabled_channel_kinds, None);
        // A form without the boxes leaves them as they were.
        assert_eq!(limits_from(&json!({ "members": "10" }), set.clone(), &host).unwrap().disabled_channel_kinds, set.disabled_channel_kinds);
    }
}
