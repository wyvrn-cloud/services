//! community/1.0's "Who members are to each other" and "Profiles".
//!
//! A member's DID is their address, and being in a community isn't agreeing to give
//! it to everyone there. So members are known to each other by a member id, and
//! their DIDs go only to those who moderate. Everything else in the server keeps
//! working in DIDs: this is where they become member ids on the way out
//! ([`Community::masked_for`]) and DIDs again on the way in
//! ([`Community::unmasked_from`]), so that no field can be missed by a builder that
//! forgot.
//!
//! It also keeps what everyone is told of each member's profile, beyond the name
//! and picture kept already (members, member_pictures): their description, banner,
//! linked accounts and public address, and what they are in this community alone.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

use crate::community::Community;
use crate::store::Store;
use wyvrn_service::db::Result;
use wyvrn_service::params;

/// The permissions whose holders are told DIDs (with being the owner).
pub const MODERATING: &[&str] = &["administrator", "kick_members", "ban_members", "timeout_members", "manage_roles", "manage_messages", "admit_members", "manage_reports"];

/// Keys whose value, a string or an array of strings, names a person where it is a DID.
const PERSON_KEYS: &[&str] = &[
    "did", "by", "author", "answered_by", "removed_by", "created_by", "last_by", "resolved_by", "who", "voters", "owner", "owners", "you", "actor", "target", "member", "participants", "speakers", "to",
    "invited_by", "holder", "from", "invoker", "presser",
];

/// Messages whose people a member told DIDs is given the DID of too, beside the
/// member id: the lists only they are sent, or where the DID is the point.
const WITH_DIDS: &[&str] = &[
    crate::community::MEMBERS,
    crate::community::ASKED_TO_JOIN,
    crate::community::MEMBER_CHANGED,
    "https://wyvrn.app/community/1.0/bans",
    "https://wyvrn.app/community/1.0/member-history",
];

/// A request passes the asker's DID to the one person it is for, whoever they are.
const DID_GIVEN: &[&str] = &[crate::community::MESSAGE_REQUEST];

/// How long a member id is: an `m` and 26 characters of a random UUID.
fn new_member_id() -> String {
    format!("m{}", &uuid::Uuid::new_v4().simple().to_string()[..26])
}

/// The address part a picture's data gives it: a new picture, a new address.
/// The most a side of the picture given to Discord may be, in pixels.
pub const SMALL_PICTURE: u32 = 256;

/// A picture (`media_type`, base64) made fit for Discord's webhook avatar: kept as
/// it is when it is a PNG, JPEG or GIF no bigger than `SMALL_PICTURE` a side;
/// otherwise scaled down to fit (never up) and written as PNG where it has
/// see-through parts, else JPEG. `None` if it can't be read.
pub fn small_picture(media_type: &str, base64_data: &str) -> Option<(String, Vec<u8>)> {
    use base64::Engine;
    use image::GenericImageView;
    let trimmed = base64_data.trim_end_matches('=');
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD.decode(trimmed).ok()?;
    let read = image::load_from_memory(&bytes).ok()?;
    let (width, height) = read.dimensions();
    let small = width <= SMALL_PICTURE && height <= SMALL_PICTURE;
    if small && matches!(media_type, "image/png" | "image/jpeg" | "image/gif") {
        return Some((media_type.to_string(), bytes));
    }
    let fitted = if small { read } else { read.thumbnail(SMALL_PICTURE, SMALL_PICTURE) };
    let mut out = std::io::Cursor::new(Vec::new());
    if fitted.color().has_alpha() {
        fitted.write_to(&mut out, image::ImageFormat::Png).ok()?;
        Some(("image/png".into(), out.into_inner()))
    } else {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&fitted.to_rgb8()).ok()?;
        Some(("image/jpeg".into(), out.into_inner()))
    }
}

pub fn picture_hash(data: &str) -> String {
    let digest = Sha256::digest(data.as_bytes());
    digest.iter().take(10).map(|byte| format!("{byte:02x}")).collect()
}

/// What a member's profile says beyond their name and picture.
#[derive(Debug, Default, Clone)]
pub struct MemberProfile {
    pub description: Option<String>,
    pub banner: Option<(String, String)>,
    pub linked: Option<Value>,
    pub public_address: Option<String>,
    pub at: i64,
}

