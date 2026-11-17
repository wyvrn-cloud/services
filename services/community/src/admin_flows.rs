//! Managing roles, as workflows the server runs (community/1.0's "The role
//! workflows"; didcomm.org's workflow/1.0, the server the Processor): `roles`
//! (the roles themselves, and handing the community on), `member-roles` (one
//! member's roles) and `channel-permissions` (what a channel allows and denies
//! each role). Each step is drawn from the Workflow UI 1.0 items its `status`
//! carries, so an app that can show any workflow can show these; every change is
//! made as the step that makes it is advanced, and held to rank.

use serde_json::{json, Value};

use crate::community::{is_emoji, line, Community};
use crate::questions::{failed, Answer, Refusal};
use crate::roles::{Powers, CHANNEL_PERMISSIONS, COMMUNITY_PERMISSIONS, EVERYONE_ROLE};
use crate::store::{Channel, Flow, Member, RoleDef};
use crate::discord::DISCORD_IMPORT_FLOW;
use crate::webhooks::WEBHOOKS_FLOW;
use crate::web::flows::{SITES_FLOW, WEB_FLOW};

pub const ROLES: &str = "roles";
pub const MEMBER_ROLES: &str = "member-roles";
pub const CHANNEL_PERMISSIONS_FLOW: &str = "channel-permissions";
pub const AUTOMOD_FLOW: &str = "automod";
pub const JOIN_GATES_FLOW: &str = "join-gates";
pub const CHANNELS_FLOW: &str = "channels";
pub const ONBOARDING_SETUP_FLOW: &str = "onboarding-setup";
pub const ONBOARDING_FLOW: &str = "onboarding";
pub const VERSION: &str = "1.0";
const P: &str = "https://didcomm.org/workflow/1.0/";
pub(crate) const MAX_ROLES: usize = 250;
const MAX_ROLE_NAME: usize = 100;

/// Each template: its id, title, states and the events that move between them.
fn templates() -> [(&'static str, &'static str, Value); 12] {
    [
        (
            ROLES,
            "Roles",
            json!({
                "states": [{ "name": "list", "type": "start" }, { "name": "edit", "type": "normal" }, { "name": "done", "type": "final" }],
                "transitions": {
                    "edit": { "from": "list", "to": "edit" }, "create": { "from": "list", "to": "edit" },
                    "raise": { "from": "list", "to": "list" }, "lower": { "from": "list", "to": "list" },
                    "hand_over": { "from": "list", "to": "list" }, "done": { "from": "list", "to": "done" },
                    "save": { "from": "edit", "to": "list" }, "delete": { "from": "edit", "to": "list" }, "back": { "from": "edit", "to": "list" }
                }
            }),
        ),
        (
            MEMBER_ROLES,
            "A member's roles",
            json!({
                "states": [{ "name": "pick", "type": "start" }, { "name": "done", "type": "final" }],
                "transitions": { "save": { "from": "pick", "to": "done" } }
            }),
        ),
        (
            CHANNEL_PERMISSIONS_FLOW,
            "A channel's permissions",
            json!({
                "states": [{ "name": "roles", "type": "start" }, { "name": "role", "type": "normal" }, { "name": "done", "type": "final" }],
                "transitions": {
                    "read_only": { "from": "roles", "to": "roles" }, "unlock": { "from": "roles", "to": "roles" }, "private": { "from": "roles", "to": "roles" }, "public": { "from": "roles", "to": "roles" },
                    "edit": { "from": "roles", "to": "role" }, "done": { "from": "roles", "to": "done" },
                    "save": { "from": "role", "to": "roles" }, "back": { "from": "role", "to": "roles" }
                }
            }),
        ),
        (
            AUTOMOD_FLOW,
            "AutoMod",
            json!({
                "states": [{ "name": "list", "type": "start" }, { "name": "edit", "type": "normal" }, { "name": "done", "type": "final" }],
                "transitions": {
                    "edit": { "from": "list", "to": "edit" }, "create": { "from": "list", "to": "edit" },
                    "preset": { "from": "list", "to": "edit" },
                    "toggle": { "from": "list", "to": "list" }, "done": { "from": "list", "to": "done" },
                    "save": { "from": "edit", "to": "list" }, "delete": { "from": "edit", "to": "list" }, "back": { "from": "edit", "to": "list" }
                }
            }),
        ),
        (
            JOIN_GATES_FLOW,
            "Join gates",
            json!({
                "states": [{ "name": "form", "type": "start" }, { "name": "done", "type": "final" }],
                "transitions": { "save": { "from": "form", "to": "done" } }
            }),
        ),
        (
            CHANNELS_FLOW,
            "Channels and categories",
            json!({
                "states": [{ "name": "list", "type": "start" }, { "name": "category", "type": "normal" }, { "name": "move", "type": "normal" }, { "name": "done", "type": "final" }],
                "transitions": {
                    "up": { "from": "list", "to": "list" }, "down": { "from": "list", "to": "list" },
                    "create_category": { "from": "list", "to": "list" }, "edit_category": { "from": "list", "to": "category" },
                    "category_up": { "from": "list", "to": "list" }, "category_down": { "from": "list", "to": "list" },
                    "move": { "from": "list", "to": "move" }, "done": { "from": "list", "to": "done" },
                    "save": { "from": "category", "to": "list" }, "delete": { "from": "category", "to": "list" }, "back": { "from": ["category", "move"], "to": "list" },
                    "to": { "from": "move", "to": "list" }
                }
            }),
        ),
        (
            ONBOARDING_SETUP_FLOW,
            "Onboarding",
            json!({
                "states": [{ "name": "form", "type": "start" }, { "name": "done", "type": "final" }],
                "transitions": { "save": { "from": "form", "to": "done" } }
            }),
        ),
        (
            ONBOARDING_FLOW,
            "Welcome",
            json!({
                "states": [{ "name": "welcome", "type": "start" }, { "name": "roles", "type": "normal" }, { "name": "channels", "type": "normal" }, { "name": "done", "type": "final" }],
                "transitions": {
                    "next": { "from": "welcome", "to": "roles" }, "pick": { "from": "roles", "to": "channels" }, "done": { "from": "channels", "to": "done" }
                }
            }),
        ),
        (
            WEB_FLOW,
            "On the web",
            json!({
                "states": [{ "name": "form", "type": "start" }, { "name": "add", "type": "normal" }, { "name": "done", "type": "final" }],
                "transitions": {
                    "save": { "from": "form", "to": "form" }, "add": { "from": "form", "to": "add" }, "remove": { "from": "form", "to": "form" }, "done": { "from": "form", "to": "done" },
                    "create": { "from": "add", "to": "form" }, "back": { "from": "add", "to": "form" }
                }
            }),
        ),
        (
            SITES_FLOW,
            "Websites you sign in to",
            json!({
                "states": [{ "name": "list", "type": "start" }, { "name": "done", "type": "final" }],
                "transitions": { "revoke": { "from": "list", "to": "list" }, "done": { "from": "list", "to": "done" } }
            }),
        ),
        (
            DISCORD_IMPORT_FLOW,
            "Import a Discord template",
            json!({
                "states": [{ "name": "link", "type": "start" }, { "name": "review", "type": "normal" }, { "name": "done", "type": "final" }],
                "transitions": { "read": { "from": "link", "to": "review" }, "back": { "from": "review", "to": "link" }, "import": { "from": "review", "to": "done" } }
            }),
        ),
        (
            WEBHOOKS_FLOW,
            "Webhooks",
            json!({
                "states": [{ "name": "list", "type": "start" }, { "name": "edit", "type": "normal" }, { "name": "done", "type": "final" }],
                "transitions": {
                    "create": { "from": "list", "to": "edit" }, "edit": { "from": "list", "to": "edit" }, "done": { "from": "list", "to": "done" },
                    "save": { "from": "edit", "to": "edit" }, "new_url": { "from": "edit", "to": "edit" }, "delete": { "from": "edit", "to": "list" }, "back": { "from": "edit", "to": "list" }
                }
            }),
        ),
    ]
}

/// A template as `fetch-template` hands it over.
pub fn template(id: &str) -> Option<Value> {
    templates().into_iter().find(|(known, _, _)| *known == id).map(|(id, title, machine)| {
        json!({
            "id": id,
            "name": title,
            "version": VERSION,
            "instance_policy": { "mode": "multi_per_connection", "multiplicity_key": "context" },
            "states": machine["states"],
            "transitions": machine["transitions"],
            "display_hints": { "profiles": { "receiver": { "states": {} } } }
        })
    })
}

pub fn is_template(id: &str) -> bool {
    templates().iter().any(|(known, _, _)| *known == id)
}

/// The capability (workflow/1.0's `status` `capabilities`) an app says it has to
/// draw a `sortable-list` (Workflow UI's sortable list, wyvrn-protocols'
/// `workflow-ui-sortable-list/1.0`).
pub const SORTABLE_LIST: &str = "ui:sortable-list";

/// Whether the app running `flow` said it can draw `capability`.
/// Workflow UI Layout's tabs (wyvrn-protocols' profiles/workflow-ui-layout/1.0).
pub const TABS: &str = "ui:tabs";

fn can_draw(flow: &Flow, capability: &str) -> bool {
    flow.data["capabilities"].as_array().is_some_and(|all| all.iter().any(|c| c == capability))
}

fn button(label: &str, event: &str, input: Value) -> Value {
    json!({ "type": "submit-button", "label": label, "event": event, "input": input })
}

fn ticked(value: &Value) -> bool {
    match value {
        Value::Bool(ticked) => *ticked,
        Value::String(text) => matches!(text.as_str(), "true" | "on" | "yes" | "1"),
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        _ => false,
    }
}

fn label_of(permission: &str) -> &'static str {
    COMMUNITY_PERMISSIONS.iter().chain(CHANNEL_PERMISSIONS).find(|(name, _)| *name == permission).map_or("", |(_, label)| *label)
}

fn refused(comment: &str) -> Refusal {
    Refusal::new("guard_failed", comment)
}

fn not_allowed(comment: &str) -> Refusal {
    Refusal::new("not_authorized", comment)
}

/// What a role's colour field holds, if it is one: `#rrggbb`, lowercase.
fn color(value: &Value) -> Result<Option<String>, Refusal> {
    let text = value.as_str().unwrap_or_default().trim().to_lowercase();
    if text.is_empty() {
        return Ok(None);
    }
    let hex = text.strip_prefix('#').unwrap_or(&text);
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(Some(format!("#{hex}")))
    } else {
        Err(refused("A colour is written #rrggbb, as #3498db."))
    }
}

