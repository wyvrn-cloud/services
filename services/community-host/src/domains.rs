//! A hosted community at a domain of its own (asked 2026-11-22): where the host
//! allows it (`[domains] enabled`), the community's owner names a domain on their
//! admin page, points it at the host (a CNAME to `[domains] cname`) and proves it is
//! theirs (a TXT record `_wyvrn.<domain>` holding a token the page gives, read over
//! DNS-over-HTTPS). Once proved, the host answers that domain as the community's:
//! its web view is at `https://<domain>/`, Caddy's on-demand TLS is told it may get
//! a certificate for it (`/domains/ask`), and the community's address at the host
//! (`<public_url>c/<name>/`) goes on answering too.
//!
//! The community's DID doesn't change: members' apps know it by it, and its keys
//! are named under it. Its web view, and the links it gives, use the domain.

use std::sync::{Arc, Weak};

use axum::extract::{Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use wyvrn_service::admin::{Page, Pages};
use wyvrn_service::params;

use crate::registry::Registry;
use crate::tenants::Host;

pub const PAGE: &str = "own-domain";

/// `[domains]`: whether owners may, where to point (`cname`), and where TXT records
/// are read (`dns_over_https`, Cloudflare's unless set; a test points it elsewhere).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DomainsConfig {
    pub enabled: bool,
    pub cname: String,
    pub dns_over_https: String,
}

impl Default for DomainsConfig {
    fn default() -> Self {
        Self { enabled: false, cname: String::new(), dns_over_https: "https://cloudflare-dns.com/dns-query".into() }
    }
}

/// A community's domain: proved (`verified_at`) or waiting on its TXT record.
#[derive(Debug, Clone, PartialEq)]
pub struct Domain {
    pub name: String,
    pub domain: String,
    pub token: String,
    pub verified_at: Option<i64>,
}

pub fn mysql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS community_domains (
            name VARCHAR(64) NOT NULL PRIMARY KEY,
            domain VARCHAR(253) NOT NULL UNIQUE,
            token VARCHAR(64) NOT NULL,
            verified_at BIGINT
        ) {}",
        wyvrn_service::db::MYSQL_TABLE
    ) + &format!(
        ";
        CREATE TABLE IF NOT EXISTS domain_claims (
            domain VARCHAR(253) NOT NULL PRIMARY KEY,
            claimed_at BIGINT NOT NULL
        ) {}",
        wyvrn_service::db::MYSQL_TABLE
    ) + &format!(
        ";
        CREATE TABLE IF NOT EXISTS domain_pending (
            name VARCHAR(64) NOT NULL PRIMARY KEY,
            domain VARCHAR(253) NOT NULL,
            token VARCHAR(64) NOT NULL,
            claimed_at BIGINT NOT NULL
        ) {};
        CREATE INDEX IF NOT EXISTS domain_pending_by_domain ON domain_pending (domain)",
        wyvrn_service::db::MYSQL_TABLE
    )
}

/// How long a domain named and not proved is kept waiting for its proof
/// (security review WYV-81): a day is time enough to add a DNS record. Naming it
/// again doesn't make it longer.
pub const UNPROVED_FOR: i64 = 24 * 3600;

/// Whether `text` is a domain a community may have: letters, digits and `-` in
/// dot-separated labels, at least two of them, and none an address of its own.
pub fn is_domain(text: &str) -> bool {
    let labels: Vec<&str> = text.split('.').collect();
    labels.len() >= 2
        && text.len() <= 253
        && labels.iter().all(|l| (1..=63).contains(&l.len()) && l.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !l.starts_with('-') && !l.ends_with('-'))
        && !labels.last().is_some_and(|tld| tld.chars().all(|c| c.is_ascii_digit()))
        && !["localhost", "local", "internal", "test", "example", "invalid"].contains(labels.last().unwrap_or(&""))
}

fn from_row(row: &wyvrn_service::db::Row) -> wyvrn_service::db::Result<Domain> {
    Ok(Domain { name: row.get(0)?, domain: row.get(1)?, token: row.get(2)?, verified_at: row.get(3)? })
}

impl Registry {
    /// `name`'s domain: the one it proved, or else the one it named and is still
    /// to prove.
    pub fn domain_of(&self, name: &str) -> anyhow::Result<Option<Domain>> {
        let mut db = self.db.lock();
        if let Some(held) = db.query_row("SELECT name, domain, token, verified_at FROM community_domains WHERE name = ?1", params![name], from_row)? {
            return Ok(Some(held));
        }
        Ok(db.query_row("SELECT name, domain, token, NULL FROM domain_pending WHERE name = ?1", params![name], from_row)?)
    }

