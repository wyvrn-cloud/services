//! The workflows this service offers, as `workflow/1.0` templates: what
//! `discover` lists and `fetch-template` returns, and what the processor itself
//! runs from. Each is a state machine and, for every state, the Workflow UI 1.0
//! items a maker's app draws there.
//!
//! They follow the conversation Telegram's sticker bot has: name the pack, send
//! stickers one at a time each with its emoji, publish, choose an icon or skip,
//! choose the short name its link will have.
//!
//! What a transition *does* is an action of this service's own, named under its
//! namespace, as the protocol provides for local actions; [`crate::processor`] is
//! where each is carried out.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const NEW_STICKER_PACK: &str = "new-sticker-pack";
pub const NEW_EMOJI_PACK: &str = "new-emoji-pack";
pub const EDIT_PACK: &str = "edit-pack";
pub const VERSION: &str = "1.0.0";

/// The namespace of this service's own actions.
const ACTIONS: &str = "https://wyvrn.app/stickers/1.0/actions/";

fn action(name: &str) -> Value {
    json!({ "typeURI": format!("{ACTIONS}{name}@1") })
}

fn form(children: Value) -> Value {
    json!({ "type": "form", "children": children })
}

/// The states and screens of making a pack, which differ for stickers and custom
/// emoji only in what each picture is known by.
fn new_pack(id: &str, emoji_pack: bool) -> Value {
    let (noun, plural) = if emoji_pack { ("emoji", "emoji") } else { ("sticker", "stickers") };
    let add_event = if emoji_pack { "add_emoji" } else { "add_sticker" };
    let known_by = if emoji_pack {
        json!({ "type": "text-input", "form-id": "shortcode", "label": "Shortcode", "placeholder": "party_cat", "required": true })
    } else {
        json!({ "type": "text-input", "form-id": "emoji", "label": "Emoji", "placeholder": "😺", "required": true })
    };
    let asking = if emoji_pack {
        "Send me a picture: a PNG, WebP or GIF, up to {{ artifacts.max_px }} by {{ artifacts.max_px }} pixels. Then give it a shortcode, which is what people type between colons to use it."
    } else {
        "Send me a sticker: a PNG, WebP or GIF picture, up to {{ artifacts.max_px }} by {{ artifacts.max_px }} pixels. A transparent background looks best. Then tell me the emoji it stands for."
    };
    json!({
        "id": id,
        "version": VERSION,
        "name": if emoji_pack { "New custom emoji pack" } else { "New sticker pack" },
        "instance_policy": { "mode": "singleton_per_connection" },
        "states": {
            "title": { "final": false },
            "stickers": { "final": false },
            "icon": { "final": false },
            "name": { "final": false },
            "published": { "final": true },
        },
        "transitions": {
            "set_title": { "from": "title", "to": "stickers", "guard": null, "action": "set_title" },
            add_event: { "from": "stickers", "to": "stickers", "guard": null, "action": "add" },
            "remove_sticker": { "from": "stickers", "to": "stickers", "guard": null, "action": "remove" },
            "publish": { "from": "stickers", "to": "icon", "guard": "artifacts.count", "action": null },
            "set_icon": { "from": "icon", "to": "name", "guard": null, "action": "set_icon" },
            "skip_icon": { "from": "icon", "to": "name", "guard": null, "action": null },
            "set_name": { "from": "name", "to": "published", "guard": null, "action": "publish" },
        },
        "actions": {
            "set_title": action("set-title"),
            "add": action("add"),
            "remove": action("remove"),
            "set_icon": action("set-icon"),
            "publish": action("publish"),
        },
        "catalog": {},
        "display_hints": {
            "ui_version": "1.0",
            "profiles": { "receiver": { "states": {
                "title": [
                    { "type": "title", "text": if emoji_pack { "A new custom emoji pack" } else { "A new sticker pack" } },
                    { "type": "text", "text": "What would you like to call it?" },
                    form(json!([
                        { "type": "text-input", "form-id": "title", "label": "Pack title", "placeholder": "Cats being cats", "required": true },
                        { "type": "submit-button", "label": "Next", "event": "set_title" },
                    ])),
                ],
                "stickers": [
                    { "type": "title", "text": "{{ context.title }}" },
                    { "type": "text", "text": asking },
                    {
                        "type": "list",
                        "showWhen": "artifacts.count",
                        "bind": "artifacts.stickers",
                        "row_template": { "type": "row", "children": [
                            { "type": "image", "url": "{{ item.url }}", "col": 3 },
                            { "type": "text", "text": "{{ item.label }}", "col": 5 },
                            { "type": "submit-button", "label": "Remove", "event": "remove_sticker", "input": { "sticker": "{{ item.id }}" }, "col": 4 },
                        ]},
                    },
                    { "type": "text", "text": "{{ artifacts.count }} of {{ artifacts.limit }}.", "showWhen": "artifacts.count" },
                    {
                        "type": "form",
                        "showWhen": "!artifacts.full",
                        "children": [
                            { "type": "photo", "form-id": "image", "label": if emoji_pack { "Picture" } else { "Sticker" } },
                            known_by,
                            { "type": "submit-button", "label": format!("Add this {noun}"), "event": add_event },
                        ],
                    },
                    { "type": "information", "text": format!("That is as many {plural} as a pack holds."), "showWhen": "artifacts.full" },
                    { "type": "submit-button", "label": "Publish the pack", "event": "publish", "showWhen": "artifacts.count" },
                ],
                "icon": [
                    { "type": "title", "text": "An icon for the pack" },
                    { "type": "text", "text": format!("Which of the {plural} should stand for the pack? If you skip this, the first one does.") },
                    {
                        "type": "list",
                        "bind": "artifacts.stickers",
                        "row_template": { "type": "tile", "title": "{{ item.label }}", "image": "{{ item.url }}", "event": "set_icon", "input": { "sticker": "{{ item.id }}" } },
                    },
                    { "type": "submit-button", "label": "Skip", "event": "skip_icon" },
                ],
                "name": [
                    { "type": "title", "text": "A short name" },
                    { "type": "text", "text": "Choose a short name for the pack. It is its address, {{ artifacts.base }}<name>, so it is first come, first served: 3 to 32 lowercase letters, digits and hyphens." },
                    form(json!([
                        { "type": "text-input", "form-id": "name", "label": "Short name", "placeholder": "cats-being-cats", "required": true },
                        { "type": "check-box", "form-id": "private", "label": "Keep it to myself: other people see what I send from it, but can't add the pack" },
                        { "type": "submit-button", "label": "Publish", "event": "set_name" },
                    ])),
                ],
                "published": [
                    { "type": "pass", "text": "Kaboom! Your pack is published." },
                    { "type": "text", "text": "Anyone you give this to can see it:" },
                    { "type": "web-link", "label": "{{ artifacts.link }}", "url": "{{ artifacts.link }}" },
                ],
            }}},
        },
    })
}