impl Community {
    /// The administration workflows `did` may start, each `(id, title)`.
    pub fn flows_for(&self, did: &str) -> anyhow::Result<Vec<(&'static str, &'static str)>> {
        let powers = self.powers(did)?;
        let opened_one = self.store.channels()?.iter().any(|channel| channel.created_by == did);
        Ok(templates()
            .into_iter()
            .filter(|(id, _, _)| match *id {
                ROLES | MEMBER_ROLES => powers.has("manage_roles"),
                AUTOMOD_FLOW => powers.has("manage_automod"),
                JOIN_GATES_FLOW | ONBOARDING_SETUP_FLOW => powers.has("manage_community"),
                CHANNELS_FLOW => powers.has("manage_channels"),
                ONBOARDING_FLOW => true,
                WEB_FLOW => self.web_url.is_some() && powers.has("manage_community"),
                SITES_FLOW => self.web_url.is_some(),
                DISCORD_IMPORT_FLOW => self.may_import(did).unwrap_or(false),
                WEBHOOKS_FLOW => self.may_manage_webhooks(did).unwrap_or(false),
                _ => powers.has("manage_channels") || opened_one,
            })
            .map(|(id, title, _)| (id, title))
            .collect())
    }

    fn role_name_taken(&self, name: &str, except: &str) -> anyhow::Result<bool> {
        Ok(self.store.roles()?.iter().any(|role| role.id != except && role.name.to_lowercase() == name.to_lowercase()))
    }

    /// Whether `powers` may manage `channel`'s permissions: `manage_channels`, or
    /// having opened it.
    fn may_manage_channel(&self, did: &str, powers: &Powers, channel: &Channel) -> bool {
        powers.has("manage_channels") || channel.created_by == did
    }

    /// What the permissions workflow sets: a channel's, or (with `category` in its
    /// context) a category's, which its synced channels follow.
    fn flow_target(&self, flow: &Flow) -> Result<Target, Refusal> {
        if let Some(id) = flow.context["category"].as_str() {
            let category = self.store.categories().map_err(failed)?.into_iter().find(|c| c.id == id).ok_or_else(|| Refusal::new("instance_not_found", "That category is gone."))?;
            return Ok(Target { key: crate::roles::category_key(&category.id), title: format!("the category {}", category.name), voice: false, channel: None, category: Some(category) });
        }
        let channel = self.flow_channel(flow)?;
        let category = match &channel.category {
            Some(id) => self.store.categories().map_err(failed)?.into_iter().find(|c| c.id == *id),
            None => None,
        };
        let title = if channel.voice.is_some() { format!("🔊{}", channel.name) } else { format!("#{}", channel.name) };
        Ok(Target { key: channel.id.clone(), title, voice: channel.voice.is_some(), channel: Some(channel), category })
    }

    /// What `owner` may set others' permissions to on `target`: what they hold there.
    fn target_held(&self, owner: &str, powers: &Powers, target: &Target) -> Result<std::collections::BTreeSet<String>, Refusal> {
        match &target.channel {
            Some(channel) => self.channel_permissions(owner, powers, channel).map_err(failed),
            None => Ok(CHANNEL_PERMISSIONS.iter().map(|(name, _)| name.to_string()).filter(|name| powers.has(name)).collect()),
        }
    }

    /// Sets `role`'s permissions on `target`; on a category, its synced channels too.
    fn set_target_override(&self, target: &Target, role: &str, allow: &[String], deny: &[String]) -> Result<(), Refusal> {
        match (&target.channel, &target.category) {
            (None, Some(category)) => self.set_category_override(&category.id, role, allow, deny).map_err(failed),
            _ => self.store.set_override(&target.key, role, allow, deny).map_err(failed),
        }
    }

    fn flow_channel(&self, flow: &Flow) -> Result<Channel, Refusal> {
        let id = flow.context["channel"].as_str().unwrap_or_default();
        self.store.channel(id).map_err(failed)?.ok_or_else(|| Refusal::new("instance_not_found", "That channel is gone."))
    }

    fn flow_member(&self, flow: &Flow) -> Result<Member, Refusal> {
        let did = flow.context["member"].as_str().unwrap_or_default();
        self.store.member(did).map_err(failed)?.ok_or_else(|| Refusal::new("instance_not_found", "They are no longer a member."))
    }

    // --- What each step shows ------------------------------------------------------

    fn roles_list_ui(&self, flow: &Flow, powers: &Powers) -> anyhow::Result<(Value, Vec<&'static str>)> {
        let roles = self.store.roles()?;
        let held = self.store.all_member_roles()?;
        let mut items = vec![json!({ "type": "title", "text": "Roles" })];
        items.push(json!({ "type": "text", "text": "Members hold every permission of every role they have. Higher roles outrank lower ones: you can change only the roles ranked below your own, and act only on members ranked below you." }));
        if let Some(note) = flow.data["note"].as_str() {
            items.push(json!({ "type": "pass", "text": note }));
        }
        let lowest_movable = roles.iter().filter(|role| role.id != EVERYONE_ROLE).map(|role| role.position).min().unwrap_or(0);
        let sortable = can_draw(flow, SORTABLE_LIST);
        let ranked: Vec<&RoleDef> = roles.iter().filter(|role| role.id != EVERYONE_ROLE).collect();
        if sortable && ranked.iter().any(|role| powers.above(role.position)) {
            // Highest first; a role this person doesn't outrank stays where it is.
            let entries: Vec<Value> = ranked
                .iter()
                .map(|role| {
                    let mut entry = json!({ "value": role.id, "label": role.name });
                    if !powers.above(role.position) {
                        entry["locked"] = json!(true);
                    }
                    entry
                })
                .collect();
            items.push(json!({ "type": "form", "children": [
                { "type": "sortable-list", "form-id": "order", "label": "Rank: drag a role higher or lower (everyone is always last)", "items": entries },
                button("Save the order", "reorder", json!({}))
            ] }));
        }
        for role in &roles {
            let count = if role.id == EVERYONE_ROLE { None } else { Some(held.iter().filter(|(_, id)| *id == role.id).count()) };
            let mut text = match (&role.icon, count) {
                (Some(icon), Some(n)) => format!("{icon} {} · {n}", role.name),
                (None, Some(n)) => format!("{} · {n}", role.name),
                _ => format!("{} (every member)", role.name),
            };
            if let Some(color) = &role.color {
                text = format!("{text}  {color}");
            }
            let mut row = vec![json!({ "type": "text", "text": text })];
            if powers.above(role.position) || (role.id == EVERYONE_ROLE && powers.has("manage_roles") && (powers.owner || powers.rank > 0)) {
                row.push(button("Edit", "edit", json!({ "role": role.id })));
                // (Dragged into order, below, where the app draws a sortable list.)
                if role.id != EVERYONE_ROLE && !sortable {
                    // Raised only past a role still below them; lowered only above everyone.
                    let above = roles.iter().filter(|other| other.id != EVERYONE_ROLE && other.position > role.position).map(|other| other.position).min();
                    if above.is_some_and(|position| powers.above(position)) {
                        row.push(button("↑", "raise", json!({ "role": role.id })));
                    }
                    if role.position > lowest_movable {
                        row.push(button("↓", "lower", json!({ "role": role.id })));
                    }
                }
            }
            items.push(json!({ "type": "row", "children": row }));
        }
        items.push(json!({ "type": "divider" }));
        items.push(json!({ "type": "form", "children": [
            { "type": "text-input", "form-id": "name", "label": "New role", "placeholder": "Artist", "required": true },
            button("Add role", "create", json!({}))
        ] }));
        let mut events = vec!["edit", "create", "raise", "lower", "reorder", "done"];
        if powers.owner {
            items.push(json!({ "type": "divider" }));
            items.push(json!({ "type": "form", "children": [
                { "type": "text", "text": "You own this community. Handing it on makes someone else its owner, who can then do anything here, including take you out." },
                { "type": "text-input", "form-id": "member", "label": "Hand the community to", "placeholder": "Their name, or their DID", "required": true },
                button("Hand it on", "hand_over", json!({}))
            ] }));
            events.push("hand_over");
        }
        items.push(button("Done", "done", json!({})));
        Ok((Value::Array(items), events))
    }

    fn role_edit_ui(&self, role: &RoleDef, powers: &Powers) -> Value {
        let everyone = role.id == EVERYONE_ROLE;
        let mut fields = Vec::new();
        if !everyone {
            fields.push(json!({ "type": "text-input", "form-id": "name", "label": "Name", "value": role.name, "required": true }));
            fields.push(json!({ "type": "text-input", "form-id": "color", "format": "color", "label": "Colour", "placeholder": "#3498db", "value": role.color.clone().unwrap_or_default() }));
            fields.push(json!({ "type": "text-input", "form-id": "icon", "format": "emoji", "label": "Badge", "placeholder": "⭐", "value": role.icon.clone().unwrap_or_default() }));
            fields.push(json!({ "type": "check-box", "form-id": "mentionable", "label": "Anyone may @mention this role", "value": role.mentionable }));
            fields.push(json!({ "type": "check-box", "form-id": "hoist", "label": "List its members apart", "value": role.hoist }));
            fields.push(json!({ "type": "check-box", "form-id": "self_assignable", "label": "Members may take it, or drop it, themselves", "value": role.self_assignable }));
        }
        let mut kept = Vec::new();
        for (heading, permissions) in [("In the community", COMMUNITY_PERMISSIONS), ("In channels (each channel can change these)", CHANNEL_PERMISSIONS)] {
            fields.push(json!({ "type": "title", "text": heading }));
            for (name, label) in permissions {
                let holds = role.permissions.iter().any(|p| p == name);
                if powers.has(name) {
                    fields.push(json!({ "type": "check-box", "form-id": format!("perm-{name}"), "label": label, "value": holds }));
                } else if holds {
                    kept.push(*label);
                }
            }
        }
        if !kept.is_empty() {
            fields.push(json!({ "type": "text", "text": format!("It also has, and only someone who has them can change: {}.", kept.join(", ")) }));
        }
        fields.push(button("Save", "save", json!({})));
        let title = if everyone { "Every member (everyone)".to_string() } else { format!("Role: {}", role.name) };
        let mut items = vec![json!({ "type": "title", "text": title }), json!({ "type": "form", "children": fields })];
        if !everyone {
            items.push(button("Delete role", "delete", json!({})));
        }
        items.push(button("Back", "back", json!({})));
        Value::Array(items)
    }

    fn member_roles_ui(&self, flow: &Flow, powers: &Powers) -> Result<Value, Refusal> {
        let member = self.flow_member(flow)?;
        let held = self.store.member_roles(&member.did).map_err(failed)?;
        let name = member.name.clone().unwrap_or_else(|| "this member".into());
        let mut fields = Vec::new();
        let mut fixed = Vec::new();
        for role in self.store.roles().map_err(failed)?.iter().filter(|role| role.id != EVERYONE_ROLE) {
            let holds = held.contains(&role.id);
            if powers.above(role.position) {
                fields.push(json!({ "type": "check-box", "form-id": format!("role-{}", role.id), "label": role.name, "value": holds }));
            } else if holds {
                fixed.push(role.name.clone());
            }
        }
        if !fixed.is_empty() {
            fields.push(json!({ "type": "text", "text": format!("Also: {} (ranked as high as yours, or higher).", fixed.join(", ")) }));
        }
        fields.push(button("Save", "save", json!({})));
        Ok(json!([{ "type": "title", "text": format!("Roles for {name}") }, { "type": "form", "children": fields }]))
    }

