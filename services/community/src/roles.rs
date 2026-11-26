//! Roles and permissions (community/1.0's "Roles and permissions"): what a member
//! may do is every permission of every role they hold, `everyone` included; the
//! owner and whoever holds `administrator` may do everything. Roles are ranked,
//! and rank limits what one member may do to another and to roles. A channel
//! allows and denies the permissions a channel gives, role by role.
//!
//! Before roles there were admins and members: a store from then is given the
//! roles `everyone`, `moderator` and `admin` once, its admins `admin`, and the
//! first of them the community (Endnote 3).

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::community::Community;
use crate::store::{Channel, RoleDef};

/// The role every member holds without being given it.
pub const EVERYONE_ROLE: &str = "everyone";
pub const ADMIN_ROLE: &str = "admin";
pub const MODERATOR_ROLE: &str = "moderator";

/// The setting that keeps, as JSON, the DID each owner named by a public address
/// was seen writing from (a setting's name is too short for a DID).
pub const OWNER_HOLDERS_SETTING: &str = "owner_holders";

/// The permissions that are the community's alone, each with what an app shows it as.
pub const COMMUNITY_PERMISSIONS: &[(&str, &str)] = &[
    ("administrator", "Administrator: everything, everywhere"),
    ("manage_community", "Manage the community: name, rules, admission, voice, onboarding"),
    ("manage_roles", "Manage roles"),
    ("manage_channels", "Manage channels"),
    ("create_channels", "Open text channels"),
    ("create_invites", "Make invites"),
    ("admit_members", "Let people in from the waiting list"),
    ("kick_members", "Take members out"),
    ("ban_members", "Ban members"),
    ("timeout_members", "Time members out"),
    ("manage_reports", "Deal with reports"),
    ("view_audit_log", "Read the audit log"),
    ("view_insights", "Read insights"),
    ("manage_automod", "Set AutoMod's rules"),
    ("manage_expressions", "Manage the community's emoji and stickers"),
    ("create_events", "Make events, and change their own"),
    ("manage_events", "Change and cancel anyone's events"),
];

/// The permissions a channel can also give or withhold.
pub const CHANNEL_PERMISSIONS: &[(&str, &str)] = &[
    ("view_channel", "See the channel: without it, it isn't there"),
    ("post", "Post"),
    ("react", "React"),
    ("create_threads", "Start threads"),
    ("create_private_threads", "Start private threads"),
    ("send_in_threads", "Post in threads"),
    ("manage_threads", "Manage threads: lock, rename, and see and manage private ones"),
    ("create_polls", "Make polls"),
    ("attach_files", "Post files"),
    ("mention_everyone", "Mention everyone, and any role"),
    ("manage_messages", "Remove and pin anyone's posts"),
    ("bypass_slow_mode", "Ignore slow mode"),
    ("connect", "Join voice channels"),
    ("speak", "Speak in voice channels"),
    ("video", "Share a camera, screen or avatar"),
    ("moderate_voice", "Mute and remove people in voice channels"),
    ("manage_webhooks", "Manage webhooks: what posts here from other services"),
];

/// Holding any of these makes someone a `moderator` in older terms.
pub const MODERATING: &[&str] = &["manage_messages", "admit_members", "kick_members", "ban_members", "timeout_members", "manage_reports"];

const EVERYONE_DEFAULT: &[&str] =
    &["view_channel", "post", "react", "create_threads", "create_private_threads", "send_in_threads", "create_polls", "attach_files", "connect", "speak", "video"];
const MODERATOR_DEFAULT: &[&str] =
    &["admit_members", "kick_members", "timeout_members", "manage_reports", "view_audit_log", "manage_messages", "manage_threads", "bypass_slow_mode", "moderate_voice", "create_events", "manage_events"];

pub fn is_permission(name: &str) -> bool {
    COMMUNITY_PERMISSIONS.iter().chain(CHANNEL_PERMISSIONS).any(|(known, _)| *known == name)
}

/// Whether a role with `permissions` may be one members take themselves: it
/// grants neither `administrator` nor anything that moderates.
pub fn may_be_self_assignable(permissions: &[String]) -> bool {
    !permissions.iter().any(|p| p == "administrator" || MODERATING.contains(&p.as_str()))
}

/// Whether a member may take `role` themselves now: marked so, and granting
/// nothing that moderates (one marked before that was refused, or since given
/// such a permission, isn't).
pub fn takeable(role: &RoleDef) -> bool {
    role.self_assignable && may_be_self_assignable(&role.permissions)
}

