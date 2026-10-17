//! The service's admin pages (wyvrn_service::admin): every pack, to take one
//! down, and the names no pack may take. Only `STICKERS_ADMINS` are let in.

use serde_json::{json, Value};
use wyvrn_service::admin::{button, Page, Pages};

use crate::server::AppState;
use crate::store::pack_scope;

pub const PACKS: &str = "admin-stickers-packs";
pub const NAMES: &str = "admin-stickers-names";

fn short(did: &str) -> String {
    if did.chars().count() <= 48 { did.to_string() } else { format!("{}…{}", did.chars().take(28).collect::<String>(), did.chars().rev().take(12).collect::<String>().chars().rev().collect::<String>()) }
}

/// Names typed one to a line, or separated by commas or spaces.
fn names_in(text: &str) -> Vec<String> {
    let mut names: Vec<String> = text.split(|c: char| c == ',' || c.is_whitespace()).map(|name| name.trim().to_lowercase()).filter(|name| !name.is_empty()).collect();
    names.sort();
    names.dedup();
    names
}

impl Pages for AppState {
    fn pages(&self) -> Vec<Page> {
        vec![
            Page { id: PACKS, title: "Stickers: every pack", summary: "Every pack hosted here and who made it, to take one down.", events: &["take-down"] },
            Page { id: NAMES, title: "Stickers: names set aside", summary: "The short names no pack may take.", events: &["save", "reset"] },
        ]
    }

    fn view(&self, page: &str, _admin: &str, _now: i64) -> anyhow::Result<Vec<Value>> {
        let store = &self.processor.store;
        Ok(match page {
            PACKS => {
                let packs = store.packs(None)?;
                if packs.is_empty() {
                    return Ok(vec![json!({ "type": "text", "text": "No packs yet." })]);
                }
                let mut items = Vec::new();
                for pack in packs.iter().rev() {
                    let count = store.stickers(&pack_scope(&pack.name))?.len();
                    let what = format!("{} ({}) · {count} {} · by {}", pack.title, pack.name, pack.kind.as_str(), short(&pack.owner));
                    items.push(json!({ "type": "row", "children": [
                        { "type": "text", "text": what },
                        if pack.removed { json!({ "type": "badge", "label": "Taken down" }) } else { button("Take down", "take-down", json!({ "pack": pack.name })) },
                    ]}));
                }
                items
            }
            NAMES => {
                let own = store.reserved_names()?.is_some();
                vec![
                    json!({ "type": "text", "text": if own { "Set here. Going back puts the settings' list in its place." } else { "As the settings have them." } }),
                    json!({ "type": "form", "children": [
                        { "type": "text-area", "form-id": "names", "label": "Names set aside, one to a line", "value": self.processor.reserved_names().join("\n") },
                        button("Save", "save", json!({})),
                    ]}),
                    button("Go back to the settings' list", "reset", json!({})),
                ]
            }
            _ => Vec::new(),
        })
    }

    fn act(&self, page: &str, event: &str, input: &Value, admin: &str, now: i64) -> Result<Option<String>, String> {
        let store = &self.processor.store;
        let failed = |error: anyhow::Error| {
            tracing::error!(error = %format!("{error:#}"), "an admin page's store call failed");
            "Something went wrong here; try again.".to_string()
        };
        match (page, event) {
            (PACKS, "take-down") => {
                let name = input["pack"].as_str().unwrap_or_default();
                if !store.remove_pack(name, now).map_err(failed)? {
                    return Err("That pack is already down.".into());
                }
                tracing::info!(pack = %name, %admin, "taken down by an admin");
                Ok(Some(format!("Took {name} down. Its name stays taken.")))
            }
            (NAMES, "save") => {
                let names = names_in(input["names"].as_str().unwrap_or_default());
                if let Some(bad) = names.iter().find(|name| name.len() > 32) {
                    return Err(format!("{bad} is longer than any pack's name could be."));
                }
                store.set_reserved_names(Some(&names)).map_err(failed)?;
                Ok(Some(format!("Saved: {} names.", names.len())))
            }
            (NAMES, "reset") => {
                store.set_reserved_names(None).map_err(failed)?;
                Ok(Some("Back to the settings' list.".into()))
            }
            _ => Err("That can't be done here.".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_read_however_they_are_typed() {
        assert_eq!(names_in("Admin, help\nsupport  help"), ["admin", "help", "support"]);
    }
}
