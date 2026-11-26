//! Importing a Discord server template (community/1.0's "Importing a Discord
//! template"): its roles, categories and channels, with their permissions, made
//! here as they are there, as near as Wyvrn has them; and imported again, what
//! changed in the template since is changed here too.
//!
//! What the template says of each thing is kept (`imported`), with what was set
//! here from it, whatever Wyvrn couldn't take yet included: imported again once
//! Wyvrn has it (stage channels, say), it comes over. Imported again, each field
//! of each thing is one of:
//!
//! - as the template has it already: nothing to do;
//! - not changed here since: the template's is taken;
//! - changed here, not in the template: what's here is kept;
//! - changed both here and in the template: the owner is asked which wins.
//!
//! Something no longer in the template is removed only where the owner says.
//! It is a workflow (`discord-import`), run by an owner: a link, then what would
//! be done, then done.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{json, Map, Value};

use crate::community::{line, Community};
use crate::questions::{failed, Refusal};
use crate::roles::{is_channel_permission, is_permission, EVERYONE_ROLE};
use crate::store::{Category, Flow, Imported, RoleDef};

pub const DISCORD_IMPORT_FLOW: &str = "discord-import";

/// Each Discord permission (its bit, its name in Discord's settings) and what it
/// is here; none where Wyvrn has nothing like it yet.
pub const PERMISSIONS: &[(u32, &str, &[&str])] = &[
    (0, "Create Invite", &["create_invites"]),
    (1, "Kick Members", &["kick_members"]),
    (2, "Ban Members", &["ban_members"]),
    (3, "Administrator", &["administrator"]),
    (4, "Manage Channels", &["manage_channels", "create_channels"]),
    (5, "Manage Server", &["manage_community", "manage_automod", "admit_members"]),
    (6, "Add Reactions", &["react"]),
    (7, "View Audit Log", &["view_audit_log"]),
    (8, "Priority Speaker", &[]),
    (9, "Video", &["video"]),
    (10, "View Channel", &["view_channel"]),
    (11, "Send Messages", &["post"]),
    (12, "Send Text-to-Speech Messages", &[]),
    (13, "Manage Messages", &["manage_messages"]),
    (14, "Embed Links", &[]),
    (15, "Attach Files", &["attach_files"]),
    (16, "Read Message History", &[]),
    (17, "Mention @everyone, @here and All Roles", &["mention_everyone"]),
    (18, "Use External Emoji", &[]),
    (19, "View Server Insights", &["view_insights"]),
    (20, "Connect", &["connect"]),
    (21, "Speak", &["speak"]),
    (22, "Mute Members", &["moderate_voice"]),
    (23, "Deafen Members", &["moderate_voice"]),
    (24, "Move Members", &["moderate_voice"]),
    (25, "Use Voice Activity", &[]),
    (26, "Change Nickname", &[]),
    (27, "Manage Nicknames", &[]),
    (28, "Manage Roles", &["manage_roles"]),
    (29, "Manage Webhooks", &["manage_webhooks"]),
    (30, "Manage Expressions", &["manage_expressions"]),
    (31, "Use Application Commands", &[]),
    (32, "Request to Speak", &[]),
    (33, "Manage Events", &["manage_events"]),
    (34, "Manage Threads", &["manage_threads"]),
    (35, "Create Public Threads", &["create_threads"]),
    (36, "Create Private Threads", &["create_private_threads"]),
    (37, "Use External Stickers", &[]),
    (38, "Send Messages in Threads", &["send_in_threads"]),
    (39, "Use Activities", &[]),
    (40, "Timeout Members", &["timeout_members", "manage_reports"]),
    (41, "View Creator Monetization Analytics", &[]),
    (42, "Use Soundboard", &[]),
    (43, "Create Expressions", &[]),
    (44, "Create Events", &["create_events"]),
    (45, "Use External Sounds", &[]),
    (46, "Send Voice Messages", &[]),
    (48, "Set Voice Channel Status", &[]),
    (49, "Create Polls", &["create_polls"]),
    (50, "Use External Apps", &[]),
    (51, "Pin Messages", &[]),
    (52, "Bypass Slowmode", &["bypass_slow_mode"]),
];

/// The template's code in a link to it (`discord.new/CODE`,
/// `discord.com/template/CODE`), or the code itself.
pub fn template_code(link: &str) -> Option<String> {
    let link = link.trim().trim_end_matches('/');
    let code = link.rsplit('/').next().unwrap_or(link);
    let code = code.split(['?', '#']).next().unwrap_or_default();
    let known = link == code || ["discord.new/", "discord.com/template/", "discordapp.com/template/"].iter().any(|host| link.contains(host));
    (known && (2..=64).contains(&code.len()) && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')).then(|| code.to_string())
}

/// The template `code`, as Discord's API (`api`) has it: its JSON, or why not.
pub async fn fetch(api: &str, code: &str) -> Result<Value, String> {
    let url = format!("{}/guilds/templates/{code}", api.trim_end_matches('/'));
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
    let response = client.get(&url).header("User-Agent", "wyvrn-community (https://wyvrn.app)").send().await.map_err(|_| "Discord couldn't be reached. Try again in a moment.".to_string())?;
    match response.status().as_u16() {
        200 => {}
        404 => return Err("Discord has no template by that link.".into()),
        429 => return Err("Discord asks to wait a little before asking again.".into()),
        status => return Err(format!("Discord answered {status}.")),
    }
    let template: Value = response.json().await.map_err(|_| "Discord's answer wasn't a template.".to_string())?;
    if !template["serialized_source_guild"].is_object() {
        return Err("Discord's answer wasn't a template.".into());
    }
    Ok(template)
}

/// Before a workflow message is handled: `read` in a template import of
/// `member`'s has the template fetched from Discord here, and put in its input
/// (`fetched`, or why not, `fetch_problem`), whatever the app put there.
pub async fn prefetch(community: &Community, member: &str, message: &mut Value) {
    let is_read = message["type"].as_str().is_some_and(|t| t.ends_with("/advance")) && message["body"]["event"] == "read";
    if !is_read {
        return;
    }
    let instance = message["body"]["instance_id"].as_str().unwrap_or_default();
    let ours = community.store.flow(instance).ok().flatten().is_some_and(|flow| flow.owner == member && flow.template == DISCORD_IMPORT_FLOW);
    if !ours {
        return;
    }
    if !message["body"]["input"].is_object() {
        message["body"]["input"] = json!({});
    }
    let input = &mut message["body"]["input"];
    input.as_object_mut().map(|i| {
        i.remove("fetched");
        i.remove("fetch_problem")
    });
    match template_code(input["link"].as_str().unwrap_or_default()) {
        None => input["fetch_problem"] = json!("That isn't a link to a Discord template: it looks like https://discord.new/ and a code."),
        Some(code) => match fetch(&community.config.discord_api, &code).await {
            Ok(template) => input["fetched"] = template,
            Err(problem) => input["fetch_problem"] = json!(problem),
        },
    }
}

/// A Discord permission set, as its number.
pub(crate) fn bits(value: &Value) -> u64 {
    match value {
        Value::String(text) => text.parse().unwrap_or(0),
        Value::Number(n) => n.as_u64().unwrap_or(0),
        _ => 0,
    }
}

/// What `bits` are here, in order.
pub(crate) fn mapped(bits: u64) -> BTreeSet<String> {
    PERMISSIONS.iter().filter(|(bit, _, _)| bits >> bit & 1 == 1).flat_map(|(_, _, ours)| ours.iter()).filter(|p| is_permission(p)).map(|p| p.to_string()).collect()
}

/// The names of `bits` Wyvrn has nothing for yet.
fn unmapped(bits: u64) -> Vec<&'static str> {
    PERMISSIONS.iter().filter(|(bit, _, ours)| bits >> bit & 1 == 1 && ours.is_empty()).map(|(_, name, _)| *name).collect()
}

/// What Discord left out of the template, said for the owner. A template leaves
/// out a community server's announcement and stage channels, saying nothing of
/// them, but it keeps every other channel's place: text-like channels (text,
/// forum, media) are numbered 0, 1, 2, ... in one run and voice-like ones in
/// another, so a number missing from a run is a channel left out there (an
/// announcement channel among the text, a stage among the voice). Its ids, 1, 2,
/// 3, ..., skip those too, and sometimes more, which have no place to tell.
fn left_out(template: &Value) -> Option<String> {
    let channels = template["serialized_source_guild"]["channels"].as_array()?;
    let by_id: HashMap<i64, &Value> = channels.iter().filter_map(|c| Some((c["id"].as_i64()?, c))).collect();
    let skipped = (1..*by_id.keys().max()?).filter(|id| !by_id.contains_key(id)).count();
    let name = |c: &Value| c["name"].as_str().unwrap_or("?").to_string();
    let category = |c: &Value| c["parent_id"].as_i64().and_then(|p| by_id.get(&p)).map(|p| name(p));
    let mut places = Vec::new();
    let mut placed = 0;
    for (voice, mark, likely) in [(false, "#", "announcement"), (true, "🔊", "stage")] {
        let mut run: Vec<(i64, &Value)> = channels
            .iter()
            .filter(|c| { let kind = kind_of(c).0; kind != "category" && (kind == "voice") == voice })
            .filter_map(|c| Some((c["position"].as_i64()?, c)))
            .collect();
        run.sort_by_key(|(at, _)| *at);
        let mut expected = 0;
        let mut before: Option<&Value> = None;
        for (at, c) in run {
            if at > expected {
                let n = (at - expected) as usize;
                placed += n;
                let after = format!("{mark}{}", name(c));
                let place = match (before, category(c)) {
                    (Some(b), cat) if category(b) == cat => format!("between {mark}{} and {after}", name(b)),
                    (_, Some(cat)) => format!("in {cat}, above {after}"),
                    (_, None) => format!("above {after}"),
                };
                let what = if n == 1 { format!("1 {place} (an {likely} channel, most likely)") } else { format!("{n} {place} ({likely} channels, most likely)") };
                places.push(what);
            }
            expected = expected.max(at + 1);
            before = Some(c);
        }
    }
    let total = skipped.max(placed);
    if total == 0 {
        return None;
    }
    if total > placed {
        let n = total - placed;
        places.push(if n == 1 { "and 1 more it gives no place to".to_string() } else { format!("and {n} more it gives no place to") });
    }
    let them = if total == 1 { "1 channel".to_string() } else { format!("{total} channels") };
    Some(format!(
        "Discord left {them} out of this template, and says nothing of what they were but where: {}. It leaves out a community server's announcement and stage channels, so make them here by hand (New channel, then Announcements or Stage).",
        places.join("; ")
    ))
}

