//! A service's name, description and picture, changed by one of its admins while
//! it runs.
//!
//! What a service is called starts as its settings say. A protocol that lets an
//! admin change it (`set-profile`, in announcements/1.0 and community/1.0) carries
//! the change as user-profile/1.0 carries a profile -- the same `profile` object,
//! the same rules for what an absent or an emptied field means -- and what was
//! changed is kept here, with the rest of what the service keeps, to be laid over
//! the settings each time it starts.

use anyhow::Context;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::address::MAX_PICTURE_BYTES;
use crate::db::{Db, Location, Schema, MYSQL_TABLE};
use crate::params;

pub const MAX_NAME_CHARS: usize = 80;
pub const MAX_DESCRIPTION_CHARS: usize = 500;

/// A picture as it is kept and sent: its media type and its bytes, in base64.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Picture {
    pub media_type: String,
    pub base64: String,
}

impl Picture {
    /// As a `data:` URL: how a profile holds a picture that is inside it.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.base64)
    }
}

/// One field of a profile, as a change says of it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Field<T> {
    /// Not mentioned: as it was.
    #[default]
    Unchanged,
    /// Set to null or to nothing: back to what the settings say.
    Removed,
    Set(T),
}

/// What an admin asked to change.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Change {
    pub name: Field<String>,
    pub description: Field<String>,
    pub picture: Field<Picture>,
}

/// Why a change was refused: a problem report's code, and words for a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub code: &'static str,
    pub comment: String,
}

fn refused(code: &'static str, comment: impl Into<String>) -> Refused {
    Refused { code, comment: comment.into() }
}

fn text(value: &Value, limit: usize, what: &str) -> Result<Field<String>, Refused> {
    match value {
        Value::Null => Ok(Field::Removed),
        Value::String(said) if said.trim().is_empty() => Ok(Field::Removed),
        Value::String(said) if said.trim().chars().count() > limit => Err(refused("e.p.msg.too-long", format!("The {what} is longer than {limit} characters."))),
        Value::String(said) => Ok(Field::Set(said.trim().to_string())),
        _ => Err(refused("e.p.msg.invalid-profile", format!("The {what} is not text."))),
    }
}

impl Change {
    /// Reads the change a message asks for: its `body.profile`, with a picture as
    /// an attachment the profile names, as in user-profile/1.0's `profile`.
    pub fn read(message: &Value) -> Result<Self, Refused> {
        let Some(profile) = message["body"]["profile"].as_object() else {
            return Err(refused("e.p.msg.invalid-profile", "There is no profile in that."));
        };
        let field = |name: &str| profile.get(name);
        let mut change = Self::default();
        if let Some(name) = field("displayName") {
            change.name = text(name, MAX_NAME_CHARS, "name")?;
        }
        if let Some(description) = field("description") {
            change.description = text(description, MAX_DESCRIPTION_CHARS, "description")?;
        }
        match field("displayPicture") {
            None => {}
            Some(Value::Null) => change.picture = Field::Removed,
            Some(Value::String(reference)) if reference.is_empty() => change.picture = Field::Removed,
            Some(Value::String(reference)) => {
                let id = reference.rsplit('#').next().unwrap_or_default();
                let attachment = message["attachments"].as_array().into_iter().flatten().find(|attachment| attachment["id"] == id || attachment["@id"] == id);
                let Some(attachment) = attachment else {
                    return Err(refused("e.p.msg.invalid-picture", "The picture the profile names was not sent with it."));
                };
                let media_type = attachment["media_type"].as_str().unwrap_or_default().to_ascii_lowercase();
                if !matches!(media_type.as_str(), "image/png" | "image/jpeg" | "image/webp" | "image/gif") {
                    return Err(refused("e.p.msg.invalid-picture", "The picture has to be a PNG, JPEG, WebP or GIF."));
                }
                // Only a picture that is in the message: one that is a link would
                // have this service fetching whatever address it was handed.
                let Some(sent) = attachment["data"]["base64"].as_str() else {
                    return Err(refused("e.p.msg.invalid-picture", "The picture has to be sent in the message, not as a link."));
                };
                let b64 = base64::engine::general_purpose::STANDARD_NO_PAD;
                let bytes = b64
                    .decode(sent.trim_end_matches('='))
                    .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(sent.trim_end_matches('=')))
                    .map_err(|_| refused("e.p.msg.invalid-picture", "The picture is not base64."))?;
                if bytes.is_empty() {
                    return Err(refused("e.p.msg.invalid-picture", "The picture is empty."));
                }
                if bytes.len() > MAX_PICTURE_BYTES {
                    return Err(refused("e.p.msg.too-large", format!("The picture is over {} KB.", MAX_PICTURE_BYTES / 1024)));
                }
                change.picture = Field::Set(Picture { media_type, base64: base64::engine::general_purpose::STANDARD.encode(bytes) });
            }
            Some(_) => return Err(refused("e.p.msg.invalid-profile", "The picture is not a reference to an attachment.")),
        }
        if change == Self::default() {
            return Err(refused("e.p.msg.invalid-profile", "The profile changes nothing."));
        }
        Ok(change)
    }
}

/// What has been changed from the settings, and to what. A field that is `None`
/// is as the settings have it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changed {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<Picture>,
}

fn laid<T>(held: &mut Option<T>, field: Field<T>) {
    match field {
        Field::Unchanged => {}
        Field::Removed => *held = None,
        Field::Set(value) => *held = Some(value),
    }
}

impl Changed {
    pub fn apply(&mut self, change: Change) {
        laid(&mut self.name, change.name);
        laid(&mut self.description, change.description);
        laid(&mut self.picture, change.picture);
    }

