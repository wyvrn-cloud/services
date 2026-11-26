//! Overrides for one member, a bot's own role, and Discord's roles and member
//! overrides carried over (community/1.0's "A channel's permissions", "A bot's
//! own role" and "Discord's roles and overrides, kept").
//!
//! - A channel (or a category, `category:<id>`) may allow and deny one member
//!   what it allows and denies a role, worked out last (`member_overrides`). One
//!   may name an imported Discord author (`did:wyvrn:tmp:discord:<id>`): it does
//!   nothing until a member claims that account, and is theirs then.
//! - An approved bot holds a role of its own, `managed`, held by it alone and gone
//!   with it (`bot_roles`).
//! - [`import`] reads a Discord server's member list, roles and channels (Discord's
//!   API, v10) and keeps, of each account, its roles as the roles here they match
//!   and its member overrides as overrides here naming the imported author
//!   (`discord_accounts`, `discord_account_overrides`, `discord_mapped`). A claim
//!   that succeeds gives the member those roles (only adding; one a moderator
//!   decides, only those ranked below them) and those overrides, once
//!   (`discord_given`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{json, Value};
use wyvrn_service::db::{Result, Session};
use wyvrn_service::params;

use crate::community::Community;
use crate::discord_history::AUTHOR_PREFIX;
use crate::roles::{category_key, is_channel_permission, Powers, EVERYONE_ROLE};
use crate::store::{RoleDef, Store};

/// What a channel or category allows and denies one member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberOverride {
    /// The channel's id, or `category:<id>`.
    pub channel: String,
    pub did: String,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

/// A Discord account, as the server's member list said it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordAccount {
    pub discord_id: String,
    pub name: Option<String>,
    pub bot: bool,
    /// Its roles there, as the roles here they matched.
    pub roles: Vec<String>,
}

/// One of Discord's roles or channels, matched to one here, or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    pub discord_id: String,
    pub local: Option<String>,
    pub name: String,
    /// A role Discord made for a bot: that bot's user id.
    pub bot: Option<String>,
}

/// What a Discord account gave the member who is it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Given {
    pub discord_id: String,
    pub did: String,
    pub roles: Vec<String>,
    pub overrides: i64,
    pub by: Option<String>,
}

fn list_from(text: String) -> Vec<String> {
    serde_json::from_str(&text).unwrap_or_default()
}

fn list_text(list: &[String]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".into())
}

fn override_from(row: &wyvrn_service::db::Row) -> Result<MemberOverride> {
    Ok(MemberOverride { channel: row.get(0)?, did: row.get(1)?, allow: list_from(row.get(2)?), deny: list_from(row.get(3)?) })
}

/// Two overrides made one: what either allows, and what either denies, a
/// permission both allow and deny being denied.
pub fn merged(a: (&[String], &[String]), b: (&[String], &[String])) -> (Vec<String>, Vec<String>) {
    let deny: BTreeSet<String> = a.1.iter().chain(b.1).cloned().collect();
    let allow: BTreeSet<String> = a.0.iter().chain(b.0).filter(|p| !deny.contains(*p)).cloned().collect();
    (allow.into_iter().collect(), deny.into_iter().collect())
}

/// Moves `prior`'s overrides for one member to `new`, each made one with an
/// override `new` has already in the same place. How many there were.
fn merge_in(tx: &mut Session<'_>, prior: &str, new: &str) -> Result<usize> {
    let theirs = tx.query("SELECT channel, did, allow, deny FROM member_overrides WHERE did = ?1", params![prior], override_from)?;
    for o in &theirs {
        let held = tx.query_row("SELECT channel, did, allow, deny FROM member_overrides WHERE channel = ?1 AND did = ?2", params![o.channel, new], override_from)?;
        let (allow, deny) = match &held {
            Some(h) => merged((&h.allow, &h.deny), (&o.allow, &o.deny)),
            None => (o.allow.clone(), o.deny.clone()),
        };
        tx.execute("INSERT OR REPLACE INTO member_overrides (channel, did, allow, deny) VALUES (?1, ?2, ?3, ?4)", params![o.channel, new, list_text(&allow), list_text(&deny)])?;
    }
    tx.execute("DELETE FROM member_overrides WHERE did = ?1", params![prior])?;
    Ok(theirs.len())
}

/// A member moving DID (`Store::rotate`): their overrides, their own role if a
/// bot, and what a Discord account gave them, follow them.
pub(crate) fn rotate_in(tx: &mut Session<'_>, prior: &str, new: &str) -> Result<()> {
    merge_in(tx, prior, new)?;
    tx.execute("UPDATE OR IGNORE bot_roles SET did = ?2 WHERE did = ?1", params![prior, new])?;
    tx.execute("DELETE FROM bot_roles WHERE did = ?1", params![prior])?;
    tx.execute("UPDATE discord_given SET did = ?2 WHERE did = ?1", params![prior, new])?;
    Ok(())
}

/// A member gone (`Store::remove_member`): their overrides go, and a bot's own
/// role, with every override naming it.
pub(crate) fn forget_member_in(db: &mut Session<'_>, did: &str) -> Result<()> {
    db.execute("DELETE FROM member_overrides WHERE did = ?1", params![did])?;
    if let Some(role) = db.query_row("SELECT role FROM bot_roles WHERE did = ?1", params![did], |row| row.get::<String>(0))? {
        db.execute("DELETE FROM member_roles WHERE role = ?1", params![role])?;
        db.execute("DELETE FROM channel_overrides WHERE role = ?1", params![role])?;
        db.execute("DELETE FROM roles WHERE id = ?1", params![role])?;
        db.execute("DELETE FROM bot_roles WHERE did = ?1", params![did])?;
    }
    Ok(())
}

impl Store {
    // --- Overrides for one member ------------------------------------------------

    /// What `channel` (or `category:<id>`) allows and denies each member it says
    /// anything of.
    pub fn member_overrides(&self, channel: &str) -> Result<Vec<MemberOverride>> {
        self.db.lock().query("SELECT channel, did, allow, deny FROM member_overrides WHERE channel = ?1 ORDER BY did", params![channel], override_from)
    }

    /// Every override naming `did`.
    pub fn member_overrides_of(&self, did: &str) -> Result<Vec<MemberOverride>> {
        self.db.lock().query("SELECT channel, did, allow, deny FROM member_overrides WHERE did = ?1 ORDER BY channel", params![did], override_from)
    }