pub fn is_channel_permission(name: &str) -> bool {
    CHANNEL_PERMISSIONS.iter().any(|(known, _)| *known == name)
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// What one member may do in the community, from the roles they hold.
#[derive(Debug, Clone)]
pub struct Powers {
    pub owner: bool,
    /// The roles they hold, `everyone` left out.
    pub roles: Vec<String>,
    /// Every permission of every role they hold.
    pub permissions: BTreeSet<String>,
    /// Their highest role's position: 0 for `everyone` alone.
    pub rank: i64,
}

impl Powers {
    /// What holding `held` of `roles` (every role there is) gives, and being the
    /// owner: worked out from what was read already, for a list of many members
    /// ([`Community::powers`] reads it for one).
    pub fn of(roles: &[RoleDef], held: &[String], owner: bool) -> Self {
        let mut permissions = BTreeSet::new();
        let mut rank = 0;
        for role in roles.iter().filter(|role| role.id == EVERYONE_ROLE || held.contains(&role.id)) {
            permissions.extend(role.permissions.iter().cloned());
            if role.id != EVERYONE_ROLE {
                rank = rank.max(role.position);
            }
        }
        let roles = held.iter().filter(|id| roles.iter().any(|role| role.id == **id)).cloned().collect();
        Self { owner, roles, permissions, rank }
    }

    /// Whether they may do everything: the owner, or `administrator`.
    pub fn all(&self) -> bool {
        self.owner || self.permissions.contains("administrator")
    }

    /// Whether they hold `permission` in the community.
    pub fn has(&self, permission: &str) -> bool {
        self.all() || self.permissions.contains(permission)
    }

    /// Whether they rank above `rank` (a role's position, or another member's rank).
    pub fn above(&self, rank: i64) -> bool {
        self.owner || self.rank > rank
    }

    /// What they are in older terms: `admin`, `moderator` or `member`.
    pub fn older_role(&self) -> &'static str {
        if self.all() {
            "admin"
        } else if MODERATING.iter().any(|permission| self.permissions.contains(*permission)) {
            "moderator"
        } else {
            "member"
        }
    }

    /// Every permission they hold, as `community` says it.
    pub fn listed(&self) -> Vec<&'static str> {
        COMMUNITY_PERMISSIONS.iter().chain(CHANNEL_PERMISSIONS).map(|(name, _)| *name).filter(|name| self.has(name)).collect()
    }
}

/// What a category's permissions are kept under, beside the channels': its own
/// channel_overrides rows, never read for a channel (community/1.0's "Category
/// permissions").
pub fn category_key(category: &str) -> String {
    format!("category:{category}")
}

/// Overrides for one member as a set to compare, as [`as_set`].
fn members_set(overrides: &[crate::discord_roles::MemberOverride]) -> std::collections::BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> {
    overrides.iter().filter(|o| !o.allow.is_empty() || !o.deny.is_empty()).map(|o| (o.did.clone(), (o.allow.iter().cloned().collect(), o.deny.iter().cloned().collect()))).collect()
}

/// Overrides as a set to compare: role -> (allowed, denied), sorted, none empty.
fn as_set(overrides: &[crate::store::Override]) -> std::collections::BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> {
    overrides.iter().filter(|o| !o.allow.is_empty() || !o.deny.is_empty()).map(|o| (o.role.clone(), (o.allow.iter().cloned().collect(), o.deny.iter().cloned().collect()))).collect()
}

impl Community {
    /// Whether `channel` has its category's permissions, exactly (Discord's
    /// "synced"), its members' overrides with its roles'; a channel in no category isn't.
    pub fn synced(&self, channel: &Channel) -> anyhow::Result<bool> {
        let Some(category) = &channel.category else { return Ok(false) };
        let key = category_key(category);
        Ok(as_set(&self.store.overrides(&key)?) == as_set(&self.store.overrides(&channel.id)?) && members_set(&self.store.member_overrides(&key)?) == members_set(&self.store.member_overrides(&channel.id)?))
    }

