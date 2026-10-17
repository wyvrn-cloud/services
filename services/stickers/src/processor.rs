//! The workflow protocol: what each `https://didcomm.org/workflow/1.0` message
//! does here. This service is that protocol's Processor, for the templates in
//! [`crate::templates`]; whoever is making a pack is its Coordinator.
//!
//! An instance is bound to the DID that started it, and nobody else can see or
//! advance it. What a maker has gathered is kept with the instance until the
//! workflow publishes it as a pack, or is cancelled, or is left too long.

use serde_json::{json, Map, Value};

use crate::config::ServiceConfig;
use crate::images::{self, Image};
use crate::render;
use crate::store::{draft_scope, pack_scope, Instance, Kind, Pack, Sticker, Store};
use crate::templates::{self, EDIT_PACK, NEW_EMOJI_PACK};

pub const PIURI: &str = "https://didcomm.org/workflow/1.0";
const P: &str = "https://didcomm.org/workflow/1.0/";

/// A refusal: the protocol's `problem-report`, with one of its codes.
#[derive(Debug)]
pub struct Refusal {
    pub code: &'static str,
    pub comment: String,
    pub args: Value,
}

impl Refusal {
    fn new(code: &'static str, comment: impl Into<String>) -> Self {
        Self { code, comment: comment.into(), args: json!({}) }
    }
}

/// What a person did wrong, said so they can put it right: the action failed.
fn mistake(comment: impl Into<String>) -> Refusal {
    Refusal::new("action_failed", comment)
}

/// An answer: a message type and its body.
pub type Answer = Result<(String, Value), Refusal>;

pub struct Processor {
    pub store: Store,
    pub config: ServiceConfig,
    /// Where packs are fetched from, ending in `/`.
    pub base: String,
}

fn text<'a>(input: &'a Value, field: &str) -> &'a str {
    input[field].as_str().map_or("", str::trim)
}