    /// Sets what `channel` allows and denies `did`; with neither, forgets it.
    pub fn set_member_override(&self, channel: &str, did: &str, allow: &[String], deny: &[String]) -> Result<()> {
        let mut db = self.db.lock();
        if allow.is_empty() && deny.is_empty() {
            db.execute("DELETE FROM member_overrides WHERE channel = ?1 AND did = ?2", params![channel, did])?;
        } else {
            db.execute("INSERT OR REPLACE INTO member_overrides (channel, did, allow, deny) VALUES (?1, ?2, ?3, ?4)", params![channel, did, list_text(allow), list_text(deny)])?;
        }
        Ok(())
    }

    /// Moves `prior`'s overrides to `new`, each made one with `new`'s own where it
    /// has one in the same place: how many.
    pub fn merge_member_overrides(&self, prior: &str, new: &str) -> Result<usize> {
        self.db.lock().transaction(|tx| merge_in(tx, prior, new))
    }

    /// What every channel and category says of `role`: `(channel, allow, deny)`.
    pub fn overrides_of_role(&self, role: &str) -> Result<Vec<(String, Vec<String>, Vec<String>)>> {
        self.db.lock().query("SELECT channel, allow, deny FROM channel_overrides WHERE role = ?1", params![role], |row| Ok((row.get(0)?, list_from(row.get(1)?), list_from(row.get(2)?))))
    }

    // --- A bot's own role --------------------------------------------------------

    pub fn bot_role_of(&self, did: &str) -> Result<Option<String>> {
        self.db.lock().query_row("SELECT role FROM bot_roles WHERE did = ?1", params![did], |row| row.get(0))
    }

    /// Every bot's own role: `(the bot's DID, the role)`.
    pub fn bot_roles(&self) -> Result<Vec<(String, String)>> {
        self.db.lock().query("SELECT did, role FROM bot_roles", params![], |row| Ok((row.get(0)?, row.get(1)?)))
    }