    /// The community a proved domain is, if it is one being served: not one
    /// deleted or moved, whose domain waits to be freed (security review WYV-75).
    pub fn community_at(&self, domain: &str) -> anyhow::Result<Option<String>> {
        Ok(self.db.lock().query_row(
            "SELECT d.name FROM community_domains d JOIN communities c ON c.name = d.name
             WHERE d.domain = ?1 AND d.verified_at IS NOT NULL AND c.state NOT IN ('deleted', 'moved')",
            params![domain],
            |row| row.get(0),
        )?)
    }

    /// Names `domain` for `name`, to be proved with `token`. `false` where another
    /// community has proved it. Any number of communities may name one domain,
    /// each with a token of its own: whichever proves it first has it, so naming
    /// it first holds it against nobody (security review WYV-81). Naming the same
    /// domain again keeps the claim as it was (its token, and when it runs out).
    pub fn claim_domain(&self, name: &str, domain: &str, token: &str, now: i64) -> anyhow::Result<bool> {
        let mut db = self.db.lock();
        let proved: Option<String> = db.query_row("SELECT name FROM community_domains WHERE domain = ?1 AND verified_at IS NOT NULL", params![domain], |row| row.get(0))?;
        if proved.as_deref().is_some_and(|other| other != name) {
            return Ok(false);
        }
        if proved.is_some() {
            return Ok(true);
        }
        let pending: Option<String> = db.query_row("SELECT domain FROM domain_pending WHERE name = ?1", params![name], |row| row.get(0))?;
        if pending.as_deref() == Some(domain) {
            return Ok(true);
        }
        // Another domain this community had, proved or not, it has no longer.
        db.execute("DELETE FROM community_domains WHERE name = ?1", params![name])?;
        db.execute("DELETE FROM domain_pending WHERE name = ?1", params![name])?;
        db.execute("INSERT INTO domain_pending (name, domain, token, claimed_at) VALUES (?1, ?2, ?3, ?4)", params![name, domain, token, now])?;
        Ok(true)
    }

    /// Unproved claims older than [`UNPROVED_FOR`], let go.
    pub fn expire_domain_claims(&self, now: i64) -> anyhow::Result<usize> {
        let mut db = self.db.lock();
        let mut gone = db.execute("DELETE FROM domain_pending WHERE claimed_at <= ?1", params![now - UNPROVED_FOR])?;
        // Unproved ones from before claims were kept apart, as they ran out then.
        gone += db.execute(
            "DELETE FROM community_domains WHERE verified_at IS NULL AND domain NOT IN (SELECT domain FROM domain_claims WHERE claimed_at > ?1)",
            params![now - UNPROVED_FOR],
        )?;
        db.execute("DELETE FROM domain_claims WHERE domain NOT IN (SELECT domain FROM community_domains WHERE verified_at IS NULL)", params![])?;
        Ok(gone)
    }

    /// Every proved domain, with its community and token, for proofs to be checked again.
    pub fn proved_domains(&self) -> anyhow::Result<Vec<Domain>> {
        Ok(self.db.lock().query("SELECT name, domain, token, verified_at FROM community_domains WHERE verified_at IS NOT NULL", params![], from_row)?)
    }

    /// A domain whose proof is gone: back to waiting on it, from now, like any
    /// other claim to it.
    pub fn domain_unproved(&self, name: &str, now: i64) -> anyhow::Result<()> {
        let mut db = self.db.lock();
        let Some(held) = db.query_row("SELECT name, domain, token, verified_at FROM community_domains WHERE name = ?1", params![name], from_row)? else { return Ok(()) };
        db.execute("DELETE FROM community_domains WHERE name = ?1", params![name])?;
        db.execute("DELETE FROM domain_pending WHERE name = ?1", params![name])?;
        db.execute("INSERT INTO domain_pending (name, domain, token, claimed_at) VALUES (?1, ?2, ?3, ?4)", params![name, held.domain, held.token, now])?;
        Ok(())
    }