/// A Discord channel's kind: `text`, `voice`, `forum` or `category`, and what it
/// is on Discord where that isn't the same (`announcement`, `stage`, `media`).
pub(crate) fn kind_of(channel: &Value) -> (&'static str, Option<&'static str>) {
    match channel["type"].as_i64().unwrap_or(0) {
        2 => ("voice", None),
        4 => ("category", None),
        5 => ("text", Some("announcement")),
        13 => ("voice", Some("stage")),
        15 => ("forum", None),
        16 => ("forum", Some("media")),
        _ => ("text", None),
    }
}

/// The kind of channel a template's channel would be here, as
/// [`crate::community::channel_kind`] names it.
fn wanted_kind(want: &Value) -> &'static str {
    match want["kind"].as_str() {
        Some("voice") if want["stage"] == true => "stage",
        Some("voice") => "voice",
        Some("forum") if want["gallery"] == true => "gallery",
        Some("forum") => "forum",
        _ if want["announcement"] == true => "announcement",
        _ => "text",
    }
}

fn capitalised(word: &str) -> String {
    let mut letters = word.chars();
    letters.next().map(|first| first.to_uppercase().chain(letters).collect()).unwrap_or_default()
}

/// A Discord name, as a text channel here may be called: in lowercase, spaces as
/// `-`, without what a name here can't hold.
pub(crate) fn text_name(name: &str) -> String {
    static NOT: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(&format!("[^{}]", crate::community::NAME_CHARACTERS)).expect("a valid pattern"));
    let spaced: String = name.to_lowercase().split_whitespace().collect::<Vec<_>>().join("-");
    let kept = NOT.replace_all(&spaced, "");
    let kept: String = kept.chars().take(crate::community::MAX_CHANNEL_NAME).collect();
    let kept = kept.trim_matches('-').to_string();
    if kept.is_empty() {
        "channel".into()
    } else {
        kept
    }
}

fn color_of(value: &Value) -> Value {
    match value.as_u64().filter(|n| *n > 0 && *n <= 0xFF_FFFF) {
        Some(n) => json!(format!("#{n:06x}")),
        None => Value::Null,
    }
}

fn sorted(set: impl IntoIterator<Item = String>) -> Value {
    let mut list: Vec<String> = set.into_iter().collect();
    list.sort();
    list.dedup();
    json!(list)
}

/// One thing in the template, matched to what is here, and what would be done.
#[derive(Debug, Clone)]
pub struct Planned {
    pub kind: &'static str,
    pub source_id: String,
    pub source: Value,
    pub label: String,
    /// What it is here, if it is: its id.
    pub local: Option<String>,
    pub record: Option<Imported>,
    /// The template's, as it would be here.
    pub want: Value,
    /// What is here now, where it is.
    pub here: Option<Value>,
    /// Fields to take from the template.
    pub take: Vec<String>,
    /// Fields changed both here and in the template since.
    pub conflicts: Vec<String>,
    /// Imported before, and deleted here since.
    pub deleted_here: bool,
}

impl Planned {
    fn key(&self) -> String {
        format!("{}-{}", self.kind, self.source_id)
    }

    fn is_new(&self) -> bool {
        self.local.is_none()
    }
}

/// What an import did, as it goes.
#[derive(Default)]
struct Counts {
    made: usize,
    changed: usize,
    removed: usize,
    no_room_channels: usize,
    no_room_roles: usize,
}

/// Everything importing would do.
#[derive(Debug, Default)]
pub struct Plan {
    pub items: Vec<Planned>,
    /// Imported before and gone from the template: `(kind, local id, label)`.
    pub removed: Vec<(&'static str, String, String)>,
    /// What Wyvrn doesn't have yet, said once each.
    pub missing: Vec<String>,
}

/// Field by field: what to take from the template, and what is changed on both
/// sides. `applied` is what was set here when last imported (none: never).
fn compare(want: &Value, here: &Value, applied: Option<&Value>, skip: &[&str]) -> (Vec<String>, Vec<String>) {
    let (mut take, mut conflicts) = (Vec::new(), Vec::new());
    for (field, wanted) in want.as_object().into_iter().flatten() {
        if skip.contains(&field.as_str()) {
            continue;
        }
        let now = &here[field];
        if wanted == now {
            continue;
        }
        match applied.map(|a| &a[field]) {
            Some(was) if was == now => take.push(field.clone()),
            Some(was) if was == wanted => {}
            _ => conflicts.push(field.clone()),
        }
    }
    (take, conflicts)
}

/// What a field is called to the owner.
fn field_label(field: &str) -> &str {
    match field {
        "name" => "name",
        "topic" => "topic",
        "category" => "category",
        "slow_mode" => "slow mode",
        "limit" => "how many may be in it",
        "stage" => "whether it is a stage",
        "announcement" => "whether it is an announcement channel",
        "gallery" => "whether it is a gallery",
        "tags" => "tags",
        "overrides" => "its permissions",
        "permissions" => "permissions",
        "color" => "colour",
        "hoist" => "shown apart",
        "mentionable" => "may be mentioned",
        "description" => "description",
        "kind" => "kind",
        other => other,
    }
}

/// A field's value, said shortly.
fn shown(value: &Value) -> String {
    match value {
        Value::Null => "none".into(),
        Value::Bool(true) => "yes".into(),
        Value::Bool(false) => "no".into(),
        Value::String(text) if text.is_empty() => "none".into(),
        Value::String(text) => {
            let short: String = text.chars().take(40).collect();
            if short.len() < text.len() {
                format!("“{short}…”")
            } else {
                format!("“{short}”")
            }
        }
        Value::Number(n) => n.to_string(),
        Value::Array(list) if list.is_empty() => "none".into(),
        Value::Array(list) => format!("{} of them", list.len()),
        Value::Object(map) if map.is_empty() => "none".into(),
        Value::Object(map) => format!("set for {} roles", map.len()),
    }
}

impl Community {
    /// What `did` may see of a template import: only an owner runs one.
    pub fn may_import(&self, did: &str) -> anyhow::Result<bool> {
        self.is_owner(did)
    }

    /// A role as it is compared with the template's.
    fn role_here(role: &RoleDef) -> Value {
        if role.id == EVERYONE_ROLE {
            return json!({ "permissions": sorted(role.permissions.iter().cloned()) });
        }
        json!({ "name": role.name, "color": role.color, "hoist": role.hoist, "mentionable": role.mentionable, "permissions": sorted(role.permissions.iter().cloned()) })
    }

    /// A channel as it is compared with the template's. Its permissions are only
    /// those for roles in `managed` (everyone, and what came from the template).
    fn channel_here(&self, channel: &crate::store::Channel, managed: &HashSet<String>) -> anyhow::Result<Value> {
        let kind = if channel.voice.is_some() {
            "voice"
        } else if channel.forum.is_some() {
            "forum"
        } else {
            "text"
        };
        let mut overrides = Map::new();
        for o in self.store.overrides(&channel.id)?.into_iter().filter(|o| managed.contains(&o.role)) {
            if o.allow.is_empty() && o.deny.is_empty() {
                continue;
            }
            overrides.insert(o.role.clone(), json!({ "allow": sorted(o.allow), "deny": sorted(o.deny) }));
        }
        let mut here = json!({ "kind": kind, "name": channel.name, "topic": channel.topic, "category": channel.category, "slow_mode": channel.slow_mode, "overrides": overrides });
        if let Some(voice) = &channel.voice {
            here["limit"] = voice["limit"].clone();
            here["stage"] = json!(voice["stage"] == true);
        }
        if let Some(tags) = &channel.forum {
            here["tags"] = json!(tags.as_array().into_iter().flatten().map(|t| json!({ "name": t["name"], "emoji": t["emoji"] })).collect::<Vec<_>>());
            here["gallery"] = json!(channel.gallery);
        }
        if kind == "text" {
            here["announcement"] = json!(channel.announcement);
        }
        Ok(here)
    }