    pub fn set_bot_role(&self, did: &str, role: &str) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO bot_roles (did, role) VALUES (?1, ?2)", params![did, role])?;
        Ok(())
    }

    /// The bots approved that are members.
    pub fn approved_bots(&self) -> Result<Vec<String>> {
        self.db.lock().query("SELECT a.did FROM bot_approvals a JOIN members m ON m.did = a.did ORDER BY a.did", params![], |row| row.get(0))
    }

    // --- Discord's roles and overrides, kept --------------------------------------

    pub fn discord_account(&self, discord_id: &str) -> Result<Option<DiscordAccount>> {
        self.db.lock().query_row("SELECT discord_id, name, bot, roles FROM discord_accounts WHERE discord_id = ?1", params![discord_id], |row| {
            Ok(DiscordAccount { discord_id: row.get(0)?, name: row.get(1)?, bot: row.get::<i64>(2)? != 0, roles: list_from(row.get(3)?) })
        })
    }

    pub fn set_discord_account(&self, account: &DiscordAccount, now: i64) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO discord_accounts (discord_id, name, bot, roles, at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![account.discord_id, account.name, account.bot as i64, list_text(&account.roles), now],
        )?;
        Ok(())
    }

    /// What each Discord channel gave `discord_id` alone, as Discord said it:
    /// `(Discord channel, the channel here or none, allow, deny)`.
    pub fn discord_account_overrides(&self, discord_id: &str) -> Result<Vec<(String, Option<String>, Vec<String>, Vec<String>)>> {
        self.db.lock().query(
            "SELECT discord_channel, channel, allow, deny FROM discord_account_overrides WHERE discord_id = ?1 ORDER BY discord_channel",
            params![discord_id],
            |row| Ok((row.get(0)?, row.get(1)?, list_from(row.get(2)?), list_from(row.get(3)?))),
        )
    }

    fn set_discord_account_overrides(&self, discord_id: &str, overrides: &[(String, Option<String>, Vec<String>, Vec<String>)]) -> Result<()> {
        self.db.lock().transaction(|tx| {
            tx.execute("DELETE FROM discord_account_overrides WHERE discord_id = ?1", params![discord_id])?;
            for (discord_channel, channel, allow, deny) in overrides {
                tx.execute(
                    "INSERT INTO discord_account_overrides (discord_id, discord_channel, channel, allow, deny) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![discord_id, discord_channel, channel, list_text(allow), list_text(deny)],
                )?;
            }
            Ok(())
        })
    }

    /// Discord's roles (`kind` `role`) or channels (`channel`) as matched here.
    pub fn discord_mapped(&self, kind: &str) -> Result<Vec<Mapped>> {
        self.db.lock().query("SELECT discord_id, local, name, bot FROM discord_mapped WHERE kind = ?1 ORDER BY discord_id", params![kind], |row| {
            Ok(Mapped { discord_id: row.get(0)?, local: row.get(1)?, name: row.get(2)?, bot: row.get(3)? })
        })
    }

    fn set_discord_mapped(&self, kind: &str, mapped: &Mapped) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO discord_mapped (kind, discord_id, local, name, bot) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![kind, mapped.discord_id, mapped.local, mapped.name, mapped.bot],
        )?;
        Ok(())
    }

    /// What the Discord account `discord_id` gave the member who is it, if it did.
    pub fn discord_given(&self, discord_id: &str) -> Result<Option<Given>> {
        self.db.lock().query_row("SELECT discord_id, did, roles, overrides, by_did FROM discord_given WHERE discord_id = ?1", params![discord_id], |row| {
            Ok(Given { discord_id: row.get(0)?, did: row.get(1)?, roles: list_from(row.get(2)?), overrides: row.get(3)?, by: row.get(4)? })
        })
    }

    fn set_discord_given(&self, given: &Given, now: i64) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO discord_given (discord_id, did, roles, overrides, by_did, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![given.discord_id, given.did, list_text(&given.roles), given.overrides, given.by, now],
        )?;
        Ok(())
    }

    /// The Discord account `did` is, here: one whose claim of it succeeded (or that
    /// someone who manages roles named a bot's).
    pub fn discord_account_of(&self, did: &str) -> Result<Option<String>> {
        Ok(self.claim_of(did)?.filter(|claim| claim.state == "claimed").map(|claim| claim.discord_id))
    }

    /// The roles the Discord account `discord_id` would give `did`: those kept of it
    /// that are here, that they don't hold, and that are no bot's own; with `below`,
    /// only those ranked below it.
    pub fn roles_to_give(&self, discord_id: &str, did: &str, below: Option<&Powers>) -> Result<Vec<String>> {
        let Some(account) = self.discord_account(discord_id)? else { return Ok(Vec::new()) };
        let roles = self.roles()?;
        let held = self.member_roles(did)?;
        let managed: HashSet<String> = self.bot_roles()?.into_iter().map(|(_, role)| role).collect();
        Ok(account
            .roles
            .into_iter()
            .filter(|id| id != EVERYONE_ROLE && !held.contains(id) && !managed.contains(id))
            .filter(|id| roles.iter().any(|role| role.id == *id && below.is_none_or(|powers| powers.above(role.position))))
            .collect())
    }

    /// Gives `did` what was kept of the Discord account `discord_id`, once: its roles
    /// ([`Store::roles_to_give`]) and its overrides (each made one with any they
    /// have in the same place). What was given; none where it gave already, or
    /// nothing was kept of it.
    pub fn give_discord_account(&self, discord_id: &str, did: &str, below: Option<&Powers>, by: Option<&str>, now: i64) -> anyhow::Result<Option<Given>> {
        if self.discord_given(discord_id)?.is_some() {
            return Ok(None);
        }
        let author = format!("{AUTHOR_PREFIX}{discord_id}");
        let kept = self.discord_account(discord_id)?.is_some() || !self.member_overrides_of(&author)?.is_empty();
        if !kept {
            return Ok(None);
        }
        let roles = self.roles_to_give(discord_id, did, below)?;
        for role in &roles {
            self.give_role(did, role)?;
        }
        let overrides = self.merge_member_overrides(&author, did)? as i64;
        let given = Given { discord_id: discord_id.to_string(), did: did.to_string(), roles, overrides, by: by.map(str::to_string) };
        self.set_discord_given(&given, now)?;
        Ok(Some(given))
    }

    /// What `did` is shown as here: their name in this community, else their name.
    fn shown_as(&self, did: &str) -> Result<Option<String>> {
        if let Some(nickname) = self.member_here(did)?.and_then(|here| here.nickname) {
            return Ok(Some(nickname));
        }
        Ok(self.member(did)?.and_then(|member| member.name))
    }

    /// Gives the approved bot `did` a role of its own if it has none; and where a
    /// Discord import made a role of its Discord bot's own (the account it is
    /// here), makes that role its own: taken over as it is, or, where it had one
    /// already, that one given the imported one's name, colour, rank, permissions
    /// and overrides and the imported one deleted. Whether anything changed.
    pub fn settle_bot_role(&self, did: &str) -> anyhow::Result<bool> {
        if self.member(did)?.is_none() || !self.bot_approved(did)? {
            return Ok(false);
        }
        let roles = self.roles()?;
        let others: HashSet<String> = self.bot_roles()?.into_iter().filter(|(bot, _)| bot != did).map(|(_, role)| role).collect();
        let own = self.bot_role_of(did)?.and_then(|id| roles.iter().find(|role| role.id == id).cloned());
        let imported = match self.discord_account_of(did)? {
            Some(discord_id) => self
                .discord_mapped("role")?
                .into_iter()
                .find(|m| m.bot.as_deref() == Some(discord_id.as_str()))
                .and_then(|m| m.local)
                .filter(|local| local != EVERYONE_ROLE && !others.contains(local) && own.as_ref().is_none_or(|own| own.id != *local))
                .and_then(|local| roles.iter().find(|role| role.id == local).cloned()),
            None => None,
        };
        match (own, imported) {
            (Some(own), None) => {
                self.give_role(did, &own.id)?;
                Ok(false)
            }
            (None, Some(mut imported)) => {
                // Held by the bot alone.
                self.db.lock().execute("DELETE FROM member_roles WHERE role = ?1 AND did <> ?2", params![imported.id, did])?;
                imported.self_assignable = false;
                self.save_role(&imported)?;
                self.give_role(did, &imported.id)?;
                self.set_bot_role(did, &imported.id)?;
                Ok(true)
            }
            (Some(mut own), Some(imported)) => {
                // Its rank: the imported one's place, its own given up.
                let order: Vec<String> = roles
                    .iter()
                    .rev()
                    .filter(|role| role.id != EVERYONE_ROLE && role.id != own.id)
                    .map(|role| if role.id == imported.id { own.id.clone() } else { role.id.clone() })
                    .collect();
                let overrides = self.overrides_of_role(&imported.id)?;
                self.delete_role(&imported.id)?;
                own.name = imported.name;
                own.color = imported.color;
                own.icon = imported.icon;
                own.permissions = imported.permissions;
                own.mentionable = imported.mentionable;
                own.hoist = imported.hoist;
                own.self_assignable = false;
                self.save_role(&own)?;
                self.rank_roles(&order)?;
                for (channel, _, _) in self.overrides_of_role(&own.id)? {
                    self.set_override(&channel, &own.id, &[], &[])?;
                }
                for (channel, allow, deny) in overrides {
                    self.set_override(&channel, &own.id, &allow, &deny)?;
                }
                self.give_role(did, &own.id)?;
                Ok(true)
            }
            (None, None) => {
                let shown = self.shown_as(did)?.filter(|name| !name.trim().is_empty()).unwrap_or_else(|| "Bot".into());
                let shown: String = shown.chars().take(90).collect();
                let taken = |name: &str| roles.iter().any(|role| role.name.to_lowercase() == name.to_lowercase());
                let mut name = shown.clone();
                let mut n = 1;
                while taken(&name) {
                    name = if n == 1 { format!("{shown} (bot)") } else { format!("{shown} (bot {n})") };
                    n += 1;
                }
                let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
                self.save_role(&RoleDef { id: id.clone(), name, color: None, icon: None, position: 1, permissions: Vec::new(), mentionable: false, hoist: false, self_assignable: false })?;
                // Just above everyone.
                let mut order: Vec<String> = roles.iter().rev().filter(|role| role.id != EVERYONE_ROLE).map(|role| role.id.clone()).collect();
                order.insert(0, id.clone());
                self.rank_roles(&order)?;
                self.give_role(did, &id)?;
                self.set_bot_role(did, &id)?;
                Ok(true)
            }
        }
    }
}

impl Community {
    /// Whether `role` is a bot's own (held by it alone: never given, taken or deleted).
    pub fn is_bot_role(&self, role: &str) -> anyhow::Result<bool> {
        Ok(self.store.bot_roles()?.iter().any(|(_, held)| held == role))
    }

    /// The bots' own roles, by role: the bot's DID.
    pub fn bot_role_holders(&self) -> anyhow::Result<HashMap<String, String>> {
        Ok(self.store.bot_roles()?.into_iter().map(|(did, role)| (role, did)).collect())
    }

    /// A server from before bots had roles of their own gives one to every
    /// approved bot already a member, once (Endnote 5).
    pub(crate) fn give_bots_roles_from_before(&self) -> anyhow::Result<usize> {
        if self.store.setting("bot_roles_given")?.is_some() {
            return Ok(0);
        }
        let mut given = 0;
        for did in self.store.approved_bots()? {
            if self.store.settle_bot_role(&did)? {
                given += 1;
            }
        }
        self.store.set_setting("bot_roles_given", "1")?;
        Ok(given)
    }