    fn channel_roles_ui(&self, flow: &Flow, powers: &Powers) -> Result<Value, Refusal> {
        let target = self.flow_target(flow)?;
        let overrides = self.store.overrides(&target.key).map_err(failed)?;
        let read_only = overrides.iter().any(|o| o.role == EVERYONE_ROLE && o.deny.iter().any(|p| p == "post"));
        let mut items = vec![json!({ "type": "title", "text": format!("Permissions in {}", target.title) })];
        items.push(json!({ "type": "text", "text": "A channel can allow or deny each role what members do in channels. Allowing wins over denying, a role's own over everyone's. Without See the channel, a role finds it isn't there: nothing of it reaches them." }));
        match (&target.channel, &target.category) {
            // A category's: what its synced channels have.
            (None, Some(_)) => items.push(json!({ "type": "text", "text": "Its channels that are synced with it follow what is set here; one whose permissions were changed on its own keeps them, until it is synced again." })),
            (Some(channel), Some(category)) => items.push(if self.synced(channel).map_err(failed)? {
                json!({ "type": "text", "text": format!("Synced with {}: it has the category's permissions, and follows them.", category.name) })
            } else {
                json!({ "type": "row", "children": [{ "type": "text", "text": format!("Not synced with {}: its permissions are its own.", category.name) }, button(&format!("Sync with {}", category.name), "sync", json!({}))] })
            }),
            _ => {}
        }
        if !target.voice {
            items.push(if read_only {
                json!({ "type": "row", "children": [{ "type": "text", "text": "Read-only: only roles allowed to post here can." }, button("Let everyone post", "unlock", json!({}))] })
            } else {
                button("Make read-only", "read_only", json!({}))
            });
        }
        let private = overrides.iter().any(|o| o.role == EVERYONE_ROLE && o.deny.iter().any(|p| p == "view_channel"));
        items.push(if private {
            json!({ "type": "row", "children": [{ "type": "text", "text": "Private: only roles allowed to see it can." }, button("Let everyone see it", "public", json!({}))] })
        } else {
            button("Make private", "private", json!({}))
        });
        for role in self.store.roles().map_err(failed)? {
            let said = overrides.iter().find(|o| o.role == role.id);
            let describe = |list: &[String]| list.iter().map(|p| label_of(p)).collect::<Vec<_>>().join(", ");
            let text = match said {
                Some(o) if !o.allow.is_empty() && !o.deny.is_empty() => format!("{}: allows {}; denies {}", role.name, describe(&o.allow), describe(&o.deny)),
                Some(o) if !o.allow.is_empty() => format!("{}: allows {}", role.name, describe(&o.allow)),
                Some(o) if !o.deny.is_empty() => format!("{}: denies {}", role.name, describe(&o.deny)),
                _ => format!("{}: as everywhere", role.name),
            };
            let mut row = vec![json!({ "type": "text", "text": text })];
            if role.id == EVERYONE_ROLE || powers.above(role.position) {
                row.push(button("Edit", "edit", json!({ "role": role.id })));
            }
            items.push(json!({ "type": "row", "children": row }));
        }
        items.push(button("Done", "done", json!({})));
        Ok(Value::Array(items))
    }

    fn channel_role_ui(&self, flow: &Flow, held: &std::collections::BTreeSet<String>) -> Result<Value, Refusal> {
        let target = self.flow_target(flow)?;
        let role_id = flow.data["role"].as_str().unwrap_or_default();
        let role = self.store.role(role_id).map_err(failed)?.ok_or_else(|| refused("That role is gone."))?;
        let said = self.store.overrides(&target.key).map_err(failed)?.into_iter().find(|o| o.role == role.id);
        let mut fields = Vec::new();
        for (name, label) in CHANNEL_PERMISSIONS {
            let allowed = said.as_ref().is_some_and(|o| o.allow.iter().any(|p| p == name));
            let denied = said.as_ref().is_some_and(|o| o.deny.iter().any(|p| p == name));
            if held.contains(*name) {
                fields.push(json!({ "type": "row", "children": [
                    { "type": "text", "text": label },
                    { "type": "check-box", "form-id": format!("allow-{name}"), "label": "Allow", "value": allowed },
                    { "type": "check-box", "form-id": format!("deny-{name}"), "label": "Deny", "value": denied }
                ] }));
            } else if allowed || denied {
                fields.push(json!({ "type": "text", "text": format!("{label}: {} (only someone who has it here can change this)", if allowed { "allowed" } else { "denied" }) }));
            }
        }
        fields.push(button("Save", "save", json!({})));
        Ok(json!([
            { "type": "title", "text": format!("{} in {}", role.name, target.title) },
            { "type": "text", "text": "Neither ticked: as the role has it everywhere." },
            { "type": "form", "children": fields },
            button("Back", "back", json!({}))
        ]))
    }

    /// A workflow's `status`, as its owner is shown it now.
    fn flow_status(&self, flow: &Flow) -> Result<Value, Refusal> {
        let powers = self.powers(&flow.owner).map_err(failed)?;
        let (ui, events): (Value, Vec<&str>) = if flow.status != "active" {
            // (What was done, where the workflow says: a template imported.)
            let said = flow.data["report"].as_str().filter(|_| flow.status == "completed");
            (json!([{ "type": "pass", "text": said.unwrap_or(if flow.status == "completed" { "Done." } else { "Closed." }) }]), Vec::new())
        } else {
            match (flow.template.as_str(), flow.state.as_str()) {
                (ROLES, "list") => self.roles_list_ui(flow, &powers).map_err(failed)?,
                (ROLES, "edit") => {
                    let role = self.store.role(flow.data["role"].as_str().unwrap_or_default()).map_err(failed)?.ok_or_else(|| refused("That role is gone."))?;
                    let mut events = vec!["save", "back"];
                    if role.id != EVERYONE_ROLE {
                        events.push("delete");
                    }
                    (self.role_edit_ui(&role, &powers), events)
                }
                (MEMBER_ROLES, _) => (self.member_roles_ui(flow, &powers)?, vec!["save"]),
                (CHANNEL_PERMISSIONS_FLOW, "roles") => (self.channel_roles_ui(flow, &powers)?, vec!["read_only", "unlock", "private", "public", "edit", "sync", "done"]),
                (AUTOMOD_FLOW, "list") => (self.automod_list_ui(flow).map_err(failed)?, vec!["edit", "create", "preset", "toggle", "done"]),
                (AUTOMOD_FLOW, "edit") => (self.automod_edit_ui(flow).map_err(failed)?, vec!["save", "delete", "back"]),
                (JOIN_GATES_FLOW, _) => (self.join_gates_ui().map_err(failed)?, vec!["save"]),
                (CHANNELS_FLOW, "list") => (self.channels_list_ui().map_err(failed)?, vec!["up", "down", "create_category", "edit_category", "category_up", "category_down", "move", "done"]),
                (CHANNELS_FLOW, "category") => (self.category_ui(flow).map_err(failed)?, vec!["save", "delete", "back"]),
                (CHANNELS_FLOW, "move") => (self.move_ui(flow).map_err(failed)?, vec!["to", "back"]),
                (ONBOARDING_SETUP_FLOW, _) => (self.onboarding_setup_ui().map_err(failed)?, vec!["save"]),
                (ONBOARDING_FLOW, state) => self.onboarding_ui(flow, state).map_err(failed)?,
                (WEB_FLOW, _) => self.web_flow_ui(flow).map_err(failed)?,
                (SITES_FLOW, _) => self.sites_flow_ui(flow).map_err(failed)?,
                (DISCORD_IMPORT_FLOW, _) => self.discord_import_ui(flow).map_err(failed)?,
                (WEBHOOKS_FLOW, _) => self.webhooks_ui(flow).map_err(failed)?,
                (CHANNEL_PERMISSIONS_FLOW, "role") => {
                    let target = self.flow_target(flow)?;
                    let held = self.target_held(&flow.owner, &powers, &target)?;
                    (self.channel_role_ui(flow, &held)?, vec!["save", "back"])
                }
                _ => (json!([]), Vec::new()),
            }
        };
        let mut artifacts = json!({ "template": flow.template });
        if let Some(object) = flow.context.as_object() {
            for (key, value) in object {
                artifacts[key] = value.clone();
            }
        }
        // The channels onboarding suggests, for an app to offer as it finishes.
        if flow.template == ONBOARDING_FLOW {
            let seen = self.seen_by(&flow.owner).map_err(failed)?;
            let suggested: Vec<Value> = self.onboarding().map_err(failed)?["suggested"].as_array().into_iter().flatten().filter(|id| id.as_str().is_some_and(|id| seen.contains(id))).cloned().collect();
            artifacts["suggested"] = json!(suggested);
        }
        Ok(json!({
            "instance_id": flow.id,
            "state": if flow.status == "active" { flow.state.as_str() } else { "done" },
            "status": flow.status,
            "allowed_events": events,
            "action_menu": {},
            "artifacts": artifacts,
            "ui": ui,
            "ui_profile": "receiver",
        }))
    }