    /// Gives `channel` its category's permissions, all of them and only them.
    pub fn sync_with_category(&self, channel: &Channel) -> anyhow::Result<()> {
        let Some(category) = &channel.category else { return Ok(()) };
        let wanted = self.store.overrides(&category_key(category))?;
        for held in self.store.overrides(&channel.id)? {
            if !wanted.iter().any(|o| o.role == held.role) {
                self.store.set_override(&channel.id, &held.role, &[], &[])?;
            }
        }
        for o in wanted {
            self.store.set_override(&channel.id, &o.role, &o.allow, &o.deny)?;
        }
        let wanted = self.store.member_overrides(&category_key(category))?;
        for held in self.store.member_overrides(&channel.id)? {
            if !wanted.iter().any(|o| o.did == held.did) {
                self.store.set_member_override(&channel.id, &held.did, &[], &[])?;
            }
        }
        for o in wanted {
            self.store.set_member_override(&channel.id, &o.did, &o.allow, &o.deny)?;
        }
        Ok(())
    }

    /// As [`Self::set_category_override`], for one member.
    pub fn set_category_member_override(&self, category: &str, did: &str, allow: &[String], deny: &[String]) -> anyhow::Result<()> {
        let synced: Vec<Channel> = self.store.channels()?.into_iter().filter(|c| c.category.as_deref() == Some(category)).filter(|c| self.synced(c).unwrap_or(false)).collect();
        self.store.set_member_override(&category_key(category), did, allow, deny)?;
        for channel in synced {
            self.sync_with_category(&channel)?;
        }
        Ok(())
    }

    /// Sets what `role` is allowed and denied in `category`, and in every channel
    /// of it that was synced with it: they follow; one changed on its own doesn't.
    pub fn set_category_override(&self, category: &str, role: &str, allow: &[String], deny: &[String]) -> anyhow::Result<()> {
        let synced: Vec<Channel> = self.store.channels()?.into_iter().filter(|c| c.category.as_deref() == Some(category)).filter(|c| self.synced(c).unwrap_or(false)).collect();
        self.store.set_override(&category_key(category), role, allow, deny)?;
        for channel in synced {
            self.sync_with_category(&channel)?;
        }
        Ok(())
    }
}

impl Community {
    /// Gives a store from before roles its roles, once: `everyone` with what
    /// members could do, then Moderator and Admin, and `admin` to every admin.
    pub fn ensure_roles(&self) -> anyhow::Result<()> {
        // A community from before invites let people in by default: it goes on
        // letting an admin decide, unless an admin or its configuration said
        // otherwise. (A new one starts at `invite`, the default since 2026-11-17.)
        let existing = self.store.role(EVERYONE_ROLE)?.is_some() || self.store.member_count()? > 0;
        if existing && self.store.setting("admission")?.is_none() && self.config.admission == crate::config::DEFAULT_ADMISSION {
            self.store.set_setting("admission", "admin")?;
        }
        if self.store.role(EVERYONE_ROLE)?.is_some() {
            // A store from before `view_channel`: everyone saw every channel, and
            // still does.
            if self.store.setting("view_channel_given")?.is_none() {
                self.set_everyone("view_channel", true)?;
                self.store.set_setting("view_channel_given", "true")?;
            }
            // A store from before events: Moderator makes and manages them, as a
            // new community's does.
            if self.store.setting("events_given")?.is_none() {
                if let Some(mut moderator) = self.store.role("moderator")? {
                    for permission in ["create_events", "manage_events"] {
                        if !moderator.permissions.iter().any(|p| p == permission) {
                            moderator.permissions.push(permission.into());
                        }
                    }
                    self.store.save_role(&moderator)?;
                }
                self.store.set_setting("events_given", "true")?;
            }
            // A store from before thread permissions and polls' own: everyone starts
            // private threads, posts in threads and makes polls, as they could; where a
            // role may not post, it may not post in threads either; Moderator manages
            // threads.
            if self.store.setting("threads_given")?.is_none() {
                for permission in ["create_private_threads", "send_in_threads", "create_polls"] {
                    self.set_everyone(permission, true)?;
                }
                let keys: Vec<String> = self.store.channels()?.into_iter().map(|c| c.id).chain(self.store.categories()?.iter().map(|c| category_key(&c.id))).collect();
                for key in keys {
                    for mut o in self.store.overrides(&key)? {
                        if o.deny.iter().any(|p| p == "post") && !o.deny.iter().any(|p| p == "send_in_threads") {
                            o.deny.push("send_in_threads".into());
                            self.store.set_override(&key, &o.role, &o.allow, &o.deny)?;
                        }
                    }
                }
                if let Some(mut moderator) = self.store.role("moderator")? {
                    if !moderator.permissions.iter().any(|p| p == "manage_threads") {
                        moderator.permissions.push("manage_threads".into());
                    }
                    self.store.save_role(&moderator)?;
                }
                self.store.set_setting("threads_given", "true")?;
            }
            return Ok(());
        }
        self.store.set_setting("view_channel_given", "true")?;
        self.store.set_setting("events_given", "true")?;
        self.store.set_setting("threads_given", "true")?;
        let mut everyone = strings(EVERYONE_DEFAULT);
        if self.config.members_may_create_channels {
            everyone.push("create_channels".into());
        }
        let may_invite = match self.store.setting("members_may_invite")?.as_deref() {
            Some(set) => set == "true",
            None => self.config.members_may_invite,
        };
        if may_invite {
            everyone.push("create_invites".into());
        }
        let role = |id: &str, name: &str, position: i64, permissions: Vec<String>| RoleDef {
            id: id.into(),
            name: name.into(),
            color: None,
            icon: None,
            position,
            permissions,
            mentionable: false,
            hoist: false,
            self_assignable: false,
        };
        self.store.save_role(&role(EVERYONE_ROLE, "everyone", 0, everyone))?;
        self.store.save_role(&RoleDef { color: Some("#3498db".into()), hoist: true, ..role(MODERATOR_ROLE, "Moderator", 1, strings(MODERATOR_DEFAULT)) })?;
        self.store.save_role(&RoleDef { color: Some("#e67e22".into()), hoist: true, ..role(ADMIN_ROLE, "Admin", 2, strings(&["administrator"])) })?;
        for member in self.store.members()?.iter().filter(|member| member.role == crate::store::Role::Admin) {
            self.store.give_role(&member.did, ADMIN_ROLE)?;
        }
        Ok(())
    }

