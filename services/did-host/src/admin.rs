//! The host's admin pages (wyvrn_service::admin): every name and how it stands,
//! and setting one aside for a holder, or for nobody, or freeing it -- what the
//! `names`, `assign` and `free` commands and an admin's did-hosting messages do.

use serde_json::{json, Value};
use wyvrn_service::admin::{button, Page, Pages};

use crate::host::State;
use crate::names::Name;
use crate::server::AppState;

pub const NAMES: &str = "admin-did-host-names";

fn short(did: &str) -> String {
    if did.chars().count() <= 48 { did.to_string() } else { format!("{}…{}", did.chars().take(28).collect::<String>(), did.chars().rev().take(12).collect::<String>().chars().rev().collect::<String>()) }
}

impl AppState {
    fn name_from(&self, input: &Value) -> Result<Name, String> {
        let text = input["name"].as_str().map_or("", str::trim).to_lowercase();
        Name::parse(&text, self.host.config.max_labels).ok_or_else(|| format!("{text:?} isn't a name this host allows."))
    }
}

impl Pages for AppState {
    fn pages(&self) -> Vec<Page> {
        vec![Page { id: NAMES, title: "DID host: names", summary: "Every name and how it stands; set one aside, or free it.", events: &["assign", "free"] }]
    }

    fn view(&self, _page: &str, _admin: &str, now: i64) -> anyhow::Result<Vec<Value>> {
        let mut items = vec![json!({ "type": "form", "children": [
            { "type": "text", "text": "Set a name aside: for the DID given, who alone may then claim it, or with none given, for nobody." },
            { "type": "text-input", "form-id": "name", "label": "Name", "value": "" },
            { "type": "text-input", "form-id": "holder", "label": "For (a DID, or empty for nobody)", "value": "" },
            button("Set aside", "assign", json!({})),
        ]})];
        let records = self.host.store.all()?;
        if records.is_empty() {
            items.push(json!({ "type": "text", "text": "No names yet." }));
        }
        for record in records {
            let state = Name::parse(&record.name, usize::MAX).map(|name| self.host.state_of(&name, Some(&record), now));
            let mut what = format!("{} · {}", record.name.replace(':', "/"), state.map_or("?", State::as_str));
            if let Some(did) = &record.did {
                what.push_str(&format!(" · {}", short(did)));
            }
            if let Some(holder) = &record.assigned {
                what.push_str(&format!(" · for {}", short(holder)));
            }
            let freeable = !matches!(state, Some(State::Free) | None);
            let mut row = vec![json!({ "type": "text", "text": what })];
            if freeable {
                row.push(button("Free", "free", json!({ "name": record.name.replace(':', "/") })));
            }
            items.push(json!({ "type": "row", "children": row }));
        }
        Ok(items)
    }

    fn act(&self, _page: &str, event: &str, input: &Value, admin: &str, now: i64) -> Result<Option<String>, String> {
        let failed = |error: anyhow::Error| {
            tracing::error!(error = %format!("{error:#}"), "an admin page's store call failed");
            "Something went wrong here; try again.".to_string()
        };
        let name = self.name_from(&json!({ "name": input["name"].as_str().unwrap_or_default().replace('/', ":") }))?;
        let key = name.as_did_path();
        let store = &self.host.store;
        match event {
            "assign" => {
                let holder = input["holder"].as_str().map(str::trim).filter(|holder| !holder.is_empty());
                if holder.is_some_and(|holder| !holder.starts_with("did:")) {
                    return Err("For: a DID, or nothing.".into());
                }
                let record = store.get(&key).map_err(failed)?;
                if matches!(self.host.state_of(&name, record.as_ref(), now), State::Published | State::Stale | State::Deactivated) {
                    return Err("A DID is hosted under that name. Free it first.".into());
                }
                store.assign(&key, holder).map_err(failed)?;
                tracing::info!(name = %key, holder = holder.unwrap_or("nobody"), by = %admin, "assigned from an admin page");
                Ok(Some(format!("{name} is set aside for {}.", holder.map_or("nobody".to_string(), short))))
            }
            "free" => {
                let was = store.get(&key).map_err(failed)?.and_then(|record| record.did);
                store.free(&key).map_err(failed)?;
                tracing::info!(name = %key, did = was.as_deref().unwrap_or("-"), by = %admin, "freed from an admin page");
                Ok(Some(format!("{name} is free{}.", was.map(|did| format!(" (it was {})", short(&did))).unwrap_or_default())))
            }
            _ => Err("That can't be done here.".into()),
        }
    }
}
