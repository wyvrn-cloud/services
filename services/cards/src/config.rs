//! The service's settings, all from the environment and all optional.

/// What the service is told.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where to listen (`CARDS_BIND`).
    pub bind: String,
    /// What the links' site is called, on the card and in its tags (`CARDS_SITE_NAME`).
    pub site_name: String,
    /// Where the links point, if that isn't where a request says it came to
    /// (`CARDS_PUBLIC_URL`): `https://chat.example`. Unset, the proxy in front is
    /// taken at its word (`X-Forwarded-Proto`, `Host`).
    pub public_url: Option<String>,
    /// The DID host whose `official:` names are marked official (`CARDS_DID_HOST`).
    pub did_host: String,
    /// The domains whose own `did:web` is marked official
    /// (`CARDS_OFFICIAL_DOMAINS`, separated by commas). Unset, the DID host.
    pub official_domains: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self { bind: "0.0.0.0:8080".into(), site_name: "Wyvrn".into(), public_url: None, did_host: "id.wyvrn.app".into(), official_domains: vec!["id.wyvrn.app".into()] }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let env = |name: &str| std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let defaults = Self::default();
        let did_host = env("CARDS_DID_HOST").unwrap_or(defaults.did_host).to_lowercase();
        let official_domains = match env("CARDS_OFFICIAL_DOMAINS") {
            Some(listed) => listed.split(',').map(|domain| domain.trim().to_lowercase()).filter(|domain| !domain.is_empty()).collect(),
            None => vec![did_host.clone()],
        };
        Self {
            bind: env("CARDS_BIND").unwrap_or(defaults.bind),
            site_name: env("CARDS_SITE_NAME").unwrap_or(defaults.site_name),
            public_url: env("CARDS_PUBLIC_URL").map(|url| url.trim_end_matches('/').to_string()),
            did_host,
            official_domains,
        }
    }
}
