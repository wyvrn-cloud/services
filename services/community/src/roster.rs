//! community/1.0's member list for a community of thousands: `members-request`'s
//! `brief`, pages (`limit`, `after`, `counts`), `query`, `ids`, `bots`,
//! `address`, `did` and `ids_only`, and `member-request`.
//!
//! Everything said of everyone is read in a handful of statements ([`Roster`]):
//! each member's roles, member id, picture, profile and what they are here, the
//! bots, who is online. Asked one member at a time, it was eleven or so a
//! member, the roles read again for each, and every picture and banner read
//! whole and hashed to make its address. The addresses' hashes are kept beside
//! the pictures now (store.rs), and a brief list reads no picture, linked
//! account or description at all.

use std::collections::{HashMap, HashSet};

use base64::Engine;
use serde_json::{json, Map, Value};

use crate::community::Community;
use crate::member_ids::picture_hash;
use crate::roles::{Powers, EVERYONE_ROLE};
use crate::store::{Channel, Member, RoleDef};

/// The most entries a page holds, brief.
pub const MOST_BRIEF: usize = 200;
/// The most entries a page holds that aren't brief: each carries a summary of
/// each linked account, never the credentials (linked_summary.rs); the byte
/// budget below (`PAGE_BYTES`) is what bounds a page, whatever its count.
pub const MOST_FULL: usize = 25;
/// A page's size where it says no `limit` and only `query`.
pub const QUERY_PAGE: usize = 20;
/// The most members `ids` may name at once.
pub const MOST_IDS: usize = 100;
/// The longest `query`, in characters.
pub const MOST_QUERY_CHARS: usize = 64;
/// The most a page's entries come to, written out: it ends early rather than be
/// more. A mediator takes a forward of a few megabytes (10 MiB by default, a
/// quarter of it again in its wrapping), so every page stays well under.
pub const PAGE_BYTES: usize = 512 * 1024;

struct ProfileRow {
    description: Option<String>,
    banner: Option<String>,
    linked: Option<Value>,
    public_address: Option<String>,
    at: i64,
}

struct HereRow {
    nickname: Option<String>,
    picture: Option<String>,
}

/// Everything the list says of everyone (or of one member), read at once.
pub struct Roster {
    full: bool,
    /// Whether an entry carries the linked accounts' credentials themselves, as
    /// one member's whole entry does (`member-request`), beside their summaries.
    credentials: bool,
    /// Each member's linked accounts' summaries and their address's statement.
    linked: HashMap<String, crate::linked_summary::Kept>,
    roles: Vec<RoleDef>,
    owners: Vec<String>,
    held: HashMap<String, Vec<String>>,
    ids: HashMap<String, String>,
    pictures: HashMap<String, (String, i64)>,
    profiles: HashMap<String, ProfileRow>,
    here: HashMap<String, HereRow>,
    /// Each member whose profile says they are a bot: their commands, and
    /// whether that was approved.
    bots: HashMap<String, (Value, bool)>,
    signatures: HashMap<String, String>,
    timeouts: HashMap<String, i64>,
    online: HashMap<String, String>,
}

/// A hash kept, or, for a row kept before hashes were (and not yet given one as
/// the store was opened), worked out from the data the statement read instead.
fn hash_of(kept: Option<String>, data: Option<String>) -> Option<String> {
    kept.filter(|hash| !hash.is_empty()).or_else(|| data.map(|data| picture_hash(&data)))
}