    /// `start` of one of these templates.
    pub fn start_flow(&self, member: &Member, message: &Value, now: i64) -> Answer {
        let body = &message["body"];
        let template = body["template_id"].as_str().unwrap_or_default();
        if body["template_version"].as_str().is_some_and(|version| version != VERSION) {
            return Err(Refusal::new("template_not_found", "This server has no such workflow."));
        }
        let powers = self.powers(&member.did).map_err(failed)?;
        let context = match template {
            ROLES => {
                if !powers.has("manage_roles") {
                    return Err(not_allowed("Managing roles takes Manage roles."));
                }
                json!({})
            }
            MEMBER_ROLES => {
                if !powers.has("manage_roles") {
                    return Err(not_allowed("Giving roles takes Manage roles."));
                }
                let did = body["context"]["member"].as_str().unwrap_or_default();
                if self.store.member(did).map_err(failed)?.is_none() {
                    return Err(refused("There is no such member."));
                }
                if did != member.did && !self.outranks(&powers, did).map_err(failed)? {
                    return Err(not_allowed("You can only give roles to someone ranked below you."));
                }
                json!({ "member": did })
            }
            CHANNEL_PERMISSIONS_FLOW if body["context"]["category"].is_string() => {
                let id = body["context"]["category"].as_str().unwrap_or_default();
                if !self.store.categories().map_err(failed)?.iter().any(|c| c.id == id) {
                    return Err(refused("There is no such category."));
                }
                if !powers.has("manage_channels") {
                    return Err(not_allowed("A category's permissions take Manage channels."));
                }
                json!({ "category": id })
            }
            CHANNEL_PERMISSIONS_FLOW => {
                let id = body["context"]["channel"].as_str().unwrap_or_default();
                let channel = self.store.channel(id).map_err(failed)?.ok_or_else(|| refused("There is no such channel."))?;
                if !self.may_manage_channel(&member.did, &powers, &channel) {
                    return Err(not_allowed("Only someone who manages channels, or whoever opened this one, sets its permissions."));
                }
                json!({ "channel": id })
            }
            AUTOMOD_FLOW => {
                if !powers.has("manage_automod") {
                    return Err(not_allowed("AutoMod takes Set AutoMod's rules."));
                }
                json!({})
            }
            JOIN_GATES_FLOW | ONBOARDING_SETUP_FLOW => {
                if !powers.has("manage_community") {
                    return Err(not_allowed("That takes Manage the community."));
                }
                json!({})
            }
            CHANNELS_FLOW => {
                if !powers.has("manage_channels") {
                    return Err(not_allowed("Ordering channels takes Manage channels."));
                }
                json!({})
            }
            ONBOARDING_FLOW => json!({}),
            WEB_FLOW => {
                if self.web_url.is_none() {
                    return Err(Refusal::new("template_not_found", "This server shows nothing on the web."));
                }
                if !powers.has("manage_community") {
                    return Err(not_allowed("The community on the web takes Manage the community."));
                }
                json!({})
            }
            SITES_FLOW => {
                if self.web_url.is_none() {
                    return Err(Refusal::new("template_not_found", "This server shows nothing on the web."));
                }
                json!({})
            }
            WEBHOOKS_FLOW => {
                if !self.may_manage_webhooks(&member.did).map_err(failed)? {
                    return Err(not_allowed("Webhooks take Manage webhooks in a channel."));
                }
                json!({})
            }
            DISCORD_IMPORT_FLOW => {
                if !self.may_import(&member.did).map_err(failed)? {
                    return Err(not_allowed("Only an owner imports a Discord template."));
                }
                json!({})
            }
            _ => return Err(Refusal::new("template_not_found", "This server has no such workflow.")),
        };
        let wanted = body["instance_id"].as_str().or(message["thid"].as_str()).or(message["id"].as_str()).unwrap_or_default();
        if let Some(existing) = self.store.active_flow(&member.did, template, &context).map_err(failed)? {
            if existing.id == wanted {
                return self.flow_status(&existing).map(|status| (format!("{P}status"), status));
            }
            let mut refusal = Refusal::new("multiplicity_violation", "You have this open already.");
            refusal.args = json!({ "instance_id": existing.id });
            return Err(refusal);
        }
        if wanted.is_empty() || wanted.len() > 64 || self.store.flow(wanted).map_err(failed)?.is_some() || self.store.answering(wanted).map_err(failed)?.is_some() {
            return Err(Refusal::new("multiplicity_violation", "That instance id has been used."));
        }
        let state = match template {
            ROLES | AUTOMOD_FLOW | CHANNELS_FLOW | SITES_FLOW | WEBHOOKS_FLOW => "list",
            MEMBER_ROLES => "pick",
            JOIN_GATES_FLOW | ONBOARDING_SETUP_FLOW | WEB_FLOW => "form",
            ONBOARDING_FLOW => "welcome",
            DISCORD_IMPORT_FLOW => "link",
            _ => "roles",
        };
        // What the app says it can draw (workflow/1.0's `capabilities`), kept with the flow.
        let data = match body.get("capabilities").filter(|c| c.is_array()) {
            Some(capabilities) => json!({ "capabilities": capabilities }),
            None => json!({}),
        };
        let flow = Flow { id: wanted.into(), owner: member.did.clone(), template: template.into(), context, state: state.into(), data, status: "active".into(), updated: now };
        self.store.save_flow(&flow).map_err(failed)?;
        self.flow_status(&flow).map(|status| (format!("{P}status"), status))
    }

    /// `owner`'s workflow `instance_id`, if it is one of these.
    pub fn own_flow(&self, owner: &str, instance_id: &str) -> Result<Option<Flow>, Refusal> {
        Ok(self.store.flow(instance_id).map_err(failed)?.filter(|flow| flow.owner == owner))
    }

    pub fn flow_status_answer(&self, flow: &Flow) -> Answer {
        self.flow_status(flow).map(|status| (format!("{P}status"), status))
    }

    pub fn cancel_flow(&self, mut flow: Flow, now: i64) -> Answer {
        if flow.status == "active" {
            flow.status = "canceled".into();
            flow.updated = now;
            self.store.save_flow(&flow).map_err(failed)?;
        }
        self.flow_status_answer(&flow)
    }

