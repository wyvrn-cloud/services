//! Imported posts claimed by whoever wrote them (community/1.0's `claim-imported`):
//! a member whose profile shows a verified Discord linked account (linked-accounts/1.0,
//! from an issuer the community trusts) takes over the posts brought in under that
//! account (`did:wyvrn:tmp:discord:<id>`, discord_history.rs).
//!
//! Done at once when nobody else has a hand in it: the account proven (its
//! credential checked then), and no other DID holding it, having claimed it or
//! asked for it, and no ban come back with it. Otherwise they ask, and someone who
//! may let members in decides. Claimed, the
//! imported author is superseded by the member (as a member changing DID is), and
//! each post says it is theirs now (`imported.claimed`), so apps name them.

use serde_json::{json, Value};
use wyvrn_service::params;

use crate::discord_history::AUTHOR_PREFIX;
use crate::store::Store;

/// The issuer trusted by default: wyvrn-verify.
pub const DEFAULT_ISSUER: &str = "did:web:verify.wyvrn.app";
/// A Discord account, as linked-accounts/1.0 names its service.
const DISCORD_HOMES: [&str; 2] = ["https://discord.com", "https://discord.gg"];

/// Where a member stands with the posts imported under their Discord account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub discord_id: String,
    pub did: String,
    /// `verified` (found, not asked), `asked`, `claimed` or `refused`.
    pub state: String,
    pub at: i64,
    pub decided_by: Option<String>,
}

/// The Discord account a linked-account credential is about, if it is one: its id.
pub fn discord_account(credential: &Value) -> Option<&str> {
    let account = &credential["credentialSubject"]["account"];
    let home = account["accountServiceHomepage"].as_str()?.trim_end_matches('/');
    (home == "https://discord.com" || home == "https://discord.gg").then_some(())?;
    account["accountName"].as_str().filter(|id| !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_digit()))
}

/// Where a did:web's document is: `https://<host>[/path]/did.json`, or
/// `/.well-known/did.json` at the host.
pub fn did_web_url(did: &str) -> Option<String> {
    let rest = did.strip_prefix("did:web:")?;
    let mut parts = rest.split(':');
    let host = parts.next()?.replace("%3A", ":");
    if host.is_empty() || host.contains('/') || host.contains('@') {
        return None;
    }
    let path: Vec<&str> = parts.collect();
    Some(if path.is_empty() { format!("https://{host}/.well-known/did.json") } else { format!("https://{host}/{}/did.json", path.join("/")) })
}

/// The origin (`https://host`) of a URL, to keep fetching to an issuer's own.
fn origin(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    (parsed.scheme() == "https").then(|| format!("https://{}", parsed.host_str().unwrap_or_default()))
}

/// The Discord id a member's profile proves (see [`verified_accounts`]).
pub async fn verified_discord<F, Fut>(linked: &[Value], address: &str, issuers: &[String], now: &str, fetch: F) -> Option<String>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Option<Value>>,
{
    let discord: Vec<Value> = linked.iter().filter(|c| discord_account(c).is_some()).cloned().collect();
    verified_accounts(&discord, address, issuers, now, fetch).await.into_iter().find_map(|(service, account)| (service == "https://discord.com" || service == "https://discord.gg").then_some(account))
}

/// The account a linked-account credential is about: its service's homepage and
/// its name or id there.
pub fn account_of(credential: &Value) -> Option<(String, String)> {
    let account = &credential["credentialSubject"]["account"];
    let home = account["accountServiceHomepage"].as_str()?.trim_end_matches('/').to_lowercase();
    let name = account["accountName"].as_str().filter(|n| !n.is_empty() && n.len() <= 200)?;
    (home.starts_with("https://") && home.len() <= 200).then(|| (home, name.to_string()))
}

