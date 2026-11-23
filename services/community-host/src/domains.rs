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
    )
}

/// How long a domain named and not proved holds it against another community
/// (security review WYV-81): a day is time enough to add a DNS record.
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
    pub fn domain_of(&self, name: &str) -> anyhow::Result<Option<Domain>> {
        Ok(self.db.lock().query_row("SELECT name, domain, token, verified_at FROM community_domains WHERE name = ?1", params![name], from_row)?)
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
    /// community has proved it, or named it less than [`UNPROVED_FOR`] ago; an
    /// older unproved claim is let go (security review WYV-81).
    pub fn claim_domain(&self, name: &str, domain: &str, token: &str, now: i64) -> anyhow::Result<bool> {
        let mut db = self.db.lock();
        let taken: Option<(String, Option<i64>)> =
            db.query_row("SELECT name, verified_at FROM community_domains WHERE domain = ?1", params![domain], |row| Ok((row.get(0)?, row.get(1)?)))?;
        if let Some((other, verified_at)) = taken.filter(|(other, _)| other != name) {
            let claimed_at: Option<i64> = db.query_row("SELECT claimed_at FROM domain_claims WHERE domain = ?1", params![domain], |row| row.get(0))?;
            // One named before claims were dated counts as run out.
            if verified_at.is_some() || claimed_at.is_some_and(|at| now - at < UNPROVED_FOR) {
                return Ok(false);
            }
            db.execute("DELETE FROM community_domains WHERE name = ?1", params![other])?;
        }
        db.execute("DELETE FROM community_domains WHERE name = ?1", params![name])?;
        db.execute("INSERT INTO community_domains (name, domain, token, verified_at) VALUES (?1, ?2, ?3, NULL)", params![name, domain, token])?;
        db.execute("DELETE FROM domain_claims WHERE domain = ?1", params![domain])?;
        db.execute("INSERT INTO domain_claims (domain, claimed_at) VALUES (?1, ?2)", params![domain, now])?;
        Ok(true)
    }

    /// Unproved claims older than [`UNPROVED_FOR`], let go.
    pub fn expire_domain_claims(&self, now: i64) -> anyhow::Result<usize> {
        let mut db = self.db.lock();
        let gone = db.execute(
            "DELETE FROM community_domains WHERE verified_at IS NULL AND domain NOT IN (SELECT domain FROM domain_claims WHERE claimed_at > ?1)",
            params![now - UNPROVED_FOR],
        )?;
        db.execute("DELETE FROM domain_claims WHERE domain NOT IN (SELECT domain FROM community_domains)", params![])?;
        Ok(gone)
    }

    /// Every proved domain, with its community and token, for proofs to be checked again.
    pub fn proved_domains(&self) -> anyhow::Result<Vec<Domain>> {
        Ok(self.db.lock().query("SELECT name, domain, token, verified_at FROM community_domains WHERE verified_at IS NOT NULL", params![], from_row)?)
    }

    /// A domain whose proof is gone: back to waiting on it.
    pub fn domain_unproved(&self, name: &str) -> anyhow::Result<()> {
        self.db.lock().execute("UPDATE community_domains SET verified_at = NULL WHERE name = ?1", params![name])?;
        Ok(())
    }

    pub fn domain_proved(&self, name: &str, now: i64) -> anyhow::Result<()> {
        self.db.lock().execute("UPDATE community_domains SET verified_at = ?2 WHERE name = ?1", params![name, now])?;
        Ok(())
    }

    pub fn drop_domain(&self, name: &str) -> anyhow::Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM domain_claims WHERE domain IN (SELECT domain FROM community_domains WHERE name = ?1)", params![name])?;
        db.execute("DELETE FROM community_domains WHERE name = ?1", params![name])?;
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
                    return Err("Another community here has that domain.".into());
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
                host.registry.domain_proved(&self.community, now).map_err(failed)?;
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
        use crate::registry::{Registry, State, Storage};
        let registry = Registry::scratch().unwrap();
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

        // Named and not proved: held a day, then anyone may name it.
        assert!(registry.claim_domain("squat", "bigbrand.com", "t2", 1_000).unwrap());
        assert!(!registry.claim_domain("brand", "bigbrand.com", "t3", 1_000 + 3600).unwrap(), "within a day");
        assert!(registry.claim_domain("brand", "bigbrand.com", "t3", 1_000 + UNPROVED_FOR).unwrap(), "after a day");
        assert!(registry.domain_of("squat").unwrap().is_none());
        // And housekeeping lets an old one go by itself.
        assert!(registry.claim_domain("squat", "other.example.org", "t4", 0).unwrap());
        assert_eq!(registry.expire_domain_claims(UNPROVED_FOR + 1).unwrap(), 1, "the day-old one, not brand's fresh one");
        assert!(registry.domain_of("squat").unwrap().is_none());
        assert!(registry.domain_of("brand").unwrap().is_some());
        // A proved one is never taken.
        assert!(registry.claim_domain("brand", "bigbrand.com", "t5", 0).unwrap());
        registry.domain_proved("brand", 1).unwrap();
        assert!(!registry.claim_domain("squat", "bigbrand.com", "t6", 10 * UNPROVED_FOR).unwrap());
    }
}