/// What a member is in this community alone.
#[derive(Debug, Clone)]
pub struct MemberHere {
    pub nickname: Option<String>,
    pub picture: Option<(String, String)>,
    pub requests: bool,
    pub at: i64,
}

impl Default for MemberHere {
    fn default() -> Self {
        Self { nickname: None, picture: None, requests: true, at: 0 }
    }
}

impl Store {
    /// The DID `did` is now: itself, or the one its member moved to, however many
    /// moves on.
    pub fn current_did(&self, did: &str) -> Result<String> {
        let mut current = did.to_string();
        for _ in 0..32 {
            match self.db.lock().query_row("SELECT became FROM superseded WHERE did = ?1", params![current], |row| row.get::<String>(0))? {
                Some(became) if became != current => current = became,
                _ => break,
            }
        }
        Ok(current)
    }

    /// `did`'s member id, made now if they have none: the one kept for the DID they
    /// are at now.
    pub fn member_id(&self, did: &str, now: i64) -> Result<String> {
        let current = self.current_did(did)?;
        let mut db = self.db.lock();
        if let Some(id) = db.query_row("SELECT member FROM member_ids WHERE did = ?1", params![current], |row| row.get::<String>(0))? {
            return Ok(id);
        }
        let id = new_member_id();
        db.execute("INSERT INTO member_ids (did, member, created) VALUES (?1, ?2, ?3)", params![current, id, now])?;
        Ok(id)
    }

    /// Whom a member id names, at the DID they are at now; none for an id nobody
    /// was given.
    pub fn did_of_member(&self, member: &str) -> Result<Option<String>> {
        let did = self.db.lock().query_row("SELECT did FROM member_ids WHERE member = ?1", params![member], |row| row.get::<String>(0))?;
        match did {
            Some(did) => Ok(Some(self.current_did(&did)?)),
            None => Ok(None),
        }
    }

    /// The member whose profile names `address` as their public address: kept only
    /// once shown to be theirs (server.rs, `holder_among`), so a link proven over
    /// DIDComm. The most recent, should two have claimed it in turn.
    pub fn member_with_address(&self, address: &str) -> Result<Option<String>> {
        self.db.lock().query_row(
            "SELECT member_profiles.did FROM member_profiles JOIN members ON members.did = member_profiles.did WHERE member_profiles.public_address = ?1 ORDER BY member_profiles.at DESC LIMIT 1",
            params![address],
            |row| row.get::<String>(0),
        )
    }

    pub fn member_profile(&self, did: &str) -> Result<Option<MemberProfile>> {
        self.db.lock().query_row(
            "SELECT description, banner_type, banner_data, linked, public_address, at FROM member_profiles WHERE did = ?1",
            params![did],
            |row| {
                let banner = match (row.get::<Option<String>>(1)?, row.get::<Option<String>>(2)?) {
                    (Some(media_type), Some(data)) => Some((media_type, data)),
                    _ => None,
                };
                let linked = row.get::<Option<String>>(3)?.and_then(|text| serde_json::from_str(&text).ok());
                Ok(MemberProfile { description: row.get(0)?, banner, linked, public_address: row.get(4)?, at: row.get(5)? })
            },
        )
    }

    pub fn set_member_profile(&self, did: &str, profile: &MemberProfile) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM member_profiles WHERE did = ?1", params![did])?;
        db.execute(
            "INSERT INTO member_profiles (did, description, banner_type, banner_data, linked, public_address, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                did,
                profile.description,
                profile.banner.as_ref().map(|(media_type, _)| media_type.clone()),
                profile.banner.as_ref().map(|(_, data)| data.clone()),
                profile.linked.as_ref().map(Value::to_string),
                profile.public_address,
                profile.at
            ],
        )?;
        Ok(())
    }

    pub fn member_here(&self, did: &str) -> Result<Option<MemberHere>> {
        self.db.lock().query_row("SELECT nickname, picture_type, picture_data, requests, at FROM member_here WHERE did = ?1", params![did], |row| {
            let picture = match (row.get::<Option<String>>(1)?, row.get::<Option<String>>(2)?) {
                (Some(media_type), Some(data)) => Some((media_type, data)),
                _ => None,
            };
            Ok(MemberHere { nickname: row.get(0)?, picture, requests: row.get::<i64>(3)? != 0, at: row.get(4)? })
        })
    }

    pub fn set_member_here(&self, did: &str, here: &MemberHere) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM member_here WHERE did = ?1", params![did])?;
        db.execute(
            "INSERT INTO member_here (did, nickname, picture_type, picture_data, requests, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                did,
                here.nickname,
                here.picture.as_ref().map(|(media_type, _)| media_type.clone()),
                here.picture.as_ref().map(|(_, data)| data.clone()),
                i64::from(here.requests),
                here.at
            ],
        )?;
        Ok(())
    }

    /// Notes a request from `from` to `to` as passed on, unless one was in the last
    /// day: whether it may be.
    pub fn take_request_turn(&self, from: &str, to: &str, now: i64) -> Result<bool> {
        let mut db = self.db.lock();
        let last = db.query_row("SELECT at FROM message_requests WHERE from_did = ?1 AND to_did = ?2", params![from, to], |row| row.get::<i64>(0))?;
        if last.is_some_and(|at| now - at < 24 * 60 * 60) {
            return Ok(false);
        }
        db.execute("DELETE FROM message_requests WHERE from_did = ?1 AND to_did = ?2", params![from, to])?;
        db.execute("INSERT INTO message_requests (from_did, to_did, at) VALUES (?1, ?2, ?3)", params![from, to, now])?;
        Ok(true)
    }
}