impl Roster {
    /// What the list says of everyone, or only of `only` (one member's DID).
    /// `full`: everything, as `members` without `brief` says it; else what a row
    /// of the list shows. `timeouts`: who is timed out, for one told it.
    pub fn load(community: &Community, only: Option<&str>, full: bool, timeouts: bool) -> anyhow::Result<Self> {
        let store = &community.store;
        let filter = |sql: &str, column: &str| match only {
            Some(_) => format!("{sql} WHERE {column} = ?1"),
            None => sql.to_string(),
        };
        let one: Vec<wyvrn_service::db::Value> = only.map(|did| wyvrn_service::db::ToValue::to_value(&did)).into_iter().collect();
        let mut db = store.db.lock();
        let mut held: HashMap<String, Vec<String>> = HashMap::new();
        for (did, role) in db.query(&filter("SELECT did, role FROM member_roles", "did"), &one, |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)))? {
            held.entry(did).or_default().push(role);
        }
        for roles in held.values_mut() {
            roles.sort();
        }
        let ids = db.query(&filter("SELECT did, member FROM member_ids", "did"), &one, |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)))?.into_iter().collect();
        // The data itself only where no hash is kept for it yet.
        let pictures = db
            .query(&filter("SELECT did, hash, CASE WHEN hash IS NULL THEN data END, at FROM member_pictures", "did"), &one, |row| {
                Ok((row.get::<String>(0)?, row.get::<Option<String>>(1)?, row.get::<Option<String>>(2)?, row.get::<i64>(3)?))
            })?
            .into_iter()
            .filter_map(|(did, hash, data, at)| Some((did, (hash_of(hash, data)?, at))))
            .collect();
        let profile_sql = if full {
            "SELECT did, banner_hash, CASE WHEN banner_hash IS NULL THEN banner_data END, public_address, at, description, linked FROM member_profiles"
        } else {
            "SELECT did, banner_hash, CASE WHEN banner_hash IS NULL THEN banner_data END, public_address, at FROM member_profiles"
        };
        let profiles = db
            .query(&filter(profile_sql, "did"), &one, |row| {
                let (description, linked) = if full {
                    (row.get::<Option<String>>(5)?, row.get::<Option<String>>(6)?.and_then(|text| serde_json::from_str(&text).ok()))
                } else {
                    (None, None)
                };
                let banner = hash_of(row.get(1)?, row.get(2)?);
                Ok((row.get::<String>(0)?, ProfileRow { description, banner, linked, public_address: row.get(3)?, at: row.get(4)? }))
            })?
            .into_iter()
            .collect();
        let here = db
            .query(&filter("SELECT did, nickname, picture_hash, CASE WHEN picture_hash IS NULL THEN picture_data END FROM member_here", "did"), &one, |row| {
                Ok((row.get::<String>(0)?, HereRow { nickname: row.get(1)?, picture: hash_of(row.get(2)?, row.get(3)?) }))
            })?
            .into_iter()
            .collect();
        let bots = db
            .query(&filter("SELECT b.did, b.commands, a.did FROM member_bots b LEFT JOIN bot_approvals a ON a.did = b.did", "b.did"), &one, |row| {
                let commands = row.get::<String>(1)?;
                Ok((row.get::<String>(0)?, (serde_json::from_str(&commands).unwrap_or_else(|_| json!([])), row.get::<Option<String>>(2)?.is_some())))
            })?
            .into_iter()
            .collect();
        let linked = if full {
            let read = |text: Option<String>| text.and_then(|text| serde_json::from_str::<Value>(&text).ok());
            db.query(&filter("SELECT did, summaries, statement FROM member_linked", "did"), &one, |row| {
                Ok((row.get::<String>(0)?, crate::linked_summary::Kept { summaries: read(row.get(1)?), statement: read(row.get(2)?) }))
            })?
            .into_iter()
            .collect()
        } else {
            HashMap::new()
        };
        let signatures = if full {
            db.query(&filter("SELECT did, signature FROM member_signatures", "did"), &one, |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)))?.into_iter().collect()
        } else {
            HashMap::new()
        };
        let timeouts = if timeouts {
            let now = crate::gates::now();
            // (`now` compared here, as `timed_out_until` does, so that the answer can be kept.)
            db.query(&filter("SELECT did, until FROM timeouts", "did"), &one, |row| Ok((row.get::<String>(0)?, row.get::<Option<i64>>(1)?)))?
                .into_iter()
                .filter_map(|(did, until)| until.filter(|until| *until > now).map(|until| (did, until)))
                .collect()
        } else {
            HashMap::new()
        };
        drop(db);
        let roles = store.roles()?;
        let owners = community.owners()?;
        let online = community.seen_online()?;
        Ok(Self { full, credentials: false, linked, roles, owners, held, ids, pictures, profiles, here, bots, signatures, timeouts, online })
    }

    /// With the linked accounts' credentials in each entry: one member's whole
    /// entry, for their profile card.
    pub fn with_credentials(mut self) -> Self {
        self.credentials = true;
        self
    }

    /// What `did` may do, from what was read.
    pub fn powers(&self, did: &str) -> Powers {
        Powers::of(&self.roles, self.held.get(did).map_or(&[][..], Vec::as_slice), self.owners.iter().any(|owner| owner == did))
    }

    /// `did`'s member id: as kept, or made now for someone who has none yet.
    pub fn member_id(&self, community: &Community, did: &str) -> anyhow::Result<String> {
        match self.ids.get(did) {
            Some(id) => Ok(id.clone()),
            None => Ok(community.store.member_id(did, crate::gates::now())?),
        }
    }

    /// The name `member` is shown by: their nickname here, else their profile's.
    pub fn shown_name<'a>(&'a self, member: &'a Member) -> Option<&'a str> {
        self.here.get(&member.did).and_then(|here| here.nickname.as_deref()).or(member.name.as_deref())
    }

    pub fn is_bot(&self, did: &str) -> bool {
        self.bots.get(did).is_some_and(|(_, approved)| *approved)
    }

    pub fn public_address(&self, did: &str) -> Option<&str> {
        self.profiles.get(did).and_then(|profile| profile.public_address.as_deref())
    }

    pub fn presence(&self, did: &str) -> Option<&str> {
        self.online.get(did).map(String::as_str)
    }

    /// The roles listed apart (`hoist`), highest first.
    fn hoisted(&self) -> impl Iterator<Item = &RoleDef> {
        self.roles.iter().filter(|role| role.hoist && role.id != EVERYONE_ROLE)
    }

    /// Where `did` is listed: under the `n`th role listed apart (`n`), the rest of
    /// those online after them, then everyone else.
    fn group(&self, did: &str) -> usize {
        let hoisted = self.hoisted().count();
        if self.presence(did).is_none() {
            return hoisted + 1;
        }
        let held = self.held.get(did);
        self.hoisted().position(|role| held.is_some_and(|held| held.contains(&role.id))).unwrap_or(hoisted)
    }

    /// The role whose heading `did` is listed under, if any.
    fn heading(&self, did: &str) -> Option<&str> {
        self.hoisted().nth(self.group(did)).map(|role| role.id.as_str())
    }

    /// Where `member` comes in a page: their group, then their name (letter case
    /// not counted), then their member id.
    pub fn key(&self, community: &Community, member: &Member) -> anyhow::Result<Key> {
        let id = self.member_id(community, &member.did)?;
        let name = self.shown_name(member).map(str::to_lowercase).unwrap_or_else(|| id.clone());
        Ok(Key { group: self.group(&member.did), name, id })
    }

    /// Whether `member`'s name, or a word of it, starts with `query` (lower case).
    pub fn named_like(&self, member: &Member, query: &str) -> bool {
        let names = [self.shown_name(member), member.name.as_deref()];
        names.into_iter().flatten().any(|name| {
            let name = name.to_lowercase();
            name.starts_with(query) || name.split(|c: char| c.is_whitespace() || c == '-' || c == '_' || c == '.').any(|word| word.starts_with(query))
        })
    }

    fn picture_url(community: &Community, member: &str, kind: &str, hash: &str) -> String {
        format!("{}/m/{member}/{kind}/{hash}", community.public_url.trim_end_matches('/'))
    }

    /// `member` as `members` lists them: by DID here, which member_ids.rs turns
    /// into their member id for whoever is sent it. `brief`: only what a row of
    /// the list shows. `full` as loaded: an entry that isn't brief needs it.
    pub fn entry(&self, community: &Community, member: &Member, brief: bool) -> anyhow::Result<Value> {
        let brief = brief || !self.full;
        let did = member.did.as_str();
        let id = self.member_id(community, did)?;
        let powers = self.powers(did);
        // (The member id is put in already, so that it isn't looked up again on the
        // way out.)
        let mut listed = json!({ "did": did, "member": id, "role": powers.older_role(), "roles": powers.roles, "joined": member.joined });
        if !brief {
            listed["since"] = json!(member.since);
        }
        // (Brief or not: an admin's list says who has left.)
        if let Some(left_at) = member.left_at.filter(|_| !member.joined) {
            listed["left_at"] = json!(left_at);
        }
        if let Some(status) = self.presence(did) {
            listed["presence"] = json!(status);
        }
        let mut said = Map::new();
        if let Some(name) = &member.name {
            said.insert("displayName".into(), json!(name));
        }
        let picture = self.pictures.get(did);
        let mut at = picture.map(|(_, at)| *at).unwrap_or(0);
        if let Some((hash, _)) = picture {
            said.insert("picture".into(), json!(Self::picture_url(community, &id, "picture", hash)));
        }
        if let Some(profile) = self.profiles.get(did) {
            at = at.max(profile.at);
            if let Some(description) = profile.description.as_ref().filter(|_| !brief) {
                said.insert("description".into(), json!(description));
            }
            if let Some(hash) = &profile.banner {
                said.insert("banner".into(), json!(Self::picture_url(community, &id, "banner", hash)));
            }
            // What the server found of the linked accounts it checked, never the
            // credentials themselves but in one member's whole entry.
            if let Some(linked) = profile.linked.as_ref().filter(|_| !brief && self.credentials) {
                said.insert("linkedAccounts".into(), linked.clone());
            }
            if let Some(kept) = self.linked.get(did).filter(|_| !brief) {
                if let Some(summaries) = &kept.summaries {
                    said.insert("linkedAccountSummaries".into(), crate::linked_summary::with_addresses(summaries, &community.public_url, &id));
                }
                if let Some(statement) = kept.statement.as_ref().filter(|_| profile.public_address.is_some()) {
                    said.insert("memberStatement".into(), statement.clone());
                }
            }
            if let Some(address) = profile.public_address.as_ref().filter(|_| !brief) {
                said.insert("publicAddress".into(), json!(address));
            }
        }
        match self.bots.get(did) {
            Some((commands, true)) => {
                said.insert("bot".into(), json!(true));
                said.insert("commands".into(), commands.clone());
            }
            // Says it is a bot, not yet approved (WYV-89): shown as asking, not as one.
            Some((_, false)) if !brief => {
                said.insert("bot_pending".into(), json!(true));
            }
            _ => {}
        }
        if !said.is_empty() {
            said.insert("at".into(), json!(at));
            listed["profile"] = Value::Object(said);
        }
        let mut shown = member.name.clone();
        if let Some(here) = self.here.get(did) {
            let mut here_said = Map::new();
            if let Some(nickname) = &here.nickname {
                here_said.insert("nickname".into(), json!(nickname));
                shown = Some(nickname.clone());
            }
            if let Some(hash) = &here.picture {
                here_said.insert("picture".into(), json!(Self::picture_url(community, &id, "here", hash)));
            }
            if !here_said.is_empty() {
                listed["here"] = Value::Object(here_said);
            }
        }
        if let Some(name) = shown {
            listed["name"] = json!(name);
        }
        if !brief {
            if let Some(signature) = self.signatures.get(did) {
                listed["signature"] = json!(signature);
            }
        }
        if let Some(until) = self.timeouts.get(did) {
            listed["timed_out_until"] = json!(until);
        }
        Ok(listed)
    }
}