fn edit_pack() -> Value {
    json!({
        "id": EDIT_PACK,
        "version": VERSION,
        "name": "Change one of your packs",
        "instance_policy": { "mode": "singleton_per_connection" },
        "states": {
            "choosing": { "final": false },
            "editing": { "final": false },
            "deleted": { "final": true },
            "done": { "final": true },
        },
        "transitions": {
            "choose": { "from": "choosing", "to": "editing", "guard": null, "action": "choose" },
            "close": { "from": "choosing", "to": "done", "guard": null, "action": null },
            "add_sticker": { "from": "editing", "to": "editing", "guard": "!artifacts.is_emoji", "action": "add" },
            "add_emoji": { "from": "editing", "to": "editing", "guard": "artifacts.is_emoji", "action": "add" },
            "remove_sticker": { "from": "editing", "to": "editing", "guard": null, "action": "remove" },
            "set_title": { "from": "editing", "to": "editing", "guard": null, "action": "set_title" },
            "set_icon": { "from": "editing", "to": "editing", "guard": null, "action": "set_icon" },
            "set_private": { "from": "editing", "to": "editing", "guard": null, "action": "set_private" },
            "delete_pack": { "from": "editing", "to": "deleted", "guard": null, "action": "delete" },
            "back": { "from": "editing", "to": "choosing", "guard": null, "action": null },
            "finish": { "from": "editing", "to": "done", "guard": null, "action": null },
        },
        "actions": {
            "choose": action("choose"),
            "add": action("add"),
            "remove": action("remove"),
            "set_title": action("set-title"),
            "set_icon": action("set-icon"),
            "set_private": action("set-private"),
            "delete": action("delete"),
        },
        "catalog": {},
        "display_hints": {
            "ui_version": "1.0",
            "profiles": { "receiver": { "states": {
                "choosing": [
                    { "type": "title", "text": "Your packs" },
                    { "type": "information", "text": "You haven't published a pack yet.", "showWhen": "!artifacts.packs" },
                    { "type": "text", "text": "Which one would you like to change?", "showWhen": "artifacts.packs" },
                    {
                        "type": "list",
                        "bind": "artifacts.packs",
                        "row_template": { "type": "tile", "title": "{{ item.title }}", "subtitle": "{{ item.summary }}", "image": "{{ item.icon }}", "event": "choose", "input": { "pack": "{{ item.name }}" } },
                    },
                    { "type": "submit-button", "label": "Done", "event": "close" },
                ],
                "editing": [
                    { "type": "title", "text": "{{ artifacts.title }}" },
                    { "type": "web-link", "label": "{{ artifacts.link }}", "url": "{{ artifacts.link }}" },
                    {
                        "type": "list",
                        "bind": "artifacts.stickers",
                        "row_template": { "type": "row", "children": [
                            { "type": "image", "url": "{{ item.url }}", "col": 2 },
                            { "type": "text", "text": "{{ item.label }}", "col": 3 },
                            { "type": "submit-button", "label": "Make icon", "event": "set_icon", "input": { "sticker": "{{ item.id }}" }, "col": 4 },
                            { "type": "submit-button", "label": "Remove", "event": "remove_sticker", "input": { "sticker": "{{ item.id }}" }, "col": 3 },
                        ]},
                    },
                    { "type": "text", "text": "{{ artifacts.count }} of {{ artifacts.limit }}." },
                    {
                        "type": "form",
                        "showWhen": "artifacts.can_add_sticker",
                        "children": [
                            { "type": "photo", "form-id": "image", "label": "Sticker" },
                            { "type": "text-input", "form-id": "emoji", "label": "Emoji", "placeholder": "😺", "required": true },
                            { "type": "submit-button", "label": "Add this sticker", "event": "add_sticker" },
                        ],
                    },
                    {
                        "type": "form",
                        "showWhen": "artifacts.can_add_emoji",
                        "children": [
                            { "type": "photo", "form-id": "image", "label": "Picture" },
                            { "type": "text-input", "form-id": "shortcode", "label": "Shortcode", "placeholder": "party_cat", "required": true },
                            { "type": "submit-button", "label": "Add this emoji", "event": "add_emoji" },
                        ],
                    },
                    { "type": "divider" },
                    form(json!([
                        { "type": "text-input", "form-id": "title", "label": "Pack title", "placeholder": "{{ artifacts.title }}", "required": true },
                        { "type": "submit-button", "label": "Rename", "event": "set_title" },
                    ])),
                    { "type": "submit-button", "label": "Keep it to myself", "event": "set_private", "input": { "private": true }, "showWhen": "artifacts.reuse" },
                    { "type": "submit-button", "label": "Let other people add it", "event": "set_private", "input": { "private": false }, "showWhen": "!artifacts.reuse" },
                    { "type": "divider" },
                    { "type": "warning", "text": "Deleting a pack removes its pictures for everyone, and its short name can't be used again. To delete it, type its short name." },
                    form(json!([
                        { "type": "text-input", "form-id": "name", "label": "Short name", "placeholder": "{{ artifacts.name }}", "required": true },
                        { "type": "submit-button", "label": "Delete this pack", "event": "delete_pack" },
                    ])),
                    { "type": "divider" },
                    { "type": "submit-button", "label": "Another pack", "event": "back" },
                    { "type": "submit-button", "label": "Done", "event": "finish" },
                ],
                "deleted": [{ "type": "pass", "text": "The pack is deleted." }],
                "done": [{ "type": "pass", "text": "All done." }],
            }}},
        },
    })
}