    /// `name` proved its claim: the domain is its, and every other claim to it is
    /// let go. `false` where another community proved it first.
    pub fn domain_proved(&self, name: &str, now: i64) -> anyhow::Result<bool> {
        let mut db = self.db.lock();
        let Some(claim) = db.query_row("SELECT name, domain, token, NULL FROM domain_pending WHERE name = ?1", params![name], from_row)? else {
            // Unproved from before claims were kept apart: proved where it is.
            return Ok(db.execute("UPDATE community_domains SET verified_at = ?2 WHERE name = ?1", params![name, now])? > 0);
        };
        let proved: Option<String> = db.query_row("SELECT name FROM community_domains WHERE domain = ?1 AND verified_at IS NOT NULL", params![claim.domain], |row| row.get(0))?;
        if proved.is_some_and(|other| other != name) {
            return Ok(false);
        }
        db.execute("DELETE FROM community_domains WHERE domain = ?1 OR name = ?2", params![claim.domain, name])?;
        db.execute("DELETE FROM domain_claims WHERE domain = ?1", params![claim.domain])?;
        db.execute("INSERT INTO community_domains (name, domain, token, verified_at) VALUES (?1, ?2, ?3, ?4)", params![name, claim.domain, claim.token, now])?;
        db.execute("DELETE FROM domain_pending WHERE domain = ?1", params![claim.domain])?;
        Ok(true)
    }

    pub fn drop_domain(&self, name: &str) -> anyhow::Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM domain_claims WHERE domain IN (SELECT domain FROM community_domains WHERE name = ?1)", params![name])?;
        db.execute("DELETE FROM community_domains WHERE name = ?1", params![name])?;
        db.execute("DELETE FROM domain_pending WHERE name = ?1", params![name])?;
        Ok(())
    }
}

/// The TXT records at `name`, by DNS-over-HTTPS's JSON (RFC 8484's companion,
/// as Cloudflare and Google answer it).
pub async fn txt_records(resolver: &str, name: &str) -> anyhow::Result<Vec<String>> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build()?;
    let answer: Value = client.get(resolver).query(&[("name", name), ("type", "TXT")]).header("accept", "application/dns-json").send().await?.error_for_status()?.json().await?;
    Ok(answer["Answer"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["type"] == 16)
        .filter_map(|a| a["data"].as_str())
        .map(|data| data.trim().trim_matches('"').replace("\" \"", ""))
        .collect())
}

/// The owner's page: name a domain, prove it, take it away.
pub struct OwnDomain {
    pub host: Weak<Host>,
    pub community: String,
}

fn button(label: &str, event: &str) -> Value {
    json!({ "type": "button", "label": label, "event": event, "data": {} })
}

impl Pages for OwnDomain {
    fn pages(&self) -> Vec<Page> {
        match self.host.upgrade() {
            Some(host) if host.config.domains.enabled => {
                vec![Page { id: PAGE, title: "Your own domain", summary: "Have this community's pages on the web at a domain of your own.", events: &["save", "check", "remove"] }]
            }
            _ => Vec::new(),
        }
    }

    fn view(&self, _page: &str, _owner: &str, _now: i64) -> anyhow::Result<Vec<Value>> {
        let Some(host) = self.host.upgrade() else { return Ok(vec![]) };
        let held = host.registry.domain_of(&self.community)?;
        let mut items = vec![json!({ "type": "text", "text": "Your community's pages on the web can be at a domain you own (pages.example.com). Members' apps find it as they do now; nothing changes for them." })];
        match held {
            None => {
                items.push(json!({ "type": "form", "children": [
                    { "type": "text-input", "form-id": "domain", "label": "The domain (a subdomain such as community.example.com is easiest)", "value": "" },
                    button("Use this domain", "save"),
                ] }));
            }
            Some(Domain { domain, token, verified_at: None, .. }) => {
                items.push(json!({ "type": "information", "text": format!("Waiting for {domain} to point here.") }));
                items.push(json!({ "type": "text", "text": format!(
                    "At whoever keeps your domain's DNS, add two records, then press Check (DNS can take a while to show them):\n\n1. CNAME  {domain}  ->  {}\n2. TXT  _wyvrn.{domain}  =  {token}",
                    if host.config.domains.cname.is_empty() { "this host's address" } else { &host.config.domains.cname }
                ) }));
                items.push(button("Check", "check"));
                items.push(button("Use another domain", "remove"));
            }
            Some(Domain { domain, verified_at: Some(_), .. }) => {
                items.push(json!({ "type": "information", "text": format!("Live at https://{domain}/") }));
                items.push(json!({ "type": "web-link", "label": format!("Open https://{domain}/"), "url": format!("https://{domain}/") }));
                items.push(json!({ "type": "text", "text": format!("Keep the CNAME record pointing here. Its address here, {}c/{}/, answers too.", host.config.server.public_url, self.community) }));
                items.push(button("Stop using it", "remove"));
            }
        }
        Ok(items)
    }