    /// A user-profile/1.0 `profile` saying all of `name`, `description` and
    /// `picture`: what a service sends the people it serves when it has changed.
    pub fn profile_message(name: &str, description: &str, picture: Option<&Picture>) -> Value {
        let mut message = json!({
            "type": "https://didcomm.org/user-profile/1.0/profile",
            "body": { "profile": { "displayName": name, "description": description, "displayPicture": Value::Null } },
        });
        if let Some(picture) = picture {
            message["body"]["profile"]["displayPicture"] = json!("#picture");
            message["attachments"] = json!([{ "id": "picture", "media_type": picture.media_type, "data": { "base64": picture.base64 } }]);
        }
        message
    }
}

fn schema() -> Schema {
    static MYSQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let mysql = MYSQL.get_or_init(|| format!("CREATE TABLE IF NOT EXISTS wyvrn_profile (id BIGINT NOT NULL PRIMARY KEY, profile MEDIUMTEXT NOT NULL) {MYSQL_TABLE}"));
    Schema { sqlite: "CREATE TABLE IF NOT EXISTS wyvrn_profile (id INTEGER PRIMARY KEY, profile TEXT NOT NULL)", mysql }
}

/// Where what was changed is kept: with the service's store, whichever that is.
pub struct Kept {
    db: Db,
}

impl Kept {
    pub fn open(location: &Location, service: &str) -> anyhow::Result<Self> {
        Ok(Self { db: Db::open(location, &schema(), service).with_context(|| format!("the profile in {location}"))? })
    }

    /// Kept nowhere that outlasts this: for a test.
    pub fn scratch() -> anyhow::Result<Self> {
        Ok(Self { db: Db::scratch(&schema())? })
    }

    pub fn load(&self) -> anyhow::Result<Changed> {
        let held = self.db.lock().query_row("SELECT profile FROM wyvrn_profile WHERE id = 1", params![], |row| row.get::<String>(0))?;
        Ok(match held {
            Some(held) => serde_json::from_str(&held).context("the profile kept in the database")?,
            None => Changed::default(),
        })
    }

    pub fn save(&self, changed: &Changed) -> anyhow::Result<()> {
        let text = serde_json::to_string(changed)?;
        let mut session = self.db.lock();
        if session.execute("UPDATE wyvrn_profile SET profile = ?1 WHERE id = 1", params![text.clone()])? == 0 {
            session.execute("INSERT OR IGNORE INTO wyvrn_profile (id, profile) VALUES (1, ?1)", params![text.clone()])?;
            session.execute("UPDATE wyvrn_profile SET profile = ?1 WHERE id = 1", params![text])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOT: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn asking(profile: Value, attachments: Value) -> Value {
        json!({ "body": { "profile": profile }, "attachments": attachments })
    }

    #[test]
    fn a_change_is_read_as_a_profile_is_absent_is_unchanged_and_emptied_is_removed() {
        let change = Change::read(&asking(json!({ "displayName": " News ", "description": null }), json!([]))).unwrap();
        assert_eq!(change, Change { name: Field::Set("News".into()), description: Field::Removed, picture: Field::Unchanged });
        let change = Change::read(&asking(json!({ "displayPicture": "#p" }), json!([{ "id": "p", "media_type": "image/PNG", "data": { "base64": DOT } }]))).unwrap();
        assert!(matches!(change.picture, Field::Set(Picture { ref media_type, .. }) if media_type == "image/png"));
        assert_eq!(Change::read(&asking(json!({ "displayPicture": "" }), json!([]))).unwrap().picture, Field::Removed);
    }

    #[test]
    fn what_is_not_a_profile_or_not_a_picture_is_refused() {
        let code = |message: Value| Change::read(&message).unwrap_err().code;
        assert_eq!(code(json!({ "body": {} })), "e.p.msg.invalid-profile");
        assert_eq!(code(asking(json!({}), json!([]))), "e.p.msg.invalid-profile");
        assert_eq!(code(asking(json!({ "displayName": "x".repeat(81) }), json!([]))), "e.p.msg.too-long");
        assert_eq!(code(asking(json!({ "displayPicture": "#p" }), json!([]))), "e.p.msg.invalid-picture");
        assert_eq!(code(asking(json!({ "displayPicture": "#p" }), json!([{ "id": "p", "media_type": "image/svg+xml", "data": { "base64": DOT } }]))), "e.p.msg.invalid-picture");
        assert_eq!(code(asking(json!({ "displayPicture": "#p" }), json!([{ "id": "p", "media_type": "image/png", "data": { "links": ["https://example.com/a.png"] } }]))), "e.p.msg.invalid-picture");
        let big = base64::engine::general_purpose::STANDARD.encode(vec![0u8; MAX_PICTURE_BYTES + 1]);
        assert_eq!(code(asking(json!({ "displayPicture": "#p" }), json!([{ "id": "p", "media_type": "image/png", "data": { "base64": big } }]))), "e.p.msg.too-large");
    }

    #[test]
    fn what_was_changed_is_kept_and_a_removed_field_is_the_settings_again() {
        let kept = Kept::scratch().unwrap();
        assert_eq!(kept.load().unwrap(), Changed::default());
        let mut changed = Changed::default();
        changed.apply(Change { name: Field::Set("News".into()), description: Field::Set("All of it.".into()), picture: Field::Unchanged });
        kept.save(&changed).unwrap();
        changed.apply(Change { name: Field::Unchanged, description: Field::Removed, picture: Field::Unchanged });
        kept.save(&changed).unwrap();
        assert_eq!(kept.load().unwrap(), Changed { name: Some("News".into()), description: None, picture: None });
    }
}