/// A pack's short name: 3 to 32 of a-z, 0-9 and hyphen, with no hyphen first,
/// last or doubled.
pub fn is_name(name: &str) -> bool {
    (3..=32).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// What a custom emoji is typed as: 2 to 32 of a-z, 0-9 and underscore.
pub fn is_shortcode(code: &str) -> bool {
    (2..=32).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Whether `text` is emoji and nothing else: short, with no letters, digits or
/// spaces of the plain kind, and something beyond them in it.
pub fn is_emoji(text: &str) -> bool {
    !text.is_empty() && text.len() <= 64 && text.chars().all(|c| !c.is_ascii() || c == '#' || c == '*') && text.chars().any(|c| c as u32 >= 0xa9)
}

impl Processor {
    pub fn new(store: Store, config: ServiceConfig, public_url: &str) -> Self {
        let base = if public_url.ends_with('/') { public_url.to_string() } else { format!("{public_url}/") };
        Self { store, config, base }
    }

    /// The names no pack may take: an admin's list, or the settings' until one has set one.
    pub fn reserved_names(&self) -> Vec<String> {
        match self.store.reserved_names() {
            Ok(Some(names)) => names,
            Ok(None) => self.config.reserved_names.clone(),
            Err(error) => {
                tracing::error!(%error, "couldn't read the names set aside; using the settings'");
                self.config.reserved_names.clone()
            }
        }
    }

    pub fn link(&self, name: &str) -> String {
        format!("{}{name}", self.base)
    }

    fn kind_of(template: &str, pack: Option<&Pack>) -> Kind {
        match (template, pack) {
            (NEW_EMOJI_PACK, _) => Kind::Emoji,
            (_, Some(pack)) => pack.kind,
            _ => Kind::Sticker,
        }
    }

    fn limits(&self, kind: Kind) -> (u32, usize) {
        match kind {
            Kind::Sticker => (self.config.max_sticker_px, self.config.max_sticker_bytes),
            Kind::Emoji => (self.config.max_emoji_px, self.config.max_emoji_bytes),
        }
    }

    /// The pack an `edit-pack` instance is changing, if it has chosen one that is
    /// still its owner's.
    fn chosen(&self, instance: &Instance) -> anyhow::Result<Option<Pack>> {
        let Some(name) = instance.context["pack"].as_str() else { return Ok(None) };
        Ok(self.store.pack(name)?.filter(|pack| pack.owner == instance.owner && !pack.removed))
    }

    /// Where an instance's stickers are: its own gathering, or the pack it edits.
    fn scope(&self, instance: &Instance) -> anyhow::Result<(String, String)> {
        Ok(match self.chosen(instance)? {
            Some(pack) if instance.template == EDIT_PACK => (pack_scope(&pack.name), format!("{}{}/", self.base, pack.name)),
            _ => (draft_scope(&instance.id), format!("{}drafts/{}/", self.base, instance.id)),
        })
    }

    fn listed(stickers: &[Sticker], files: &str) -> Value {
        stickers
            .iter()
            .map(|sticker| {
                let label = match &sticker.shortcode {
                    Some(code) => format!(":{code}:"),
                    None => sticker.emoji.clone(),
                };
                json!({ "id": sticker.id, "url": format!("{files}{}", sticker.file()), "emoji": sticker.emoji, "shortcode": sticker.shortcode, "label": label })
            })
            .collect()
    }

    /// What an instance has to show: the values its screens are drawn from, and
    /// its guards read. Worked out from what is stored, never stored itself.
    fn artifacts(&self, instance: &Instance) -> anyhow::Result<Value> {
        let pack = self.chosen(instance)?;
        let kind = Self::kind_of(&instance.template, pack.as_ref());
        let (scope, files) = self.scope(instance)?;
        let stickers = self.store.stickers(&scope)?;
        let full = stickers.len() >= self.config.max_per_pack;
        let mut artifacts = json!({
            "count": stickers.len(),
            "limit": self.config.max_per_pack,
            "full": full,
            "max_px": self.limits(kind).0,
            "stickers": Self::listed(&stickers, &files),
            "base": self.base,
            "is_emoji": kind == Kind::Emoji,
        });
        match (instance.template.as_str(), pack) {
            (EDIT_PACK, Some(pack)) => {
                artifacts["name"] = json!(pack.name);
                artifacts["title"] = json!(pack.title);
                artifacts["reuse"] = json!(pack.reuse);
                artifacts["link"] = json!(self.link(&pack.name));
                artifacts["can_add_sticker"] = json!(!full && kind == Kind::Sticker);
                artifacts["can_add_emoji"] = json!(!full && kind == Kind::Emoji);
            }
            (EDIT_PACK, None) => {
                let mut packs = Vec::new();
                for pack in self.store.packs(Some(&instance.owner))?.into_iter().filter(|pack| !pack.removed) {
                    let stickers = self.store.stickers(&pack_scope(&pack.name))?;
                    let icon = pack.icon.as_deref().and_then(|id| stickers.iter().find(|s| s.id == id)).or(stickers.first());
                    let what = if pack.kind == Kind::Emoji { "custom emoji" } else if stickers.len() == 1 { "sticker" } else { "stickers" };
                    packs.push(json!({
                        "name": pack.name,
                        "title": pack.title,
                        "kind": pack.kind.as_str(),
                        "summary": format!("{} · {} {what}", pack.name, stickers.len()),
                        "icon": icon.map(|sticker| format!("{}{}/{}", self.base, pack.name, sticker.file())),
                    }));
                }
                artifacts["packs"] = Value::Array(packs);
            }
            _ => {
                if let Some(name) = instance.context["published"].as_str() {
                    artifacts["name"] = json!(name);
                    artifacts["link"] = json!(self.link(name));
                }
            }
        }
        Ok(artifacts)
    }

    fn scope_of(&self, instance: &Instance, artifacts: &Value) -> Value {
        json!({ "context": instance.context, "artifacts": artifacts, "participants": { "holder": { "did": instance.owner } } })
    }

    /// The `status` of an instance: where it is, what can be done there, and what
    /// to draw.
    fn status(&self, instance: &Instance) -> anyhow::Result<Value> {
        let template = templates::find(&instance.template).unwrap_or(Value::Null);
        let artifacts = self.artifacts(instance)?;
        let scope = self.scope_of(instance, &artifacts);
        let allowed: Vec<String> = if instance.status == "active" {
            templates::events_from(&template, &instance.state)
                .into_iter()
                .filter(|event| template["transitions"][event]["guard"].as_str().is_none_or(|guard| render::holds(guard, &scope)))
                .collect()
        } else {
            Vec::new()
        };
        let ui = if instance.status == "canceled" {
            json!([{ "type": "information", "text": "Cancelled. Nothing was published." }])
        } else {
            render::render(templates::hints(&template, &instance.state), &scope).unwrap_or(json!([]))
        };
        Ok(json!({
            "instance_id": instance.id,
            "state": instance.state,
            "status": instance.status,
            "allowed_events": allowed,
            "action_menu": {},
            "artifacts": artifacts,
            "ui": ui,
            "ui_profile": "receiver",
        }))
    }

    fn mine(&self, sender: &str, body: &Value) -> Result<Instance, Refusal> {
        let id = body["instance_id"].as_str().unwrap_or_default();
        // Someone else's instance is, to anyone asking, not there.
        match self.store.instance(id) {
            Ok(Some(instance)) if instance.owner == sender => Ok(instance),
            Ok(_) => Err(Refusal::new("instance_not_found", "There is no such workflow under way here.")),
            Err(error) => Err(failed(error)),
        }
    }

    /// Acts on one message from `sender`. `None` for one that isn't this protocol's.
    pub fn handle(&self, sender: &str, message: &Value, now: i64) -> Option<Answer> {
        let kind = message["type"].as_str()?.strip_prefix(P)?;
        let body = &message["body"];
        Some(match kind {
            "discover" => self.discover(body),
            "fetch-template" => self.fetch_template(body),
            "start" => self.start(sender, message, now),
            "status" => self.mine(sender, body).and_then(|instance| self.status(&instance).map_err(failed)).map(|status| (format!("{P}status"), status)),
            "advance" => self.advance(sender, body, now),
            "cancel" => self.set_status(sender, body, "canceled", now),
            "pause" => self.set_status(sender, body, "paused", now),
            "resume" => self.set_status(sender, body, "active", now),
            // The templates here are the service's own.
            "publish-template" => Err(Refusal::new("not_authorized", "This service runs its own workflows, and takes no others.")),
            _ => return None,
        })
    }

    fn discover(&self, body: &Value) -> Answer {
        let filters = &body["filters"];
        let wanted = |template: &Value| {
            let id = template["id"].as_str().unwrap_or_default();
            let name = template["name"].as_str().unwrap_or_default();
            filters["template_id"].as_str().is_none_or(|wanted| wanted == id)
                && filters["version"].as_str().is_none_or(|wanted| wanted == templates::VERSION)
                && filters["text"].as_str().is_none_or(|text| format!("{id} {name}").to_lowercase().contains(&text.to_lowercase()))
        };
        let include_hash = body["include_hash"] == true;
        let workflows: Vec<Value> = templates::all()
            .iter()
            .filter(|template| wanted(template))
            .map(|template| {
                let mut listed = json!({ "template_id": template["id"], "versions": [templates::VERSION], "title": template["name"] });
                if include_hash {
                    listed["hash"] = json!(templates::hash(template));
                }
                listed
            })
            .collect();
        Ok((format!("{P}workflows"), json!({ "paging": { "total": workflows.len(), "next_offset": 0 }, "workflows": workflows })))
    }

    fn fetch_template(&self, body: &Value) -> Answer {
        let found = templates::find(body["template_id"].as_str().unwrap_or_default())
            .filter(|_| body["template_version"].as_str().is_none_or(|version| version == templates::VERSION))
            .filter(|template| body["prefer_hash"].as_str().is_none_or(|hash| hash == templates::hash(template)));
        match found {
            Some(template) => Ok((format!("{P}template"), json!({ "template": template }))),
            None => Err(Refusal::new("not_found_remote_template", "This service has no such workflow.")),
        }
    }

    fn start(&self, sender: &str, message: &Value, now: i64) -> Answer {
        let body = &message["body"];
        let template_id = body["template_id"].as_str().unwrap_or_default();
        let Some(template) = templates::find(template_id).filter(|_| body["template_version"].as_str().is_none_or(|version| version == templates::VERSION)) else {
            return Err(Refusal::new("template_not_found", "This service has no such workflow."));
        };
        let wanted = body["instance_id"].as_str().or(message["thid"].as_str()).or(message["id"].as_str()).unwrap_or_default();
        // One of each at a time for each maker: starting again finds the one under way.
        if let Some(existing) = self.store.active_instance(sender, template_id).map_err(failed)? {
            if existing.id == wanted {
                return self.status(&existing).map(|status| (format!("{P}status"), status)).map_err(failed);
            }
            let mut refusal = Refusal::new("multiplicity_violation", "You have one of these under way already. Carry on with it, or cancel it first.");
            refusal.args = json!({ "instance_id": existing.id });
            return Err(refusal);
        }
        if wanted.is_empty() || self.store.instance(wanted).map_err(failed)?.is_some() {
            return Err(Refusal::new("multiplicity_violation", "That instance id has been used."));
        }
        // An instance of `edit-pack` may be started on the pack to change.
        let mut context = Map::new();
        if template_id == EDIT_PACK {
            if let Some(pack) = body["context"]["pack"].as_str() {
                context.insert("pack".to_string(), json!(pack));
            }
        }
        let mut instance = Instance {
            id: wanted.to_string(),
            template: template_id.to_string(),
            owner: sender.to_string(),
            state: templates::initial_state(&template).to_string(),
            status: "active".to_string(),
            context: Value::Object(context),
            updated: now,
        };
        if template_id == EDIT_PACK && self.chosen(&instance).map_err(failed)?.is_some() {
            instance.state = "editing".to_string();
        } else if let Some(context) = instance.context.as_object_mut() {
            context.remove("pack");
        }
        self.store.save_instance(&instance).map_err(failed)?;
        tracing::info!(instance = %instance.id, template = template_id, "started");
        self.status(&instance).map(|status| (format!("{P}status"), status)).map_err(failed)
    }

    fn set_status(&self, sender: &str, body: &Value, status: &str, now: i64) -> Answer {
        let mut instance = self.mine(sender, body)?;
        let allowed = match status {
            "paused" => instance.status == "active",
            "active" => instance.status == "paused",
            _ => instance.status == "active" || instance.status == "paused",
        };
        if allowed {
            instance.status = status.to_string();
            instance.updated = now;
            if status == "canceled" {
                // What it gathered goes with it. A pack it was changing is untouched.
                self.store.drop_draft(&instance.id).map_err(failed)?;
            }
            self.store.save_instance(&instance).map_err(failed)?;
        }
        self.status(&instance).map(|status| (format!("{P}status"), status)).map_err(failed)
    }

    fn advance(&self, sender: &str, body: &Value, now: i64) -> Answer {
        let mut instance = self.mine(sender, body)?;
        let answer = |this: &Self, instance: &Instance| -> Answer {
            let template = templates::find(&instance.template).unwrap_or(Value::Null);
            if instance.status == "completed" && templates::is_final(&template, &instance.state) {
                return Ok((format!("{P}complete"), json!({ "instance_id": instance.id })));
            }
            this.status(instance).map(|status| (format!("{P}status"), status)).map_err(failed)
        };
        // The same press of a button, arriving twice, is answered and not done again.
        if let Some(key) = body["idempotency_key"].as_str() {
            if !self.store.first_advance(&instance.id, key).map_err(failed)? {
                return answer(self, &instance);
            }
        }
        if instance.status != "active" {
            return Err(Refusal::new("guard_failed", "This workflow isn't under way."));
        }
        let template = templates::find(&instance.template).unwrap_or(Value::Null);
        let event = body["event"].as_str().unwrap_or_default();
        let transition = &template["transitions"][event];
        if transition["from"] != instance.state.as_str() {
            return Err(Refusal::new("guard_failed", "That can't be done at this step."));
        }
        if let Some(guard) = transition["guard"].as_str() {
            let artifacts = self.artifacts(&instance).map_err(failed)?;
            if !render::holds(guard, &self.scope_of(&instance, &artifacts)) {
                return Err(Refusal::new("guard_failed", if event == "publish" { "Add at least one first." } else { "That can't be done here." }));
            }
        }
        if let Some(action) = transition["action"].as_str() {
            self.act(action, &mut instance, &body["input"], now)?;
        }
        instance.state = transition["to"].as_str().unwrap_or_default().to_string();
        instance.updated = now;
        if templates::is_final(&template, &instance.state) {
            instance.status = "completed".to_string();
            self.store.drop_draft(&instance.id).map_err(failed)?;
        }
        // Going back to choose a pack lets go of the one that was chosen.
        if event == "back" {
            if let Some(context) = instance.context.as_object_mut() {
                context.remove("pack");
            }
        }
        self.store.save_instance(&instance).map_err(failed)?;
        answer(self, &instance)
    }

    /// Reads an uploaded picture and holds it to the limits for its kind.
    fn picture(&self, input: &Value, kind: Kind) -> Result<(Image, Vec<u8>), Refusal> {
        let bytes = images::from_data_url(text(input, "image")).map_err(mistake)?;
        let (max_px, max_bytes) = self.limits(kind);
        if bytes.len() > max_bytes {
            return Err(mistake(format!("That picture is {} KB. The most allowed is {} KB.", bytes.len().div_ceil(1024), max_bytes / 1024)));
        }
        let image = images::inspect(&bytes).map_err(mistake)?;
        if image.width > max_px || image.height > max_px {
            return Err(mistake(format!("That picture is {} by {} pixels. The most allowed is {max_px} by {max_px}.", image.width, image.height)));
        }
        Ok((image, bytes))
    }

    /// Carries out one of this service's actions for a transition.
    fn act(&self, action: &str, instance: &mut Instance, input: &Value, now: i64) -> Result<(), Refusal> {
        let pack = self.chosen(instance).map_err(failed)?;
        let kind = Self::kind_of(&instance.template, pack.as_ref());
        let (scope, _) = self.scope(instance).map_err(failed)?;
        let editing = pack.as_ref().filter(|_| instance.template == EDIT_PACK);
        let touch = |this: &Self| editing.map_or(Ok(()), |pack| this.store.touch_pack(&pack.name, now)).map_err(failed);
        match action {
            "set_title" => {
                let title = text(input, "title");
                if title.is_empty() || title.chars().count() > 64 {
                    return Err(mistake("A title is 1 to 64 characters."));
                }
                match editing {
                    Some(pack) => self.store.update_pack(&pack.name, title, pack.reuse, pack.icon.as_deref(), now).map_err(failed)?,
                    None => instance.context["title"] = json!(title),
                }
            }
            "add" => {
                let stickers = self.store.stickers(&scope).map_err(failed)?;
                if stickers.len() >= self.config.max_per_pack {
                    return Err(mistake(format!("A pack holds {} at most.", self.config.max_per_pack)));
                }
                let (emoji, shortcode) = (text(input, "emoji"), text(input, "shortcode"));
                let shortcode = match kind {
                    Kind::Sticker => {
                        if !is_emoji(emoji) {
                            return Err(mistake("Tell me the emoji this sticker stands for: one or two emoji, and nothing else."));
                        }
                        None
                    }
                    Kind::Emoji => {
                        if !is_shortcode(shortcode) {
                            return Err(mistake("A shortcode is 2 to 32 lowercase letters, digits and underscores."));
                        }
                        if stickers.iter().any(|sticker| sticker.shortcode.as_deref() == Some(shortcode)) {
                            return Err(mistake(format!("This pack already has :{shortcode}:.")));
                        }
                        if !emoji.is_empty() && !is_emoji(emoji) {
                            return Err(mistake("The emoji it stands for has to be an emoji."));
                        }
                        Some(shortcode)
                    }
                };
                let (image, bytes) = self.picture(input, kind)?;
                if !self.store.add_sticker(&scope, &image, &bytes, emoji, shortcode).map_err(failed)? {
                    return Err(mistake("That picture is in the pack already."));
                }
                touch(self)?;
            }
            "remove" => {
                if !self.store.remove_sticker(&scope, text(input, "sticker")).map_err(failed)? {
                    return Err(mistake("That one isn't in the pack."));
                }
                match editing {
                    // A pack whose icon was removed goes back to having none.
                    Some(pack) if pack.icon.as_deref() == Some(text(input, "sticker")) => self.store.update_pack(&pack.name, &pack.title, pack.reuse, None, now).map_err(failed)?,
                    _ => touch(self)?,
                }
            }
            "set_icon" => {
                // The icon is one of the pack's own pictures, named as the one that stands for it.
                let chosen = text(input, "sticker");
                if !self.store.stickers(&scope).map_err(failed)?.iter().any(|sticker| sticker.id == chosen) {
                    return Err(mistake("That one isn't in the pack."));
                }
                match editing {
                    Some(pack) => self.store.update_pack(&pack.name, &pack.title, pack.reuse, Some(chosen), now).map_err(failed)?,
                    None => instance.context["icon"] = json!(chosen),
                }
            }
            "set_private" => {
                let Some(pack) = editing else { return Err(mistake("Choose a pack first.")) };
                self.store.update_pack(&pack.name, &pack.title, input["private"] != true, pack.icon.as_deref(), now).map_err(failed)?;
            }
            "choose" => {
                let name = text(input, "pack");
                match self.store.pack(name).map_err(failed)? {
                    Some(pack) if pack.owner == instance.owner && !pack.removed => instance.context["pack"] = json!(pack.name),
                    _ => return Err(mistake("That isn't one of your packs.")),
                }
            }
            "delete" => {
                let Some(pack) = editing else { return Err(mistake("Choose a pack first.")) };
                if text(input, "name") != pack.name {
                    return Err(mistake(format!("To delete the pack, type its short name: {}", pack.name)));
                }
                self.store.remove_pack(&pack.name, now).map_err(failed)?;
                tracing::info!(pack = %pack.name, "deleted by its maker");
            }
            "publish" => {
                let name = text(input, "name").to_lowercase();
                if !is_name(&name) {
                    return Err(mistake("A short name is 3 to 32 lowercase letters, digits and hyphens, with no hyphen first, last or doubled."));
                }
                if self.reserved_names().contains(&name) {
                    return Err(mistake("That name is set aside. Please choose another."));
                }
                let mine = self.store.packs(Some(&instance.owner)).map_err(failed)?.iter().filter(|pack| !pack.removed).count();
                if mine >= self.config.max_packs_per_maker {
                    return Err(mistake(format!("You have {mine} packs, which is as many as one maker may. Delete one first.")));
                }
                let pack = Pack {
                    name: name.clone(),
                    kind,
                    title: text(&instance.context, "title").to_string(),
                    owner: instance.owner.clone(),
                    reuse: input["private"] != true,
                    icon: instance.context["icon"].as_str().map(str::to_string),
                    version: 1,
                    created: now,
                    updated: now,
                    removed: false,
                };
                if !self.store.publish(&pack, &scope).map_err(failed)? {
                    return Err(mistake("Sorry, that short name is taken. Please choose another."));
                }
                instance.context["published"] = json!(name);
                tracing::info!(pack = %name, kind = kind.as_str(), "published");
            }
            _ => return Err(Refusal::new("action_failed", "This service doesn't know how to do that.")),
        }
        Ok(())
    }

    /// A pack as anyone may fetch it: `stickers/1.0`'s pack manifest.
    pub fn manifest(&self, pack: &Pack) -> anyhow::Result<Value> {
        let stickers = self.store.stickers(&pack_scope(&pack.name))?;
        let listed: Vec<Value> = stickers
            .iter()
            .map(|sticker| {
                let mut entry = json!({
                    "id": sticker.id,
                    "file": sticker.file(),
                    "media_type": sticker.media_type,
                    "width": sticker.width,
                    "height": sticker.height,
                    "bytes": sticker.bytes,
                    "sha256": sticker.sha256,
                    "animated": sticker.animated,
                });
                if !sticker.emoji.is_empty() {
                    entry["emoji"] = json!(sticker.emoji);
                }
                if let Some(code) = &sticker.shortcode {
                    entry["shortcode"] = json!(code);
                }
                entry
            })
            .collect();
        let mut manifest = json!({
            "type": "https://wyvrn.app/stickers/1.0/pack",
            "url": self.link(&pack.name),
            "name": pack.name,
            "title": pack.title,
            "kind": pack.kind.as_str(),
            "reuse": pack.reuse,
            "version": pack.version,
            "updated": pack.updated,
            "stickers": listed,
        });
        if let Some(icon) = pack.icon.as_deref().filter(|icon| stickers.iter().any(|sticker| sticker.id == *icon)) {
            manifest["icon"] = json!(icon);
        }
        // A pack kept to its maker says who that is, to whoever already knows: the
        // maker's app tells its own pack from someone else's by it.
        if !pack.reuse {
            manifest["maker"] = json!(maker_check(&self.link(&pack.name), &pack.owner));
        }
        Ok(manifest)
    }
}

/// What a pack kept to its maker names its maker by (stickers/1.0's `maker`): the
/// SHA-256 of its address and the maker's DID, so that only someone who has the DID
/// can tell whose it is, and two packs of one maker aren't seen to be by it.
pub fn maker_check(address: &str, maker: &str) -> String {
    use sha2::{Digest, Sha256};
    images::hex(&Sha256::digest(format!("{address}\n{maker}").as_bytes()))
}

fn failed(error: anyhow::Error) -> Refusal {
    tracing::error!(%error, "the store failed");
    Refusal::new("action_failed", "Something went wrong here. Please try again.")
}