    fn act(&self, _page: &str, event: &str, input: &Value, owner: &str, now: i64) -> Result<Option<String>, String> {
        let host = self.host.upgrade().ok_or("The host is stopping.")?;
        if !host.config.domains.enabled {
            return Err("This host doesn't offer domains of their own.".into());
        }
        let failed = |error: anyhow::Error| format!("That didn't work: {error}");
        let said = match event {
            "save" => {
                let domain = input["domain"].as_str().unwrap_or_default().trim().trim_end_matches('.').to_ascii_lowercase();
                let domain = domain.strip_prefix("https://").or(domain.strip_prefix("http://")).unwrap_or(&domain).trim_end_matches('/').to_string();
                if !is_domain(&domain) {
                    return Err("That isn't a domain: write it as community.example.com.".into());
                }
                if host.config.server.public_url.contains(&format!("//{domain}")) {
                    return Err("That is this host's own domain.".into());
                }
                let token = format!("wyvrn-{}", uuid::Uuid::new_v4().simple());
                if !host.registry.claim_domain(&self.community, &domain, &token, now).map_err(failed)? {
                    return Err("Another community here has proved that domain.".into());
                }
                format!("Now add the two DNS records shown, then press Check.")
            }
            "check" => {
                let held = host.registry.domain_of(&self.community).map_err(failed)?.ok_or("Name a domain first.")?;
                if held.verified_at.is_some() {
                    return Ok(Some("It is live already.".into()));
                }
                let resolver = host.config.domains.dns_over_https.clone();
                let name = format!("_wyvrn.{}", held.domain);
                let records = tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(txt_records(&resolver, &name))).map_err(|e| format!("Its DNS couldn't be read just now: {e}"))?;
                if !records.iter().any(|r| r == &held.token) {
                    return Err(format!("No TXT record _wyvrn.{} holding {} yet. DNS can take a while; press Check again later.", held.domain, held.token));
                }
                if !host.registry.domain_proved(&self.community, now).map_err(failed)? {
                    return Err("Another community here proved that domain first.".into());
                }
                // Opened again, it takes up its web view's new address.
                host.unload(&self.community);
                format!("It's yours: https://{}/ is this community's, once the CNAME points here.", held.domain)
            }
            "remove" => {
                host.registry.drop_domain(&self.community).map_err(failed)?;
                host.unload(&self.community);
                "It no longer uses a domain of its own.".to_string()
            }
            _ => return Err("That can't be done here.".into()),
        };
        tracing::info!(community = %self.community, event, by = %owner, "the owner's domain");
        Ok(Some(said))
    }
}

/// More than one set of owner pages, as one.
pub struct Both(pub Arc<dyn Pages>, pub Arc<dyn Pages>);

impl Pages for Both {
    fn pages(&self) -> Vec<Page> {
        let mut pages = self.0.pages();
        pages.extend(self.1.pages());
        pages
    }
    fn view(&self, page: &str, owner: &str, now: i64) -> anyhow::Result<Vec<Value>> {
        if self.0.pages().iter().any(|p| p.id == page) { self.0.view(page, owner, now) } else { self.1.view(page, owner, now) }
    }
    fn act(&self, page: &str, event: &str, input: &Value, owner: &str, now: i64) -> Result<Option<String>, String> {
        if self.0.pages().iter().any(|p| p.id == page) { self.0.act(page, event, input, owner, now) } else { self.1.act(page, event, input, owner, now) }
    }
    fn artifacts(&self, page: &str, owner: &str) -> Value {
        if self.0.pages().iter().any(|p| p.id == page) { self.0.artifacts(page, owner) } else { self.1.artifacts(page, owner) }
    }
}

#[derive(Deserialize)]
pub struct Asked {
    domain: String,
}