    /// A claim of the Discord account `discord_id` by `did` succeeding: what was
    /// kept of the account given them (by `decider`, only the roles ranked below
    /// theirs), the audit log told, and a bot's own role settled.
    pub(crate) fn give_claimed(&self, discord_id: &str, did: &str, decider: Option<&str>, now: i64) -> anyhow::Result<Option<Given>> {
        let below = match decider {
            Some(decider) => Some(self.powers(decider)?),
            None => None,
        };
        let given = self.store.give_discord_account(discord_id, did, below.as_ref(), decider, now)?;
        if let Some(given) = given.as_ref().filter(|given| !given.roles.is_empty()) {
            self.audit(now, decider.unwrap_or(did), "member.roles", Some(did), Some(json!({ "given": given.roles, "taken": [], "claim": discord_id })));
        }
        self.store.settle_bot_role(did)?;
        Ok(given)
    }

    /// `imported_claim`'s `given`: what a claim that succeeded gave.
    pub(crate) fn claim_given(&self, discord_id: &str) -> anyhow::Result<Value> {
        Ok(match self.store.discord_given(discord_id)? {
            Some(given) => json!({ "roles": given.roles, "overrides": given.overrides }),
            None => json!({ "roles": [], "overrides": 0 }),
        })
    }

    /// Someone who manages roles (`actor`) says the bot `did` is the Discord bot
    /// `discord_id` (its user id there), as a claim no bot can make (decision of
    /// 2026-11-25): the posts brought in under it become its, and what was kept of
    /// it is given it as a claim `actor` decided. Why not, else.
    pub(crate) fn name_discord_bot(&self, actor: &str, did: &str, discord_id: &str, now: i64) -> anyhow::Result<std::result::Result<String, &'static str>> {
        let discord_id = discord_id.trim();
        if discord_id.is_empty() || discord_id.len() > 32 || !discord_id.chars().all(|c| c.is_ascii_digit()) {
            return Ok(Err("A Discord bot's user id is a number."));
        }
        if self.store.bot_asked(did)?.is_none() {
            return Ok(Err("Only a member whose profile says it is a bot is one."));
        }
        if self.store.discord_account(discord_id)?.is_some_and(|account| !account.bot) {
            return Ok(Err("That Discord account isn't a bot's."));
        }
        if let Some(claim) = self.store.claim(discord_id)? {
            if claim.state == "claimed" && self.store.current_did(&claim.did)? != did {
                return Ok(Err("Another member is that Discord account already."));
            }
            if claim.state == "claimed" {
                return Ok(Ok("It was that Discord bot already.".into()));
            }
        }
        if self.store.discord_account_of(did)?.is_some_and(|held| held != discord_id) {
            return Ok(Err("It is another Discord account already."));
        }
        let given = self.give_claimed(discord_id, did, Some(actor), now)?;
        let posts = self.store.claim_imported(discord_id, did, now)?;
        self.store.set_claim(&crate::claims::Claim { discord_id: discord_id.to_string(), did: did.to_string(), state: "claimed".into(), at: now, decided_by: Some(actor.to_string()) })?;
        // (Its own role, now that it is known which Discord bot it is.)
        self.store.settle_bot_role(did)?;
        self.audit(now, actor, "claim.bot", Some(did), Some(json!({ "discord_id": discord_id, "posts": posts })));
        let roles = given.as_ref().map_or(0, |g| g.roles.len());
        Ok(Ok(format!("It is that Discord bot now: {posts} posts brought in are its, and {roles} roles given.")))
    }

    /// What a member override names someone as, in the channel-permissions
    /// workflow: a member by the name they are shown as; an imported Discord
    /// author by its name there, marked not yet claimed.
    pub(crate) fn override_name(&self, did: &str) -> anyhow::Result<String> {
        if let Some(discord_id) = did.strip_prefix(AUTHOR_PREFIX) {
            let name = match self.store.discord_account(discord_id)?.and_then(|account| account.name) {
                Some(name) => Some(name),
                None => self.store.db.lock().query_row("SELECT author_name FROM posts WHERE author_did = ?1 AND author_name IS NOT NULL LIMIT 1", params![did], |row| row.get::<String>(0))?,
            };
            return Ok(format!("{} (on Discord, not yet claimed)", name.unwrap_or_else(|| format!("Discord account {discord_id}"))));
        }
        Ok(self.store.shown_as(did)?.unwrap_or_else(|| "A member".into()))
    }
}

// ---------------------------------------------------------------------------
// Reading a Discord server's members, roles and channels.

/// What `import-discord-roles` found and did: counts, and the channels skipped.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub dry_run: bool,
    pub roles: usize,
    pub roles_by_record: usize,
    pub roles_by_template: usize,
    pub roles_by_name: usize,
    pub roles_unmatched: usize,
    pub bot_roles: usize,
    pub bot_roles_matched: usize,
    pub channels: usize,
    pub categories: usize,
    pub channels_matched: usize,
    pub channels_by_history: usize,
    pub channels_skipped: Vec<String>,
    pub accounts: usize,
    pub members_listed: usize,
    pub bots: usize,
    pub accounts_with_roles: usize,
    pub role_memberships: usize,
    pub role_memberships_unmatched: usize,
    pub overrides: usize,
    pub overrides_kept: usize,
    pub overrides_skipped: usize,
    pub overrides_empty: usize,
    pub claimed: usize,
    pub claimed_given_before: usize,
    pub claimed_given: usize,
    pub claimed_roles_given: usize,
    pub claimed_overrides_given: usize,
    pub bots_settled: usize,
}

fn row(out: &mut String, what: &str, n: impl std::fmt::Display) {
    out.push_str(&format!("| {what} | {n} |\n"));
}

