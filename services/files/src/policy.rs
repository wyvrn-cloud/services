//! Who may put files here, and how much: what an admin can change while the
//! service runs (its admin pages, [`crate::admin`]), starting from the settings.
//!
//! - **Who.** The DIDs the settings name are uploaders, and so is anyone an admin
//!   adds. With sign-up open, anyone else who authenticates becomes one at their
//!   first `upload`; locking down stops that, and leaves those already in as they
//!   are. An uploader can be blocked. Admins always may.
//! - **Anonymous.** An uploader the service has no user-profile/1.0 profile for
//!   is anonymous to it. With anonymous uploads blocked, such an uploader is
//!   asked for one, and refused until it comes.
//! - **How much.** One limit on everything held, then a tier of limits for
//!   everyone and another for admins, and an uploader's own limits in place of
//!   their tier's where an admin has set them.

use serde::{Deserialize, Serialize};

use crate::config::ServiceConfig;

/// What one uploader may do here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// How much they may keep in all, uploads in progress counted whole.
    pub quota_bytes: u64,
    /// The largest file.
    pub max_file_bytes: u64,
    /// The longest a file may be kept from now.
    pub max_keep_seconds: i64,
    /// How many uploads may be going at once.
    pub max_uploads_in_progress: usize,
}

/// The service's rules as they stand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// Anyone who authenticates may become an uploader at their first upload.
    pub open: bool,
    /// Nobody new becomes an uploader, however they would have.
    pub locked: bool,
    /// An uploader the service has no profile for is refused (and asked for one).
    pub require_profile: bool,
    /// The most the service holds of everyone's, in bytes. 0: no limit but the disk.
    pub total_bytes: u64,
    /// How long a file is kept when its uploader doesn't say.
    pub default_keep_seconds: i64,
    pub everyone: Limits,
    pub admins: Limits,
}

impl Policy {
    /// The rules the settings start the service with, before an admin changes any.
    pub fn from_config(config: &ServiceConfig) -> Self {
        let everyone = Limits {
            quota_bytes: config.quota_bytes,
            max_file_bytes: config.max_file_bytes,
            max_keep_seconds: config.max_keep_seconds,
            max_uploads_in_progress: config.max_uploads_in_progress,
        };
        Self {
            open: config.open_signup,
            locked: false,
            require_profile: false,
            total_bytes: config.total_bytes,
            default_keep_seconds: config.default_keep_seconds,
            everyone,
            admins: everyone,
        }
    }

    /// What `uploader` may do: their own limits, or their tier's.
    pub fn limits_for(&self, is_admin: bool, own: Option<Limits>) -> Limits {
        own.unwrap_or(if is_admin { self.admins } else { self.everyone })
    }

    /// How long a new file is kept: what was asked, or the default, at most `limits` allow.
    pub fn keep(&self, limits: &Limits, asked: Option<i64>) -> i64 {
        asked.filter(|&s| s > 0).unwrap_or(self.default_keep_seconds).min(limits.max_keep_seconds)
    }
}

/// Someone the service knows as an uploader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uploader {
    pub did: String,
    /// How they came to be one: `settings`, `admin` or `signup`.
    pub source: String,
    /// What their profile calls them, once one has come.
    pub name: Option<String>,
    /// Their own limits, in place of their tier's.
    pub limits: Option<Limits>,
    pub blocked: bool,
    pub added: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_uploaders_limits_are_their_own_or_their_tiers() {
        let mut policy = Policy::from_config(&ServiceConfig::default());
        policy.admins.quota_bytes = 99;
        let own = Limits { quota_bytes: 5, ..policy.everyone };
        assert_eq!(policy.limits_for(false, None), policy.everyone);
        assert_eq!(policy.limits_for(true, None).quota_bytes, 99);
        assert_eq!(policy.limits_for(true, Some(own)).quota_bytes, 5);
        let short = Limits { max_keep_seconds: 10, ..policy.everyone };
        assert_eq!(policy.keep(&short, None), 10);
        assert_eq!(policy.keep(&policy.everyone, Some(60)), 60);
    }
}