/// `/domains/ask?domain=`: Caddy's on-demand TLS asking whether it may get a
/// certificate for a domain. Yes only for a proved one.
pub async fn ask(State(state): State<Arc<crate::server::AppState>>, Query(asked): Query<Asked>) -> Response {
    match state.host.registry.community_at(&asked.domain.to_ascii_lowercase()) {
        Ok(Some(_)) => StatusCode::OK.into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A request for a proved domain is its community's, whatever its path.
pub async fn by_host(State(state): State<Arc<crate::server::AppState>>, request: Request, next: Next) -> Response {
    let host = request.headers().get(axum::http::header::HOST).and_then(|h| h.to_str().ok()).map(|h| h.split(':').next().unwrap_or(h).to_ascii_lowercase());
    if let Some(host) = host.filter(|h| is_domain(h)) {
        if let Ok(Some(name)) = state.host.registry.community_at(&host) {
            return state.host.forward(&name, request).await;
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_domain_is_one_of_the_internets() {
        for good in ["community.example.com", "frosty.chat", "a-b.c-d.org"] {
            assert!(is_domain(good), "{good}");
        }
        for bad in ["localhost", "box.local", "127.0.0.1", "nodot", "-a.com", "a..com", "Caps.com", "x.example", "under_score.com", "a.test"] {
            assert!(!is_domain(bad), "{bad}");
        }
    }

    #[test]
    fn a_domain_is_freed_with_its_community_served_only_while_it_is_and_an_unproved_claim_runs_out() {
        claims_go_as_they_should(crate::registry::Registry::scratch().unwrap());
    }

    #[test]
    fn domain_claims_go_as_they_should_in_mysql_too() {
        let Some(url) = std::env::var("WYVRN_TEST_DATABASE_URL").ok().filter(|url| !url.is_empty()) else { return };
        let server = url.trim_end_matches('/');
        let database = format!("wyvrn_test_domains_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        {
            let mut admin = mysql::Conn::new(mysql::Opts::from_url(server).unwrap()).unwrap();
            mysql::prelude::Queryable::query_drop(&mut admin, format!("CREATE DATABASE {database}")).unwrap();
        }
        claims_go_as_they_should(crate::registry::Registry::open(&wyvrn_service::db::Location::MySql(format!("{server}/{database}"))).unwrap());
    }

    fn claims_go_as_they_should(registry: crate::registry::Registry) {
        use crate::registry::{State, Storage};
        for name in ["shop", "squat", "brand"] {
            registry.create(name, name, Storage::Sqlite, "did:example:o", "did:example:o", 0).unwrap();
        }
        // Proved, served; deleted, no longer served; purged, free for the next.
        assert!(registry.claim_domain("shop", "shop.example.com", "t1", 0).unwrap());
        registry.domain_proved("shop", 10).unwrap();
        assert_eq!(registry.community_at("shop.example.com").unwrap().as_deref(), Some("shop"));
        registry.set_state("shop", State::Deleted, Some(100)).unwrap();
        assert_eq!(registry.community_at("shop.example.com").unwrap(), None, "deleted: not served");
        registry.forget("shop").unwrap();
        registry.create("shop", "shop", Storage::Sqlite, "did:example:x", "did:example:x", 200).unwrap();
        assert_eq!(registry.community_at("shop.example.com").unwrap(), None, "a newcomer with the name doesn't get it");
        assert!(registry.domain_of("shop").unwrap().is_none());

        // Named and not proved: it holds the domain against nobody. Another names it
        // too, with a token of its own, and whichever proves it first has it (WYV-81).
        assert!(registry.claim_domain("squat", "bigbrand.com", "t2", 1_000).unwrap());
        assert!(registry.claim_domain("brand", "bigbrand.com", "t3", 1_000 + 3600).unwrap(), "named by two at once");
        assert_eq!(registry.domain_of("squat").unwrap().unwrap().token, "t2");
        assert_eq!(registry.domain_of("brand").unwrap().unwrap().token, "t3");
        assert!(registry.domain_proved("brand", 1_000 + 7200).unwrap());
        assert_eq!(registry.community_at("bigbrand.com").unwrap().as_deref(), Some("brand"));
        assert!(registry.domain_of("squat").unwrap().is_none(), "the other claim is let go");
        assert!(!registry.claim_domain("squat", "bigbrand.com", "t6", 10 * UNPROVED_FOR).unwrap(), "a proved one is never taken");
        assert!(!registry.domain_proved("squat", 10 * UNPROVED_FOR).unwrap());

        // An unproved claim runs out a day after it was made, and naming it again
        // doesn't make it longer.
        assert!(registry.claim_domain("squat", "other.example.org", "t4", 0).unwrap());
        assert!(registry.claim_domain("squat", "other.example.org", "t7", UNPROVED_FOR - 10).unwrap());
        assert_eq!(registry.domain_of("squat").unwrap().unwrap().token, "t4", "the claim as it was");
        assert_eq!(registry.expire_domain_claims(UNPROVED_FOR).unwrap(), 1);
        assert!(registry.domain_of("squat").unwrap().is_none());
        assert!(registry.domain_of("brand").unwrap().is_some(), "a proved one stays");

        // A proof that is gone is a claim again, from then.
        registry.domain_unproved("brand", 50 * UNPROVED_FOR).unwrap();
        assert_eq!(registry.community_at("bigbrand.com").unwrap(), None);
        assert!(registry.claim_domain("squat", "bigbrand.com", "t8", 50 * UNPROVED_FOR).unwrap());
        assert!(registry.domain_proved("brand", 50 * UNPROVED_FOR + 5).unwrap());
        assert!(registry.domain_of("squat").unwrap().is_none());
    }
}
