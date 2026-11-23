//! Bots in a community (bot-commands/1.0): a bot is a member like anyone else, over
//! DIDComm like anyone else. Its profile says it is one (`bot: true`) and what
//! commands it takes; members see that in the member list, and an app offers the
//! commands as `/` is typed. A command is sent to the community (`bot-invoke`),
//! which, having checked the member may post where they typed it, passes it on to
//! the bot with where it was typed and by whom (member ids, as everything here
//! names members); the bot answers by posting, as any member does. A bot's post may
//! carry buttons; pressing one is passed on the same way (`bot-press`).

use serde_json::{json, Value};
use wyvrn_service::params;

use crate::community::{line, refuse, reply_to, Community, Outcome};
use crate::store::{Member, Store};

pub const PIURI: &str = "https://wyvrn.app/bot-commands/1.0";
/// From the bot's side: a command, and a press of one of its post's buttons.
pub const INVOKE: &str = "https://wyvrn.app/bot-commands/1.0/invoke";
pub const PRESS: &str = "https://wyvrn.app/bot-commands/1.0/press";
/// To the community: `{ member, channel, command, options?, thread? }`, `{ to, button }`.
pub const BOT_INVOKE: &str = "https://wyvrn.app/community/1.0/bot-invoke";
pub const BOT_PRESS: &str = "https://wyvrn.app/community/1.0/bot-press";
pub const BOT_SENT: &str = "https://wyvrn.app/community/1.0/bot-sent";

pub const MAX_COMMANDS: usize = 50;
const MAX_OPTIONS: usize = 10;
const MAX_CHOICES: usize = 25;
pub const MAX_BUTTONS: usize = 5;
const OPTION_TYPES: &[&str] = &["string", "number", "boolean", "member", "channel"];

