//! Welcome messages (community/1.0's "Welcome messages"): when someone is let in,
//! a line saying so in the channel an admin chose, as the community itself. The
//! lines are the admin's, or a built-in set, one picked at random each time, with
//! `{name}` where the newcomer's name goes.

use serde_json::{json, Value};

use crate::community::Community;
use crate::store::Role;

/// The setting: `{ channel, lines? }` as JSON; none, or no channel, for none.
pub const WELCOME: &str = "welcome";

/// The longest line, and the most lines, an admin may give.
pub const MAX_LINE: usize = 200;
pub const MAX_LINES: usize = 50;

/// Said when an admin has given none.
pub const BUILT_IN: [&str; 12] = [
    "{name} just arrived. Say hello!",
    "Welcome, {name}! Make yourself at home.",
    "{name} wandered in.",
    "Everyone, give {name} a warm welcome!",
    "Look who's here: {name}!",
    "{name} has joined the party.",
    "Glad you could make it, {name}.",
    "{name} found their way in.",
    "A new face: {name}. Welcome!",
    "{name} is here. Pull up a chair!",
    "Hello, {name}! Grab a snack and settle in.",
    "{name} stepped through the door.",
];

/// Lines as an admin gives them: each trimmed, one per line, with `{name}` in it,
/// none longer than [`MAX_LINE`], at most [`MAX_LINES`]; none, the built-in set.
pub fn read_lines(given: &Value) -> Result<Option<Vec<String>>, &'static str> {
    let Some(given) = given.as_array() else { return if given.is_null() { Ok(None) } else { Err("lines is a list of lines, or null for the built-in ones.") } };
    let lines: Vec<String> = given.iter().filter_map(Value::as_str).map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
    if lines.is_empty() {
        return Ok(None);
    }
    if lines.len() > MAX_LINES {
        return Err("At most 50 lines.");
    }
    if lines.iter().any(|l| l.chars().count() > MAX_LINE) {
        return Err("A line is at most 200 characters.");
    }
    if lines.iter().any(|l| !l.contains("{name}")) {
        return Err("Each line says {name} where the newcomer's name goes.");
    }
    Ok(Some(lines))
}

impl Community {
    /// The welcome as set: `{ channel?, lines? }`.
    pub fn welcome_setting(&self) -> anyhow::Result<Value> {
        Ok(self.store.setting(WELCOME)?.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_else(|| json!({})))
    }

    pub fn set_welcome(&self, channel: Option<&str>, lines: Option<Vec<String>>) -> anyhow::Result<()> {
        let mut set = json!({});
        if let Some(channel) = channel {
            set["channel"] = json!(channel);
        }
        if let Some(lines) = lines {
            set["lines"] = json!(lines);
        }
        self.store.set_setting(WELCOME, &set.to_string())?;
        Ok(())
    }

    /// Says `did` has been let in, where the community welcomes newcomers: in its
    /// channel, as the community. Whether it did.
    pub fn welcome(&self, did: &str, now: i64) -> anyhow::Result<bool> {
        let set = self.welcome_setting()?;
        let Some(channel) = set["channel"].as_str().map(|id| self.store.channel(id)).transpose()?.flatten() else { return Ok(false) };
        if channel.closed || channel.voice.is_some() {
            return Ok(false);
        }
        let Some(own) = self.own.clone() else { return Ok(false) };
        self.store.admit(&own, Role::Member, now)?;
        let Some(mut author) = self.store.member(&own)? else { return Ok(false) };
        author.name = Some(self.changed().name.unwrap_or_else(|| self.config.name.clone()));
        let lines: Vec<String> = set["lines"].as_array().map(|lines| lines.iter().filter_map(Value::as_str).map(str::to_string).collect()).filter(|l: &Vec<String>| !l.is_empty()).unwrap_or_else(|| BUILT_IN.iter().map(|l| l.to_string()).collect());
        // (Which one: any will do, and the id below is random already.)
        let id = uuid::Uuid::new_v4();
        let line = &lines[id.as_bytes()[0] as usize % lines.len()];
        let name = self.store.member(did)?.and_then(|m| m.name).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "A new member".to_string());
        let content = line.replace("{name}", &name);
        let added = self.store.add_post(&channel.id, &id.to_string(), &content, &author, None, None, now)?;
        self.store.set_written_with(&added.id, None, None, &[])?;
        // Who it is, by member id, for an app to show their name as it is now.
        let member = self.store.member_id(did, now)?;
        self.store.set_post_extra(&added.id, &json!({ "welcome": { "member": member, "line": line } }))?;
        Ok(true)
    }
}