/// The accounts a member's profile proves: of its linked-account credentials, those
/// about `address` (the public address the profile showed to be theirs), from one of
/// `issuers`, signed, in date and not revoked, each `(service homepage, account)`.
/// Nothing is fetched but from an issuer's own origin. `fetch` gets a URL's JSON.
pub async fn verified_accounts<F, Fut>(linked: &[Value], address: &str, issuers: &[String], now: &str, fetch: F) -> Vec<(String, String)>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Option<Value>>,
{
    let mut found = Vec::new();
    for credential in linked.iter().take(10) {
        let Some(id) = account_of(credential) else { continue };
        let service = id.0.as_str();
        // Why one isn't taken is logged (never the credential): a member whose
        // posts don't come to them is otherwise a mystery.
        let issuer = credential["issuer"].as_str().or(credential["issuer"]["id"].as_str()).unwrap_or_default();
        if credential["credentialSubject"]["id"].as_str() != Some(address) {
            tracing::info!(service, issuer, "a linked account isn't taken: it is about another address than the profile's");
            continue;
        }
        if !issuers.iter().any(|trusted| trusted == issuer) {
            tracing::info!(service, issuer, "a linked account isn't taken: its issuer isn't one this community trusts");
            continue;
        }
        let Some(document_url) = did_web_url(issuer) else {
            tracing::info!(service, issuer, "a linked account isn't taken: its issuer isn't a did:web");
            continue;
        };
        let Some(issuer_origin) = origin(&document_url) else { continue };
        let Some(document) = fetch(document_url).await else {
            tracing::info!(service, issuer, "a linked account isn't taken: its issuer's DID document couldn't be fetched");
            continue;
        };
        let status = match wyvrn_hosted_did::linked::status_of(credential) {
            Some((url, _)) if origin(&url).as_deref() == Some(issuer_origin.as_str()) => match fetch(url).await {
                Some(list) => Some(list),
                None => {
                    tracing::info!(service, issuer, "a linked account isn't taken: its status list couldn't be fetched");
                    continue;
                }
            },
            Some(_) => {
                tracing::info!(service, issuer, "a linked account isn't taken: its status list is elsewhere than its issuer's");
                continue;
            }
            None => None,
        };
        match wyvrn_hosted_did::linked::check(credential, &document, status.as_ref(), now) {
            Ok(()) if !found.contains(&id) => found.push(id),
            Ok(()) => {}
            Err(why) => tracing::info!(service, issuer, %why, "a linked account isn't taken: it doesn't check out"),
        }
    }
    found
}

impl Store {
    /// Whether anything was imported under the Discord account `discord_id`.
    pub fn has_imported_from(&self, discord_id: &str) -> wyvrn_service::db::Result<bool> {
        Ok(self.db.lock().query_row("SELECT 1 FROM posts WHERE author_did = ?1 LIMIT 1", params![format!("{AUTHOR_PREFIX}{discord_id}")], |_| Ok(()))?.is_some())
    }

    pub fn claim(&self, discord_id: &str) -> wyvrn_service::db::Result<Option<Claim>> {
        self.db.lock().query_row("SELECT discord_id, did, state, at, decided_by FROM discord_claims WHERE discord_id = ?1", params![discord_id], claim_from)
    }

    pub fn claim_of(&self, did: &str) -> wyvrn_service::db::Result<Option<Claim>> {
        Ok(self.db.lock().query("SELECT discord_id, did, state, at, decided_by FROM discord_claims WHERE did = ?1 ORDER BY at DESC", params![did], claim_from)?.into_iter().next())
    }

    /// Claims asked for and not yet decided, oldest first.
    pub fn claims_asked(&self) -> wyvrn_service::db::Result<Vec<Claim>> {
        self.db.lock().query("SELECT discord_id, did, state, at, decided_by FROM discord_claims WHERE state = 'asked' ORDER BY at", params![], claim_from)
    }