    /// `advance`: does what the event says, and answers where the workflow is now.
    /// The flag is whether roles or permissions changed.
    pub fn advance_flow(&self, mut flow: Flow, body: &Value, now: i64) -> Result<(Answer, bool), Refusal> {
        if flow.status != "active" {
            return Ok((self.flow_status_answer(&flow), false));
        }
        let event = body["event"].as_str().unwrap_or_default();
        let input = &body["input"];
        let powers = self.powers(&flow.owner).map_err(failed)?;
        // Someone who no longer may is shown so, not let go on.
        let still = match flow.template.as_str() {
            CHANNEL_PERMISSIONS_FLOW if flow.context["category"].is_string() => powers.has("manage_channels"),
            CHANNEL_PERMISSIONS_FLOW => self.may_manage_channel(&flow.owner, &powers, &self.flow_channel(&flow)?),
            AUTOMOD_FLOW => powers.has("manage_automod"),
            JOIN_GATES_FLOW | ONBOARDING_SETUP_FLOW => powers.has("manage_community"),
            CHANNELS_FLOW => powers.has("manage_channels"),
            ONBOARDING_FLOW | SITES_FLOW => true,
            WEB_FLOW => powers.has("manage_community"),
            DISCORD_IMPORT_FLOW => self.may_import(&flow.owner).map_err(failed)?,
            WEBHOOKS_FLOW => self.may_manage_webhooks(&flow.owner).map_err(failed)?,
            _ => powers.has("manage_roles"),
        };
        if !still {
            return Err(not_allowed("You no longer may do this."));
        }
        flow.data.as_object_mut().map(|data| data.remove("note"));
        if !flow.data.is_object() {
            flow.data = json!({});
        }
        let changed = match (flow.template.as_str(), flow.state.as_str(), event) {
            (ROLES, "list", "edit") => {
                let role = self.editable_role(&powers, input["role"].as_str().unwrap_or_default())?;
                flow.data["role"] = json!(role.id);
                flow.state = "edit".into();
                false
            }
            (ROLES, "list", "create") => {
                let name = line(&input["name"], MAX_ROLE_NAME).ok_or_else(|| refused("A role needs a name."))?;
                let roles = self.store.roles().map_err(failed)?;
                if roles.len() >= MAX_ROLES {
                    return Err(refused("This community has as many roles as it can hold."));
                }
                if self.role_name_taken(&name, "").map_err(failed)? {
                    return Err(refused("There is a role called that already."));
                }
                let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
                self.store
                    .save_role(&RoleDef { id: id.clone(), name, color: None, icon: None, position: 1, permissions: Vec::new(), mentionable: false, hoist: false, self_assignable: false })
                    .map_err(failed)?;
                // New roles go to the bottom, just above everyone.
                let mut order: Vec<String> = roles.iter().rev().filter(|role| role.id != EVERYONE_ROLE).map(|role| role.id.clone()).collect();
                order.insert(0, id.clone());
                self.store.rank_roles(&order).map_err(failed)?;
                self.audit(now, &flow.owner, "role.create", Some(&id), Some(json!({ "name": line(&input["name"], MAX_ROLE_NAME) })));
                flow.data["role"] = json!(id);
                flow.state = "edit".into();
                true
            }
            (ROLES, "list", "raise" | "lower") => {
                let role = self.editable_role(&powers, input["role"].as_str().unwrap_or_default())?;
                if role.id == EVERYONE_ROLE {
                    return Err(refused("Every member's role is always lowest."));
                }
                let mut order: Vec<RoleDef> = self.store.roles().map_err(failed)?.into_iter().rev().filter(|r| r.id != EVERYONE_ROLE).collect();
                let at = order.iter().position(|r| r.id == role.id).unwrap_or_default();
                let other = if event == "raise" { at + 1 } else { at.wrapping_sub(1) };
                let Some(neighbour) = order.get(other) else {
                    return Err(refused(if event == "raise" { "It is as high as it goes." } else { "It is as low as it goes." }));
                };
                if !powers.above(neighbour.position) {
                    return Err(not_allowed("You can't rank a role as high as your own."));
                }
                order.swap(at, other);
                self.store.rank_roles(&order.iter().map(|r| r.id.clone()).collect::<Vec<_>>()).map_err(failed)?;
                self.audit(now, &flow.owner, "role.rank", Some(&role.id), Some(json!({ "moved": event })));
                true
            }
            (ROLES, "list", "reorder") => {
                // Highest first, every role but everyone, each once; one this person
                // doesn't outrank where it was.
                let held: Vec<RoleDef> = self.store.roles().map_err(failed)?.into_iter().filter(|r| r.id != EVERYONE_ROLE).collect();
                let given: Vec<String> = input["order"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
                let mut sorted_given = given.clone();
                sorted_given.sort();
                let mut sorted_held: Vec<String> = held.iter().map(|r| r.id.clone()).collect();
                sorted_held.sort();
                if sorted_given != sorted_held {
                    return Err(refused("The order names each role once, and no others."));
                }
                for (at, role) in held.iter().enumerate() {
                    if !powers.above(role.position) && given.get(at) != Some(&role.id) {
                        return Err(not_allowed("You can only rank roles below your own."));
                    }
                }
                let lowest_first: Vec<String> = given.iter().rev().cloned().collect();
                self.store.rank_roles(&lowest_first).map_err(failed)?;
                self.audit(now, &flow.owner, "role.rank", None, Some(json!({ "order": given })));
                flow.data["note"] = json!("Saved the order.");
                true
            }
            (ROLES, "list", "hand_over") => {
                if !powers.owner {
                    return Err(not_allowed("Only the owner hands the community on."));
                }
                if !self.config.owners.is_empty() {
                    return Err(refused("This community's owners are set where it is run, not handed on from here."));
                }
                let given = input["member"].as_str().unwrap_or_default().trim();
                let members = self.store.members().map_err(failed)?;
                let found: Vec<&Member> = members.iter().filter(|m| m.did == given || m.name.as_deref().is_some_and(|name| name.eq_ignore_ascii_case(given))).collect();
                let heir = match found.as_slice() {
                    [one] => *one,
                    [] => return Err(refused("There is no member by that name or DID.")),
                    _ => return Err(refused("More than one member is called that: give their DID.")),
                };
                if heir.did == flow.owner {
                    return Err(refused("You own it already."));
                }
                self.store.set_setting("owner", &heir.did).map_err(failed)?;
                self.audit(now, &flow.owner, "owner.transfer", Some(&heir.did), None);
                flow.data["note"] = json!(format!("{} owns the community now.", heir.name.clone().unwrap_or_else(|| heir.did.clone())));
                true
            }
            (AUTOMOD_FLOW, _, _) | (JOIN_GATES_FLOW, _, _) => self.advance_moderation_flow(&mut flow, event, input, now)?,
            (CHANNELS_FLOW, _, _) | (ONBOARDING_SETUP_FLOW, _, _) | (ONBOARDING_FLOW, _, _) => self.advance_structure_flow(&mut flow, event, input, now)?,
            (WEB_FLOW, _, _) => self.advance_web_flow(&mut flow, event, input, now)?,
            (SITES_FLOW, _, _) => self.advance_sites_flow(&mut flow, event, input)?,
            (DISCORD_IMPORT_FLOW, _, _) => self.advance_discord_import(&mut flow, event, input, now)?,
            (WEBHOOKS_FLOW, _, _) => self.advance_webhooks(&mut flow, event, input, now)?,
            (ROLES, "list", "done") | (CHANNEL_PERMISSIONS_FLOW, "roles", "done") => {
                flow.status = "completed".into();
                false
            }
            (ROLES, "edit", "save") => {
                let mut role = self.editable_role(&powers, flow.data["role"].as_str().unwrap_or_default())?;
                if role.id != EVERYONE_ROLE {
                    let name = line(&input["name"], MAX_ROLE_NAME).ok_or_else(|| refused("A role needs a name."))?;
                    if self.role_name_taken(&name, &role.id).map_err(failed)? {
                        return Err(refused("There is a role called that already."));
                    }
                    let icon = input["icon"].as_str().unwrap_or_default().trim().to_string();
                    if !icon.is_empty() && !is_emoji(&icon) {
                        return Err(refused("A badge is one emoji."));
                    }
                    role.name = name;
                    role.color = color(&input["color"])?;
                    role.icon = (!icon.is_empty()).then_some(icon);
                    role.mentionable = ticked(&input["mentionable"]);
                    role.hoist = ticked(&input["hoist"]);
                    role.self_assignable = ticked(&input["self_assignable"]);
                }
                // What the editor holds is theirs to set; anything else stays as it was.
                for (name, _) in COMMUNITY_PERMISSIONS.iter().chain(CHANNEL_PERMISSIONS) {
                    if powers.has(name) {
                        role.permissions.retain(|p| p != name);
                        if ticked(&input[format!("perm-{name}")]) {
                            role.permissions.push(name.to_string());
                        }
                    }
                }
                self.store.save_role(&role).map_err(failed)?;
                self.audit(now, &flow.owner, "role.update", Some(&role.id), Some(json!({ "name": role.name, "permissions": role.permissions, "color": role.color, "mentionable": role.mentionable })));
                flow.data["note"] = json!(format!("Saved {}.", role.name));
                flow.state = "list".into();
                true
            }
            (ROLES, "edit", "delete") => {
                let role = self.editable_role(&powers, flow.data["role"].as_str().unwrap_or_default())?;
                if role.id == EVERYONE_ROLE {
                    return Err(refused("Every member's role can't be deleted."));
                }
                self.store.delete_role(&role.id).map_err(failed)?;
                self.audit(now, &flow.owner, "role.delete", Some(&role.id), Some(json!({ "name": role.name })));
                flow.data["note"] = json!(format!("Deleted {}.", role.name));
                flow.state = "list".into();
                true
            }
            (ROLES, "edit", "back") | (CHANNEL_PERMISSIONS_FLOW, "role", "back") => {
                flow.state = if flow.template == ROLES { "list" } else { "roles" }.into();
                false
            }
            (MEMBER_ROLES, "pick", "save") => {
                let member = self.flow_member(&flow)?;
                if member.did != flow.owner && !self.outranks(&powers, &member.did).map_err(failed)? {
                    return Err(not_allowed("You can only give roles to someone ranked below you."));
                }
                let held = self.store.member_roles(&member.did).map_err(failed)?;
                let (mut given, mut taken) = (Vec::new(), Vec::new());
                for role in self.store.roles().map_err(failed)?.iter().filter(|role| role.id != EVERYONE_ROLE && powers.above(role.position)) {
                    let wanted = ticked(&input[format!("role-{}", role.id)]);
                    match (wanted, held.contains(&role.id)) {
                        (true, false) => {
                            self.store.give_role(&member.did, &role.id).map_err(failed)?;
                            given.push(role.id.clone());
                        }
                        (false, true) => {
                            self.store.take_role(&member.did, &role.id).map_err(failed)?;
                            taken.push(role.id.clone());
                        }
                        _ => {}
                    }
                }
                if !given.is_empty() || !taken.is_empty() {
                    self.audit(now, &flow.owner, "member.roles", Some(&member.did), Some(json!({ "given": given, "taken": taken })));
                }
                flow.status = "completed".into();
                true
            }
            (CHANNEL_PERMISSIONS_FLOW, "roles", "sync") => {
                let target = self.flow_target(&flow)?;
                let Some(channel) = &target.channel else { return Err(refused("A category is what channels sync with.")) };
                self.sync_with_category(channel).map_err(failed)?;
                self.audit(now, &flow.owner, "channel.permissions", Some(&channel.id), Some(json!({ "synced": true })));
                true
            }
            (CHANNEL_PERMISSIONS_FLOW, "roles", "read_only" | "unlock" | "private" | "public") => {
                let target = self.flow_target(&flow)?;
                let said = self.store.overrides(&target.key).map_err(failed)?.into_iter().find(|o| o.role == EVERYONE_ROLE);
                let (mut allow, mut deny) = said.map(|o| (o.allow, o.deny)).unwrap_or_default();
                let (permission, on, said) = match event {
                    "read_only" | "unlock" => ("post", event == "read_only", "read_only"),
                    _ => ("view_channel", event == "private", "private"),
                };
                allow.retain(|p| p != permission);
                deny.retain(|p| p != permission);
                if on {
                    deny.push(permission.into());
                }
                self.set_target_override(&target, EVERYONE_ROLE, &allow, &deny)?;
                self.audit(now, &flow.owner, "channel.permissions", Some(&target.key), Some(json!({ "role": EVERYONE_ROLE, said: on })));
                true
            }
            (CHANNEL_PERMISSIONS_FLOW, "roles", "edit") => {
                let id = input["role"].as_str().unwrap_or_default();
                let role = self.store.role(id).map_err(failed)?.ok_or_else(|| refused("There is no such role."))?;
                if role.id != EVERYONE_ROLE && !powers.above(role.position) {
                    return Err(not_allowed("You can only set this for roles ranked below your own."));
                }
                flow.data["role"] = json!(role.id);
                flow.state = "role".into();
                false
            }
            (CHANNEL_PERMISSIONS_FLOW, "role", "save") => {
                let target = self.flow_target(&flow)?;
                let role_id = flow.data["role"].as_str().unwrap_or_default().to_string();
                let held = self.target_held(&flow.owner, &powers, &target)?;
                let said = self.store.overrides(&target.key).map_err(failed)?.into_iter().find(|o| o.role == role_id);
                let (mut allow, mut deny) = said.map(|o| (o.allow, o.deny)).unwrap_or_default();
                for (name, label) in CHANNEL_PERMISSIONS.iter().filter(|(name, _)| held.contains(*name)) {
                    let (allowing, denying) = (ticked(&input[format!("allow-{name}")]), ticked(&input[format!("deny-{name}")]));
                    if allowing && denying {
                        return Err(refused(&format!("{label} can't be both allowed and denied.")));
                    }
                    allow.retain(|p| p != name);
                    deny.retain(|p| p != name);
                    if allowing {
                        allow.push(name.to_string());
                    }
                    if denying {
                        deny.push(name.to_string());
                    }
                }
                self.set_target_override(&target, &role_id, &allow, &deny)?;
                self.audit(now, &flow.owner, "channel.permissions", Some(&target.key), Some(json!({ "role": role_id, "allow": allow, "deny": deny })));
                flow.state = "roles".into();
                true
            }
            _ => return Err(refused("That can't be done here.")),
        };
        flow.updated = now;
        self.store.save_flow(&flow).map_err(failed)?;
        let status = self.flow_status(&flow)?;
        let kind = if flow.status == "completed" { "complete" } else { "status" };
        Ok((Ok((format!("{P}{kind}"), status)), changed))
    }

    /// The role `id`, if `powers` may change it: ranked below them, or `everyone`
    /// for anyone with a role of their own.
    fn editable_role(&self, powers: &Powers, id: &str) -> Result<RoleDef, Refusal> {
        let role = self.store.role(id).map_err(failed)?.ok_or_else(|| refused("There is no such role."))?;
        let allowed = if role.id == EVERYONE_ROLE { powers.owner || powers.rank > 0 } else { powers.above(role.position) };
        if !allowed {
            return Err(not_allowed("You can only change roles ranked below your own."));
        }
        Ok(role)
    }

    // --- AutoMod's rules, and the join gates ------------------------------------------

    fn automod_list_ui(&self, flow: &Flow) -> anyhow::Result<Value> {
        let mut items = vec![json!({ "type": "title", "text": "AutoMod" })];
        items.push(json!({ "type": "text", "text": "Rules that check each post, and each edit, before it is placed, in order. A rule that refuses or holds a post is the last to check it; one that censors it passes what is left to the next. Whoever manages posts in a channel isn't checked there." }));
        if let Some(note) = flow.data["note"].as_str() {
            items.push(json!({ "type": "pass", "text": note }));
        }
        let rules = self.store.automod_rules()?;
        if rules.is_empty() {
            items.push(json!({ "type": "text", "text": "No rules yet." }));
        }
        for rule in &rules {
            let on = rule["enabled"] == true;
            let id = rule["id"].as_str().unwrap_or_default();
            items.push(json!({ "type": "row", "children": [
                { "type": "text", "text": format!("{} ({}, {})", rule["name"].as_str().unwrap_or("A rule"), if on { "on" } else { "off" }, crate::automod::action_label(rule)) },
                button("Edit", "edit", json!({ "rule": id })),
                button(if on { "Turn off" } else { "Turn on" }, "toggle", json!({ "rule": id }))
            ] }));
        }
        items.push(json!({ "type": "divider" }));
        items.push(json!({ "type": "title", "text": "Start from a ready-made rule" }));
        for (key, name, about, _) in crate::automod::PRESETS {
            items.push(json!({ "type": "row", "children": [
                { "type": "text", "text": format!("{name}: {about}") },
                button("Add", "preset", json!({ "preset": key }))
            ] }));
        }
        items.push(json!({ "type": "divider" }));
        items.push(json!({ "type": "form", "children": [
            { "type": "text-input", "form-id": "name", "label": "Or a new rule of your own", "placeholder": "Spam links", "required": true },
            button("Add rule", "create", json!({}))
        ] }));
        items.push(button("Done", "done", json!({})));
        Ok(Value::Array(items))
    }

    fn automod_rule(&self, id: &str) -> Result<Value, Refusal> {
        self.store.automod_rules().map_err(failed)?.into_iter().find(|rule| rule["id"] == id).ok_or_else(|| refused("There is no such rule."))
    }

    fn automod_edit_ui(&self, flow: &Flow) -> anyhow::Result<Value> {
        let rule = self.automod_rule(flow.data["rule"].as_str().unwrap_or_default()).map_err(|refusal| anyhow::anyhow!(refusal.comment))?;
        let lines = |field: &str| rule[field].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>();
        let alert = rule["alert_channel"].as_str().and_then(|id| self.store.channel(id).ok().flatten()).map(|c| c.name).unwrap_or_default();
        let mut fields = vec![
            json!({ "type": "text-input", "form-id": "name", "label": "Name", "value": rule["name"], "required": true }),
            json!({ "type": "check-box", "form-id": "enabled", "label": "On", "value": rule["enabled"] == true }),
            json!({ "type": "title", "text": "What it looks for" }),
            json!({ "type": "text", "text": "The filters know words however they are disguised: Sh#t, $hit, s h i t, sh1t, letters from other alphabets." }),
        ];
        for (key, label, _) in crate::filters::FILTERS {
            let mut options = vec![json!({ "value": "off", "label": "Off" })];
            options.extend(crate::filters::STRICTNESS.iter().map(|(value, label)| json!({ "value": value, "label": label })));
            fields.push(json!({ "type": "drop-down", "form-id": format!("filter-{key}"), "label": label, "options": options, "value": rule["filters"][key].as_str().unwrap_or("off") }));
        }
        fields.extend([
            json!({ "type": "text-area", "form-id": "words", "label": "Words of your own, one a line (* stands for any letters). They're found disguised too.", "value": lines("words").join("\n") }),
            json!({ "type": "text-area", "form-id": "allowed_words", "label": "Words never to act on, one a line: a town, a name, a word a filter takes wrongly (shiitake)", "value": lines("allowed_words").join("\n") }),
            json!({ "type": "text-area", "form-id": "patterns", "label": "Patterns, one a line (regular expressions)", "value": lines("patterns").join("\n") }),
            json!({ "type": "title", "text": "Links" }),
            json!({ "type": "check-box", "form-id": "links", "label": "Any link", "value": rule["links"] == true }),
        ]);
        let lists = lines("domain_lists");
        for (key, label, _) in crate::filters::DOMAIN_LISTS {
            fields.push(json!({ "type": "check-box", "form-id": format!("list-{key}"), "label": label, "value": lists.contains(&key) }));
        }
        fields.extend([
            json!({ "type": "text-input", "form-id": "blocked_domains", "label": "Links never allowed to (domains, separated by commas)", "value": lines("blocked_domains").join(", ") }),
            json!({ "type": "text-input", "form-id": "allowed_domains", "label": "Links always allowed to (domains, separated by commas)", "value": lines("allowed_domains").join(", ") }),
            json!({ "type": "check-box", "form-id": "link_words", "label": "Cover a whole link for a word above in its address (part of a link is never dotted: that would break it)", "value": rule["link_words"] == true }),
            json!({ "type": "text-input", "form-id": "max_mentions", "label": "Most mentions in one post (empty: no limit)", "value": rule["max_mentions"].as_u64().map(|n| n.to_string()).unwrap_or_default() }),
            json!({ "type": "title", "text": "What it does" }),
            json!({ "type": "drop-down", "form-id": "action", "label": "With a post it finds something in", "value": crate::automod::action_of(&rule), "options": crate::automod::ACTIONS.iter().map(|(value, label)| json!({ "value": value, "label": label })).collect::<Vec<_>>() }),
            json!({ "type": "text", "text": "Censored, words become dots and links <UNSAFE URL BLOCKED>. Whoever manages posts in the channel can still see what it said." }),
            // (Its suggestions are the text channels, for an app to offer as it is
            // typed: Workflow UI Layout's `suggestions`; any other shows the box.)
            json!({ "type": "text-input", "form-id": "alert_channel", "label": "Tell a channel of it (the channel's name)", "placeholder": "mod-log", "value": alert,
                "suggestions": self.store.channels()?.iter().filter(|c| c.voice.is_none() && !c.closed).map(|c| c.name.clone()).collect::<Vec<_>>() }),
            json!({ "type": "text-input", "form-id": "timeout_minutes", "label": "Time its author out for (minutes; empty: no)", "value": rule["timeout_seconds"].as_i64().map(|s| (s / 60).to_string()).unwrap_or_default() }),
            json!({ "type": "title", "text": "Who and where it doesn't check" }),
        ]);
        let exempt_roles = lines("exempt_roles");
        let roles: Vec<Value> = self
            .store
            .roles()?
            .iter()
            .filter(|role| role.id != EVERYONE_ROLE)
            .map(|role| json!({ "type": "check-box", "form-id": format!("exempt-role-{}", role.id), "label": role.name, "value": exempt_roles.contains(&role.id.as_str()) }))
            .collect();
        let exempt_channels = lines("exempt_channels");
        let channels: Vec<Value> = self
            .store
            .channels()?
            .iter()
            .filter(|c| c.voice.is_none() && !c.closed)
            .map(|channel| json!({ "type": "check-box", "form-id": format!("exempt-channel-{}", channel.id), "label": format!("#{}", channel.name), "value": exempt_channels.contains(&channel.id.as_str()) }))
            .collect();
        // In tabs, roles and channels, where the app draws them (Workflow UI
        // Layout's `tabs`); as two lists under their names otherwise.
        if can_draw(flow, TABS) {
            fields.push(json!({ "type": "tabs", "tabs": [{ "label": "Roles", "children": roles }, { "label": "Channels", "children": channels }] }));
        } else {
            fields.push(json!({ "type": "text", "text": "Roles" }));
            fields.extend(roles);
            fields.push(json!({ "type": "text", "text": "Channels" }));
            fields.extend(channels);
        }
        fields.push(button("Save", "save", json!({})));
        Ok(json!([
            { "type": "title", "text": format!("Rule: {}", rule["name"].as_str().unwrap_or("A rule")) },
            { "type": "form", "children": fields },
            button("Delete rule", "delete", json!({})),
            button("Back", "back", json!({}))
        ]))
    }

    fn join_gates_ui(&self) -> anyhow::Result<Value> {
        let gates = self.gates()?;
        let questions = gates["questions"].as_array().cloned().unwrap_or_default();
        let mut fields = vec![json!({ "type": "text-input", "form-id": "post_wait", "label": "Minutes a new member waits before posting (empty: none). Anyone with a role doesn't wait.", "value": gates["post_wait"].as_i64().map(|m| m.to_string()).unwrap_or_default() })];
        fields.push(json!({ "type": "title", "text": "Questions on joining" }));
        fields.push(json!({ "type": "text", "text": "Asked of whoever asks to join; their answers are shown beside them on the waiting list." }));
        for n in 0..crate::gates::MAX_QUESTIONS {
            let question = questions.get(n);
            let options = question.and_then(|q| q["options"].as_array()).map(|o| o.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" | ")).unwrap_or_default();
            fields.push(json!({ "type": "text-input", "form-id": format!("q{n}"), "label": format!("Question {}", n + 1), "value": question.map(|q| q["text"].clone()).unwrap_or(json!("")) }));
            fields.push(json!({ "type": "text-input", "form-id": format!("q{n}-options"), "label": "Answers to choose from, separated by | (empty: they write one)", "value": options }));
            fields.push(json!({ "type": "check-box", "form-id": format!("q{n}-required"), "label": "Needs an answer", "value": question.is_some_and(|q| q["required"] == true) }));
        }
        fields.push(json!({ "type": "text", "text": "A credential to show on joining: not available on this server yet." }));
        fields.push(button("Save", "save", json!({})));
        Ok(json!([{ "type": "title", "text": "Join gates" }, { "type": "form", "children": fields }]))
    }