/// Where a member comes in a page ([`Roster::key`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    group: usize,
    name: String,
    id: String,
}

impl Key {
    /// As `next` says it: a string the app sends back as it is.
    fn written(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!([self.group, self.name, self.id]).to_string())
    }

    fn read(written: &str) -> Option<Self> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(written.trim()).ok()?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        Some(Self { group: usize::try_from(value[0].as_u64()?).ok()?, name: value[1].as_str()?.to_string(), id: value[2].as_str()?.to_string() })
    }
}

/// What a `members-request` asks for (community/1.0, "A community of thousands").
#[derive(Debug, Default)]
pub struct Asked {
    pub brief: bool,
    pub ids_only: bool,
    /// A page of at most this many, with `counts`.
    pub page: Option<usize>,
    pub after: Option<Key>,
    pub query: Option<String>,
    pub ids: Option<Vec<String>>,
    pub bots: bool,
    pub address: Option<String>,
    pub did: Option<String>,
}

impl Asked {
    pub fn read(body: &Value) -> Self {
        let brief = body["brief"] == true;
        let ids_only = body["ids_only"] == true;
        let query = body["query"].as_str().map(|query| query.trim().chars().take(MOST_QUERY_CHARS).collect::<String>().to_lowercase()).filter(|query| !query.is_empty());
        let after = body["after"].as_str().and_then(Key::read);
        let most = if brief || ids_only { MOST_BRIEF } else { MOST_FULL };
        let limit = body["limit"].as_u64().map(|limit| usize::try_from(limit).unwrap_or(usize::MAX).clamp(1, most));
        let page = limit.or_else(|| query.as_ref().map(|_| QUERY_PAGE.min(most))).or_else(|| body["after"].is_string().then_some(most));
        let ids = body["ids"].as_array().map(|ids| ids.iter().filter_map(Value::as_str).take(MOST_IDS).map(str::to_string).collect());
        let address = body["address"].as_str().map(str::to_string);
        let did = body["did"].as_str().map(str::to_string);
        let mut asked = Self { brief, ids_only, page, after, query, ids, bots: body["bots"] == true, address, did };
        // Whatever narrows the list is answered as a page, so that it too stops
        // at `PAGE_BYTES`: a hundred members named by `ids`, whole, could be
        // 12 MiB of linked accounts. Only the whole list, as asked for before
        // pages, is unbounded.
        if asked.page.is_none() && asked.narrows() && !asked.ids_only {
            asked.page = Some(most);
        }
        asked
    }

