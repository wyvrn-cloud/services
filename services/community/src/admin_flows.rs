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

pub const ROLES: &str = "roles";
pub const MEMBER_ROLES: &str = "member-roles";
pub const CHANNEL_PERMISSIONS_FLOW: &str = "channel-permissions";
pub const VERSION: &str = "1.0";
const P: &str = "https://didcomm.org/workflow/1.0/";
const MAX_ROLES: usize = 100;
const MAX_ROLE_NAME: usize = 32;

/// Each template: its id, title, states and the events that move between them.
fn templates() -> [(&'static str, &'static str, Value); 3] {
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
                    "read_only": { "from": "roles", "to": "roles" }, "unlock": { "from": "roles", "to": "roles" },
                    "edit": { "from": "roles", "to": "role" }, "done": { "from": "roles", "to": "done" },
                    "save": { "from": "role", "to": "roles" }, "back": { "from": "role", "to": "roles" }
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
                if role.id != EVERYONE_ROLE {
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
        let mut events = vec!["edit", "create", "raise", "lower", "done"];
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
        let channel = self.flow_channel(flow)?;
        let overrides = self.store.overrides(&channel.id).map_err(failed)?;
        let read_only = self.read_only(&channel.id).map_err(failed)?;
        let mut items = vec![json!({ "type": "title", "text": format!("Permissions in #{}", channel.name) })];
        items.push(json!({ "type": "text", "text": "A channel can allow or deny each role what members do in channels. Allowing wins over denying, a role's own over everyone's. It never hides the channel: every member can read it." }));
        if channel.voice.is_none() {
            items.push(if read_only {
                json!({ "type": "row", "children": [{ "type": "text", "text": "Read-only: only roles allowed to post here can." }, button("Let everyone post", "unlock", json!({}))] })
            } else {
                button("Make read-only", "read_only", json!({}))
            });
        }
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
        let channel = self.flow_channel(flow)?;
        let role_id = flow.data["role"].as_str().unwrap_or_default();
        let role = self.store.role(role_id).map_err(failed)?.ok_or_else(|| refused("That role is gone."))?;
        let said = self.store.overrides(&channel.id).map_err(failed)?.into_iter().find(|o| o.role == role.id);
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
            { "type": "title", "text": format!("{} in #{}", role.name, channel.name) },
            { "type": "text", "text": "Neither ticked: as the role has it everywhere." },
            { "type": "form", "children": fields },
            button("Back", "back", json!({}))
        ]))
    }

    /// A workflow's `status`, as its owner is shown it now.
    fn flow_status(&self, flow: &Flow) -> Result<Value, Refusal> {
        let powers = self.powers(&flow.owner).map_err(failed)?;
        let (ui, events): (Value, Vec<&str>) = if flow.status != "active" {
            (json!([{ "type": "pass", "text": if flow.status == "completed" { "Done." } else { "Closed." } }]), Vec::new())
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
                (CHANNEL_PERMISSIONS_FLOW, "roles") => (self.channel_roles_ui(flow, &powers)?, vec!["read_only", "unlock", "edit", "done"]),
                (CHANNEL_PERMISSIONS_FLOW, "role") => {
                    let channel = self.flow_channel(flow)?;
                    let held = self.channel_permissions(&flow.owner, &powers, &channel).map_err(failed)?;
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
            CHANNEL_PERMISSIONS_FLOW => {
                let id = body["context"]["channel"].as_str().unwrap_or_default();
                let channel = self.store.channel(id).map_err(failed)?.ok_or_else(|| refused("There is no such channel."))?;
                if !self.may_manage_channel(&member.did, &powers, &channel) {
                    return Err(not_allowed("Only someone who manages channels, or whoever opened this one, sets its permissions."));
                }
                json!({ "channel": id })
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
            ROLES => "list",
            MEMBER_ROLES => "pick",
            _ => "roles",
        };
        let flow = Flow { id: wanted.into(), owner: member.did.clone(), template: template.into(), context, state: state.into(), data: json!({}), status: "active".into(), updated: now };
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
            CHANNEL_PERMISSIONS_FLOW => self.may_manage_channel(&flow.owner, &powers, &self.flow_channel(&flow)?),
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
                true
            }
            (ROLES, "list", "hand_over") => {
                if !powers.owner {
                    return Err(not_allowed("Only the owner hands the community on."));
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
                flow.data["note"] = json!(format!("{} owns the community now.", heir.name.clone().unwrap_or_else(|| heir.did.clone())));
                true
            }
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
                for role in self.store.roles().map_err(failed)?.iter().filter(|role| role.id != EVERYONE_ROLE && powers.above(role.position)) {
                    let wanted = ticked(&input[format!("role-{}", role.id)]);
                    match (wanted, held.contains(&role.id)) {
                        (true, false) => self.store.give_role(&member.did, &role.id).map_err(failed)?,
                        (false, true) => self.store.take_role(&member.did, &role.id).map_err(failed)?,
                        _ => {}
                    }
                }
                flow.status = "completed".into();
                true
            }
            (CHANNEL_PERMISSIONS_FLOW, "roles", "read_only" | "unlock") => {
                let channel = self.flow_channel(&flow)?;
                let said = self.store.overrides(&channel.id).map_err(failed)?.into_iter().find(|o| o.role == EVERYONE_ROLE);
                let (mut allow, mut deny) = said.map(|o| (o.allow, o.deny)).unwrap_or_default();
                allow.retain(|p| p != "post");
                deny.retain(|p| p != "post");
                if event == "read_only" {
                    deny.push("post".into());
                }
                self.store.set_override(&channel.id, EVERYONE_ROLE, &allow, &deny).map_err(failed)?;
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
                let channel = self.flow_channel(&flow)?;
                let role_id = flow.data["role"].as_str().unwrap_or_default().to_string();
                let held = self.channel_permissions(&flow.owner, &powers, &channel).map_err(failed)?;
                let said = self.store.overrides(&channel.id).map_err(failed)?.into_iter().find(|o| o.role == role_id);
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
                self.store.set_override(&channel.id, &role_id, &allow, &deny).map_err(failed)?;
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
}