    /// An event of the `automod` or `join-gates` workflow; whether `community`
    /// changed for anyone (the join gates' wait).
    fn advance_moderation_flow(&self, flow: &mut Flow, event: &str, input: &Value, now: i64) -> Result<bool, Refusal> {
        let words = |field: &str, split: &[char]| -> Vec<String> {
            input[field].as_str().unwrap_or_default().split(split).map(str::trim).filter(|w| !w.is_empty()).map(str::to_string).collect()
        };
        match (flow.template.as_str(), flow.state.as_str(), event) {
            (AUTOMOD_FLOW, "list", "create") => {
                let name = line(&input["name"], 60).ok_or_else(|| refused("A rule needs a name."))?;
                let rules = self.store.automod_rules().map_err(failed)?;
                if rules.len() >= 50 {
                    return Err(refused("That is as many rules as AutoMod holds."));
                }
                let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
                let rule = json!({ "id": id, "name": name, "enabled": false, "words": [], "patterns": [], "links": false, "allowed_domains": [], "block": true, "hold": false, "exempt_roles": [], "exempt_channels": [] });
                self.store.save_automod_rule(&id, rules.len() as i64, &rule).map_err(failed)?;
                flow.data["rule"] = json!(id);
                flow.state = "edit".into();
                Ok(false)
            }
            (AUTOMOD_FLOW, "list", "preset") => {
                let Some((_, name, _, rule)) = crate::automod::PRESETS.iter().find(|(key, ..)| input["preset"] == *key) else {
                    return Err(refused("There is no such ready-made rule."));
                };
                let rules = self.store.automod_rules().map_err(failed)?;
                if rules.len() >= 50 {
                    return Err(refused("That is as many rules as AutoMod holds."));
                }
                let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
                let mut rule = rule();
                rule["id"] = json!(id);
                rule["name"] = json!(name);
                rule["enabled"] = json!(true);
                self.store.save_automod_rule(&id, rules.len() as i64, &rule).map_err(failed)?;
                self.audit(now, &flow.owner, "community.automod", None, Some(rule.clone()));
                flow.data["rule"] = json!(id);
                flow.state = "edit".into();
                Ok(false)
            }
            (AUTOMOD_FLOW, "list", "edit") => {
                let rule = self.automod_rule(input["rule"].as_str().unwrap_or_default())?;
                flow.data["rule"] = rule["id"].clone();
                flow.state = "edit".into();
                Ok(false)
            }
            (AUTOMOD_FLOW, "list", "toggle") => {
                let mut rule = self.automod_rule(input["rule"].as_str().unwrap_or_default())?;
                let on = rule["enabled"] != true;
                rule["enabled"] = json!(on);
                let position = self.store.automod_rules().map_err(failed)?.iter().position(|r| r["id"] == rule["id"]).unwrap_or(0) as i64;
                self.store.save_automod_rule(rule["id"].as_str().unwrap_or_default(), position, &rule).map_err(failed)?;
                self.audit(now, &flow.owner, "community.automod", None, Some(json!({ "rule": rule["name"], "enabled": on })));
                Ok(false)
            }
            (AUTOMOD_FLOW, "list", "done") => {
                flow.status = "completed".into();
                Ok(false)
            }
            (AUTOMOD_FLOW, "edit", "save") => {
                let held = self.automod_rule(flow.data["rule"].as_str().unwrap_or_default())?;
                let id = held["id"].as_str().unwrap_or_default().to_string();
                let name = line(&input["name"], 60).ok_or_else(|| refused("A rule needs a name."))?;
                let patterns = words("patterns", &['\n']);
                for pattern in &patterns {
                    crate::automod::compile(pattern).map_err(|why| refused(&format!("“{pattern}” isn't a pattern this can use: {why}")))?;
                }
                let alert = match line(&input["alert_channel"], 40).map(|name| name.trim_start_matches('#').to_string()) {
                    Some(name) => Some(self.store.channel_named(&name).map_err(failed)?.filter(|c| c.voice.is_none()).ok_or_else(|| refused("There is no text channel by that name."))?.id),
                    None => None,
                };
                let timeout = match input["timeout_minutes"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
                    Some(text) => Some(text.parse::<i64>().ok().filter(|m| (1..=40_320).contains(m)).ok_or_else(|| refused("A timeout is from 1 minute to 28 days (40,320 minutes)."))? * 60),
                    None => None,
                };
                let mentions = match input["max_mentions"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
                    Some(text) => Some(text.parse::<u64>().ok().filter(|n| (1..=50).contains(n)).ok_or_else(|| refused("The most mentions is a number from 1 to 50."))?),
                    None => None,
                };
                let roles: Vec<String> = self.store.roles().map_err(failed)?.into_iter().filter(|role| ticked(&input[format!("exempt-role-{}", role.id)])).map(|role| role.id).collect();
                let channels: Vec<String> = self.store.channels().map_err(failed)?.into_iter().filter(|c| ticked(&input[format!("exempt-channel-{}", c.id)])).map(|c| c.id).collect();
                let mut filters = serde_json::Map::new();
                for (key, _, _) in crate::filters::FILTERS {
                    if let Some(level) = input[format!("filter-{key}")].as_str().filter(|level| crate::filters::STRICTNESS.iter().any(|(known, _)| known == level)) {
                        filters.insert(key.to_string(), json!(level));
                    }
                }
                let lists: Vec<&str> = crate::filters::DOMAIN_LISTS.iter().map(|(key, _, _)| *key).filter(|key| ticked(&input[format!("list-{key}")])).collect();
                // (A form from before the drop-down says it in check-boxes.)
                let action = match input["action"].as_str().filter(|action| crate::automod::ACTIONS.iter().any(|(known, _)| known == action)) {
                    Some(action) => action,
                    None if ticked(&input["block"]) => "block",
                    None if ticked(&input["hold"]) => "hold",
                    None if ticked(&input["censor"]) => "censor",
                    None => "note",
                };
                let rule = json!({
                    "id": id, "name": name, "enabled": ticked(&input["enabled"]),
                    "filters": filters, "words": words("words", &['\n', ',']), "allowed_words": words("allowed_words", &['\n', ',']), "patterns": patterns,
                    "links": ticked(&input["links"]), "domain_lists": lists, "link_words": ticked(&input["link_words"]),
                    "allowed_domains": words("allowed_domains", &[',', ' ']), "blocked_domains": words("blocked_domains", &[',', ' ']),
                    "max_mentions": mentions, "block": action == "block", "hold": action == "hold", "censor": action == "censor",
                    "alert_channel": alert, "timeout_seconds": timeout, "exempt_roles": roles, "exempt_channels": channels,
                });
                let position = self.store.automod_rules().map_err(failed)?.iter().position(|r| r["id"] == id.as_str()).unwrap_or(0) as i64;
                self.store.save_automod_rule(&id, position, &rule).map_err(failed)?;
                self.audit(now, &flow.owner, "community.automod", None, Some(rule.clone()));
                flow.data["note"] = json!(format!("Saved {name}."));
                flow.state = "list".into();
                Ok(false)
            }
            (AUTOMOD_FLOW, "edit", "delete") => {
                let rule = self.automod_rule(flow.data["rule"].as_str().unwrap_or_default())?;
                self.store.delete_automod_rule(rule["id"].as_str().unwrap_or_default()).map_err(failed)?;
                self.audit(now, &flow.owner, "community.automod", None, Some(json!({ "deleted": rule["name"] })));
                flow.data["note"] = json!(format!("Deleted {}.", rule["name"].as_str().unwrap_or("the rule")));
                flow.state = "list".into();
                Ok(false)
            }
            (AUTOMOD_FLOW, "edit", "back") => {
                flow.state = "list".into();
                Ok(false)
            }
            (JOIN_GATES_FLOW, "form", "save") => {
                let post_wait = match input["post_wait"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
                    Some(text) => Some(text.parse::<i64>().ok().filter(|m| (0..=10_080).contains(m)).ok_or_else(|| refused("The wait is from 0 minutes to a week (10,080)."))?),
                    None => None,
                };
                let mut questions = Vec::new();
                for n in 0..crate::gates::MAX_QUESTIONS {
                    let Some(text) = line(&input[format!("q{n}")], 200) else { continue };
                    let options = words(&format!("q{n}-options"), &['|']);
                    if options.len() > 10 {
                        return Err(refused("A question offers at most ten answers."));
                    }
                    let mut question = json!({ "id": format!("q{}", n + 1), "text": text, "required": ticked(&input[format!("q{n}-required")]) });
                    if !options.is_empty() {
                        question["options"] = json!(options);
                    }
                    questions.push(question);
                }
                let mut gates = json!({ "questions": questions });
                if let Some(wait) = post_wait.filter(|w| *w > 0) {
                    gates["post_wait"] = json!(wait);
                }
                self.store.set_setting("gates", &gates.to_string()).map_err(failed)?;
                self.audit(now, &flow.owner, "community.gates", None, Some(gates));
                flow.status = "completed".into();
                Ok(true)
            }
            _ => Err(refused("That can't be done here.")),
        }
    }