    /// Whether it lists only some: not the whole list, whose answer has `waiting`.
    fn narrows(&self) -> bool {
        self.query.is_some() || self.ids.is_some() || self.bots || self.address.is_some() || self.did.is_some()
    }

    /// Whether it asks for anything an app written before pages doesn't.
    pub fn anything(&self) -> bool {
        self.brief || self.ids_only || self.page.is_some() || self.after.is_some() || self.narrows()
    }
}

impl Community {
    /// `members` for `asker`, as `asked` asks: the whole list, as before, or what
    /// it narrows it to, brief, a page.
    pub(crate) fn members_answer(&self, asker: &Member, asked: &Asked, channel: Option<&Channel>) -> anyhow::Result<Value> {
        let asker_powers = self.powers(&asker.did)?;
        let roster = Roster::load(self, None, !asked.brief && !asked.ids_only, asker_powers.has("timeout_members"))?;
        let own: HashSet<String> = self.own_dids().into_iter().collect();
        let mut members: Vec<Member> = self.store.members()?.into_iter().filter(|member| !own.contains(&member.did)).collect();
        // A DID tells one not told DIDs nothing: as for a DID nobody is at.
        let told = if asked.did.is_some() || asked.ids.as_ref().is_some_and(|ids| ids.iter().any(|id| id.starts_with("did:"))) { self.told_dids(&asker.did)? } else { false };
        if let Some(did) = &asked.did {
            let did = if told { Some(self.store.current_did(did)?) } else { None };
            members.retain(|member| Some(&member.did) == did.as_ref());
        }
        if let Some(ids) = &asked.ids {
            let mut by_id = HashMap::with_capacity(members.len());
            for member in &members {
                by_id.insert(roster.member_id(self, &member.did)?, member.did.clone());
            }
            let mut wanted = HashSet::new();
            for id in ids {
                if id.starts_with("did:") {
                    if told {
                        wanted.insert(self.store.current_did(id)?);
                    }
                } else if let Some(did) = by_id.get(id) {
                    wanted.insert(did.clone());
                } else if let Some(did) = self.store.did_of_member(id)? {
                    wanted.insert(did);
                }
            }
            members.retain(|member| wanted.contains(&member.did));
        }
        if let Some(address) = &asked.address {
            members.retain(|member| roster.public_address(&member.did) == Some(address.as_str()));
        }
        if asked.bots {
            members.retain(|member| roster.is_bot(&member.did));
        }
        if let Some(query) = &asked.query {
            members.retain(|member| roster.named_like(member, query));
        }
        if let Some(channel) = channel {
            let mut seeing = Vec::with_capacity(members.len());
            for member in members {
                if self.sees(&member.did, &roster.powers(&member.did), channel)? {
                    seeing.push(member);
                }
            }
            members = seeing;
        }
        let mut body = json!({});
        let mut next = None;
        if let Some(limit) = asked.page {
            let mut keyed = Vec::with_capacity(members.len());
            for member in members {
                keyed.push((roster.key(self, &member)?, member));
            }
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            let mut roles = Map::new();
            let mut online = 0;
            for (_, member) in &keyed {
                if roster.presence(&member.did).is_some() {
                    online += 1;
                    if let Some(role) = roster.heading(&member.did) {
                        let n = roles.get(role).and_then(Value::as_u64).unwrap_or(0);
                        roles.insert(role.to_string(), json!(n + 1));
                    }
                }
            }
            body["counts"] = json!({ "total": keyed.len(), "online": online, "roles": roles });
            let start = asked.after.as_ref().map_or(0, |after| keyed.partition_point(|(key, _)| key <= after));
            let rest: Vec<(Key, Member)> = keyed.into_iter().skip(start).collect();
            let mut taken = Vec::new();
            let mut bytes = 0;
            for (key, member) in &rest {
                if taken.len() == limit {
                    break;
                }
                let entry = if asked.ids_only { json!(roster.member_id(self, &member.did)?) } else { roster.entry(self, member, asked.brief)? };
                let size = entry.to_string().len() + 1;
                if !taken.is_empty() && bytes + size > PAGE_BYTES {
                    break;
                }
                bytes += size;
                taken.push((key.clone(), entry));
            }
            if taken.len() < rest.len() {
                next = taken.last().map(|(key, _)| key.written());
            }
            let entries: Vec<Value> = taken.into_iter().map(|(_, entry)| entry).collect();
            body[if asked.ids_only { "ids" } else { "members" }] = json!(entries);
        } else if asked.ids_only {
            body["ids"] = json!(members.iter().map(|member| roster.member_id(self, &member.did)).collect::<anyhow::Result<Vec<_>>>()?);
        } else {
            body["members"] = json!(members.iter().map(|member| roster.entry(self, member, asked.brief)).collect::<anyhow::Result<Vec<_>>>()?);
        }
        if let Some(next) = next {
            body["next"] = json!(next);
        }
        if let Some(channel) = channel {
            body["channel"] = json!(channel.id);
        } else if !asked.narrows() && asked.after.is_none() && !asked.ids_only && asker_powers.has("admit_members") {
            body["waiting"] = json!(self.waiting_body()?);
        }
        Ok(body)
    }