impl Report {
    /// The report as Markdown: counts, and the channels skipped by name; nobody's
    /// name, nor any account's.
    pub fn markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("# Discord's roles and member overrides{}\n\n", if self.dry_run { " (dry run: nothing is changed)" } else { "" }));
        out.push_str("Counts only: no one's name is copied here.\n\n## Roles\n\n| | |\n|---|---|\n");
        row(&mut out, "Discord's roles (everyone left out)", self.roles);
        row(&mut out, "Matched as an earlier run matched them", self.roles_by_record);
        row(&mut out, "Matched to the role a template import made of them", self.roles_by_template);
        row(&mut out, "Matched by name", self.roles_by_name);
        row(&mut out, "Matching no role here (not given)", self.roles_unmatched);
        row(&mut out, "Of them, a bot's own (Discord's managed)", format!("{} ({} matched)", self.bot_roles, self.bot_roles_matched));
        out.push_str("\n## Channels\n\n| | |\n|---|---|\n");
        row(&mut out, "Discord's channels and categories (threads left out)", format!("{} ({} categories)", self.channels, self.categories));
        row(&mut out, "Matched", format!("{} ({} by the history brought in)", self.channels_matched, self.channels_by_history));
        row(&mut out, "Skipped: matching nothing here", self.channels_skipped.len());
        out.push_str("\n## Accounts\n\n| | |\n|---|---|\n");
        row(&mut out, "Discord accounts kept", self.accounts);
        row(&mut out, "In the member list", format!("{} ({} bots)", self.members_listed, self.bots));
        row(&mut out, "Holding a role matched here", self.accounts_with_roles);
        row(&mut out, "Roles held, matched / not matched", format!("{} / {}", self.role_memberships, self.role_memberships_unmatched));
        row(&mut out, "Member overrides (Discord's for one account)", self.overrides);
        row(&mut out, "Kept, on a channel matched here", self.overrides_kept);
        row(&mut out, "Kept as Discord had them, their channel skipped", self.overrides_skipped);
        row(&mut out, "Giving only what Wyvrn has nothing like yet", self.overrides_empty);
        out.push_str("\n## Claimed already\n\n| | |\n|---|---|\n");
        row(&mut out, "Accounts claimed by a member", self.claimed);
        row(&mut out, "Given their roles and overrides before (not again)", self.claimed_given_before);
        row(&mut out, if self.dry_run { "To be given them now" } else { "Given them now" }, self.claimed_given);
        row(&mut out, "Roles given / overrides that became theirs", format!("{} / {}", self.claimed_roles_given, self.claimed_overrides_given));
        if !self.dry_run {
            row(&mut out, "Bots given their own role, or the one Discord made them", self.bots_settled);
        }
        if !self.channels_skipped.is_empty() {
            out.push_str("\n## Channels skipped\n\nTheir member overrides are kept, and taken up by a run once a channel here matches.\n\n");
            for skipped in &self.channels_skipped {
                out.push_str(&format!("- {skipped}\n"));
            }
        }
        out
    }
}

/// A Discord permission overwrite's kind: 0 a role, 1 a member.
fn is_member_overwrite(o: &Value) -> bool {
    o["type"] == 1 || o["type"] == "1" || o["type"] == "member"
}

/// What a Discord permission set gives in a channel here.
fn channel_permissions_of(bits: &Value) -> BTreeSet<String> {
    crate::discord::mapped(crate::discord::bits(bits)).into_iter().filter(|p| is_channel_permission(p)).collect()
}

/// A Discord channel's kind, as a channel here is one.
fn kind_here(channel: &crate::store::Channel) -> &'static str {
    if channel.voice.is_some() {
        "voice"
    } else if channel.forum.is_some() {
        "forum"
    } else {
        "text"
    }
}