    /// What importing `template` would do here now.
    pub fn plan_import(&self, template: &Value) -> anyhow::Result<Plan> {
        let guild = &template["serialized_source_guild"];
        let mut plan = Plan::default();
        let mut missing: Vec<String> = Vec::new();
        let mut miss = |text: String| {
            if !missing.contains(&text) {
                missing.push(text)
            }
        };
        let mut unmapped_permissions: BTreeSet<&str> = BTreeSet::new();

        // The community itself: its name and what it says of itself.
        let name = guild["name"].as_str().or(template["name"].as_str()).unwrap_or("A community");
        let want = json!({ "name": line(&json!(name), 100).unwrap_or_else(|| "A community".into()), "description": guild["description"].as_str().or(template["description"].as_str()).unwrap_or_default() });
        let changed = self.changed();
        let here = json!({ "name": changed.name.unwrap_or_else(|| self.config.name.clone()), "description": changed.description.unwrap_or_else(|| self.config.description.clone()) });
        let record = self.store.imported("community")?.into_iter().next();
        let (take, conflicts) = compare(&want, &here, record.as_ref().map(|r| &r.applied), &[]);
        plan.items.push(Planned { kind: "community", source_id: "guild".into(), source: json!({ "name": name, "description": guild["description"] }), label: "The community".into(), local: Some("community".into()), record, want, here: Some(here), take, conflicts, deleted_here: false });
        for (field, says) in [
            ("verification_level", "Verification level (who may post, by how long they have been there)"),
            ("explicit_content_filter", "Discord's filter of explicit images"),
            ("default_message_notifications", "Default notifications (all posts or mentions only)"),
            ("afk_channel_id", "An AFK voice channel"),
        ] {
            if guild[field].as_i64().is_some_and(|n| n > 0) {
                miss(says.to_string());
            }
        }

        // Roles: `everyone` is ours, the rest are matched by name, else by id.
        let roles_here = self.store.roles()?;
        let mut records: Vec<Imported> = self.store.imported("role")?;
        let mut role_ids: HashMap<String, String> = HashMap::new();
        let mut used: HashSet<String> = HashSet::new();
        for role in guild["roles"].as_array().into_iter().flatten() {
            let source_id = role["id"].to_string();
            let is_everyone = role["id"] == 0 || role["name"] == "@everyone";
            let permissions = bits(&role["permissions"]);
            unmapped_permissions.extend(unmapped(permissions));
            let name = line(&role["name"], 100).unwrap_or_else(|| "A role".into());
            let want = if is_everyone {
                json!({ "permissions": sorted(mapped(permissions)) })
            } else {
                json!({ "name": name, "color": color_of(&role["color"]), "hoist": role["hoist"] == true, "mentionable": role["mentionable"] == true, "permissions": sorted(mapped(permissions)) })
            };
            let at = if is_everyone {
                records.iter().position(|r| r.local == EVERYONE_ROLE)
            } else {
                // (Same name and id first: two roles may have one name. Of two copies
                // of one, the one still called what the template calls it.)
                records.iter().position(|r| r.local != EVERYONE_ROLE && r.source_id == source_id && !used.contains(&r.local) && roles_here.iter().any(|here| here.id == r.local && here.name == name))
                .or_else(|| records.iter().position(|r| r.local != EVERYONE_ROLE && r.source["name"] == role["name"] && r.source_id == source_id && !used.contains(&r.local))).or_else(|| records.iter().position(|r| r.local != EVERYONE_ROLE && r.source["name"] == role["name"] && !used.contains(&r.local))).or_else(|| records.iter().position(|r| r.local != EVERYONE_ROLE && r.source_id == source_id && !used.contains(&r.local)))
            };
            let record = at.map(|at| records.remove(at));
            let local = match &record {
                Some(record) => roles_here.iter().find(|r| r.id == record.local).map(|r| r.id.clone()),
                None if is_everyone => Some(EVERYONE_ROLE.to_string()),
                // Never imported: one of the same name here is it.
                None => roles_here.iter().find(|r| r.id != EVERYONE_ROLE && r.name.to_lowercase() == name.to_lowercase() && !used.contains(&r.id) && !self.store.imported("role").map(|all| all.iter().any(|i| i.local == r.id)).unwrap_or(false)).map(|r| r.id.clone()),
            };
            if let Some(local) = &local {
                used.insert(local.clone());
            }
            let deleted_here = record.is_some() && local.is_none();
            let here = local.as_ref().and_then(|id| roles_here.iter().find(|r| r.id == *id)).map(Self::role_here);
            let (take, conflicts) = match &here {
                Some(here) => compare(&want, here, record.as_ref().map(|r| &r.applied), &[]),
                None => (Vec::new(), Vec::new()),
            };
            role_ids.insert(source_id.clone(), local.clone().unwrap_or_else(|| format!("new:role:{source_id}")));
            let label = if is_everyone { "The role everyone".to_string() } else { format!("The role {name}") };
            plan.items.push(Planned { kind: "role", source_id, source: role.clone(), label, local, record, want, here, take, conflicts, deleted_here });
        }
        for gone in records {
            if let Some(role) = roles_here.iter().find(|r| r.id == gone.local) {
                plan.removed.push(("role", role.id.clone(), format!("The role {}", role.name)));
            }
        }
        let managed: HashSet<String> = role_ids.values().cloned().chain(std::iter::once(EVERYONE_ROLE.to_string())).collect();

        // Categories, then channels.
        let channels: Vec<&Value> = guild["channels"].as_array().into_iter().flatten().collect();
        let categories_here = self.store.categories()?;
        let mut records: Vec<Imported> = self.store.imported("category")?;
        let mut category_ids: HashMap<String, String> = HashMap::new();
        let mut used: HashSet<String> = HashSet::new();
        for category in channels.iter().filter(|c| kind_of(c).0 == "category") {
            let source_id = category["id"].to_string();
            let name = line(&category["name"], 100).unwrap_or_else(|| "Channels".into());
            // Its permissions, which its synced channels have here too (community/1.0's
            // "Category permissions"), as a channel's are read.
            let mut overrides = Map::new();
            for o in category["permission_overwrites"].as_array().into_iter().flatten() {
                if o["type"] != 0 {
                    miss("Permissions a channel gives one person, not a role".into());
                    continue;
                }
                let Some(role) = role_ids.get(&o["id"].to_string()) else { continue };
                let (allow_bits, deny_bits) = (bits(&o["allow"]), bits(&o["deny"]));
                unmapped_permissions.extend(unmapped(allow_bits));
                unmapped_permissions.extend(unmapped(deny_bits));
                let allow: BTreeSet<String> = mapped(allow_bits).into_iter().filter(|p| is_channel_permission(p)).collect();
                let deny: BTreeSet<String> = mapped(deny_bits).into_iter().filter(|p| is_channel_permission(p) && !allow.contains(p)).collect();
                if !allow.is_empty() || !deny.is_empty() {
                    overrides.insert(role.clone(), json!({ "allow": sorted(allow), "deny": sorted(deny) }));
                }
            }
            let want = json!({ "name": name, "overrides": overrides });
            let at = records.iter().position(|r| r.source["name"] == category["name"] && r.source_id == source_id && !used.contains(&r.local)).or_else(|| records.iter().position(|r| r.source["name"] == category["name"] && !used.contains(&r.local))).or_else(|| records.iter().position(|r| r.source_id == source_id && !used.contains(&r.local)));
            let record = at.map(|at| records.remove(at));
            let local = match &record {
                Some(record) => categories_here.iter().find(|c| c.id == record.local).map(|c| c.id.clone()),
                None => categories_here.iter().find(|c| c.name.to_lowercase() == name.to_lowercase() && !used.contains(&c.id) && !self.store.imported("category").map(|all| all.iter().any(|i| i.local == c.id)).unwrap_or(false)).map(|c| c.id.clone()),
            };
            if let Some(local) = &local {
                used.insert(local.clone());
            }
            let here = match local.as_ref().and_then(|id| categories_here.iter().find(|c| c.id == *id)) {
                Some(c) => {
                    let mut overrides = Map::new();
                    for o in self.store.overrides(&crate::roles::category_key(&c.id))?.into_iter().filter(|o| managed.contains(&o.role)) {
                        overrides.insert(o.role.clone(), json!({ "allow": sorted(o.allow), "deny": sorted(o.deny) }));
                    }
                    Some(json!({ "name": c.name, "overrides": overrides }))
                }
                None => None,
            };
            // Imported before categories had permissions: what is here is as it was.
            let applied = record.as_ref().map(|r| {
                let mut applied = r.applied.clone();
                if applied.get("overrides").is_none() {
                    applied["overrides"] = here.as_ref().map(|h| h["overrides"].clone()).unwrap_or_else(|| json!({}));
                }
                applied
            });
            let (take, conflicts) = match &here {
                Some(here) => compare(&want, here, applied.as_ref(), &[]),
                None => (Vec::new(), Vec::new()),
            };
            let deleted_here = record.is_some() && local.is_none();
            category_ids.insert(source_id.clone(), local.clone().unwrap_or_else(|| format!("new:category:{source_id}")));
            plan.items.push(Planned { kind: "category", source_id, source: (*category).clone(), label: format!("The category {name}"), local, record, want, here, take, conflicts, deleted_here });
        }
        for gone in records {
            if let Some(category) = categories_here.iter().find(|c| c.id == gone.local) {
                plan.removed.push(("category", category.id.clone(), format!("The category {}", category.name)));
            }
        }

        let channels_here = self.store.channels()?;
        let mut records: Vec<Imported> = self.store.imported("channel")?;
        let mut used: HashSet<String> = HashSet::new();
        let ever_imported: HashSet<String> = records.iter().map(|r| r.local.clone()).collect();
        for channel in channels.iter().filter(|c| kind_of(c).0 != "category") {
            let source_id = channel["id"].to_string();
            let (kind, as_on_discord) = kind_of(channel);
            let raw_name = channel["name"].as_str().unwrap_or("channel");
            let name = if kind == "voice" { line(&channel["name"], crate::community::MAX_CHANNEL_NAME).unwrap_or_else(|| "Voice".into()) } else { text_name(raw_name) };
            let shown_name = if kind == "voice" { format!("🔊{name}") } else { format!("#{name}") };
            if kind == "voice" && self.voice.is_none() {
                miss(format!("{shown_name}: this server has no voice channels"));
                continue;
            }
            if kind != "voice" && name != raw_name {
                miss(format!("#{raw_name} is called #{name} here: a text channel's name here is one word in lowercase"));
            }
            if channel["nsfw"] == true {
                miss(format!("{shown_name} is age-restricted on Discord"));
            }
            let topic_full = channel["topic"].as_str().unwrap_or_default();
            let topic: String = topic_full.chars().take(crate::community::MAX_TOPIC).collect();
            let category = channel["parent_id"].as_i64().map(|parent| category_ids.get(&parent.to_string()).cloned().unwrap_or_default()).filter(|id| !id.is_empty());
            let mut overrides = Map::new();
            for o in channel["permission_overwrites"].as_array().into_iter().flatten() {
                if o["type"] != 0 {
                    miss("Permissions a channel gives one person, not a role".into());
                    continue;
                }
                let Some(role) = role_ids.get(&o["id"].to_string()) else { continue };
                let (allow_bits, deny_bits) = (bits(&o["allow"]), bits(&o["deny"]));
                unmapped_permissions.extend(unmapped(allow_bits));
                unmapped_permissions.extend(unmapped(deny_bits));
                let allow: BTreeSet<String> = mapped(allow_bits).into_iter().filter(|p| is_channel_permission(p)).collect();
                let deny: BTreeSet<String> = mapped(deny_bits).into_iter().filter(|p| is_channel_permission(p) && !allow.contains(p)).collect();
                if !allow.is_empty() || !deny.is_empty() {
                    overrides.insert(role.clone(), json!({ "allow": sorted(allow), "deny": sorted(deny) }));
                }
            }
            let mut want = json!({ "kind": kind, "name": name, "topic": topic, "category": category, "slow_mode": channel["rate_limit_per_user"].as_i64().unwrap_or(0).clamp(0, crate::moderation::MAX_SLOW_MODE), "overrides": overrides });
            if kind == "voice" {
                let limit = channel["user_limit"].as_i64().filter(|n| *n > 0).map(|n| n.min(500));
                want["limit"] = json!(limit);
                // A stage on Discord is one here (community-voice/1.0's "Stages").
                want["stage"] = json!(as_on_discord == Some("stage"));
                if channel["bitrate"].as_i64().is_some_and(|b| b != 64_000) {
                    miss("A voice channel's sound quality (bitrate)".into());
                }
            }
            // An announcement channel on Discord is one here.
            if kind == "text" {
                want["announcement"] = json!(as_on_discord == Some("announcement"));
            }
            if kind == "forum" {
                // A media channel on Discord is a gallery here.
                want["gallery"] = json!(as_on_discord == Some("media"));
                let tags: Vec<Value> = channel["available_tags"].as_array().into_iter().flatten().take(20).map(|t| json!({ "name": line(&t["name"], 20), "emoji": t["emoji_name"].as_str().filter(|e| crate::community::is_emoji(e)) })).collect();
                want["tags"] = json!(tags);
                for (field, says) in [("default_reaction_emoji", "A forum's default reaction"), ("default_sort_order", "A forum's order"), ("default_forum_layout", "A forum's layout"), ("default_thread_rate_limit_per_user", "A forum's slow mode for posts in it")] {
                    if !channel[field].is_null() && channel[field] != 0 {
                        miss(says.into());
                    }
                }
            }
            // Of two copies of one (#bot-room and #bot-room-2), the one still called
            // what the template calls it.
            let at = records
                .iter()
                .position(|r| r.source_id == source_id && kind_of(&r.source).0 == kind && !used.contains(&r.local) && channels_here.iter().any(|here| here.id == r.local && here.name == name))
                .or_else(|| records.iter().position(|r| r.source["name"] == channel["name"] && kind_of(&r.source).0 == kind && r.source_id == source_id && !used.contains(&r.local)))
                .or_else(|| records.iter().position(|r| r.source["name"] == channel["name"] && kind_of(&r.source).0 == kind && r.source["parent_id"] == channel["parent_id"] && !used.contains(&r.local)))
                .or_else(|| records.iter().position(|r| r.source["name"] == channel["name"] && kind_of(&r.source).0 == kind && !used.contains(&r.local)))
                .or_else(|| records.iter().position(|r| r.source_id == source_id && kind_of(&r.source).0 == kind && !used.contains(&r.local)));
            let record = at.map(|at| records.remove(at));
            let local = match &record {
                Some(record) => channels_here.iter().find(|c| c.id == record.local).map(|c| c.id.clone()),
                None => channels_here
                    .iter()
                    .find(|c| c.name == name && (c.voice.is_some()) == (kind == "voice") && !used.contains(&c.id) && !ever_imported.contains(&c.id))
                    .map(|c| c.id.clone()),
            };
            if let Some(local) = &local {
                used.insert(local.clone());
            }
            let here = match local.as_ref().and_then(|id| channels_here.iter().find(|c| c.id == *id)) {
                Some(found) => Some(self.channel_here(found, &managed)?),
                None => None,
            };
            let (take, conflicts) = match &here {
                Some(here) => {
                    if here["kind"] != want["kind"] {
                        miss(format!("{shown_name} is a {} here and a {} in the template: a channel's kind can't be changed yet", here["kind"].as_str().unwrap_or_default(), kind));
                    }
                    compare(&want, here, record.as_ref().map(|r| &r.applied), &["kind"])
                }
                None => (Vec::new(), Vec::new()),
            };
            // One of a kind this community's host turned off isn't made.
            let wanted = wanted_kind(&want);
            if local.is_none() && self.kind_off(wanted) {
                miss(format!("{} channels: whoever hosts this community has turned them off", capitalised(wanted)));
                continue;
            }
            let deleted_here = record.is_some() && local.is_none();
            plan.items.push(Planned { kind: "channel", source_id, source: (*channel).clone(), label: shown_name, local, record, want, here, take, conflicts, deleted_here });
        }
        for gone in records {
            if let Some(channel) = channels_here.iter().find(|c| c.id == gone.local) {
                let label = if channel.voice.is_some() { format!("🔊{}", channel.name) } else { format!("#{}", channel.name) };
                plan.removed.push(("channel", channel.id.clone(), label));
            }
        }
        if !unmapped_permissions.is_empty() {
            miss(format!("Permissions Wyvrn has nothing like yet: {}", unmapped_permissions.into_iter().collect::<Vec<_>>().join(", ")));
        }
        plan.missing = missing;
        Ok(plan)
    }