impl Community {
    /// Whether `did` is told DIDs: the owner, or one holding a moderation
    /// permission, `manage_messages` in any channel included.
    pub fn told_dids(&self, did: &str) -> anyhow::Result<bool> {
        if self.is_owner(did)? {
            return Ok(true);
        }
        let powers = self.powers(did)?;
        if MODERATING.iter().any(|permission| powers.has(permission)) {
            return Ok(true);
        }
        for channel in self.store.channels()? {
            if self.channel_permissions(did, &powers, &channel).is_ok_and(|held| held.contains("manage_messages")) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The DIDs that are the community itself, never a person's: what it writes as.
    pub(crate) fn own_dids(&self) -> Vec<String> {
        self.own.iter().chain(self.speaks_as.iter()).cloned().collect()
    }

    /// `message` as `to` may be sent it: every person in its body named by member
    /// id, and by DID too only where `to` is told DIDs and the message is one that
    /// carries them (`WITH_DIDS`), or where the DID was given to them (`DID_GIVEN`).
    pub fn masked_for(&self, to: &str, message: &Value) -> anyhow::Result<Value> {
        let kind = message["type"].as_str().unwrap_or_default();
        let keep_dids = DID_GIVEN.contains(&kind) || (WITH_DIDS.contains(&kind) && self.told_dids(to)?);
        let mut masked = message.clone();
        if let Some(body) = masked.get_mut("body") {
            let mut masker = Masker { community: self, own: self.own_dids(), keep_dids, ids: HashMap::new(), now: crate::gates::now() };
            masker.walk(body, None)?;
        }
        Ok(masked)
    }

    /// `message` from `sender` with every member id it names a person by (in
    /// `member`, `to`, `who`, `author`, `did`) turned into whom it names: where a
    /// field was `did`, as before, a `did` beside it.
    pub fn unmasked_from(&self, message: &Value) -> anyhow::Result<Value> {
        let mut unmasked = message.clone();
        if let Some(body) = unmasked.get_mut("body") {
            unmask(&self.store, body)?;
        }
        Ok(unmasked)
    }

    /// The address a member's picture is at (community/1.0's "A picture is an
    /// address"): `kind` is `picture`, `banner` or `here`.
    /// The community's picture as Discord's webhook avatar takes it: at most
    /// `SMALL_PICTURE` pixels a side, and a PNG, JPEG or GIF (`media type, bytes`).
    pub fn own_picture_small(&self) -> Option<(String, Vec<u8>)> {
        let picture = self.changed().picture?;
        small_picture(&picture.media_type, &picture.base64)
    }

    /// Where the community's own picture is, if it has one, as `own_picture_small`
    /// makes it: an address that names this picture, so a new one is at a new
    /// address. For what can only be given a link (a Discord webhook's `avatar_url`).
    pub fn own_picture_url(&self) -> Option<String> {
        let picture = self.changed().picture?;
        Some(format!("{}/picture/{}", self.public_url.trim_end_matches('/'), picture_hash(&picture.base64)))
    }

    pub fn picture_url(&self, member: &str, kind: &str, data: &str) -> String {
        let base = self.public_url.trim_end_matches('/');
        format!("{base}/m/{member}/{kind}/{}", picture_hash(data))
    }

    /// The picture at an address [`Self::picture_url`] gave, while it is still that
    /// picture: `(media type, base64)`.
    pub fn picture_at(&self, member: &str, kind: &str, hash: &str) -> anyhow::Result<Option<(String, String)>> {
        let Some(did) = self.store.did_of_member(member)? else { return Ok(None) };
        let picture = match kind {
            "picture" => self.store.member_picture(&did)?.map(|(media_type, data, _)| (media_type, data)),
            "banner" => self.store.member_profile(&did)?.and_then(|profile| profile.banner),
            "here" => self.store.member_here(&did)?.and_then(|here| here.picture),
            _ => None,
        };
        Ok(picture.filter(|(_, data)| picture_hash(data) == hash))
    }

    /// What `members` says of `did`'s profile, and of what they are here:
    /// `(name, profile, here)`, each `None` with nothing to say.
    pub fn profile_entry(&self, did: &str, name: Option<&str>) -> anyhow::Result<(Option<String>, Option<Value>, Option<Value>)> {
        let member = self.store.member_id(did, crate::gates::now())?;
        let profile = self.store.member_profile(did)?;
        let picture = self.store.member_picture(did)?;
        let here = self.store.member_here(did)?;
        let mut said = Map::new();
        if let Some(name) = name {
            said.insert("displayName".into(), json!(name));
        }
        let mut at = picture.as_ref().map(|(_, _, at)| *at).unwrap_or(0);
        if let Some((_, data, _)) = &picture {
            said.insert("picture".into(), json!(self.picture_url(&member, "picture", data)));
        }
        if let Some(profile) = &profile {
            at = at.max(profile.at);
            if let Some(description) = &profile.description {
                said.insert("description".into(), json!(description));
            }
            if let Some((_, data)) = &profile.banner {
                said.insert("banner".into(), json!(self.picture_url(&member, "banner", data)));
            }
            if let Some(linked) = &profile.linked {
                said.insert("linkedAccounts".into(), linked.clone());
            }
            if let Some(address) = &profile.public_address {
                said.insert("publicAddress".into(), json!(address));
            }
        }
        if let Some(commands) = self.store.bot(&did)? {
            said.insert("bot".into(), json!(true));
            said.insert("commands".into(), commands);
        } else if self.store.bot_asked(&did)?.is_some() {
            // Says it is a bot, not yet approved (WYV-89): shown as asking, not as one.
            said.insert("bot_pending".into(), json!(true));
        }
        let profile_value = (!said.is_empty()).then(|| {
            said.insert("at".into(), json!(at));
            Value::Object(said)
        });
        let mut here_said = Map::new();
        let mut shown = name.map(str::to_string);
        if let Some(here) = &here {
            if let Some(nickname) = &here.nickname {
                here_said.insert("nickname".into(), json!(nickname));
                shown = Some(nickname.clone());
            }
            if let Some((_, data)) = &here.picture {
                here_said.insert("picture".into(), json!(self.picture_url(&member, "here", data)));
            }
        }
        Ok((shown, profile_value, (!here_said.is_empty()).then_some(Value::Object(here_said))))
    }
}

struct Masker<'a> {
    community: &'a Community,
    own: Vec<String>,
    keep_dids: bool,
    ids: HashMap<String, String>,
    now: i64,
}

impl Masker<'_> {
    fn id_of(&mut self, did: &str) -> anyhow::Result<String> {
        if let Some(id) = self.ids.get(did) {
            return Ok(id.clone());
        }
        let id = self.community.store.member_id(did, self.now)?;
        self.ids.insert(did.to_string(), id.clone());
        Ok(id)
    }