    /// The owners' DIDs, all equal: those the configuration names (`owners`),
    /// each where it is now ([`Self::configured_owners`]), else the one owner
    /// there is ([`Self::owner`]).
    pub fn owners(&self) -> anyhow::Result<Vec<String>> {
        if !self.config.owners.is_empty() {
            return self.configured_owners();
        }
        Ok(self.owner()?.into_iter().collect())
    }

    /// Whether `did` is one of the owners.
    pub fn is_owner(&self, did: &str) -> anyhow::Result<bool> {
        if !self.config.owners.is_empty() {
            return Ok(self.configured_owners()?.iter().any(|owner| owner == did));
        }
        Ok(self.owner()?.as_deref() == Some(did))
    }

    /// The owners the configuration names, at the DIDs they write from now. One
    /// named by a public address is at the DID its holder was seen writing from
    /// ([`OWNER_HOLDERS_SETTING`]); and a DID its member has since moved from
    /// is the one they moved to, however many moves on, as their roles follow
    /// them. A public address nobody has written from yet stays as it is named.
    pub fn configured_owners(&self) -> anyhow::Result<Vec<String>> {
        let holders = self.owner_holders()?;
        let mut owners = Vec::new();
        for named in &self.config.owners {
            let written_from = holders.get(named).map(String::as_str).unwrap_or(named);
            let now = self.store.current_did(written_from)?;
            if !owners.contains(&now) {
                owners.push(now);
            }
        }
        Ok(owners)
    }