    /// One member's whole entry, as `members` lists it, for `asker`: `None` for
    /// someone who isn't in the community.
    pub(crate) fn member_entry_for(&self, asker: &Member, did: &str) -> anyhow::Result<Option<Value>> {
        let did = self.store.current_did(did)?;
        let Some(member) = self.store.member(&did)?.filter(|member| !self.own_dids().contains(&member.did)) else {
            return Ok(None);
        };
        let roster = Roster::load(self, Some(&did), true, self.powers(&asker.did)?.has("timeout_members"))?.with_credentials();
        Ok(Some(roster.entry(self, &member, false)?))
    }

    /// `message` as it is read, where it is a `member-request` naming someone by
    /// DID from a member not told DIDs: as naming nobody. Seen before member ids
    /// are turned into DIDs (server.rs), which they are as `member` too: after,
    /// one given can't be told from one turned.
    pub fn untold_dids_dropped(&self, sender: &str, message: &Value) -> anyhow::Result<Value> {
        let names_did = |key: &str| message["body"][key].as_str().is_some_and(|named| named.starts_with("did:"));
        if message["type"] != crate::community::MEMBER_REQUEST || !(names_did("member") || names_did("did")) || self.told_dids(sender)? {
            return Ok(message.clone());
        }
        let mut dropped = message.clone();
        dropped["body"] = json!({ "member": "" });
        Ok(dropped)
    }

    /// The answer to a change of one member (`set-member`, `timeout`,
    /// `set-member-profile`): the whole of `members`, or, where it says
    /// `"answer": "member"`, `member`, their entry as it now stands.
    pub(crate) fn change_answer(&self, message: &Value, asker: &Member, changed: &str) -> anyhow::Result<Value> {
        use crate::community::{reply_to, MEMBER, MEMBERS};
        if message["body"]["answer"] == "member" {
            let entry = self.member_entry_for(asker, changed)?.unwrap_or_else(|| json!({ "did": changed, "gone": true }));
            return Ok(reply_to(message, MEMBER, json!({ "member": entry })));
        }
        Ok(reply_to(message, MEMBERS, self.members_for(asker)?))
    }
}