fn is_name(text: &str) -> bool {
    (1..=32).contains(&text.len()) && text.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// A bot's commands as its profile gives them, checked: `None` for none that do.
pub fn commands_from(given: &Value) -> Option<Value> {
    let mut commands = Vec::new();
    for command in given.as_array()?.iter().take(MAX_COMMANDS) {
        let Some(name) = command["name"].as_str().filter(|n| is_name(n)) else { continue };
        let mut options = Vec::new();
        for option in command["options"].as_array().into_iter().flatten().take(MAX_OPTIONS) {
            let Some(option_name) = option["name"].as_str().filter(|n| is_name(n)) else { continue };
            let kind = option["type"].as_str().filter(|t| OPTION_TYPES.contains(t)).unwrap_or("string");
            let mut kept = json!({ "name": option_name, "type": kind });
            if let Some(description) = line(&option["description"], 100) {
                kept["description"] = json!(description);
            }
            if option["required"] == true {
                kept["required"] = json!(true);
            }
            let choices: Vec<String> = option["choices"].as_array().into_iter().flatten().filter_map(|c| line(c, 100)).take(MAX_CHOICES).collect();
            if !choices.is_empty() {
                kept["choices"] = json!(choices);
            }
            options.push(kept);
        }
        let mut kept = json!({ "name": name, "description": line(&command["description"], 100).unwrap_or_default() });
        if !options.is_empty() {
            kept["options"] = json!(options);
        }
        commands.push(kept);
    }
    (!commands.is_empty()).then(|| Value::Array(commands))
}

/// A bot post's buttons, checked: `[{ id, label, style? }]`.
pub fn buttons_from(given: &Value) -> Option<Value> {
    let buttons: Vec<Value> = given
        .as_array()?
        .iter()
        .take(MAX_BUTTONS)
        .filter_map(|b| {
            let id = b["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 64)?;
            let label = line(&b["label"], 80)?;
            let mut kept = json!({ "id": id, "label": label });
            if let Some(style) = b["style"].as_str().filter(|s| ["primary", "danger"].contains(s)) {
                kept["style"] = json!(style);
            }
            Some(kept)
        })
        .collect();
    (!buttons.is_empty()).then(|| Value::Array(buttons))
}

/// Let a member's bot: true take effect, or no longer (`approved`). For whoever
/// may manage roles (WYV-89).
pub const APPROVE_BOT: &str = "https://wyvrn.app/community/1.0/approve-bot";

impl Store {
    /// A bot member's commands, if they are one: their profile says so and it was
    /// approved (WYV-89). `Some(commands)` (maybe `[]`).
    pub fn bot(&self, did: &str) -> wyvrn_service::db::Result<Option<Value>> {
        let found: Option<String> = self.db.lock().query_row(
            "SELECT b.commands FROM member_bots b JOIN bot_approvals a ON a.did = b.did WHERE b.did = ?1",
            params![did],
            |row| row.get(0),
        )?;
        Ok(found.map(|text| serde_json::from_str(&text).unwrap_or_else(|_| json!([]))))
    }

    /// What a member's profile says of being a bot, approved or not.
    pub fn bot_asked(&self, did: &str) -> wyvrn_service::db::Result<Option<Value>> {
        let found: Option<String> = self.db.lock().query_row("SELECT commands FROM member_bots WHERE did = ?1", params![did], |row| row.get(0))?;
        Ok(found.map(|text| serde_json::from_str(&text).unwrap_or_else(|_| json!([]))))
    }

    /// Whether a member's being a bot was approved.
    pub fn bot_approved(&self, did: &str) -> wyvrn_service::db::Result<bool> {
        Ok(self.db.lock().query_row("SELECT did FROM bot_approvals WHERE did = ?1", params![did], |row| row.get::<String>(0))?.is_some())
    }

    pub fn set_bot_approved(&self, did: &str, by: Option<&str>, now: i64) -> wyvrn_service::db::Result<()> {
        let mut db = self.db.lock();
        match by {
            Some(by) => db.execute("INSERT OR REPLACE INTO bot_approvals (did, by_did, at) VALUES (?1, ?2, ?3)", params![did, by, now])?,
            None => db.execute("DELETE FROM bot_approvals WHERE did = ?1", params![did])?,
        };
        Ok(())
    }

    /// Bots there were before approval was asked for are let be, once, as the
    /// table is first met: they were working, and approving them is no news.
    pub fn approve_bots_from_before(&self, now: i64) -> wyvrn_service::db::Result<usize> {
        if self.setting("bot_approvals_seeded")?.is_some() {
            return Ok(0);
        }
        let before: Vec<String> = self.db.lock().query("SELECT did FROM member_bots", params![], |row| row.get(0))?;
        for did in &before {
            self.set_bot_approved(did, Some("from-before-approval"), now)?;
        }
        self.set_setting("bot_approvals_seeded", "1")?;
        Ok(before.len())
    }

    pub fn set_bot(&self, did: &str, commands: Option<&Value>, now: i64) -> wyvrn_service::db::Result<()> {
        let mut db = self.db.lock();
        match commands {
            Some(commands) => db.execute("INSERT OR REPLACE INTO member_bots (did, commands, at) VALUES (?1, ?2, ?3)", params![did, commands.to_string(), now])?,
            None => db.execute("DELETE FROM member_bots WHERE did = ?1", params![did])?,
        };
        Ok(())
    }
}

impl Community {
    /// What a member's profile says of being a bot, kept. Whether it changed.
    pub(crate) fn take_bot(&self, sender: &str, profile: &Value, now: i64) -> anyhow::Result<bool> {
        if profile.get("bot").is_none() && profile.get("commands").is_none() {
            return Ok(false);
        }
        let held = self.store.bot_asked(sender)?;
        let wanted = (profile["bot"] == true).then(|| commands_from(&profile["commands"]).unwrap_or_else(|| json!([])));
        if held == wanted {
            return Ok(false);
        }
        self.store.set_bot(sender, wanted.as_ref(), now)?;
        // No longer one: an approval doesn't wait for them to be one again.
        if wanted.is_none() {
            self.store.set_bot_approved(sender, None, now)?;
        }
        Ok(true)
    }

    pub fn handle_bot_message(&self, member: &Member, message: &Value, now: i64) -> anyhow::Result<Option<Outcome>> {
        let body = &message["body"];
        let sender = member.did.as_str();
        match message["type"].as_str().unwrap_or_default() {
            APPROVE_BOT => {
                if !self.powers(sender)?.has("manage_roles") {
                    return refuse(message, "e.p.trust.not-authorized", "Approving a bot takes Manage roles.").map(Some);
                }
                let did = body["member"].as_str().unwrap_or_default().to_string();
                let Some(approved) = body["approved"].as_bool() else {
                    return refuse(message, "e.p.msg.invalid-approve-bot", "approved is true or false.").map(Some);
                };
                if approved && self.store.bot_asked(&did)?.is_none() {
                    return refuse(message, "e.p.msg.unknown-bot", "Their profile doesn't say they are a bot.").map(Some);
                }
                self.store.set_bot_approved(&did, approved.then_some(sender), now)?;
                self.audit(now, sender, if approved { "bot.approve" } else { "bot.unapprove" }, Some(&did), None);
                return Ok(Some(Outcome::reply(crate::community::reply_to(message, crate::community::MEMBERS, self.members_for(member)?))));
            }
            BOT_INVOKE => {
                let Some(channel) = self.channel_for(sender, body["channel"].as_str().unwrap_or_default())? else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such channel.").map(Some);
                };
                let bot = body["member"].as_str().unwrap_or_default();
                let Some(commands) = self.store.member(bot)?.filter(|m| m.joined).map(|_| self.store.bot(bot)).transpose()?.flatten() else {
                    return refuse(message, "e.p.msg.unknown-bot", "There is no such bot here.").map(Some);
                };
                let name = body["command"].as_str().unwrap_or_default();
                let Some(command) = commands.as_array().into_iter().flatten().find(|c| c["name"] == name) else {
                    return refuse(message, "e.p.msg.unknown-command", "That bot has no such command.").map(Some);
                };
                // As a post there would be: where they may post, and the bot may see.
                let thread = body["thread"].as_str().filter(|t| !t.is_empty());
                let may = if thread.is_some() { "send_in_threads" } else { "post" };
                if !self.may_in(sender, &channel, may)? || !self.sees_id(bot, &channel.id)? {
                    return refuse(message, "e.p.trust.not-authorized", "You can't use that here.").map(Some);
                }
                if let Some(thread) = thread {
                    if !self.reads_thread(sender, &self.powers(sender)?, &channel, thread)? || !self.reads_thread(bot, &self.powers(bot)?, &channel, thread)? {
                        return refuse(message, "e.p.msg.unknown-thread", "There is no such thread in this channel.").map(Some);
                    }
                }
                if self.timed_out(sender, now)? {
                    return refuse(message, "e.p.msg.timed-out", "You are timed out here for now.").map(Some);
                }
                // Only the options the command has, each a line of text.
                let mut options = serde_json::Map::new();
                for option in command["options"].as_array().into_iter().flatten() {
                    let key = option["name"].as_str().unwrap_or_default();
                    if let Some(value) = body["options"].get(key).filter(|v| !v.is_null()) {
                        let value = match value {
                            Value::String(text) => json!(text.chars().take(1000).collect::<String>()),
                            Value::Number(_) | Value::Bool(_) => value.clone(),
                            _ => continue,
                        };
                        options.insert(key.into(), value);
                    } else if option["required"] == true {
                        return refuse(message, "e.p.msg.invalid-command", &format!("/{name} needs {key}.")).map(Some);
                    }
                }
                let id = uuid::Uuid::new_v4().to_string();
                let mut said = json!({ "command": name, "options": options, "channel": channel.id, "channel_name": channel.name, "invoker": sender });
                if let Some(thread) = thread {
                    said["thread"] = json!(thread);
                }
                let mut outcome = Outcome::reply(reply_to(message, BOT_SENT, json!({ "id": id })));
                outcome.notices.push((bot.to_string(), json!({ "type": INVOKE, "id": id, "body": said })));
                Ok(Some(outcome))
            }

            BOT_PRESS => {
                let Some(post) = self.referred(&body["to"])? else {
                    return refuse(message, "e.p.msg.unknown-post", "There is no such post.").map(Some);
                };
                if !self.reads_post_id(sender, &post)? {
                    return refuse(message, "e.p.msg.unknown-post", "There is no such post.").map(Some);
                }
                let button = body["button"].as_str().unwrap_or_default();
                let offered = post.extra.as_ref().and_then(|e| e["buttons"].as_array().cloned()).unwrap_or_default();
                if !offered.iter().any(|b| b["id"] == button) || self.store.bot(&post.author_did)?.is_none() {
                    return refuse(message, "e.p.msg.unknown-button", "That post has no such button.").map(Some);
                }
                let id = uuid::Uuid::new_v4().to_string();
                let mut outcome = Outcome::reply(reply_to(message, BOT_SENT, json!({ "id": id })));
                outcome.notices.push((
                    post.author_did.clone(),
                    json!({ "type": PRESS, "id": id, "thid": post.id, "body": { "post": post.id, "button": button, "channel": post.channel, "presser": sender } }),
                ));
                Ok(Some(outcome))
            }

            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bots_commands_and_buttons_are_held_to_their_shape() {
        let commands = commands_from(&json!([
            { "name": "weather", "description": "The weather somewhere", "options": [{ "name": "city", "type": "string", "required": true }, { "name": "Bad Name" }] },
            { "name": "Shout!" },
            { "name": "roll", "options": [{ "name": "sides", "type": "dice", "choices": ["6", "20"] }] },
        ]))
        .unwrap();
        assert_eq!(
            commands,
            json!([
                { "name": "weather", "description": "The weather somewhere", "options": [{ "name": "city", "type": "string", "required": true }] },
                { "name": "roll", "description": "", "options": [{ "name": "sides", "type": "string", "choices": ["6", "20"] }] },
            ])
        );
        assert_eq!(commands_from(&json!("nope")), None);
        let buttons = buttons_from(&json!([{ "id": "yes", "label": "Yes", "style": "primary" }, { "id": "", "label": "x" }, { "id": "no", "label": "No", "style": "rainbow" }])).unwrap();
        assert_eq!(buttons, json!([{ "id": "yes", "label": "Yes", "style": "primary" }, { "id": "no", "label": "No" }]));
    }
}

#[cfg(test)]
mod approval_tests {
    use crate::store::Store;
    use serde_json::json;

    #[test]
    fn a_bot_is_one_once_approved_and_those_from_before_are_approved_once() {
        let store = Store::in_memory().unwrap();
        store.set_bot("did:example:old", Some(&json!([])), 1).unwrap();
        assert_eq!(store.approve_bots_from_before(2).unwrap(), 1);
        assert!(store.bot("did:example:old").unwrap().is_some(), "there before: let be");
        store.set_bot("did:example:new", Some(&json!([])), 3).unwrap();
        assert_eq!(store.approve_bots_from_before(4).unwrap(), 0, "once only");
        assert!(store.bot("did:example:new").unwrap().is_none(), "new: waits");
        assert!(store.bot_asked("did:example:new").unwrap().is_some());
        store.set_bot_approved("did:example:new", Some("did:example:admin"), 5).unwrap();
        assert!(store.bot("did:example:new").unwrap().is_some());
    }
}