    /// For each owner named by a public address, the DID its holder was seen
    /// writing from.
    pub fn owner_holders(&self) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
        Ok(match self.store.setting(OWNER_HOLDERS_SETTING)? {
            Some(kept) => serde_json::from_str(&kept).unwrap_or_default(),
            None => Default::default(),
        })
    }

    /// `holder` wrote as the owner named by the public address `named`.
    pub fn note_owner_holder(&self, named: &str, holder: &str) -> anyhow::Result<()> {
        let mut holders = self.owner_holders()?;
        if holders.get(named).map(String::as_str) == Some(holder) {
            return Ok(());
        }
        holders.insert(named.to_string(), holder.to_string());
        Ok(self.store.set_setting(OWNER_HOLDERS_SETTING, &serde_json::to_string(&holders)?)?)
    }

    /// The owners named by a public address that `sender` isn't already the
    /// holder of: those a message from them may show they hold.
    pub fn owners_named_by_address_besides(&self, sender: &str) -> anyhow::Result<Vec<String>> {
        let holders = self.owner_holders()?;
        let mut besides = Vec::new();
        for named in self.config.owners.iter().filter(|did| wyvrn_service::named::is_hosted(did)) {
            let current = match holders.get(named) {
                Some(holder) => Some(self.store.current_did(holder)?),
                None => None,
            };
            if current.as_deref() != Some(sender) {
                besides.push(named.clone());
            }
        }
        Ok(besides)
    }

    /// The owner's DID: the first the configuration names (where it is now),
    /// else as handed on, else the first member let in who holds
    /// `administrator`, from then on.
    pub fn owner(&self) -> anyhow::Result<Option<String>> {
        if let Some(first) = self.configured_owners()?.into_iter().next() {
            return Ok(Some(first));
        }
        if let Some(owner) = self.store.setting("owner")? {
            if self.store.member(&owner)?.is_some() {
                return Ok(Some(owner));
            }
        }
        let roles = self.store.roles()?;
        let administrator = |id: &str| roles.iter().any(|role| role.id == id && role.permissions.iter().any(|p| p == "administrator"));
        let held = self.store.all_member_roles()?;
        let mut members = self.store.members()?;
        members.sort_by_key(|member| member.since);
        let first = members.into_iter().find(|member| held.iter().any(|(did, role)| *did == member.did && administrator(role)));
        if let Some(first) = &first {
            self.store.set_setting("owner", &first.did)?;
        }
        Ok(first.map(|member| member.did))
    }

    /// What `did` may do in the community.
    pub fn powers(&self, did: &str) -> anyhow::Result<Powers> {
        Ok(Powers::of(&self.store.roles()?, &self.store.member_roles(did)?, self.is_owner(did)?))
    }

    /// Whether `actor` may act on the member `target`: ranked above them, and not
    /// against the owner.
    pub fn outranks(&self, actor: &Powers, target: &str) -> anyhow::Result<bool> {
        let target = self.powers(target)?;
        Ok(!target.owner && (actor.owner || actor.above(target.rank)))
    }

    /// What someone with `powers` (`did`) may do in `channel`, of the permissions
    /// a channel gives, worked out as "A channel's permissions" says.
    pub fn channel_permissions(&self, did: &str, powers: &Powers, channel: &Channel) -> anyhow::Result<BTreeSet<String>> {
        let mut every: BTreeSet<String> = CHANNEL_PERMISSIONS.iter().map(|(name, _)| name.to_string()).collect();
        // A channel of a kind its host turned off is read-only, to everyone.
        if self.channel_off(channel) {
            every.retain(|permission| !crate::community::ADDING.contains(&permission.as_str()));
        }
        if powers.all() {
            return Ok(every);
        }
        let mut have: BTreeSet<String> = powers.permissions.intersection(&every).cloned().collect();
        let overrides = self.store.overrides(&channel.id)?;
        if let Some(everyone) = overrides.iter().find(|o| o.role == EVERYONE_ROLE) {
            for denied in &everyone.deny {
                have.remove(denied);
            }
            have.extend(everyone.allow.iter().filter(|p| every.contains(*p)).cloned());
        }
        let theirs: Vec<_> = overrides.iter().filter(|o| powers.roles.contains(&o.role)).collect();
        for denied in theirs.iter().flat_map(|o| &o.deny) {
            have.remove(denied);
        }
        have.extend(theirs.iter().flat_map(|o| &o.allow).filter(|p| every.contains(*p)).cloned());
        // Last, what it says of them alone (discord_roles.rs).
        if let Some(own) = self.store.member_overrides(&channel.id)?.into_iter().find(|o| o.did == did) {
            for denied in &own.deny {
                have.remove(denied);
            }
            have.extend(own.allow.iter().filter(|p| every.contains(*p)).cloned());
        }
        // Whoever opened a channel keeps order in it.
        if channel.created_by == did {
            have.insert("manage_messages".into());
        }
        // (Overrides may allow only what a channel gives.)
        have.retain(|permission| every.contains(permission));
        Ok(have)
    }

    /// Whether someone with `powers` (`did`) may see `channel` at all: without
    /// `view_channel` there, it isn't there for them (nothing of it reaches them,
    /// and asked of by id it is unknown). Whoever opened it sees it, while they are
    /// a member and not banned: having opened it gives no sight to someone gone
    /// (WYV-99).
    pub fn sees(&self, did: &str, powers: &Powers, channel: &Channel) -> anyhow::Result<bool> {
        Ok(powers.all() || (channel.created_by == did && self.maker_here(did, crate::gates::now())) || self.channel_permissions(did, powers, channel)?.contains("view_channel"))
    }

    /// Whether `did` reads the thread `thread` of `channel`: any thread but a private
    /// one; a private one, whoever is in it and whoever may manage threads there.
    pub fn reads_thread(&self, did: &str, powers: &Powers, channel: &Channel, thread: &str) -> anyhow::Result<bool> {
        if !self.store.is_private_thread(thread)? {
            return Ok(true);
        }
        Ok(powers.all() || self.store.in_thread(thread, did)? || self.channel_permissions(did, powers, channel)?.contains("manage_threads"))
    }

    /// Whether `did`, who sees `channel`, reads `post` of it: not one in a private
    /// thread they aren't in.
    pub fn reads_post(&self, did: &str, powers: &Powers, channel: &Channel, post: &crate::store::Post) -> anyhow::Result<bool> {
        match &post.thread {
            Some(thread) => self.reads_thread(did, powers, channel, thread),
            None => Ok(true),
        }
    }

    /// Whether `did` sees and reads `post` (its channel, and its thread).
    pub fn reads_post_id(&self, did: &str, post: &crate::store::Post) -> anyhow::Result<bool> {
        let Some(channel) = self.store.channel(&post.channel)? else { return Ok(false) };
        let powers = self.powers(did)?;
        Ok(self.sees(did, &powers, &channel)? && self.reads_post(did, &powers, &channel, post)?)
    }

    /// Whether `did` may see the channel `id` (an unknown one, no).
    pub fn sees_id(&self, did: &str, id: &str) -> anyhow::Result<bool> {
        let Some(channel) = self.store.channel(id)? else { return Ok(false) };
        let powers = self.powers(did)?;
        self.sees(did, &powers, &channel)
    }

    /// The channel `id`, if `did` may see it: one they may not is as unknown as one
    /// that isn't.
    pub fn channel_for(&self, did: &str, id: &str) -> anyhow::Result<Option<Channel>> {
        let Some(channel) = self.store.channel(id)? else { return Ok(None) };
        let powers = self.powers(did)?;
        Ok(self.sees(did, &powers, &channel)?.then_some(channel))
    }

    /// Whether `everyone` may see `channel`: what someone not signed in to the web
    /// view may see of it, public or not, rests on this.
    pub fn everyone_sees(&self, channel: &Channel) -> anyhow::Result<bool> {
        let mut sees = self.everyone_has("view_channel")?;
        if let Some(everyone) = self.store.overrides(&channel.id)?.into_iter().find(|o| o.role == EVERYONE_ROLE) {
            if everyone.deny.iter().any(|p| p == "view_channel") {
                sees = false;
            }
            if everyone.allow.iter().any(|p| p == "view_channel") {
                sees = true;
            }
        }
        Ok(sees)
    }

    /// The channels `did` may see, by id.
    pub fn seen_by(&self, did: &str) -> anyhow::Result<std::collections::HashSet<String>> {
        let powers = self.powers(did)?;
        let mut seen = std::collections::HashSet::new();
        for channel in self.store.channels()? {
            if self.sees(did, &powers, &channel)? {
                seen.insert(channel.id);
            }
        }
        Ok(seen)
    }

    /// Whether `did` holds `permission` in `channel`.
    pub fn may_in(&self, did: &str, channel: &Channel, permission: &str) -> anyhow::Result<bool> {
        let powers = self.powers(did)?;
        Ok(self.channel_permissions(did, &powers, channel)?.contains(permission))
    }

    /// Whether a channel is read-only: it denies `everyone` `post`.
    pub fn read_only(&self, channel: &str) -> anyhow::Result<bool> {
        Ok(self.store.overrides(channel)?.iter().any(|o| o.role == EVERYONE_ROLE && o.deny.iter().any(|p| p == "post")))
    }

    /// Whether `everyone` holds `permission`.
    pub fn everyone_has(&self, permission: &str) -> anyhow::Result<bool> {
        Ok(self.store.role(EVERYONE_ROLE)?.is_some_and(|role| role.permissions.iter().any(|p| p == permission)))
    }

    /// Gives `everyone` `permission`, or takes it away.
    pub fn set_everyone(&self, permission: &str, on: bool) -> anyhow::Result<()> {
        let Some(mut everyone) = self.store.role(EVERYONE_ROLE)? else { return Ok(()) };
        everyone.permissions.retain(|p| p != permission);
        if on {
            everyone.permissions.push(permission.to_string());
        }
        Ok(self.store.save_role(&everyone)?)
    }

    /// The roles as `community` lists them: highest first, `everyone` last, without
    /// their permissions.
    pub fn roles_body(&self) -> anyhow::Result<Vec<Value>> {
        let bots = self.bot_role_holders()?;
        let now = crate::gates::now();
        self
            .store
            .roles()?
            .iter()
            .map(|role| -> anyhow::Result<Value> {
                let mut listed = json!({
                    "id": role.id,
                    "name": role.name,
                    "mentionable": role.mentionable,
                    "hoist": role.hoist,
                    "self_assignable": role.self_assignable,
                });
                if let Some(color) = &role.color {
                    listed["color"] = json!(color);
                }
                if let Some(icon) = &role.icon {
                    listed["icon"] = json!(icon);
                }
                // Only whether it grants everything, for showing an owner who holds
                // no such role apart (a crown); its other permissions stay unsaid.
                if role.permissions.iter().any(|p| p == "administrator") {
                    listed["administrator"] = json!(true);
                }
                // A bot's own: held by it alone (by its member id).
                if let Some(bot) = bots.get(&role.id) {
                    listed["managed"] = json!({ "bot": self.store.member_id(bot, now)? });
                }
                Ok(listed)
            })
            .collect()
    }

    /// Who among the joined holds `permission`.
    pub fn joined_holding(&self, permission: &str) -> anyhow::Result<Vec<crate::store::Member>> {
        let mut holding = Vec::new();
        for member in self.store.joined()? {
            if self.powers(&member.did)?.has(permission) {
                holding.push(member);
            }
        }
        Ok(holding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_permission_is_named_once() {
        let mut seen = BTreeSet::new();
        for (name, _) in COMMUNITY_PERMISSIONS.iter().chain(CHANNEL_PERMISSIONS) {
            assert!(seen.insert(*name), "{name} twice");
        }
        assert!(EVERYONE_DEFAULT.iter().chain(MODERATOR_DEFAULT).all(|p| is_permission(p)));
    }
}

#[cfg(test)]
mod view_channel_tests {
    use super::*;
    use crate::config::CommunityConfig;
    use crate::store::Store;

    #[test]
    fn a_store_from_before_view_channel_lets_everyone_see_every_channel_still() {
        // Its `everyone` as it was: no `view_channel`, and nothing said of it.
        let store = Store::in_memory().unwrap();
        let mut everyone = RoleDef { id: EVERYONE_ROLE.into(), name: "everyone".into(), color: None, icon: None, position: 0, permissions: strings(&["post", "react"]), mentionable: false, hoist: false, self_assignable: false };
        store.save_role(&everyone).unwrap();
        let community = Community::new(store, CommunityConfig::default(), None).unwrap();
        assert!(community.everyone_has("view_channel").unwrap());
        // Once: taken away after, it stays away.
        everyone.permissions = strings(&["post"]);
        community.store.save_role(&everyone).unwrap();
        community.ensure_roles().unwrap();
        assert!(!community.everyone_has("view_channel").unwrap());
        // A new store's `everyone` has it from the start.
        let fresh = Community::new(Store::in_memory().unwrap(), CommunityConfig::default(), None).unwrap();
        assert!(fresh.everyone_has("view_channel").unwrap());
    }

    #[test]
    fn a_store_from_before_thread_permissions_keeps_what_everyone_could_do_and_read_only_stays_so() {
        let store = Store::in_memory().unwrap();
        let everyone = RoleDef { id: EVERYONE_ROLE.into(), name: "everyone".into(), color: None, icon: None, position: 0, permissions: strings(&["view_channel", "post", "create_threads"]), mentionable: false, hoist: false, self_assignable: false };
        store.save_role(&everyone).unwrap();
        store.add_channel("news", "news", "", "did:example:owner", 1).unwrap();
        store.set_override("news", EVERYONE_ROLE, &[], &strings(&["post"])).unwrap();
        let community = Community::new(store, CommunityConfig::default(), None).unwrap();
        for permission in ["send_in_threads", "create_private_threads", "create_polls"] {
            assert!(community.everyone_has(permission).unwrap(), "{permission}");
        }
        let news = community.store.overrides("news").unwrap();
        assert!(news[0].deny.contains(&"send_in_threads".to_string()), "a channel nobody posted in takes no thread posts either");
    }
}
