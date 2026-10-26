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
];

/// The permissions a channel can also give or withhold.
pub const CHANNEL_PERMISSIONS: &[(&str, &str)] = &[
    ("post", "Post"),
    ("react", "React"),
    ("create_threads", "Start threads"),
    ("attach_files", "Post files"),
    ("mention_everyone", "Mention everyone, and any role"),
    ("manage_messages", "Remove and pin anyone's posts"),
    ("bypass_slow_mode", "Ignore slow mode"),
    ("connect", "Join voice channels"),
    ("speak", "Speak in voice channels"),
    ("video", "Share a camera, screen or avatar"),
    ("moderate_voice", "Mute and remove people in voice channels"),
];

/// Holding any of these makes someone a `moderator` in older terms.
const MODERATING: &[&str] = &["manage_messages", "admit_members", "kick_members", "ban_members", "timeout_members", "manage_reports"];

const EVERYONE_DEFAULT: &[&str] = &["post", "react", "create_threads", "attach_files", "connect", "speak", "video"];
const MODERATOR_DEFAULT: &[&str] =
    &["admit_members", "kick_members", "timeout_members", "manage_reports", "view_audit_log", "manage_messages", "bypass_slow_mode", "moderate_voice"];

pub fn is_permission(name: &str) -> bool {
    COMMUNITY_PERMISSIONS.iter().chain(CHANNEL_PERMISSIONS).any(|(known, _)| *known == name)
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

impl Community {
    /// Gives a store from before roles its roles, once: `everyone` with what
    /// members could do, then Moderator and Admin, and `admin` to every admin.
    pub fn ensure_roles(&self) -> anyhow::Result<()> {
        if self.store.role(EVERYONE_ROLE)?.is_some() {
            return Ok(());
        }
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

    /// The owner's DID: as handed on, else the first member let in who holds
    /// `administrator`, from then on.
    pub fn owner(&self) -> anyhow::Result<Option<String>> {
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
        let roles = self.store.roles()?;
        let held = self.store.member_roles(did)?;
        let mut permissions = BTreeSet::new();
        let mut rank = 0;
        for role in roles.iter().filter(|role| role.id == EVERYONE_ROLE || held.contains(&role.id)) {
            permissions.extend(role.permissions.iter().cloned());
            if role.id != EVERYONE_ROLE {
                rank = rank.max(role.position);
            }
        }
        let owner = self.owner()?.as_deref() == Some(did);
        let roles = held.into_iter().filter(|id| roles.iter().any(|role| role.id == *id)).collect();
        Ok(Powers { owner, roles, permissions, rank })
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
        let every: BTreeSet<String> = CHANNEL_PERMISSIONS.iter().map(|(name, _)| name.to_string()).collect();
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
        // Whoever opened a channel keeps order in it.
        if channel.created_by == did {
            have.insert("manage_messages".into());
        }
        Ok(have)
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
        Ok(self
            .store
            .roles()?
            .iter()
            .map(|role| {
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
                listed
            })
            .collect())
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
