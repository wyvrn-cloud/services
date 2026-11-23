//! Where the community's sealed secrets are bound (security review WYV-32,
//! WYV-124): the web secret, the ID-token signing keys, and the signing secrets
//! of event receivers and announcement followers.
//!
//! A sealed secret opens only where it was sealed for, so a row copied into another
//! community's store doesn't open there. It was bound to the community's DID, but a
//! community moves to another server (community/1.0, "Hosted communities, and
//! moving") and its `did:web` changes with the move, while its keys go with it.
//! So it is bound to the community's keys instead: the `did:key` of its signing
//! key, which is the community's wherever it runs. One sealed before, under its
//! DID then, still opens, and is sealed again under its keys.

use didcomm_agent::Identity;

/// What the community's secrets are sealed for: `to`, and the DID they were sealed
/// for before (`before`), which still opens them ([`Community::sealing`](crate::community::Community::sealing)).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sealing {
    pub to: String,
    pub before: Option<String>,
}

impl Sealing {
    /// Where `what` (one of the community's secrets) is bound.
    pub fn place(&self, what: &str) -> String {
        format!("{}#{what}", self.to)
    }

    /// `plain` as it is kept: sealed with the service's seal key where it has one.
    pub fn seal(&self, plain: &str, what: &str) -> anyhow::Result<String> {
        wyvrn_service::identity::seal_text(plain, &self.place(what))
    }

    /// `kept` opened, and whether it should be kept again (found unsealed, sealed
    /// with a previous key, or sealed under the DID the community had then).
    pub fn open(&self, kept: &str, what: &str) -> anyhow::Result<(String, bool)> {
        let error = match wyvrn_service::identity::open_text(kept, &self.place(what)) {
            Ok(opened) => return Ok(opened),
            Err(error) => error,
        };
        match self.before.as_deref().filter(|before| *before != self.to) {
            Some(before) => match wyvrn_service::identity::open_text(kept, &format!("{before}#{what}")) {
                Ok((plain, _)) => Ok((plain, true)),
                Err(_) => Err(error),
            },
            None => Err(error),
        }
    }
}

/// The `did:key` of `identity`'s signing key: what the community's secrets are
/// bound to, the same wherever it runs.
pub fn bound_to(identity: &Identity) -> String {
    let document = identity.did_document("did:key:x", "");
    format!("did:key:{}", document["verificationMethod"][0]["publicKeyMultibase"].as_str().unwrap_or_default())
}

/// Every secret in `store` kept unsealed, as they are taken to another server
/// (community-host's export, community/1.0's "Hosted communities, and moving"):
/// there, the seal key they were sealed with isn't, as the community's keys go
/// unsealed too. One that doesn't open here is said, and left as it is.
pub fn unseal_all(store: &crate::store::Store, sealing: &Sealing) -> anyhow::Result<()> {
    let open = |kept: &str, what: &str| match sealing.open(kept, what) {
        Ok((plain, _)) => Some(plain),
        Err(error) => {
            tracing::warn!(what, error = %format!("{error:#}"), "a sealed secret doesn't open, and goes as it is");
            None
        }
    };
    if let Some(plain) = store.setting(crate::web::SECRET)?.and_then(|kept| open(&kept, "web-secret")) {
        store.set_setting(crate::web::SECRET, &plain)?;
    }
    for (kid, kept, made) in store.oidc_keys()? {
        if let Some(plain) = open(&kept, &format!("oidc-{kid}")) {
            store.remove_oidc_key(&kid)?;
            store.add_oidc_key(&kid, &plain, made)?;
        }
    }
    for mut hook in store.event_hooks()? {
        if let Some(plain) = open(&hook.secret, &format!("event-hook-{}", hook.id)) {
            hook.secret = plain;
            store.save_event_hook(&hook)?;
        }
    }
    for mut follower in store.followers()? {
        if let Some(plain) = follower.secret.as_deref().and_then(|kept| open(kept, &format!("follower-{}", follower.id))) {
            follower.secret = Some(plain);
            store.save_follower(&follower)?;
        }
    }
    Ok(())
}