    pub fn set_claim(&self, claim: &Claim) -> wyvrn_service::db::Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO discord_claims (discord_id, did, state, at, decided_by) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![claim.discord_id, claim.did, claim.state, claim.at, claim.decided_by],
        )?;
        Ok(())
    }

    /// As [`Store::claim_imported`], giving back the posts claimed, to be sent as changed.
    pub fn claim_imported_posts(&self, discord_id: &str, did: &str, now: i64) -> anyhow::Result<Vec<String>> {
        let posts: Vec<String> = self.db.lock().query("SELECT id FROM posts WHERE author_did = ?1", params![format!("{AUTHOR_PREFIX}{discord_id}")], |row| row.get(0))?;
        self.claim_imported(discord_id, did, now)?;
        Ok(posts)
    }

    /// Makes the posts imported under `discord_id` `did`'s: their author, the
    /// reactions and mentions that were the Discord account's, and each post marked
    /// claimed and changed (so apps take it up). How many posts.
    pub fn claim_imported(&self, discord_id: &str, did: &str, now: i64) -> anyhow::Result<usize> {
        let prior = format!("{AUTHOR_PREFIX}{discord_id}");
        self.db.grouped(|| -> anyhow::Result<usize> {
            let posts: Vec<String> = self.db.lock().query("SELECT id FROM posts WHERE author_did = ?1", params![prior], |row| row.get(0))?;
            self.rotate(&prior, did, now)?;
            {
                let mut db = self.db.lock();
                // (rotate moves a member's posts; an imported author was never one.)
                db.execute("UPDATE posts SET author_did = ?2, author_name = NULL WHERE author_did = ?1", params![prior, did])?;
                db.execute("UPDATE OR IGNORE reactions SET did = ?2 WHERE did = ?1", params![prior, did])?;
                db.execute("DELETE FROM reactions WHERE did = ?1", params![prior])?;
                db.execute("UPDATE post_state SET reply_author = ?2 WHERE reply_author = ?1", params![prior, did])?;
                db.execute("UPDATE post_state SET mentions = REPLACE(mentions, ?1, ?2) WHERE mentions LIKE ?3", params![format!("\"{prior}\""), format!("\"{did}\""), format!("%\"{prior}\"%")])?;
            }
            for post in &posts {
                let extra: Option<String> = self.db.lock().query_row("SELECT extra FROM post_extra WHERE post = ?1", params![post], |row| row.get(0))?;
                if let Some(mut extra) = extra.and_then(|text| serde_json::from_str::<Value>(&text).ok()) {
                    extra["imported"]["claimed"] = json!(true);
                    self.set_post_extra(post, &extra)?;
                }
                self.touch_post(post)?;
            }
            Ok(posts.len())
        })
    }
}

/// What came of a member being shown to hold a Discord account
/// ([`crate::community::Community::proven_discord`]).
#[derive(Debug, PartialEq, Eq)]
pub enum Proven {
    /// Nothing was imported under it (or nothing more), or they aren't a member.
    Nothing,
    /// Theirs at once: the posts made theirs, to be sent as changed; and whether
    /// they had asked for them (so those who decide are told it's done).
    Claimed { changed: Vec<String>, had_asked: bool },
    /// Someone else has a hand in it, or it was refused them: theirs only once
    /// asked for and approved (`claim-imported`, `decide-claim`).
    Review,
}

impl crate::community::Community {
    /// `did`, a member, was just shown to hold the Discord account `discord_id`
    /// (its credential checked now, [`verified_accounts`]). The posts imported
    /// under it are made theirs at once if nobody else has a hand in it: no other
    /// DID holds the account (a member, or someone waiting), has claimed it or
    /// asked for it, and no ban came back with it; nor was it refused them. Else
    /// where they stand is noted, without undoing anyone else's claim or ask.
    pub(crate) fn proven_discord(&self, did: &str, discord_id: &str, now: i64) -> anyhow::Result<Proven> {
        let store = &self.store;
        if !store.has_imported_from(discord_id)? || store.member(did)?.is_none() {
            return Ok(Proven::Nothing);
        }
        let held = store.claim(discord_id)?;
        // (A claim made under a DID they have since moved from is theirs.)
        let held_by_another = match &held {
            Some(claim) => claim.did != did && store.current_did(&claim.did)? != did,
            None => false,
        };
        let refused = !held_by_another && held.as_ref().is_some_and(|claim| claim.state == "refused");
        let accounts: Vec<(String, String)> = DISCORD_HOMES.iter().map(|home| (home.to_string(), discord_id.to_string())).collect();
        let mut others_hold = false;
        for holder in store.holders_of(&accounts, did)? {
            if store.current_did(&holder)? != did && (store.member(&holder)?.is_some() || store.waiting_one(&holder)?.is_some()) {
                others_hold = true;
                break;
            }
        }
        let ban_back = store.banned_holding(&accounts, now)?.is_some_and(|(banned, _, _)| banned != did);
        if held_by_another || refused || others_hold || ban_back {
            // One claim is kept per account: someone else's ask or claim stands,
            // and so does their own ask or refusal.
            let stands = held.as_ref().is_some_and(|claim| if held_by_another { matches!(claim.state.as_str(), "asked" | "claimed") } else { matches!(claim.state.as_str(), "asked" | "refused") });
            if !stands {
                store.set_claim(&Claim { discord_id: discord_id.to_string(), did: did.to_string(), state: "verified".into(), at: now, decided_by: None })?;
            }
            return Ok(Proven::Review);
        }
        let had_asked = held.as_ref().is_some_and(|claim| claim.state == "asked");
        let changed = store.claim_imported_posts(discord_id, did, now)?;
        store.set_claim(&Claim { discord_id: discord_id.to_string(), did: did.to_string(), state: "claimed".into(), at: now, decided_by: None })?;
        self.audit(now, did, "claim.proven", None, Some(json!({ "discord_id": discord_id, "posts": changed.len() })));
        Ok(Proven::Claimed { changed, had_asked })
    }
}