/// Every template, in the order `discover` lists them.
pub fn all() -> Vec<Value> {
    vec![new_pack(NEW_STICKER_PACK, false), new_pack(NEW_EMOJI_PACK, true), edit_pack()]
}

pub fn find(id: &str) -> Option<Value> {
    all().into_iter().find(|template| template["id"] == id)
}

/// Objects with their keys in order, all the way down: the "stable JSON" a
/// template's hash is taken over.
fn stable(value: &Value) -> Value {
    match value {
        Value::Object(fields) => {
            let mut keys: Vec<&String> = fields.keys().collect();
            keys.sort();
            Value::Object(keys.into_iter().map(|key| (key.clone(), stable(&fields[key]))).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(stable).collect()),
        other => other.clone(),
    }
}

/// A template's hash: SHA-256 over its JSON with keys sorted, in hex.
pub fn hash(template: &Value) -> String {
    Sha256::digest(stable(template).to_string().as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Where an instance of `template` starts: the first state it lists. (The
/// protocol's template has no field for it.)
pub fn initial_state(template: &Value) -> &str {
    template["states"].as_object().and_then(|states| states.keys().next()).map_or("", String::as_str)
}

pub fn is_final(template: &Value, state: &str) -> bool {
    template["states"][state]["final"] == true
}

/// The events that leave `state`, in the order the template gives them.
pub fn events_from(template: &Value, state: &str) -> Vec<String> {
    template["transitions"].as_object().into_iter().flatten().filter(|(_, transition)| transition["from"] == state).map(|(event, _)| event.clone()).collect()
}

/// What a template's display hints say to draw in `state`.
pub fn hints<'a>(template: &'a Value, state: &str) -> &'a Value {
    &template["display_hints"]["profiles"]["receiver"]["states"][state]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_is_a_whole_state_machine() {
        for template in all() {
            let id = template["id"].as_str().unwrap();
            let states = template["states"].as_object().unwrap();
            assert!(!is_final(&template, initial_state(&template)), "{id} starts where it ends");
            assert!(states.values().any(|state| state["final"] == true), "{id} never ends");
            for (event, transition) in template["transitions"].as_object().unwrap() {
                for end in ["from", "to"] {
                    assert!(states.contains_key(transition[end].as_str().unwrap()), "{id}: {event} {end}");
                }
                // An action a transition names is one the template defines.
                if let Some(action) = transition["action"].as_str() {
                    assert!(template["actions"][action]["typeURI"].as_str().is_some_and(|uri| uri.starts_with(ACTIONS)), "{id}: {event} -> {action}");
                }
            }
            // Every state has something to draw, and a final state nothing to press.
            for (state, definition) in states {
                let drawn = hints(&template, state);
                assert!(drawn.as_array().is_some_and(|items| !items.is_empty()), "{id}: nothing to draw in {state}");
                if definition["final"] == true {
                    assert!(events_from(&template, state).is_empty(), "{id}: {state} is final and has a way out");
                    assert!(!drawn.to_string().contains("\"event\""), "{id}: {state} is final and offers an event");
                }
            }
            // Every event a screen offers is one that leaves the state it is drawn in.
            for state in states.keys() {
                let offered = events_from(&template, state);
                let text = hints(&template, state).to_string();
                for (at, _) in text.match_indices("\"event\":\"") {
                    let event = text[at + 9..].split('"').next().unwrap();
                    assert!(offered.iter().any(|e| e == event), "{id}: {state} offers {event}, which doesn't leave it");
                }
            }
        }
    }

    #[test]
    fn a_hash_is_of_what_the_template_says_not_how_it_is_written() {
        let a = json!({ "id": "x", "states": { "a": {}, "b": {} } });
        let b = json!({ "states": { "b": {}, "a": {} }, "id": "x" });
        assert_eq!(hash(&a), hash(&b));
        assert_ne!(hash(&a), hash(&json!({ "id": "y", "states": { "a": {}, "b": {} } })));
        assert_eq!(hash(&a).len(), 64);
    }

    #[test]
    fn the_two_kinds_of_pack_differ_in_what_a_picture_is_known_by() {
        let stickers = find(NEW_STICKER_PACK).unwrap();
        let emoji = find(NEW_EMOJI_PACK).unwrap();
        assert_eq!(initial_state(&stickers), "title");
        assert_eq!(events_from(&stickers, "stickers"), ["add_sticker", "remove_sticker", "publish"]);
        assert!(events_from(&emoji, "stickers").contains(&"add_emoji".to_string()));
        assert!(hints(&stickers, "stickers").to_string().contains("\"form-id\":\"emoji\""));
        assert!(hints(&emoji, "stickers").to_string().contains("\"form-id\":\"shortcode\""));
    }
}