/// Discord's ids for the channels the history brought in went to: from each
/// channel's earliest post not in a thread, the Discord channel it was in.
fn channels_by_history(store: &Store) -> anyhow::Result<HashMap<String, String>> {
    let firsts: Vec<(String, String)> = store.db.lock().query(
        "SELECT p.channel, MIN(p.id) FROM posts p LEFT JOIN post_state s ON s.post = p.id WHERE p.id LIKE 'discord-%' AND s.thread IS NULL GROUP BY p.channel",
        params![],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut found = HashMap::new();
    for (channel, post) in firsts {
        let extra: Option<String> = store.db.lock().query_row("SELECT extra FROM post_extra WHERE post = ?1", params![post], |row| row.get(0))?;
        if let Some(from) = extra.and_then(|text| serde_json::from_str::<Value>(&text).ok()).and_then(|extra| extra["imported"]["channel"].as_str().map(str::to_string)) {
            found.entry(from).or_insert(channel);
        }
    }
    Ok(found)
}

/// Reads a Discord server's `members` (`GET /guilds/{id}/members`), `roles`
/// (`/roles`) and `channels` (`/channels`), Discord's API v10, into what is kept
/// (see the module's notes); with `dry_run`, only says what it would do. Run
/// again, what is kept of each account nobody has claimed is replaced; a member
/// who claimed an account is given what was kept of it, once, and never again.
pub fn import(store: &Store, members: &[Value], roles: &[Value], channels: &[Value], dry_run: bool, now: i64) -> anyhow::Result<Report> {
    let mut report = Report { dry_run, ..Default::default() };
    let guild = channels.iter().find_map(|c| c["guild_id"].as_str()).map(str::to_string);

    // Roles: as an earlier run matched them, else as a template import made them
    // (its record keeps the name it had on Discord), else by name.
    let here = store.roles()?;
    let exists = |id: &str| here.iter().any(|role| role.id == id && role.id != EVERYONE_ROLE);
    let before: HashMap<String, Mapped> = store.discord_mapped("role")?.into_iter().map(|m| (m.discord_id.clone(), m)).collect();
    let template = store.imported("role")?;
    let mut role_map: HashMap<String, Mapped> = HashMap::new();
    for role in roles {
        let Some(id) = role["id"].as_str().map(str::to_string).or_else(|| role["id"].as_u64().map(|n| n.to_string())) else { continue };
        let name = role["name"].as_str().unwrap_or_default().to_string();
        if name == "@everyone" || Some(&id) == guild.as_ref() {
            continue;
        }
        report.roles += 1;
        let bot = role["tags"]["bot_id"].as_str().map(str::to_string);
        if bot.is_some() {
            report.bot_roles += 1;
        }
        let lower = name.to_lowercase();
        let local = if let Some(local) = before.get(&id).and_then(|m| m.local.clone()).filter(|l| exists(l)) {
            report.roles_by_record += 1;
            Some(local)
        } else if let Some(local) = template.iter().find(|r| r.source["name"].as_str().is_some_and(|n| n.to_lowercase() == lower) && exists(&r.local)).map(|r| r.local.clone()) {
            report.roles_by_template += 1;
            Some(local)
        } else if let Some(local) = here.iter().find(|r| r.id != EVERYONE_ROLE && r.name.to_lowercase() == lower).map(|r| r.id.clone()) {
            report.roles_by_name += 1;
            Some(local)
        } else {
            report.roles_unmatched += 1;
            None
        };
        if bot.is_some() && local.is_some() {
            report.bot_roles_matched += 1;
        }
        role_map.insert(id.clone(), Mapped { discord_id: id, local, name, bot });
    }

    // Channels and categories: as an earlier run matched them, else where the
    // history brought in from them went, else as a template import made them,
    // else by name and kind.
    let channels_here: Vec<crate::store::Channel> = store.channels()?.into_iter().filter(|c| !c.closed).collect();
    let categories_here = store.categories()?;
    let before: HashMap<String, Mapped> = store.discord_mapped("channel")?.into_iter().map(|m| (m.discord_id.clone(), m)).collect();
    let history = channels_by_history(store)?;
    let template_channels = store.imported("channel")?;
    let template_categories = store.imported("category")?;
    let channel_exists = |key: &str| match key.strip_prefix("category:") {
        Some(category) => categories_here.iter().any(|c| c.id == category),
        None => channels_here.iter().any(|c| c.id == key),
    };
    let mut channel_map: HashMap<String, Mapped> = HashMap::new();
    for channel in channels {
        let Some(id) = channel["id"].as_str().map(str::to_string) else { continue };
        let kind_number = channel["type"].as_i64().unwrap_or(0);
        if matches!(kind_number, 10..=12) {
            continue;
        }
        let (kind, _) = crate::discord::kind_of(channel);
        let name = channel["name"].as_str().unwrap_or_default().to_string();
        report.channels += 1;
        if kind == "category" {
            report.categories += 1;
        }
        let lower = name.to_lowercase();
        let named = |here_name: &str| here_name.to_lowercase() == lower || here_name == crate::discord::text_name(&name);
        let mut by_history = false;
        let local = before.get(&id).and_then(|m| m.local.clone()).filter(|l| channel_exists(l)).or_else(|| {
            if kind == "category" {
                template_categories
                    .iter()
                    .find(|r| r.source["name"].as_str().is_some_and(|n| n.to_lowercase() == lower) && categories_here.iter().any(|c| c.id == r.local))
                    .map(|r| category_key(&r.local))
                    .or_else(|| {
                        let found: Vec<_> = categories_here.iter().filter(|c| c.name.to_lowercase() == lower).collect();
                        (found.len() == 1).then(|| category_key(&found[0].id))
                    })
            } else {
                if let Some(local) = history.get(&id).filter(|l| channel_exists(l)) {
                    by_history = true;
                    return Some(local.clone());
                }
                template_channels
                    .iter()
                    .find(|r| r.source["name"] == channel["name"] && crate::discord::kind_of(&r.source).0 == kind && channel_exists(&r.local))
                    .map(|r| r.local.clone())
                    .or_else(|| {
                        let found: Vec<_> = channels_here.iter().filter(|c| kind_here(c) == kind && named(&c.name)).collect();
                        (found.len() == 1).then(|| found[0].id.clone())
                    })
            }
        });
        match &local {
            Some(_) => {
                report.channels_matched += 1;
                if by_history {
                    report.channels_by_history += 1;
                }
            }
            None => report.channels_skipped.push(match kind {
                "category" => format!("The category {name}"),
                "voice" => format!("🔊{name}"),
                _ => format!("#{name}"),
            }),
        }
        channel_map.insert(id.clone(), Mapped { discord_id: id, local, name, bot: None });
    }

    // Each account: its roles, from the member list; its overrides, from every
    // channel's overwrites for one member (whether or not it is still a member).
    let mut accounts: BTreeMap<String, DiscordAccount> = BTreeMap::new();
    for member in members {
        let user = &member["user"];
        let Some(id) = user["id"].as_str().map(str::to_string) else { continue };
        report.members_listed += 1;
        let bot = user["bot"] == true;
        if bot {
            report.bots += 1;
        }
        let name = [&member["nick"], &user["global_name"], &user["username"]].into_iter().find_map(|n| n.as_str().filter(|n| !n.is_empty())).map(str::to_string);
        let mut held = Vec::new();
        for role in member["roles"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            match role_map.get(role).and_then(|m| m.local.clone()) {
                Some(local) if !held.contains(&local) => {
                    held.push(local);
                    report.role_memberships += 1;
                }
                Some(_) => {}
                None => report.role_memberships_unmatched += 1,
            }
        }
        if !held.is_empty() {
            report.accounts_with_roles += 1;
        }
        accounts.insert(id.clone(), DiscordAccount { discord_id: id, name, bot, roles: held });
    }
    let mut overrides: BTreeMap<String, Vec<(String, Option<String>, Vec<String>, Vec<String>)>> = BTreeMap::new();
    for channel in channels {
        let Some(mapped) = channel["id"].as_str().and_then(|id| channel_map.get(id)) else { continue };
        for o in channel["permission_overwrites"].as_array().into_iter().flatten().filter(|o| is_member_overwrite(o)) {
            let Some(user) = o["id"].as_str() else { continue };
            report.overrides += 1;
            let allow = channel_permissions_of(&o["allow"]);
            let deny: BTreeSet<String> = channel_permissions_of(&o["deny"]).into_iter().filter(|p| !allow.contains(p)).collect();
            if allow.is_empty() && deny.is_empty() {
                report.overrides_empty += 1;
                continue;
            }
            match &mapped.local {
                Some(_) => report.overrides_kept += 1,
                None => report.overrides_skipped += 1,
            }
            overrides.entry(user.to_string()).or_default().push((mapped.discord_id.clone(), mapped.local.clone(), allow.into_iter().collect(), deny.into_iter().collect()));
            accounts.entry(user.to_string()).or_insert_with(|| DiscordAccount { discord_id: user.to_string(), name: None, bot: false, roles: Vec::new() });
        }
    }
    report.accounts = accounts.len();

    if !dry_run {
        for mapped in role_map.values() {
            store.set_discord_mapped("role", mapped)?;
        }
        for mapped in channel_map.values() {
            store.set_discord_mapped("channel", mapped)?;
        }
    }
    for (id, account) in &accounts {
        let theirs = overrides.remove(id).unwrap_or_default();
        let author = format!("{AUTHOR_PREFIX}{id}");
        let claimed = match store.claim(id)?.filter(|claim| claim.state == "claimed") {
            Some(claim) => Some(store.current_did(&claim.did)?),
            None => None,
        };
        let given_before = store.discord_given(id)?.is_some();
        if claimed.is_some() {
            report.claimed += 1;
            if given_before {
                report.claimed_given_before += 1;
            }
        }
        if dry_run {
            if let (Some(did), false) = (&claimed, given_before) {
                report.claimed_given += 1;
                let held = store.member_roles(did)?;
                let managed: HashSet<String> = store.bot_roles()?.into_iter().map(|(_, role)| role).collect();
                report.claimed_roles_given += account.roles.iter().filter(|r| !held.contains(r) && !managed.contains(*r)).count();
                report.claimed_overrides_given += theirs.iter().filter(|(_, local, _, _)| local.is_some()).count();
            }
            continue;
        }
        store.set_discord_account(account, now)?;
        store.set_discord_account_overrides(id, &theirs)?;
        if given_before {
            continue;
        }
        // Nobody's yet (or theirs, to be given now): the overrides name the
        // imported author, as this run has them.
        store.db.lock().execute("DELETE FROM member_overrides WHERE did = ?1", params![author])?;
        for (_, local, allow, deny) in &theirs {
            if let Some(local) = local {
                store.set_member_override(local, &author, allow, deny)?;
            }
        }
        if let Some(did) = &claimed {
            if let Some(given) = store.give_discord_account(id, did, None, Some("import:discord"), now)? {
                report.claimed_given += 1;
                report.claimed_roles_given += given.roles.len();
                report.claimed_overrides_given += given.overrides as usize;
                if !given.roles.is_empty() {
                    store.audit(now, "import:discord", "member.roles", Some(did), Some(&json!({ "given": given.roles, "taken": [], "claim": id })))?;
                }
            }
        }
    }
    if !dry_run {
        for did in store.approved_bots()? {
            if store.settle_bot_role(&did)? {
                report.bots_settled += 1;
            }
        }
    }
    Ok(report)
}

/// The three files `import-discord-roles` reads from `folder`: `members.json`,
/// `roles.json`, `channels.json`, each a JSON array as Discord's API gives it.
pub fn read_folder(folder: &std::path::Path) -> anyhow::Result<(Vec<Value>, Vec<Value>, Vec<Value>)> {
    use anyhow::Context as _;
    let read = |name: &str| -> anyhow::Result<Vec<Value>> {
        let path = folder.join(name);
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let value: Value = serde_json::from_str(&text).with_context(|| format!("{} isn't JSON", path.display()))?;
        match value {
            Value::Array(items) => Ok(items),
            _ => anyhow::bail!("{} isn't a list, as Discord's API gives it", path.display()),
        }
    };
    Ok((read("members.json")?, read("roles.json")?, read("channels.json")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Role;

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn role(id: &str, name: &str, position: i64) -> RoleDef {
        RoleDef { id: id.into(), name: name.into(), color: None, icon: None, position, permissions: Vec::new(), mentionable: false, hoist: false, self_assignable: false }
    }

    #[test]
    fn two_overrides_made_one_deny_what_either_denies() {
        let (allow, deny) = merged((&strings(&["post", "react"]), &[]), (&strings(&["attach_files"]), &strings(&["react"])));
        assert_eq!(allow, strings(&["attach_files", "post"]));
        assert_eq!(deny, strings(&["react"]));
    }

    #[test]
    fn a_members_overrides_follow_them_and_go_with_them() {
        let store = Store::in_memory().unwrap();
        store.admit("did:example:ann", Role::Member, 1).unwrap();
        store.set_member_override("c1", "did:example:ann", &strings(&["post"]), &[]).unwrap();
        store.set_member_override("c1", "did:example:new", &[], &strings(&["react"])).unwrap();
        store.rotate("did:example:ann", "did:example:new", 2).unwrap();
        let moved = store.member_overrides("c1").unwrap();
        assert_eq!(moved, vec![MemberOverride { channel: "c1".into(), did: "did:example:new".into(), allow: strings(&["post"]), deny: strings(&["react"]) }]);
        store.remove_member("did:example:new").unwrap();
        assert!(store.member_overrides("c1").unwrap().is_empty());
    }

    #[test]
    fn a_bots_own_role_is_made_once_just_above_everyone_and_goes_with_the_bot() {
        let store = Store::in_memory().unwrap();
        store.save_role(&role(EVERYONE_ROLE, "everyone", 0)).unwrap();
        store.save_role(&role("mod", "Moderator", 1)).unwrap();
        store.set_member("did:example:bot", Role::Member, 1).unwrap();
        store.set_name("did:example:bot", Some("Dicebot")).unwrap();
        // Not approved: nothing.
        assert!(!store.settle_bot_role("did:example:bot").unwrap());
        store.set_bot("did:example:bot", Some(&json!([])), 1).unwrap();
        store.set_bot_approved("did:example:bot", Some("did:example:mod"), 1).unwrap();
        assert!(store.settle_bot_role("did:example:bot").unwrap());
        assert!(!store.settle_bot_role("did:example:bot").unwrap(), "once");
        let own = store.bot_role_of("did:example:bot").unwrap().unwrap();
        let roles = store.roles().unwrap();
        let made = roles.iter().find(|r| r.id == own).unwrap();
        assert_eq!(made.position, 1);
        assert!(made.permissions.is_empty());
        assert_eq!(roles.iter().find(|r| r.id == "mod").unwrap().position, 2);
        assert_eq!(store.member_roles("did:example:bot").unwrap(), vec![own.clone()]);
        store.set_override("c1", &own, &strings(&["post"]), &[]).unwrap();
        store.remove_member("did:example:bot").unwrap();
        assert!(store.role(&own).unwrap().is_none());
        assert!(store.overrides("c1").unwrap().is_empty());
        assert!(store.bot_roles().unwrap().is_empty());
    }

    #[test]
    fn a_discord_bots_managed_role_becomes_its_own_and_one_given_before_takes_its_place() {
        let store = Store::in_memory().unwrap();
        store.save_role(&role(EVERYONE_ROLE, "everyone", 0)).unwrap();
        store.save_role(&role("low", "Low", 1)).unwrap();
        store.save_role(&RoleDef { permissions: strings(&["manage_messages"]), color: Some("#123456".into()), ..role("dice", "Dice", 2) }).unwrap();
        store.save_role(&role("high", "High", 3)).unwrap();
        store.set_override("c1", "dice", &strings(&["post"]), &[]).unwrap();
        store.set_member("did:example:bot", Role::Member, 1).unwrap();
        store.set_member("did:example:ann", Role::Member, 1).unwrap();
        store.give_role("did:example:ann", "dice").unwrap();
        store.set_bot("did:example:bot", Some(&json!([])), 1).unwrap();
        store.set_bot_approved("did:example:bot", Some("did:example:mod"), 1).unwrap();
        // Approved before it is known which Discord bot it is: a role of its own.
        store.settle_bot_role("did:example:bot").unwrap();
        let own = store.bot_role_of("did:example:bot").unwrap().unwrap();
        // Then it is the Discord bot whose managed role the template made "Dice".
        store.set_discord_mapped("role", &Mapped { discord_id: "500".into(), local: Some("dice".into()), name: "Dice".into(), bot: Some("900".into()) }).unwrap();
        store.set_claim(&crate::claims::Claim { discord_id: "900".into(), did: "did:example:bot".into(), state: "claimed".into(), at: 2, decided_by: Some("did:example:mod".into()) }).unwrap();
        assert!(store.settle_bot_role("did:example:bot").unwrap());
        assert!(store.role("dice").unwrap().is_none(), "the imported one is deleted");
        let taken = store.role(&own).unwrap().unwrap();
        assert_eq!((taken.name.as_str(), taken.color.as_deref(), taken.permissions.clone()), ("Dice", Some("#123456"), strings(&["manage_messages"])));
        let order: Vec<String> = store.roles().unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(order, vec!["high".to_string(), own.clone(), "low".into(), EVERYONE_ROLE.into()], "its rank");
        assert_eq!(store.overrides("c1").unwrap()[0].role, own);
        assert!(store.member_roles("did:example:ann").unwrap().is_empty(), "nobody else holds it");
    }

    fn sample() -> (Vec<Value>, Vec<Value>, Vec<Value>) {
        // Synthetic, in Discord API v10's shapes.
        let roles = vec![
            json!({ "id": "1", "name": "@everyone", "position": 0, "permissions": "1024", "managed": false }),
            json!({ "id": "11", "name": "Artists", "position": 1, "permissions": "0", "managed": false }),
            json!({ "id": "12", "name": "Elders", "position": 2, "permissions": "0", "managed": false }),
            json!({ "id": "13", "name": "Nowhere Here", "position": 3, "permissions": "0", "managed": false }),
            json!({ "id": "14", "name": "Dicebot", "position": 4, "permissions": "0", "managed": true, "tags": { "bot_id": "900" } }),
        ];
        let channels = vec![
            json!({ "id": "20", "guild_id": "1", "type": 4, "name": "Hangout", "position": 0, "permission_overwrites": [] }),
            json!({ "id": "21", "guild_id": "1", "type": 0, "name": "fanart", "parent_id": "20", "position": 0, "permission_overwrites": [
                { "id": "100", "type": 1, "allow": "0", "deny": "2048" },
                { "id": "11", "type": 0, "allow": "2048", "deny": "0" },
                { "id": "300", "type": 1, "allow": "1024", "deny": "0" }
            ] }),
            json!({ "id": "22", "guild_id": "1", "type": 0, "name": "gone-elsewhere", "position": 1, "permission_overwrites": [{ "id": "100", "type": 1, "allow": "2048", "deny": "0" }] }),
            json!({ "id": "23", "guild_id": "1", "type": 0, "name": "fanart", "position": 2, "permission_overwrites": [{ "id": "100", "type": 1, "allow": "256", "deny": "0" }] }),
        ];
        let members = vec![
            json!({ "user": { "id": "100", "username": "ann", "global_name": "Ann" }, "nick": null, "roles": ["11", "12", "13"] }),
            json!({ "user": { "id": "200", "username": "bob" }, "roles": ["11"] }),
            json!({ "user": { "id": "900", "username": "dicebot", "bot": true }, "roles": ["14"] }),
        ];
        (members, roles, channels)
    }

    #[test]
    fn an_import_keeps_each_accounts_roles_and_overrides_and_again_replaces_them() {
        let store = Store::in_memory().unwrap();
        store.save_role(&role(EVERYONE_ROLE, "everyone", 0)).unwrap();
        store.save_role(&role("art", "artists", 1)).unwrap();
        store.save_role(&role("eld", "Elders", 2)).unwrap();
        store.save_role(&role("dice", "Dicebot", 3)).unwrap();
        store.add_channel("fan", "fanart", "", "did:example:owner", 1).unwrap();
        let (members, roles, channels) = sample();
        let dry = import(&store, &members, &roles, &channels, true, 5).unwrap();
        assert_eq!((dry.roles, dry.roles_by_name, dry.roles_unmatched, dry.bot_roles_matched), (4, 3, 1, 1));
        assert!(store.discord_account("100").unwrap().is_none(), "a dry run changes nothing");
        let report = import(&store, &members, &roles, &channels, false, 5).unwrap();
        assert_eq!(report.channels, 4);
        // Two channels called fanart on Discord and one here: both are it.
        assert_eq!(report.channels_skipped, vec!["The category Hangout".to_string(), "#gone-elsewhere".to_string()]);
        assert_eq!((report.accounts, report.members_listed, report.bots), (4, 3, 1));
        let ann = store.discord_account("100").unwrap().unwrap();
        assert_eq!(ann.roles, strings(&["art", "eld"]));
        assert_eq!(ann.name.as_deref(), Some("Ann"));
        let author = format!("{AUTHOR_PREFIX}100");
        let held: Vec<MemberOverride> = store.member_overrides_of(&author).unwrap();
        assert_eq!(held.len(), 1, "{held:?}");
        assert_eq!(held[0].channel, "fan");
        // Kept as Discord had it, its channel skipped.
        assert!(store.discord_account_overrides("100").unwrap().iter().any(|(c, local, _, _)| c == "22" && local.is_none()));
        // Again, with Ann no longer denied: replaced.
        let mut channels = channels;
        channels[1]["permission_overwrites"] = json!([]);
        channels[3]["permission_overwrites"] = json!([]);
        import(&store, &members, &roles, &channels, false, 6).unwrap();
        assert!(store.member_overrides_of(&author).unwrap().is_empty());
    }

    #[test]
    fn a_claim_gives_the_accounts_roles_below_the_decider_and_its_overrides_once() {
        let store = Store::in_memory().unwrap();
        store.save_role(&role(EVERYONE_ROLE, "everyone", 0)).unwrap();
        store.save_role(&role("art", "Artists", 1)).unwrap();
        store.save_role(&role("eld", "Elders", 3)).unwrap();
        store.save_role(&role("mod", "Moderator", 2)).unwrap();
        store.set_member("did:example:mod", Role::Member, 1).unwrap();
        store.give_role("did:example:mod", "mod").unwrap();
        store.set_member("did:example:ann", Role::Member, 1).unwrap();
        store.set_discord_account(&DiscordAccount { discord_id: "100".into(), name: None, bot: false, roles: strings(&["art", "eld"]) }, 1).unwrap();
        let author = format!("{AUTHOR_PREFIX}100");
        store.set_member_override("fan", &author, &[], &strings(&["post"])).unwrap();
        store.set_member_override("fan", "did:example:ann", &strings(&["post", "react"]), &[]).unwrap();
        let moderator = Powers::of(&store.roles().unwrap(), &store.member_roles("did:example:mod").unwrap(), false);
        let given = store.give_discord_account("100", "did:example:ann", Some(&moderator), Some("did:example:mod"), 2).unwrap().unwrap();
        assert_eq!(given.roles, strings(&["art"]), "Elders is ranked above the moderator");
        assert_eq!(given.overrides, 1);
        assert_eq!(store.member_overrides("fan").unwrap(), vec![MemberOverride { channel: "fan".into(), did: "did:example:ann".into(), allow: strings(&["react"]), deny: strings(&["post"]) }]);
        assert!(store.give_discord_account("100", "did:example:ann", None, None, 3).unwrap().is_none(), "never again");
        assert_eq!(store.member_roles("did:example:ann").unwrap(), strings(&["art"]));
    }
}