    /// `plan`, made to leave everything here as the template has it: every field
    /// that differs is the template's, whatever changed where; what was deleted
    /// here is made again; and everything here that isn't in the template (made
    /// here, a copy, imported and since dropped from it) is to be removed.
    pub fn exactly(&self, mut plan: Plan) -> anyhow::Result<Plan> {
        for item in &mut plan.items {
            item.conflicts.clear();
            item.take = match &item.here {
                Some(here) => item.want.as_object().into_iter().flatten().filter(|(field, wanted)| field.as_str() != "kind" && here[field.as_str()] != **wanted).map(|(field, _)| field.clone()).collect(),
                None => Vec::new(),
            };
        }
        let kept = |kind: &str| -> HashSet<String> { plan.items.iter().filter(|i| i.kind == kind).filter_map(|i| i.local.clone()).collect() };
        let listed: HashSet<(String, String)> = plan.removed.iter().map(|(kind, local, _)| (kind.to_string(), local.clone())).collect();
        let mut extra: Vec<(&'static str, String, String)> = Vec::new();
        let (channels, categories) = (kept("channel"), kept("category"));
        for channel in self.store.channels()? {
            if !channels.contains(&channel.id) && !listed.contains(&("channel".to_string(), channel.id.clone())) {
                let label = if channel.voice.is_some() { format!("🔊{}", channel.name) } else { format!("#{}", channel.name) };
                extra.push(("channel", channel.id, if channel.closed { format!("{label} (closed)") } else { label }));
            }
        }
        for category in self.store.categories()? {
            if !categories.contains(&category.id) && !listed.contains(&("category".to_string(), category.id.clone())) {
                extra.push(("category", category.id, format!("The category {}", category.name)));
            }
        }
        // Roles only if the template's are here at all: `everyone` always stays.
        let roles = kept("role");
        if plan.items.iter().any(|i| i.kind == "role") {
            for role in self.store.roles()? {
                if role.id != EVERYONE_ROLE && !roles.contains(&role.id) && !listed.contains(&("role".to_string(), role.id.clone())) {
                    extra.push(("role", role.id, format!("The role {}", role.name)));
                }
            }
        }
        plan.removed.extend(extra);
        // A role someone holds says so: taking it takes what they may do (an
        // owner keeps everything regardless). Ticked all the same: the template's
        // roles are what there is to be.
        let mut holders: HashMap<String, usize> = HashMap::new();
        for (_, role) in self.store.all_member_roles()? {
            *holders.entry(role).or_default() += 1;
        }
        for (kind, local, label) in &mut plan.removed {
            if *kind == "role" {
                if let Some(n) = holders.get(local.as_str()) {
                    label.push_str(&if *n == 1 { " (1 member has it)".to_string() } else { format!(" ({n} members have it)") });
                }
            }
        }
        Ok(plan)
    }

    /// The workflow's page for its state now.
    pub fn discord_import_ui(&self, flow: &Flow) -> anyhow::Result<(Value, Vec<&'static str>)> {
        let last = self.store.setting("discord_template")?.and_then(|text| serde_json::from_str::<Value>(&text).ok());
        match flow.state.as_str() {
            "review" => {
                let template = &flow.data["template"];
                let plan = self.plan_import(template)?;
                Ok((self.review_ui(template, &plan, false), vec!["import", "exact", "back"]))
            }
            "exact" => {
                let template = &flow.data["template"];
                let plan = self.exactly(self.plan_import(template)?)?;
                Ok((self.review_ui(template, &plan, true), vec!["import", "back"]))
            }
            "done" => Ok((json!([{ "type": "title", "text": "Imported" }, { "type": "text", "text": flow.data["report"].as_str().unwrap_or("Done.") }]), Vec::new())),
            _ => {
                let mut items = vec![
                    json!({ "type": "title", "text": "Import a Discord template" }),
                    json!({ "type": "text", "text": "A Discord server template has its roles, categories and channels, with their permissions: no posts, members or emoji. In Discord: Server Settings, Server Template, then copy its link." }),
                ];
                let mut link = String::new();
                if let Some(last) = &last {
                    items.push(json!({ "type": "text", "text": format!("Imported last: {} (discord.new/{}). Importing it again brings over what changed in it since, and what Wyvrn can take now that it couldn't then.", last["name"].as_str().unwrap_or("a template"), last["code"].as_str().unwrap_or_default()) }));
                    link = format!("https://discord.new/{}", last["code"].as_str().unwrap_or_default());
                }
                if let Some(problem) = flow.data["problem"].as_str() {
                    items.push(json!({ "type": "information", "text": problem }));
                }
                items.push(json!({ "type": "form", "children": [
                    { "type": "text-input", "form-id": "link", "label": "The template's link", "placeholder": "https://discord.new/…", "value": link, "required": true },
                    { "type": "submit-button", "label": "Read it", "event": "read", "input": {} }
                ] }));
                Ok((Value::Array(items), vec!["read"]))
            }
        }
    }

    fn review_ui(&self, template: &Value, plan: &Plan, exact: bool) -> Value {
        let name = template["name"].as_str().unwrap_or("the template");
        let mut items = vec![json!({ "type": "title", "text": if exact { format!("Make it match “{name}”") } else { format!("Import “{name}”") } })];
        if exact {
            items.push(json!({ "type": "text", "text": "Everything here is made as the template has it, as if imported for the first time: whatever differs is changed to the template's, even what was changed here on purpose; what's missing is made, what was deleted here too; and what isn't in the template is removed, below." }));
        }
        let count = |kind: &str, new: bool| plan.items.iter().filter(|i| i.kind == kind && i.is_new() == new && !i.deleted_here).count();
        let order = if exact || first_import(plan) {
            "Channels and roles are put in the template's order."
        } else {
            "What is here stays in the order it has here; what is new goes after it. (Make it match exactly to put everything in the template's order.)"
        };
        items.push(json!({ "type": "text", "text": format!(
            "To be made: {} roles, {} categories, {} channels. Already here: {} roles, {} categories, {} channels. {order}",
            count("role", true), count("category", true), count("channel", true), count("role", false), count("category", false), count("channel", false)
        ) }));
        if let Some(warning) = self.room_warning(plan) {
            items.push(json!({ "type": "information", "text": warning }));
        }
        if let Some(note) = left_out(template) {
            items.push(json!({ "type": "information", "text": note }));
        }
        let mut fields: Vec<Value> = Vec::new();

        let changing: Vec<&Planned> = plan.items.iter().filter(|i| !i.take.is_empty()).collect();
        if !changing.is_empty() {
            fields.push(json!({ "type": "title", "text": "Changed to the template's" }));
            for item in &changing {
                let what: Vec<String> = item.take.iter().map(|f| format!("{} to {}", field_label(f), shown(&item.want[f]))).collect();
                fields.push(json!({ "type": "text", "text": format!("{}: {}", item.label, what.join("; ")) }));
            }
        }
        let asked: Vec<&Planned> = plan.items.iter().filter(|i| asks(i)).collect();
        if !asked.is_empty() {
            fields.push(json!({ "type": "title", "text": "Changed both here and in the template: which stays?" }));
            for item in asked {
                if item.deleted_here {
                    fields.push(json!({ "type": "drop-down", "form-id": format!("choose-{}", item.key()), "label": format!("{}: deleted here, and changed in the template since", item.label), "value": "", "options": [
                        { "value": "", "label": "Choose…" }, { "value": "template", "label": "Bring it back, as the template has it" }, { "value": "here", "label": "Leave it deleted" }
                    ] }));
                    continue;
                }
                let here = item.here.clone().unwrap_or_default();
                let ours: Vec<String> = item.conflicts.iter().map(|f| format!("{} {}", field_label(f), shown(&here[f]))).collect();
                let theirs: Vec<String> = item.conflicts.iter().map(|f| format!("{} {}", field_label(f), shown(&item.want[f]))).collect();
                fields.push(json!({ "type": "drop-down", "form-id": format!("choose-{}", item.key()), "label": item.label, "value": "", "options": [
                    { "value": "", "label": "Choose…" },
                    { "value": "here", "label": format!("What's here: {}", ours.join("; ")) },
                    { "value": "template", "label": format!("The template's: {}", theirs.join("; ")) }
                ] }));
            }
        }
        if !plan.removed.is_empty() {
            if exact {
                fields.push(json!({ "type": "title", "text": "Not in the template" }));
                fields.push(json!({ "type": "text", "text": "Made here, copies, or dropped from the template since. Ticked, it is removed: a channel to the trash, where it can be brought back for 30 days; a role or category deleted, and with a role whatever it let its members do (an owner keeps everything). Untick what should stay." }));
            } else {
                fields.push(json!({ "type": "title", "text": "No longer in the template" }));
                fields.push(json!({ "type": "text", "text": "Ticked, it is removed (a channel to the trash, where it can be brought back for 30 days). Left, it stays, as something made here." }));
            }
            for (kind, local, label) in &plan.removed {
                fields.push(json!({ "type": "check-box", "form-id": format!("remove-{kind}-{local}"), "label": format!("Remove {label}"), "value": exact }));
            }
        }
        fields.push(json!({ "type": "submit-button", "label": if exact { "Make it match" } else { "Import" }, "event": "import", "input": {} }));
        items.push(json!({ "type": "form", "children": fields }));
        if !exact {
            items.push(json!({ "type": "text", "text": "Or make everything here exactly as the template has it: change what differs, even what was changed here, and remove what isn't in it (copies included)." }));
            items.push(json!({ "type": "submit-button", "label": "Make it match exactly...", "event": "exact", "input": {} }));
        }
        if !plan.missing.is_empty() {
            items.push(json!({ "type": "title", "text": "Not in Wyvrn yet" }));
            items.push(json!({ "type": "text", "text": "Kept with what was imported: once Wyvrn has it, import the template again and it comes over." }));
            for missing in &plan.missing {
                items.push(json!({ "type": "text", "text": format!("• {missing}") }));
            }
        }
        items.push(json!({ "type": "submit-button", "label": "Back", "event": "back", "input": {} }));
        Value::Array(items)
    }

    /// What of the template there's no room for here, if anything: channels past
    /// the community's limit (which a hosted community's host sets) and roles past
    /// 250. Those are left out, and come over on importing again once there's room.
    fn room_warning(&self, plan: &Plan) -> Option<String> {
        let new = |kind: &str| plan.items.iter().filter(|i| i.kind == kind && (i.is_new() || i.deleted_here)).count();
        let open = self.store.open_channel_count().ok()?;
        let room = self.config.max_channels.saturating_sub(open);
        let roles_room = self.max_roles().saturating_sub(self.store.roles().ok()?.len());
        let mut said = Vec::new();
        if new("channel") > room {
            said.push(format!(
                "This community may have {} channels and has {open}, so only {room} of the template's {} new channels can be made: the first ones, in the template's order. Whoever hosts the community can let it have more; then import the template again and the rest come over.",
                self.config.max_channels, new("channel")
            ));
        }
        if new("role") > roles_room {
            said.push(format!("A community may have {} roles, so only {roles_room} of the template's {} new roles can be made.", self.max_roles(), new("role")));
        }
        (!said.is_empty()).then(|| said.join(" "))
    }

    /// Advances the import: `read` (the template fetched, as the server put it in
    /// `fetched`), `back`, `import`. Whether the community changed.
    pub fn advance_discord_import(&self, flow: &mut Flow, event: &str, input: &Value, now: i64) -> Result<bool, Refusal> {
        flow.data.as_object_mut().map(|data| data.remove("problem"));
        match (flow.state.as_str(), event) {
            ("link", "read") => {
                if let Some(problem) = input["fetch_problem"].as_str() {
                    flow.data["problem"] = json!(problem);
                    return Ok(false);
                }
                if !input["fetched"]["serialized_source_guild"].is_object() {
                    flow.data["problem"] = json!("That isn't a link to a Discord template: it looks like https://discord.new/ and a code.");
                    return Ok(false);
                }
                flow.data["template"] = input["fetched"].clone();
                flow.state = "review".into();
                Ok(false)
            }
            ("review", "back") => {
                flow.state = "link".into();
                Ok(false)
            }
            ("review", "exact") => {
                flow.state = "exact".into();
                Ok(false)
            }
            ("exact", "back") => {
                flow.state = "review".into();
                Ok(false)
            }
            ("review" | "exact", "import") => {
                // One at a time: a second, planned before the first had made
                // anything, would make it all again (#general-2).
                let Ok(_importing) = self.importing.try_lock() else {
                    return Err(Refusal::new("guard_failed", "The template is being imported already: wait for that to finish, then look again."));
                };
                let template = flow.data["template"].clone();
                let exact = flow.state == "exact";
                let plan = self.plan_import(&template).map_err(failed)?;
                let plan = if exact { self.exactly(plan).map_err(failed)? } else { plan };
                let mut choice: HashMap<String, bool> = HashMap::new();
                if exact {
                    // Everything the template's, what was deleted here made again.
                    for item in plan.items.iter().filter(|i| i.deleted_here) {
                        choice.insert(item.key(), true);
                    }
                } else {
                    // Each thing changed on both sides is chosen for, first.
                    for item in plan.items.iter().filter(|i| asks(i)) {
                        match input[format!("choose-{}", item.key())].as_str() {
                            Some("template") => choice.insert(item.key(), true),
                            Some("here") => choice.insert(item.key(), false),
                            _ => return Err(Refusal::new("guard_failed", "Choose, for each thing changed both here and in the template, which stays.")),
                        };
                    }
                }
                let report = self.import(&flow.owner, &template, &plan, &choice, input, exact, now).map_err(failed)?;
                flow.data["report"] = json!(report);
                flow.data.as_object_mut().map(|data| data.remove("template"));
                flow.state = "done".into();
                flow.status = "completed".into();
                Ok(true)
            }
            _ => Err(Refusal::new("guard_failed", "That can't be done here.")),
        }
    }

    /// Imports `template` as the owner chose (`input`: `choose-*` and `remove-*`).
    /// What was done, in a sentence or two.
    #[allow(clippy::too_many_arguments)]
    fn import(&self, owner: &str, template: &Value, plan: &Plan, choice: &HashMap<String, bool>, input: &Value, exact: bool, now: i64) -> anyhow::Result<String> {
        let mut ids: HashMap<String, String> = HashMap::new();
        let mut counts = Counts::default();

        // The community's own name and description first, by themselves: they are
        // kept apart (profile::Kept, over a connection of its own), whose write
        // would wait on the transaction below until the database gave up.
        self.import_items(owner, plan, choice, &["community"], &mut ids, &mut counts, now)?;

        // The rest in one transaction: hundreds of writes, committed together
        // rather than each by itself (a phone's import timed out otherwise).
        self.store.db.grouped(|| -> anyhow::Result<()> {
            // Removals first: what goes frees its name.
            for (kind, local, _) in &plan.removed {
                let wanted = matches!(&input[format!("remove-{kind}-{local}")], Value::Bool(true)) || matches!(input[format!("remove-{kind}-{local}")].as_str(), Some("true" | "on" | "1"));
                if wanted {
                    match *kind {
                        "channel" => self.store.trash_channel(local, owner, now)?,
                        "role" => {
                            self.store.delete_role(local)?;
                        }
                        _ => self.store.delete_category(local)?,
                    }
                    counts.removed += 1;
                }
                self.store.forget_imported(kind, local)?;
            }

            // Then roles and categories, and then channels, which name roles and
            // categories by the ids they now have.
            self.import_items(owner, plan, choice, &["role", "category", "channel"], &mut ids, &mut counts, now)?;
            // The template's order the first time, and when made to match exactly.
            // Imported again, what is here stays where the owner put it, and what
            // is new goes after it (asked 2026-11-16: an owner's arrangement was
            // put back without asking).
            if exact || first_import(plan) {
                self.order_as_template(plan, &ids)?;
            }
            // Discord's system messages channel, where it says who joined, is where
            // newcomers are welcomed, unless this community has a welcome channel.
            let system = template["serialized_source_guild"]["system_channel_id"].as_i64().and_then(|sid| ids.get(&format!("new:channel:{sid}")).cloned()).filter(|id| !id.is_empty());
            let welcome = self.welcome_setting()?;
            if let (Some(channel), true) = (system, welcome["channel"].is_null()) {
                let lines = welcome["lines"].as_array().map(|lines| lines.iter().filter_map(Value::as_str).map(str::to_string).collect());
                self.set_welcome(Some(&channel), lines)?;
            }
            // The rules channel, unless this community has one: Discord's, where the
            // template names it (it never has yet: a template leaves out a community
            // server's rules channel), else a text channel called just "rules"
            // ("📜rules" too).
            let rules = self.store.setting(crate::community::RULES_CHANNEL)?;
            if rules.is_none_or(|id| self.store.channel(&id).ok().flatten().is_none()) {
                let named = template["serialized_source_guild"]["rules_channel_id"].as_i64().and_then(|sid| ids.get(&format!("new:channel:{sid}")).cloned()).filter(|id| !id.is_empty());
                let called = || -> anyhow::Result<Option<String>> {
                    let rules = |name: &str| name.chars().filter(|c| c.is_alphanumeric()).collect::<String>() == "rules";
                    Ok(self.store.channels()?.into_iter().find(|c| c.voice.is_none() && c.forum.is_none() && rules(&c.name)).map(|c| c.id))
                };
                if let Some(id) = named.map_or_else(called, |id| Ok(Some(id)))? {
                    self.store.set_setting(crate::community::RULES_CHANNEL, &id)?;
                }
            }
            Ok(())
        })?;
        let Counts { made, changed, removed, no_room_channels, no_room_roles } = counts;
        let code = template["code"].as_str().unwrap_or_default();
        let name = template["name"].as_str().unwrap_or_default();
        self.store.set_setting("discord_template", &json!({ "code": code, "name": name, "imported_at": now, "guild": template["serialized_source_guild"].as_object().map(|g| g.iter().filter(|(k, _)| *k != "roles" && *k != "channels").map(|(k, v)| (k.clone(), v.clone())).collect::<Map<_, _>>()) }).to_string())?;
        self.audit(now, owner, "community.import", None, Some(json!({ "template": code, "name": name, "made": made, "changed": changed, "removed": removed, "no_room": { "channels": no_room_channels, "roles": no_room_roles } })));
        let mut report = format!("From “{name}”: {made} made, {changed} changed, {removed} removed.");
        if no_room_channels > 0 {
            report.push_str(&format!(
                " {no_room_channels} channels weren't made: this community may have only {} channels. Once it may have more, import the template again and they come over.",
                self.config.max_channels
            ));
        }
        if no_room_roles > 0 {
            report.push_str(&format!(" {no_room_roles} roles weren't made: a community may have only {} roles.", self.max_roles()));
        }
        Ok(report)
    }


    /// Imports the plan's things of `passes`' kinds, in that order, as the owner
    /// chose: `ids` gains what each is here, `counts` what was done.
    #[allow(clippy::too_many_arguments)]
    fn import_items(&self, owner: &str, plan: &Plan, choice: &HashMap<String, bool>, passes: &[&str], ids: &mut HashMap<String, String>, counts: &mut Counts, now: i64) -> anyhow::Result<()> {
        let resolve = |value: &Value, ids: &HashMap<String, String>| -> Value {
            match value {
                Value::String(text) if text.starts_with("new:") => json!(ids.get(text).cloned().unwrap_or_default()),
                other => other.clone(),
            }
        };
        for pass in passes {
        for item in plan.items.iter().filter(|i| i.kind == *pass) {
            let chosen = choice.get(&item.key()).copied();
            if item.deleted_here {
                // Left deleted, unless the owner brings it back.
                if chosen != Some(true) {
                    if let Some(record) = &item.record {
                        self.store.set_imported(item.kind, &record.local, &item.source_id, &item.source, &record.applied)?;
                    }
                    continue;
                }
                if let Some(record) = &item.record {
                    self.store.forget_imported(item.kind, &record.local)?;
                }
            }
            let mut want = item.want.clone();
            for field in ["category"] {
                if want.get(field).is_some() {
                    want[field] = resolve(&want[field], ids);
                }
            }
            if let Some(overrides) = want.get("overrides").and_then(Value::as_object).cloned() {
                let resolved: Map<String, Value> = overrides.into_iter().filter_map(|(role, said)| {
                    let id = if role.starts_with("new:") { ids.get(&role).cloned()? } else { role };
                    Some((id, said))
                }).collect();
                want["overrides"] = Value::Object(resolved);
            }
            let fields: Vec<String> = match (&item.local, item.deleted_here) {
                (Some(_), false) => {
                    let mut fields = item.take.clone();
                    if chosen == Some(true) {
                        fields.extend(item.conflicts.iter().cloned());
                    }
                    fields
                }
                _ => want.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default(),
            };
            let local = match (&item.local, item.deleted_here) {
                (Some(local), false) => {
                    if !fields.is_empty() {
                        counts.changed += 1;
                    }
                    self.apply_fields(item.kind, local, &want, &fields, owner, now)?;
                    local.clone()
                }
                _ => match self.make(item.kind, &want, owner, now)? {
                    Some(local) => {
                        counts.made += 1;
                        local
                    }
                    None => {
                        match item.kind {
                            "channel" => counts.no_room_channels += 1,
                            "role" => counts.no_room_roles += 1,
                            _ => {}
                        }
                        continue;
                    }
                },
            };
            ids.insert(format!("new:{}:{}", item.kind, item.source_id), local.clone());
            // What is kept to compare with next time is the template's, as it
            // would be here, whatever was kept here instead: unchanged in the
            // template since, what's here is left as it is.
            self.store.set_imported(item.kind, &local, &item.source_id, &item.source, &want)?;
        }
        }
        Ok(())
    }
    /// A channel name not yet taken (`name`, else `name-2`, ...), but by `except`.
    fn free_channel_name(&self, name: &str, voice: bool, except: Option<&str>) -> anyhow::Result<String> {
        let mut candidate = name.to_string();
        let mut n = 2;
        while self.store.channel_named_among(&candidate, voice)?.is_some_and(|c| Some(c.id.as_str()) != except) {
            let suffix = format!("-{n}");
            let room = crate::community::MAX_CHANNEL_NAME.saturating_sub(suffix.chars().count());
            candidate = format!("{}{suffix}", name.chars().take(room).collect::<String>());
            n += 1;
        }
        Ok(candidate)
    }

    fn free_role_name(&self, name: &str, except: Option<&str>) -> anyhow::Result<String> {
        let roles = self.store.roles()?;
        let taken = |candidate: &str| roles.iter().any(|r| Some(r.id.as_str()) != except && r.name.to_lowercase() == candidate.to_lowercase());
        let mut candidate = name.to_string();
        let mut n = 2;
        while taken(&candidate) {
            candidate = format!("{name} {n}");
            n += 1;
        }
        Ok(candidate)
    }

    /// Makes something the template has: its id here, none if it can't be made.
    fn make(&self, kind: &str, want: &Value, owner: &str, now: i64) -> anyhow::Result<Option<String>> {
        match kind {
            "role" => {
                if self.store.roles()?.len() >= self.max_roles() {
                    return Ok(None);
                }
                let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
                let position = self.store.roles()?.iter().map(|r| r.position).max().unwrap_or(0) + 1;
                let name = self.free_role_name(want["name"].as_str().unwrap_or("A role"), None)?;
                self.store.save_role(&RoleDef {
                    id: id.clone(),
                    name,
                    color: want["color"].as_str().map(str::to_string),
                    icon: None,
                    position,
                    permissions: strings(&want["permissions"]),
                    mentionable: want["mentionable"] == true,
                    hoist: want["hoist"] == true,
                    self_assignable: false,
                })?;
                Ok(Some(id))
            }
            "category" => {
                let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
                let position = self.store.categories()?.len() as i64;
                self.store.save_category(&Category { id: id.clone(), name: want["name"].as_str().unwrap_or("Channels").to_string(), position })?;
                self.apply_fields("category", &id, want, &["overrides".to_string()], owner, now)?;
                Ok(Some(id))
            }
            "channel" => {
                if self.store.open_channel_count()? >= self.config.max_channels || self.kind_off(wanted_kind(want)) {
                    return Ok(None);
                }
                let voice = want["kind"] == "voice";
                let id = uuid::Uuid::new_v4().to_string();
                let name = self.free_channel_name(want["name"].as_str().unwrap_or("channel"), voice, None)?;
                self.store.add_channel(&id, &name, want["topic"].as_str().unwrap_or_default(), owner, now)?;
                if voice {
                    self.store.set_channel_voice(&id, &crate::community::clean_voice(&json!({ "limit": want["limit"], "stage": want["stage"] == true }), None).unwrap_or_else(|_| json!({})))?;
                }
                if want["kind"] == "forum" {
                    self.store.set_forum(&id, &crate::posting::read_tags(&tags_given(&want["tags"], None), None).unwrap_or_else(|_| json!([])))?;
                    if want["gallery"] == true {
                        self.store.set_gallery(&id, true)?;
                    }
                }
                if want["announcement"] == true {
                    self.store.set_announcement(&id, true)?;
                }
                let fields: Vec<String> = ["slow_mode", "overrides", "category"].iter().map(|f| f.to_string()).collect();
                self.apply_fields("channel", &id, want, &fields, owner, now)?;
                self.audit(now, owner, "channel.create", Some(&id), Some(json!({ "name": name, "kind": want["kind"], "imported": true })));
                Ok(Some(id))
            }
            _ => Ok(None),
        }
    }

    /// Sets `fields` of something here to the template's.
    fn apply_fields(&self, kind: &str, local: &str, want: &Value, fields: &[String], _owner: &str, _now: i64) -> anyhow::Result<()> {
        if fields.is_empty() {
            return Ok(());
        }
        let has = |field: &str| fields.iter().any(|f| f == field);
        match kind {
            "community" => {
                let changed = self.changed();
                let name = if has("name") { want["name"].as_str().unwrap_or_default().to_string() } else { changed.name.unwrap_or_else(|| self.config.name.clone()) };
                let description = if has("description") { want["description"].as_str().unwrap_or_default().to_string() } else { changed.description.unwrap_or_else(|| self.config.description.clone()) };
                self.set_name_and_description(&name, &description)?;
            }
            "role" => {
                let Some(mut role) = self.store.role(local)? else { return Ok(()) };
                if has("name") && role.id != EVERYONE_ROLE {
                    role.name = self.free_role_name(want["name"].as_str().unwrap_or(&role.name), Some(local))?;
                }
                if has("color") {
                    role.color = want["color"].as_str().map(str::to_string);
                }
                if has("hoist") {
                    role.hoist = want["hoist"] == true;
                }
                if has("mentionable") {
                    role.mentionable = want["mentionable"] == true;
                }
                if has("permissions") {
                    role.permissions = strings(&want["permissions"]);
                }
                self.store.save_role(&role)?;
            }
            "category" => {
                if let Some(mut category) = self.store.categories()?.into_iter().find(|c| c.id == local) {
                    if has("name") {
                        category.name = want["name"].as_str().unwrap_or(&category.name).to_string();
                        self.store.save_category(&category)?;
                    }
                    // Its own permissions only: the template's channels bring theirs.
                    if has("overrides") {
                        let key = crate::roles::category_key(&category.id);
                        let managed: HashSet<String> = self.store.imported("role")?.into_iter().map(|r| r.local).chain(std::iter::once(EVERYONE_ROLE.to_string())).chain(want["overrides"].as_object().into_iter().flatten().map(|(k, _)| k.clone())).collect();
                        for o in self.store.overrides(&key)?.into_iter().filter(|o| managed.contains(&o.role)) {
                            if want["overrides"].get(&o.role).is_none() {
                                self.store.set_override(&key, &o.role, &[], &[])?;
                            }
                        }
                        for (role, said) in want["overrides"].as_object().into_iter().flatten() {
                            self.store.set_override(&key, role, &strings(&said["allow"]), &strings(&said["deny"]))?;
                        }
                    }
                }
            }
            _ => {
                let Some(channel) = self.store.channel(local)? else { return Ok(()) };
                let voice = channel.voice.is_some();
                if has("name") || has("topic") {
                    let name = if has("name") { self.free_channel_name(want["name"].as_str().unwrap_or(&channel.name), voice, Some(local))? } else { channel.name.clone() };
                    let topic = if has("topic") { want["topic"].as_str().unwrap_or_default().to_string() } else { channel.topic.clone() };
                    self.store.update_channel(local, &name, &topic, channel.closed)?;
                }
                if has("slow_mode") {
                    self.store.set_slow_mode(local, want["slow_mode"].as_i64().unwrap_or(0))?;
                }
                if has("limit") && voice {
                    let settings = crate::community::clean_voice(&json!({ "limit": want["limit"] }), channel.voice.as_ref()).unwrap_or_else(|_| channel.voice.clone().unwrap_or_default());
                    self.store.set_channel_voice(local, &settings)?;
                }
                // Imported before as a voice channel, and a stage on Discord: one now too.
                if has("stage") && voice {
                    let held = self.store.channel(local)?.and_then(|c| c.voice);
                    let settings = crate::community::clean_voice(&json!({ "stage": want["stage"] == true }), held.as_ref()).unwrap_or_else(|_| held.clone().unwrap_or_default());
                    self.store.set_channel_voice(local, &settings)?;
                }
                // Imported before as a text channel, and an announcement channel on
                // Discord: one now too (and no longer one, followers let go).
                if has("announcement") && !voice && channel.forum.is_none() && channel.announcement != (want["announcement"] == true) {
                    if want["announcement"] != true {
                        let notices = self.drop_followers(local)?;
                        self.outbox.lock().unwrap_or_else(|p| p.into_inner()).extend(notices);
                    }
                    self.store.set_announcement(local, want["announcement"] == true)?;
                }
                // Imported before as a forum, and a media channel on Discord: a gallery now.
                if has("gallery") && channel.forum.is_some() {
                    self.store.set_gallery(local, want["gallery"] == true)?;
                }
                if has("tags") && channel.forum.is_some() {
                    let tags = crate::posting::read_tags(&tags_given(&want["tags"], channel.forum.as_ref()), channel.forum.as_ref()).unwrap_or_else(|_| json!([]));
                    self.store.set_forum(local, &tags)?;
                }
                if has("overrides") {
                    let managed: HashSet<String> = self.store.imported("role")?.into_iter().map(|r| r.local).chain(std::iter::once(EVERYONE_ROLE.to_string())).chain(want["overrides"].as_object().into_iter().flatten().map(|(k, _)| k.clone())).collect();
                    for o in self.store.overrides(local)?.into_iter().filter(|o| managed.contains(&o.role)) {
                        if want["overrides"].get(&o.role).is_none() {
                            self.store.set_override(local, &o.role, &[], &[])?;
                        }
                    }
                    for (role, said) in want["overrides"].as_object().into_iter().flatten() {
                        self.store.set_override(local, role, &strings(&said["allow"]), &strings(&said["deny"]))?;
                    }
                }
                if has("category") {
                    // Only its category: `order_as_template` places every channel after.
                    self.store.set_channel_category(local, want["category"].as_str().filter(|c| !c.is_empty()))?;
                }
            }
        }
        Ok(())
    }

    /// Puts roles, categories and channels in the template's order: what came
    /// from it first, as it has them, and what was made here after.
    fn order_as_template(&self, plan: &Plan, ids: &HashMap<String, String>) -> anyhow::Result<()> {
        let local_of = |item: &Planned| ids.get(&format!("new:{}:{}", item.kind, item.source_id)).cloned().or_else(|| item.local.clone());
        // Roles: everyone lowest, then the template's as it lists them, then the rest.
        let template_roles: Vec<String> = plan.items.iter().filter(|i| i.kind == "role").filter_map(local_of).filter(|id| id != EVERYONE_ROLE).collect();
        let mut roles = self.store.roles()?;
        roles.sort_by_key(|r| r.position);
        let mut order: Vec<String> = template_roles.clone();
        order.extend(roles.iter().filter(|r| r.id != EVERYONE_ROLE && !template_roles.contains(&r.id)).map(|r| r.id.clone()));
        for (n, id) in order.iter().enumerate() {
            if let Some(mut role) = roles.iter().find(|r| r.id == *id).cloned() {
                if role.position != n as i64 + 1 {
                    role.position = n as i64 + 1;
                    self.store.save_role(&role)?;
                }
            }
        }
        // Categories by their place on Discord, then the rest.
        let mut template_categories: Vec<(i64, String)> = plan.items.iter().filter(|i| i.kind == "category").filter_map(|i| local_of(i).map(|id| (i.source["position"].as_i64().unwrap_or(0), id))).collect();
        template_categories.sort();
        let mut categories = self.store.categories()?;
        categories.sort_by_key(|c| c.position);
        let mut placed: Vec<String> = template_categories.into_iter().map(|(_, id)| id).collect();
        placed.extend(categories.iter().filter(|c| !placed.contains(&c.id)).map(|c| c.id.clone()).collect::<Vec<_>>());
        for (n, id) in placed.iter().enumerate() {
            if let Some(mut category) = categories.iter().find(|c| c.id == *id).cloned() {
                if category.position != n as i64 {
                    category.position = n as i64;
                    self.store.save_category(&category)?;
                }
            }
        }
        // Channels: by category, Discord's text before voice, each by its place.
        let place: BTreeMap<String, (i64, i64)> = plan
            .items
            .iter()
            .filter(|i| i.kind == "channel")
            .filter_map(|i| local_of(i).map(|id| (id, (i64::from(i.want["kind"] == "voice"), i.source["position"].as_i64().unwrap_or(0)))))
            .collect();
        let channels = self.store.channels()?;
        let mut keyed: Vec<(PlaceKey, String, Option<String>)> = channels
            .iter()
            .enumerate()
            .map(|(n, c)| {
                let group = c.category.as_ref().and_then(|id| placed.iter().position(|p| p == id)).map_or(-1, |p| p as i64);
                let (voice, position) = place.get(&c.id).copied().unwrap_or((i64::from(c.voice.is_some()), i64::MAX));
                ((group, i64::from(!place.contains_key(&c.id)), voice, position, n), c.id.clone(), c.category.clone())
            })
            .collect();
        keyed.sort();
        self.store.order_channels(&keyed.into_iter().map(|(_, id, category)| (id, category)).collect::<Vec<_>>())?;
        Ok(())
    }
}

/// Whether nothing of the template has been imported here before.
fn first_import(plan: &Plan) -> bool {
    plan.items.iter().all(|item| item.record.is_none()) && plan.removed.is_empty()
}

/// Where a channel goes: its category's place (-1 for none), whether it was made
/// here (after the template's), voice after text, its place on Discord, its place now.
type PlaceKey = (i64, i64, i64, i64, usize);

/// Whether the owner is asked about `item`: changed both here and in the
/// template, or deleted here and changed in the template since.
fn asks(item: &Planned) -> bool {
    if item.deleted_here {
        return item.record.as_ref().is_some_and(|r| r.source != item.source);
    }
    !item.conflicts.is_empty()
}

/// A forum's tags as the template has them, with the ids those of the same name
/// here have already (so the posts tagged keep them).
fn tags_given(tags: &Value, held: Option<&Value>) -> Value {
    json!(tags
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["name"].is_string())
        .map(|t| {
            let id = held.and_then(Value::as_array).into_iter().flatten().find(|h| h["name"] == t["name"]).map(|h| h["id"].clone()).unwrap_or(Value::Null);
            json!({ "id": id, "name": t["name"], "emoji": t["emoji"] })
        })
        .collect::<Vec<_>>())
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_where_discord_left_channels_out_of_a_template() {
        let channel = |id: i64, kind: i64, at: i64, name: &str, parent: Option<i64>| json!({ "id": id, "type": kind, "position": at, "name": name, "parent_id": parent });
        let template = |channels: Vec<Value>| json!({ "serialized_source_guild": { "channels": channels } });
        // As the Frosty Pillow Fort's: two announcement channels after
        // #unlock-all-channels (text places 2 and 3), two stages above 🔊Nyaflix
        // in Special VCs (voice places 1 and 2), and id 2 skipped with no place.
        let fort = template(vec![
            channel(1, 0, 0, "newcomers", None),
            channel(3, 0, 1, "unlock-all-channels", None),
            channel(6, 0, 4, "📜rules", None),
            channel(7, 2, 0, "Sub Voice", None),
            channel(8, 4, 0, "Special VCs", None),
            channel(9, 0, 5, "special-mute", Some(8)),
            channel(12, 2, 3, "Nyaflix", Some(8)),
        ]);
        assert_eq!(
            left_out(&fort).unwrap(),
            "Discord left 5 channels out of this template, and says nothing of what they were but where: 2 between #unlock-all-channels and #📜rules (announcement channels, most likely); 2 in Special VCs, above 🔊Nyaflix (stage channels, most likely); and 1 more it gives no place to. It leaves out a community server's announcement and stage channels, so make them here by hand (New channel, then Announcements or Stage)."
        );
        // One missing at the very top, which the ids alone wouldn't show.
        let top = template(vec![channel(1, 0, 1, "general", None)]);
        assert!(left_out(&top).unwrap().contains(": 1 above #general (an announcement channel, most likely)."));
        assert_eq!(left_out(&template(vec![channel(1, 0, 0, "general", None), channel(2, 2, 0, "Lounge", None)])), None);
    }