    // --- Channels' order and categories, and onboarding --------------------------------

    /// The channels as listed now, each with its category: what moving one changes.
    fn channel_order(&self) -> anyhow::Result<Vec<(String, Option<String>)>> {
        let categories = self.store.categories()?;
        Ok(self.store.channels()?.into_iter().map(|c| (c.id, c.category.filter(|id| categories.iter().any(|k| k.id == *id)))).collect())
    }

    fn channels_list_ui(&self) -> anyhow::Result<Value> {
        let channels = self.store.channels()?;
        let categories = self.store.categories()?;
        let mut items = vec![json!({ "type": "title", "text": "Channels and categories" })];
        items.push(json!({ "type": "text", "text": "The order everyone sees them in. Categories only group channels: they change no permissions." }));
        let row = |channel: &crate::store::Channel| {
            let kind = if channel.voice.is_some() { "🔊 " } else { "#" };
            let closed = if channel.closed { " (closed)" } else { "" };
            json!({ "type": "row", "children": [
                { "type": "text", "text": format!("{kind}{}{closed}", channel.name) },
                button("↑", "up", json!({ "channel": channel.id })),
                button("↓", "down", json!({ "channel": channel.id })),
                button("Category…", "move", json!({ "channel": channel.id }))
            ] })
        };
        for channel in channels.iter().filter(|c| c.category.as_ref().is_none_or(|id| !categories.iter().any(|k| k.id == *id))) {
            items.push(row(channel));
        }
        for category in &categories {
            items.push(json!({ "type": "row", "children": [
                { "type": "title", "text": category.name },
                button("↑", "category_up", json!({ "category": category.id })),
                button("↓", "category_down", json!({ "category": category.id })),
                button("Rename…", "edit_category", json!({ "category": category.id }))
            ] }));
            for channel in channels.iter().filter(|c| c.category.as_deref() == Some(category.id.as_str())) {
                items.push(row(channel));
            }
        }
        items.push(json!({ "type": "divider" }));
        items.push(json!({ "type": "form", "children": [
            { "type": "text-input", "form-id": "name", "label": "New category", "placeholder": "Projects", "required": true },
            button("Add category", "create_category", json!({}))
        ] }));
        items.push(button("Done", "done", json!({})));
        Ok(Value::Array(items))
    }

    fn category_ui(&self, flow: &Flow) -> anyhow::Result<Value> {
        let id = flow.data["category"].as_str().unwrap_or_default();
        let name = self.store.categories()?.into_iter().find(|c| c.id == id).map(|c| c.name).unwrap_or_default();
        Ok(json!([
            { "type": "title", "text": format!("Category: {name}") },
            { "type": "form", "children": [
                { "type": "text-input", "form-id": "name", "label": "Name", "value": name, "required": true },
                button("Save", "save", json!({}))
            ] },
            button("Delete category", "delete", json!({})),
            button("Back", "back", json!({}))
        ]))
    }

    fn move_ui(&self, flow: &Flow) -> anyhow::Result<Value> {
        let id = flow.data["channel"].as_str().unwrap_or_default();
        let name = self.store.channel(id)?.map(|c| c.name).unwrap_or_default();
        let mut items = vec![json!({ "type": "title", "text": format!("Move #{name} to") }), button("No category", "to", json!({ "category": "" }))];
        for category in self.store.categories()? {
            items.push(button(&category.name, "to", json!({ "category": category.id })));
        }
        items.push(button("Back", "back", json!({})));
        Ok(Value::Array(items))
    }