fn claim_from(row: &wyvrn_service::db::Row) -> wyvrn_service::db::Result<Claim> {
    Ok(Claim { discord_id: row.get(0)?, did: row.get(1)?, state: row.get(2)?, at: row.get(3)?, decided_by: row.get(4)? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_did_web_is_fetched_from_its_host() {
        assert_eq!(did_web_url("did:web:verify.wyvrn.app").as_deref(), Some("https://verify.wyvrn.app/.well-known/did.json"));
        assert_eq!(did_web_url("did:web:example.com%3A8443:users:ann").as_deref(), Some("https://example.com:8443/users/ann/did.json"));
        assert_eq!(did_web_url("did:peer:2abc"), None);
    }

    #[test]
    fn only_a_discord_account_is_one() {
        let credential = |home: &str, name: &str| json!({ "credentialSubject": { "account": { "accountServiceHomepage": home, "accountName": name } } });
        assert_eq!(discord_account(&credential("https://discord.com", "111")), Some("111"));
        assert_eq!(discord_account(&credential("https://twitch.tv", "111")), None);
        assert_eq!(discord_account(&credential("https://discord.com", "ann")), None);
    }

    #[tokio::test]
    async fn nothing_is_fetched_but_from_a_trusted_issuer() {
        let credential = json!({
            "type": ["VerifiableCredential", "LinkedAccountCredential"],
            "issuer": "did:web:evil.example",
            "credentialSubject": { "id": "did:web:ann.example", "account": { "accountServiceHomepage": "https://discord.com", "accountName": "111" } },
        });
        let fetched = std::sync::Mutex::new(Vec::new());
        let found = verified_discord(&[credential], "did:web:ann.example", &[DEFAULT_ISSUER.to_string()], "2026-11-22T00:00:00Z", |url| {
            fetched.lock().unwrap().push(url);
            async { None }
        })
        .await;
        assert_eq!(found, None);
        assert!(fetched.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_signed_discord_credential_from_a_trusted_issuer_names_its_account() {
        use affinidi_data_integrity::{DataIntegrityProof, SignOptions};
        use wyvrn_hosted_did::write::{public_multikey, signing_secret};
        const ISSUER: &str = "did:web:verify.example";
        const JWK: &str = r#"{"kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo","d":"nWGxne_9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A"}"#;
        let sign = |mut value: Value| async move {
            let signer = signing_secret(JWK, Some(&format!("{ISSUER}#key-1"))).unwrap();
            let contexts = value["@context"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
            let proof = DataIntegrityProof::sign(&value, &signer, SignOptions::new().with_proof_purpose("assertionMethod").with_context(contexts)).await.unwrap();
            value["proof"] = serde_json::to_value(proof).unwrap();
            value
        };
        let document = json!({ "id": ISSUER, "verificationMethod": [{ "id": format!("{ISSUER}#key-1"), "type": "Multikey", "controller": ISSUER, "publicKeyMultibase": public_multikey(JWK).unwrap() }], "assertionMethod": [format!("{ISSUER}#key-1")] });
        let credential = sign(json!({
            "@context": ["https://www.w3.org/ns/credentials/v2", "https://wyvrn.app/ns/linked-account/v1"],
            "type": ["VerifiableCredential", "LinkedAccountCredential"],
            "issuer": ISSUER,
            "validFrom": "2026-01-01T00:00:00Z",
            "credentialSubject": { "id": "did:web:ann.example", "account": { "type": "OnlineAccount", "accountServiceHomepage": "https://discord.com", "accountName": "111" } },
        }))
        .await;
        let fetched = std::sync::Mutex::new(Vec::new());
        let fetch = |url: String| {
            fetched.lock().unwrap().push(url.clone());
            let document = document.clone();
            async move { (url == "https://verify.example/.well-known/did.json").then_some(document) }
        };
        let trusted = [ISSUER.to_string()];
        let now = "2026-11-22T00:00:00Z";
        assert_eq!(verified_discord(&[credential.clone()], "did:web:ann.example", &trusted, now, &fetch).await.as_deref(), Some("111"));
        assert_eq!(fetched.lock().unwrap().as_slice(), ["https://verify.example/.well-known/did.json"]);
        // About another address, or changed after signing: not theirs.
        assert_eq!(verified_discord(&[credential.clone()], "did:web:bob.example", &trusted, now, &fetch).await, None);
        let mut changed = credential.clone();
        changed["credentialSubject"]["account"]["accountName"] = json!("222");
        assert_eq!(verified_discord(&[changed], "did:web:ann.example", &trusted, now, &fetch).await, None);
        // A status list elsewhere than the issuer's own address isn't fetched.
        let mut elsewhere = credential;
        elsewhere["credentialStatus"] = json!({ "type": "BitstringStatusListEntry", "statusPurpose": "revocation", "statusListCredential": "https://elsewhere.example/1", "statusListIndex": "0" });
        fetched.lock().unwrap().clear();
        assert_eq!(verified_discord(&[elsewhere], "did:web:ann.example", &trusted, now, &fetch).await, None);
        assert!(!fetched.lock().unwrap().iter().any(|u| u.contains("elsewhere")));
    }

    #[test]
    fn claimed_posts_and_reactions_become_the_members() {
        let store = Store::in_memory().unwrap();
        store.add_channel("fan", "fanart", "", "did:example:owner", 100).unwrap();
        store.admit("did:example:ann", crate::store::Role::Member, 100).unwrap();
        let prior = format!("{AUTHOR_PREFIX}111");
        {
            let mut db = store.db.lock();
            db.execute("INSERT INTO posts (channel, seq, id, content, posted_at, author_did, author_name) VALUES ('fan', 1, 'discord-1', 'art', 5, ?1, 'Ann on Discord')", params![prior]).unwrap();
            db.execute("INSERT INTO post_state (post, rev, mentions) VALUES ('discord-1', 0, ?1)", params![format!("[{{\"who\":\"{prior}\",\"name\":\"Ann\"}}]")]).unwrap();
            db.execute("INSERT INTO post_extra (post, extra) VALUES ('discord-1', ?1)", params![json!({ "imported": { "from": "discord", "id": "1" } }).to_string()]).unwrap();
            db.execute("INSERT INTO reactions (post, did, emoji, at) VALUES ('discord-1', ?1, '❤️', 5)", params![prior]).unwrap();
        }
        assert!(store.has_imported_from("111").unwrap());
        assert_eq!(store.claim_imported("111", "did:example:ann", 200).unwrap(), 1);
        let post = store.post_by_id("discord-1").unwrap().unwrap();
        assert_eq!(post.author_did, "did:example:ann");
        assert_eq!(post.reactions, vec![("❤️".to_string(), vec!["did:example:ann".to_string()])]);
        assert_eq!(post.mentions[0].who, "did:example:ann");
        assert_eq!(post.extra.unwrap()["imported"]["claimed"], true);
        assert!(post.rev > 0);
        assert!(store.is_superseded(&prior).unwrap());
        assert!(!store.has_imported_from("111").unwrap());
    }
}