    #[test]
    fn a_templates_code_is_read_from_its_link() {
        for link in ["https://discord.new/dj2cDaTEQce2", "discord.new/dj2cDaTEQce2/", "https://discord.com/template/dj2cDaTEQce2", "dj2cDaTEQce2", " https://discord.new/dj2cDaTEQce2?x=1 "] {
            assert_eq!(template_code(link).as_deref(), Some("dj2cDaTEQce2"), "{link}");
        }
        for link in ["https://example.com/dj2cDaTEQce2", "", "discord.new/", "not a code"] {
            assert_eq!(template_code(link), None, "{link}");
        }
    }

    #[test]
    fn discord_permissions_are_ours_where_we_have_them() {
        // Send Messages, View Channel, Add Reactions; and Embed Links, which we haven't.
        let bits = (1 << 11) | (1 << 10) | (1 << 6) | (1 << 14);
        assert_eq!(mapped(bits).into_iter().collect::<Vec<_>>(), ["post", "react", "view_channel"]);
        assert_eq!(unmapped(bits), ["Embed Links"]);
        // Every one we map to is one we know.
        for (_, _, ours) in PERMISSIONS {
            for permission in *ours {
                assert!(is_permission(permission), "{permission}");
            }
        }
    }

    #[test]
    fn a_discord_name_is_made_a_text_channels() {
        assert_eq!(text_name("📜rules"), "📜rules");
        assert_eq!(text_name("General Chat"), "general-chat");
        assert_eq!(text_name("Hideout!"), "hideout");
        assert_eq!(text_name("!!!"), "channel");
    }

    #[test]
    fn a_field_is_taken_kept_or_asked_about() {
        let want = json!({ "name": "rules", "topic": "new" });
        // Never imported: whatever differs is asked about.
        assert_eq!(compare(&want, &json!({ "name": "rules", "topic": "old" }), None, &[]), (vec![], vec!["topic".to_string()]));
        // Not changed here since: the template's.
        assert_eq!(compare(&want, &json!({ "name": "rules", "topic": "old" }), Some(&json!({ "name": "rules", "topic": "old" })), &[]), (vec!["topic".to_string()], vec![]));
        // Changed here only: kept.
        assert_eq!(compare(&json!({ "topic": "old" }), &json!({ "topic": "mine" }), Some(&json!({ "topic": "old" })), &[]), (vec![], vec![]));
        // Changed on both sides: asked.
        assert_eq!(compare(&want, &json!({ "name": "rules", "topic": "mine" }), Some(&json!({ "name": "rules", "topic": "old" })), &[]), (vec![], vec!["topic".to_string()]));
    }
}