    fn onboarding_setup_ui(&self) -> anyhow::Result<Value> {
        let set = self.onboarding()?;
        let suggested: Vec<&str> = set["suggested"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let mut fields = vec![json!({ "type": "text-area", "form-id": "welcome", "label": "Welcome (Markdown)", "value": set["welcome"].as_str().unwrap_or_default() })];
        fields.push(json!({ "type": "title", "text": "Channels to suggest" }));
        for channel in self.store.channels()?.iter().filter(|c| !c.closed) {
            let kind = if channel.voice.is_some() { "🔊 " } else { "#" };
            fields.push(json!({ "type": "check-box", "form-id": format!("suggest-{}", channel.id), "label": format!("{kind}{}", channel.name), "value": suggested.contains(&channel.id.as_str()) }));
        }
        let offered: Vec<String> = self.store.roles()?.into_iter().filter(|role| role.self_assignable).map(|role| role.name).collect();
        fields.push(json!({ "type": "text", "text": if offered.is_empty() {
            "No roles are offered to pick. A role is, once Roles marks it “Members may take it, or drop it, themselves”.".to_string()
        } else {
            format!("Roles members may pick: {}. (Which, is each role's own setting, in Roles.)", offered.join(", "))
        } }));
        fields.push(button("Save", "save", json!({})));
        Ok(json!([{ "type": "title", "text": "Onboarding" }, { "type": "text", "text": "What a newcomer is shown as they come in: a welcome, roles to pick, and channels to start in." }, { "type": "form", "children": fields }]))
    }

    fn onboarding_ui(&self, flow: &Flow, state: &str) -> anyhow::Result<(Value, Vec<&'static str>)> {
        let set = self.onboarding()?;
        let name = self.changed().name.unwrap_or_else(|| self.config.name.clone());
        Ok(match state {
            "welcome" => {
                let mut items = vec![json!({ "type": "title", "text": format!("Welcome to {name}") })];
                if let Some(welcome) = set["welcome"].as_str().filter(|w| !w.trim().is_empty()) {
                    items.push(json!({ "type": "text", "text": welcome }));
                }
                if self.rules()?.is_some() {
                    items.push(json!({ "type": "information", "text": "This community has rules: they are in its menu." }));
                }
                items.push(button("Next", "next", json!({})));
                (Value::Array(items), vec!["next"])
            }
            "roles" => {
                let held = self.store.member_roles(&flow.owner)?;
                let mut fields = Vec::new();
                for role in self.store.roles()?.into_iter().filter(|role| role.self_assignable) {
                    let label = match &role.icon {
                        Some(icon) => format!("{icon} {}", role.name),
                        None => role.name.clone(),
                    };
                    fields.push(json!({ "type": "check-box", "form-id": format!("role-{}", role.id), "label": label, "value": held.contains(&role.id) }));
                }
                if fields.is_empty() {
                    fields.push(json!({ "type": "text", "text": "There are no roles to pick here." }));
                }
                fields.push(button("Next", "pick", json!({})));
                (json!([{ "type": "title", "text": "Pick your roles" }, { "type": "form", "children": fields }]), vec!["pick"])
            }
            _ => {
                let suggested: Vec<String> = set["suggested"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
                let mut items = vec![json!({ "type": "title", "text": "Where to start" })];
                let seen = self.seen_by(&flow.owner)?;
                for channel in self.store.channels()?.into_iter().filter(|c| suggested.contains(&c.id) && !c.closed && seen.contains(&c.id)) {
                    let kind = if channel.voice.is_some() { "🔊 " } else { "#" };
                    let topic = if channel.topic.is_empty() { String::new() } else { format!(" — {}", channel.topic) };
                    items.push(json!({ "type": "text", "text": format!("{kind}{}{topic}", channel.name) }));
                }
                if items.len() == 1 {
                    items.push(json!({ "type": "text", "text": "Every channel is open to you: look around." }));
                }
                items.push(button("Done", "done", json!({})));
                (Value::Array(items), vec!["done"])
            }
        })
    }

    /// An event of the `channels`, `onboarding-setup` or `onboarding` workflow;
    /// whether `community` changed for anyone.
    fn advance_structure_flow(&self, flow: &mut Flow, event: &str, input: &Value, now: i64) -> Result<bool, Refusal> {
        match (flow.template.as_str(), flow.state.as_str(), event) {
            (CHANNELS_FLOW, "list", "up" | "down") => {
                let id = input["channel"].as_str().unwrap_or_default();
                let mut order = self.channel_order().map_err(failed)?;
                let at = order.iter().position(|(c, _)| c == id).ok_or_else(|| refused("There is no such channel."))?;
                let category = order[at].1.clone();
                // Past the next channel of the same group, if there is one.
                let other = if event == "up" {
                    order[..at].iter().rposition(|(_, k)| *k == category)
                } else {
                    order[at + 1..].iter().position(|(_, k)| *k == category).map(|n| at + 1 + n)
                };
                let Some(other) = other else { return Err(refused(if event == "up" { "It is first already." } else { "It is last already." })) };
                order.swap(at, other);
                self.store.order_channels(&order).map_err(failed)?;
                self.audit(now, &flow.owner, "channel.update", Some(id), Some(json!({ "moved": event })));
                Ok(true)
            }
            (CHANNELS_FLOW, "list", "create_category") => {
                let name = line(&input["name"], 40).ok_or_else(|| refused("A category needs a name."))?;
                let categories = self.store.categories().map_err(failed)?;
                if categories.len() >= 50 {
                    return Err(refused("That is as many categories as a community holds."));
                }
                let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
                let position = categories.iter().map(|c| c.position).max().unwrap_or(-1) + 1;
                self.store.save_category(&crate::store::Category { id: id.clone(), name: name.clone(), position }).map_err(failed)?;
                self.audit(now, &flow.owner, "channel.update", None, Some(json!({ "category": name, "created": true })));
                Ok(true)
            }
            (CHANNELS_FLOW, "list", "category_up" | "category_down") => {
                let id = input["category"].as_str().unwrap_or_default();
                let mut categories = self.store.categories().map_err(failed)?;
                let at = categories.iter().position(|c| c.id == id).ok_or_else(|| refused("There is no such category."))?;
                let other = if event == "category_up" { at.checked_sub(1) } else { Some(at + 1).filter(|n| *n < categories.len()) };
                let Some(other) = other else { return Err(refused("It can't go further.")) };
                categories.swap(at, other);
                for (n, category) in categories.iter_mut().enumerate() {
                    category.position = n as i64;
                    self.store.save_category(category).map_err(failed)?;
                }
                Ok(true)
            }
            (CHANNELS_FLOW, "list", "edit_category") => {
                let id = input["category"].as_str().unwrap_or_default();
                if !self.store.categories().map_err(failed)?.iter().any(|c| c.id == id) {
                    return Err(refused("There is no such category."));
                }
                flow.data["category"] = json!(id);
                flow.state = "category".into();
                Ok(false)
            }
            (CHANNELS_FLOW, "list", "move") => {
                let id = input["channel"].as_str().unwrap_or_default();
                if self.store.channel(id).map_err(failed)?.is_none() {
                    return Err(refused("There is no such channel."));
                }
                flow.data["channel"] = json!(id);
                flow.state = "move".into();
                Ok(false)
            }
            (CHANNELS_FLOW, "list", "done") => {
                flow.status = "completed".into();
                Ok(false)
            }
            (CHANNELS_FLOW, "category", "save") => {
                let id = flow.data["category"].as_str().unwrap_or_default().to_string();
                let name = line(&input["name"], 40).ok_or_else(|| refused("A category needs a name."))?;
                let mut category = self.store.categories().map_err(failed)?.into_iter().find(|c| c.id == id).ok_or_else(|| refused("That category is gone."))?;
                category.name = name;
                self.store.save_category(&category).map_err(failed)?;
                flow.state = "list".into();
                Ok(true)
            }
            (CHANNELS_FLOW, "category", "delete") => {
                let id = flow.data["category"].as_str().unwrap_or_default().to_string();
                self.store.delete_category(&id).map_err(failed)?;
                flow.state = "list".into();
                Ok(true)
            }
            (CHANNELS_FLOW, "move", "to") => {
                let id = flow.data["channel"].as_str().unwrap_or_default().to_string();
                let target = input["category"].as_str().filter(|c| !c.is_empty()).map(str::to_string);
                if let Some(target) = &target {
                    if !self.store.categories().map_err(failed)?.iter().any(|c| c.id == *target) {
                        return Err(refused("There is no such category."));
                    }
                }
                // Last in its new group.
                let mut order = self.channel_order().map_err(failed)?;
                order.retain(|(c, _)| *c != id);
                let after = order.iter().rposition(|(_, k)| *k == target).map_or(order.len(), |n| n + 1);
                order.insert(after.min(order.len()), (id.clone(), target.clone()));
                self.store.order_channels(&order).map_err(failed)?;
                self.audit(now, &flow.owner, "channel.update", Some(&id), Some(json!({ "category": target })));
                flow.state = "list".into();
                Ok(true)
            }
            (CHANNELS_FLOW, _, "back") => {
                flow.state = "list".into();
                Ok(false)
            }
            (ONBOARDING_SETUP_FLOW, "form", "save") => {
                let welcome: String = input["welcome"].as_str().unwrap_or_default().trim().chars().take(4000).collect();
                let suggested: Vec<String> = self.store.channels().map_err(failed)?.into_iter().filter(|c| ticked(&input[format!("suggest-{}", c.id)])).map(|c| c.id).collect();
                let set = json!({ "welcome": welcome, "suggested": suggested });
                self.store.set_setting("onboarding", &set.to_string()).map_err(failed)?;
                self.audit(now, &flow.owner, "community.onboarding", None, Some(set));
                flow.status = "completed".into();
                Ok(true)
            }
            (ONBOARDING_FLOW, "welcome", "next") => {
                flow.state = "roles".into();
                Ok(false)
            }
            (ONBOARDING_FLOW, "roles", "pick") => {
                let held = self.store.member_roles(&flow.owner).map_err(failed)?;
                let mut changed = false;
                for role in self.store.roles().map_err(failed)?.into_iter().filter(|role| role.self_assignable) {
                    let wanted = ticked(&input[format!("role-{}", role.id)]);
                    match (wanted, held.contains(&role.id)) {
                        (true, false) => self.store.give_role(&flow.owner, &role.id).map_err(failed)?,
                        (false, true) => self.store.take_role(&flow.owner, &role.id).map_err(failed)?,
                        _ => continue,
                    }
                    changed = true;
                }
                flow.state = "channels".into();
                Ok(changed)
            }
            (ONBOARDING_FLOW, "channels", "done") => {
                self.store.set_onboarded(&flow.owner, now).map_err(failed)?;
                flow.status = "completed".into();
                Ok(true)
            }
            _ => Err(refused("That can't be done here.")),
        }
    }
}

/// What the permissions workflow sets (`flow_target`).
struct Target {
    /// Its overrides' key: a channel's id, or `category:<id>`.
    key: String,
    /// "#general", "🔊lounge" or "the category Art".
    title: String,
    voice: bool,
    /// The channel, where it is one.
    channel: Option<Channel>,
    /// The category: the one set, or the channel's.
    category: Option<crate::store::Category>,
}