    /// A person's DID: one that isn't the community's own.
    fn is_person(&self, value: &str) -> bool {
        value.starts_with("did:") && !self.own.iter().any(|own| own == value)
    }

    fn walk(&mut self, value: &mut Value, key: Option<&str>) -> anyhow::Result<()> {
        let names_person = key.is_some_and(|key| PERSON_KEYS.contains(&key));
        match value {
            Value::String(text) if names_person && self.is_person(text) => {
                *text = self.id_of(text)?;
            }
            Value::Array(items) => {
                for item in items {
                    // An array of people (`voters`, `by`) is its key's; of objects, each its own.
                    let inner = if item.is_string() { key } else { None };
                    self.walk(item, inner)?;
                }
            }
            Value::Object(fields) => {
                // `{ did, name? }`: `{ member, name? }`, with the DID kept beside it
                // for a member told DIDs, where the message carries them.
                if let Some(did) = fields.get("did").and_then(Value::as_str).filter(|did| self.is_person(did)).map(str::to_string) {
                    let id = self.id_of(&did)?;
                    if !fields.get("member").and_then(Value::as_str).is_some_and(|member| !member.starts_with("did:")) {
                        fields.insert("member".into(), json!(id));
                    }
                    if !self.keep_dids {
                        fields.remove("did");
                    } else {
                        fields.insert("did".into(), json!(self.community.store.current_did(&did)?));
                    }
                }
                for (field, inner) in fields.iter_mut() {
                    if field == "did" || field == "content" || field == "snippet" {
                        continue;
                    }
                    self.walk(inner, Some(field.as_str()))?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Member ids in a message's body back to DIDs (see `Community::unmasked_from`).
fn unmask(store: &Store, value: &mut Value) -> anyhow::Result<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                unmask(store, item)?;
            }
        }
        Value::Object(fields) => {
            if let Some(member) = fields.get("member").and_then(Value::as_str).filter(|member| !member.starts_with("did:")).map(str::to_string) {
                if let Some(did) = store.did_of_member(&member)? {
                    fields.entry("did").or_insert_with(|| json!(did));
                    fields.insert("member".into(), json!(did));
                }
            }
            for key in ["to", "who", "author", "did"] {
                let Some(field) = fields.get_mut(key) else { continue };
                match field {
                    Value::String(member) if !member.starts_with("did:") => {
                        if let Some(did) = store.did_of_member(member)? {
                            *member = did;
                        }
                    }
                    Value::Array(members) => {
                        for entry in members.iter_mut() {
                            if let Value::String(member) = entry {
                                if !member.starts_with("did:") {
                                    if let Some(did) = store.did_of_member(member)? {
                                        *member = did;
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            for (key, inner) in fields.iter_mut() {
                if key != "content" {
                    unmask(store, inner)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// The picture a profile field names, as `Change::read` reads `displayPicture`:
/// one the message carries, never a link.
fn attached_picture(message: &Value, reference: &Value) -> std::result::Result<wyvrn_service::profile::Field<wyvrn_service::profile::Picture>, wyvrn_service::profile::Refused> {
    let asked = json!({ "body": { "profile": { "displayPicture": reference } }, "attachments": message["attachments"] });
    wyvrn_service::profile::Change::read(&asked).map(|change| change.picture)
}

const MAX_LINKED_ACCOUNTS: usize = 10;
const MAX_LINKED_BYTES: usize = 64 * 1024;
const MAX_NICKNAME_CHARS: usize = 32;
const MAX_NOTE_CHARS: usize = 200;

impl Community {
    /// `member-changed` for `did`, for every live connection: their entry in
    /// `members` as it now stands, or that they are gone.
    pub(crate) fn member_changed(&self, did: &str) -> anyhow::Result<Value> {
        let entry = match self.store.member(did)? {
            Some(member) => self.member_entry(&member, &self.powers(did)?, &self.seen_online()?)?,
            None => json!({ "did": did, "gone": true }),
        };
        Ok(json!({ "type": crate::community::MEMBER_CHANGED, "id": uuid::Uuid::new_v4().to_string(), "body": { "member": entry } }))
    }

    /// The rest of a member's user-profile/1.0 `profile`, beyond the name and
    /// picture `PROFILE` keeps: description, banner, linked accounts and public
    /// address. Whether anything changed.
    pub(crate) fn take_profile_rest(&self, sender: &str, message: &Value, now: i64) -> anyhow::Result<bool> {
        let profile = &message["body"]["profile"];
        let mut kept = self.store.member_profile(sender)?.unwrap_or_default();
        let before = (kept.description.clone(), kept.banner.clone(), kept.linked.clone(), kept.public_address.clone());
        if let Some(description) = profile.get("description") {
            kept.description = crate::community::line(description, wyvrn_service::profile::MAX_DESCRIPTION_CHARS);
        }
        if let Some(banner) = profile.get("banner") {
            match attached_picture(message, banner) {
                Ok(wyvrn_service::profile::Field::Set(picture)) => kept.banner = Some((picture.media_type, picture.base64)),
                Ok(wyvrn_service::profile::Field::Removed) => kept.banner = None,
                _ => {}
            }
        }
        if let Some(linked) = profile.get("linkedAccounts") {
            kept.linked = match linked {
                Value::Array(accounts) if !accounts.is_empty() && accounts.len() <= MAX_LINKED_ACCOUNTS && accounts.iter().all(Value::is_object) && linked.to_string().len() <= MAX_LINKED_BYTES => Some(linked.clone()),
                Value::Array(accounts) if accounts.is_empty() => None,
                Value::Null => None,
                _ => kept.linked,
            };
        }
        if let Some(address) = profile.get("publicAddress") {
            kept.public_address = address.as_str().filter(|address| address.starts_with("did:webvh:") && address.len() <= 1024).map(str::to_string);
        }
        let after = (kept.description.clone(), kept.banner.clone(), kept.linked.clone(), kept.public_address.clone());
        // Being a bot, and its commands (bots.rs).
        let bot_changed = self.take_bot(sender, profile, now)?;
        if before == after {
            return Ok(bot_changed);
        }
        kept.at = now;
        self.store.set_member_profile(sender, &kept)?;
        Ok(true)
    }

    /// `set-member-profile`: what this community alone shows of the sender.
    pub(crate) fn set_member_profile(&self, sender: &str, message: &Value, now: i64) -> anyhow::Result<crate::community::Outcome> {
        use crate::community::{refuse, reply_to, Outcome, MEMBERS};
        let body = &message["body"];
        let mut here = self.store.member_here(sender)?.unwrap_or_default();
        let mut said = false;
        if let Some(nickname) = body.get("nickname") {
            said = true;
            match nickname {
                Value::Null => here.nickname = None,
                Value::String(text) if text.trim().is_empty() => here.nickname = None,
                Value::String(text) if text.trim().chars().count() > MAX_NICKNAME_CHARS => return refuse(message, "e.p.msg.too-long", "A nickname is at most 32 characters."),
                Value::String(_) => here.nickname = crate::community::line(nickname, MAX_NICKNAME_CHARS),
                _ => return refuse(message, "e.p.msg.invalid-profile", "The nickname is not text."),
            }
        }
        if let Some(picture) = body.get("displayPicture") {
            said = true;
            match attached_picture(message, picture) {
                Ok(wyvrn_service::profile::Field::Set(picture)) => here.picture = Some((picture.media_type, picture.base64)),
                Ok(wyvrn_service::profile::Field::Removed) => here.picture = None,
                Ok(wyvrn_service::profile::Field::Unchanged) => {}
                Err(refused) => return refuse(message, refused.code, &refused.comment),
            }
        }
        if let Some(signature) = body.get("signature") {
            said = true;
            match signature {
                Value::Null => self.store.set_signature(sender, None)?,
                Value::String(text) if text.trim().is_empty() => self.store.set_signature(sender, None)?,
                Value::String(text) if text.trim().chars().count() > 300 => return refuse(message, "e.p.msg.too-long", "A signature is at most 300 characters."),
                Value::String(text) => self.store.set_signature(sender, Some(text.trim()))?,
                _ => return refuse(message, "e.p.msg.invalid-profile", "A signature is text."),
            }
        }
        if let Some(requests) = body.get("requests") {
            said = true;
            let Some(requests) = requests.as_bool() else {
                return refuse(message, "e.p.msg.invalid-profile", "Whether you take requests is true or false.");
            };
            here.requests = requests;
        }
        if !said {
            return refuse(message, "e.p.msg.invalid-profile", "That changes nothing.");
        }
        here.at = now;
        self.store.set_member_here(sender, &here)?;
        let Some(member) = self.store.member(sender)? else {
            return refuse(message, "e.p.trust.not-authorized", "You aren't in this community.");
        };
        let mut outcome = Outcome::reply(reply_to(message, MEMBERS, self.members_for(&member)?));
        outcome.live.push(self.member_changed(sender)?);
        Ok(outcome)
    }

    /// `message-request`: passed on to the member it names, with the sender's DID,
    /// unless that member takes none, either is banned or timed out, or one went
    /// from the sender to them in the last day. The sender is answered the same
    /// either way.
    pub(crate) fn message_request(&self, sender: &str, message: &Value, now: i64) -> anyhow::Result<crate::community::Outcome> {
        use crate::community::{refuse, reply_to, Outcome, MESSAGE_REQUEST, MESSAGE_REQUEST_SENT};
        let body = &message["body"];
        let Some(id) = body["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 64) else {
            return refuse(message, "e.p.msg.invalid", "A request needs an id.");
        };
        let Some(to) = body["to"].as_str().filter(|to| to.starts_with("did:")) else {
            return refuse(message, "e.p.msg.invalid-member", "There is nobody like that here.");
        };
        let to = self.store.current_did(to)?;
        let mut outcome = Outcome::reply(reply_to(message, MESSAGE_REQUEST_SENT, json!({ "id": id })));
        let Some(_) = self.store.member(&to)? else { return Ok(outcome) };
        let held_back = |did: &str| -> anyhow::Result<bool> { Ok(self.store.banned(did, now)?.is_some() || self.store.timed_out_until(did, now)?.is_some()) };
        let takes = self.store.member_here(&to)?.is_none_or(|here| here.requests);
        if to == sender || !takes || held_back(sender)? || held_back(&to)? || !self.store.take_request_turn(sender, &to, now)? {
            return Ok(outcome);
        }
        let mut from = json!({ "did": sender });
        let name = self.store.member(sender)?.and_then(|member| member.name);
        let shown = self.store.member_here(sender)?.and_then(|here| here.nickname).or(name);
        if let Some(name) = shown {
            from["name"] = json!(name);
        }
        let mut passed = json!({ "id": id, "from": from });
        if let Some(note) = crate::community::line(&body["note"], MAX_NOTE_CHARS) {
            passed["note"] = json!(note);
        }
        outcome.notices.push((to, json!({ "type": MESSAGE_REQUEST, "id": uuid::Uuid::new_v4().to_string(), "body": passed })));
        Ok(outcome)
    }
}

#[cfg(test)]
mod small_picture_tests {
    use super::*;
    use base64::Engine;
    use image::GenericImageView;

    fn encoded(picture: image::DynamicImage, format: image::ImageFormat) -> String {
        let mut out = std::io::Cursor::new(Vec::new());
        picture.write_to(&mut out, format).unwrap();
        base64::engine::general_purpose::STANDARD.encode(out.into_inner())
    }

    #[test]
    fn a_picture_is_made_at_most_256_a_side_and_of_a_kind_discord_takes() {
        // Big, and solid: scaled to fit, its shape kept, as JPEG.
        let big = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(600, 300, image::Rgb([200, 30, 30])));
        let (media_type, bytes) = small_picture("image/png", &encoded(big, image::ImageFormat::Png)).unwrap();
        assert_eq!(media_type, "image/jpeg");
        assert_eq!(image::load_from_memory(&bytes).unwrap().dimensions(), (256, 128));

        // Small and already a PNG: as it was.
        let small = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(64, 64, image::Rgba([0, 0, 0, 0])));
        let png = encoded(small.clone(), image::ImageFormat::Png);
        let (media_type, bytes) = small_picture("image/png", &png).unwrap();
        assert_eq!((media_type.as_str(), base64::engine::general_purpose::STANDARD.encode(&bytes)), ("image/png", png));

        // Small but a WebP, which Discord's avatar doesn't take: a PNG, its
        // see-through parts kept.
        let (media_type, bytes) = small_picture("image/webp", &encoded(small, image::ImageFormat::WebP)).unwrap();
        assert_eq!(media_type, "image/png");
        assert_eq!(image::load_from_memory(&bytes).unwrap().dimensions(), (64, 64));

        // Not a picture at all.
        assert!(small_picture("image/png", "bm90IGEgcGljdHVyZQ").is_none());
    }
}

#[cfg(test)]
mod address_tests {
    use super::*;
    use crate::store::{Role, Store};

    #[test]
    fn an_address_is_a_member_only_where_their_profile_proved_it() {
        let store = Store::in_memory().unwrap();
        store.set_member("did:peer:alice", Role::Member, 1).unwrap();
        let profile = |address: Option<&str>, at| MemberProfile { description: None, banner: None, linked: None, public_address: address.map(str::to_string), at };
        assert_eq!(store.member_with_address("did:web:id.example:alice").unwrap(), None);
        store.set_member_profile("did:peer:alice", &profile(Some("did:web:id.example:alice"), 5)).unwrap();
        assert_eq!(store.member_with_address("did:web:id.example:alice").unwrap().as_deref(), Some("did:peer:alice"));
        // Gone from the community: their address is nobody here.
        store.remove_member("did:peer:alice").unwrap();
        assert_eq!(store.member_with_address("did:web:id.example:alice").unwrap(), None);
    }
}
