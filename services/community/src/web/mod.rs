//! The web view (community/1.0's "On the web"): the community's page and the
//! channels its admins make public, server-rendered, at an address of the
//! server's choosing; signing in to it with website-sign-in/1.0; and the
//! community as an OpenID Connect provider (community-sign-in/1.0).
//!
//! - [`pages`]: the pages, permalinks, sitemap, theme and what they show.
//! - [`render`]: posts' Markdown as HTML that runs nothing, and dates.
//! - [`sign_in`]: signing in, and sessions.
//! - [`oidc`]: the identity provider.
//! - [`flows`]: the admin's `web` workflow and a member's `signed-in-sites`.
//! - [`store`]: what all of it keeps.
//!
//! **At any address.** `[web] public_url` may be the server's own address, a path
//! of it, or another origin altogether, behind a proxy that forwards that path
//! with its prefix or without. Every link inside a page is relative, and every
//! absolute address (canonical links, the sitemap, the issuer) is made from
//! `public_url`, never from a request's `Host` or forwarded headers.
//!
//! **Hardened.** Every page forbids framing and has a content security policy
//! that lets nothing load from elsewhere and no script run but the server's own
//! file; the pages that read the session send no CORS headers; forms carry a
//! token and every `POST` has its `Origin` checked.

pub mod flows;
pub mod oidc;
pub mod outside;
pub mod pages;
pub mod posting;
pub mod preview;
pub mod render;
pub mod sign_in;
pub mod store;

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;
use sha2::{Digest, Sha256};

use crate::config::{Config, WebConfig};
use crate::server::AppState;
use crate::store::Store;

/// Settings, in the community's store.
pub const PAGE: &str = "web.page";
pub const VOICE: &str = "web.voice";
/// Which channels are public: `everyone`, every channel everyone may see (as
/// long as they may), or `chosen`, those an admin chose (`web_channels`).
pub const PUBLIC: &str = "web.public";
pub const PUBLIC_CHOICES: [&str; 2] = ["everyone", "chosen"];
pub const THEME: &str = "web.theme";
/// Whether public posts' pictures from other sites are shown, through the server
/// (`set-web`'s `outside_pictures`): `1`, or off.
pub const OUTSIDE: &str = "web.outside_pictures";
pub const CSS: &str = "web.css";
pub const DYNAMIC: &str = "oidc.dynamic";
pub const INDIEAUTH: &str = "oidc.indieauth";
pub(crate) const SECRET: &str = "web.secret";

/// Who is shown who is in voice: `off`, `members`, `counts` or `names`.
pub const VOICE_CHOICES: [&str; 4] = ["off", "members", "counts", "names"];
pub const DEFAULT_VOICE: &str = "counts";

/// The CSP every page gets unless it sets its own. Pictures come from this
/// server, except KLIPY's GIFs, which its terms require visitors to fetch from it
/// ([`outside`]).
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: https://static.klipy.com https://static1.klipy.com https://static2.klipy.com; media-src 'self'; font-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// A response is a page of the web view, which the outer layer gives its headers
/// (and takes CORS off).
#[derive(Clone, Copy)]
pub struct WebPage;

/// A response of the identity provider's that any origin may read, without
/// credentials.
#[derive(Clone, Copy)]
pub struct OpenToAll;

/// The web view of one community: where it is, and what it holds in memory.
pub struct Web {
    pub config: WebConfig,
    /// The server's own address (`[server] public_url`), where its files are.
    pub server_url: String,
    /// Its address, ending in `/`.
    pub url: url::Url,
    /// Its path, ending in `/`: `/` at the root of its origin.
    pub path: String,
    pub sign_in: sign_in::Signing,
    limits: Mutex<HashMap<IpAddr, (i64, u32)>>,
}

impl Web {
    /// The web view `config` asks for, if the server shows one.
    pub fn new(config: &Config) -> anyhow::Result<Option<Self>> {
        if !config.web.enabled {
            return Ok(None);
        }
        let mut address = config.web.public_url.clone().filter(|url| !url.trim().is_empty()).unwrap_or_else(|| config.server.public_url.clone());
        if !address.ends_with('/') {
            address.push('/');
        }
        let url = url::Url::parse(address.trim()).map_err(|e| anyhow::anyhow!("[web] public_url {address:?}: {e}"))?;
        if !matches!(url.scheme(), "https" | "http") || url.query().is_some() || url.fragment().is_some() {
            anyhow::bail!("[web] public_url is an http(s) address with a path and nothing after it");
        }
        Ok(Some(Self { server_url: config.server.public_url.clone(), path: url.path().to_string(), url, config: config.web.clone(), sign_in: sign_in::Signing::default(), limits: Mutex::new(HashMap::new()) }))
    }

    /// An absolute address inside the web view: `relative` is a path from its root,
    /// without a leading `/`.
    pub fn absolute(&self, relative: &str) -> String {
        format!("{}{}", self.url.as_str(), relative.trim_start_matches('/'))
    }

    /// Its address without the final `/`: the identity provider's issuer.
    pub fn issuer(&self) -> String {
        self.url.as_str().trim_end_matches('/').to_string()
    }

    /// Its origin, as a browser says it in `Origin`.
    pub fn origin(&self) -> String {
        self.url.origin().ascii_serialization()
    }

    /// Whether `Secure` belongs on its cookies (always, but for a plain-HTTP test).
    pub fn secure(&self) -> bool {
        self.url.scheme() == "https"
    }

    /// Whether `client` may make another request this minute.
    fn allowed(&self, client: IpAddr, now: i64) -> bool {
        let mut limits = self.limits.lock().unwrap_or_else(|p| p.into_inner());
        let minute = now / 60;
        if limits.len() > 10_000 {
            limits.retain(|_, (at, _)| *at == minute);
        }
        let entry = limits.entry(client).or_insert((minute, 0));
        if entry.0 != minute {
            *entry = (minute, 0);
        }
        entry.1 += 1;
        entry.1 <= self.config.requests_per_minute.max(1)
    }
}


/// HMAC-SHA256 of `message` under `key`, hex: pairwise ids and picture addresses.
pub fn keyed(key: &str, message: &str) -> String {
    let mut block = [0u8; 64];
    let key = key.as_bytes();
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let inner: Vec<u8> = block.iter().map(|b| b ^ 0x36).collect();
    let outer: Vec<u8> = block.iter().map(|b| b ^ 0x5c).collect();
    let inner_hash = Sha256::new().chain_update(&inner).chain_update(message.as_bytes()).finalize();
    let mac = Sha256::new().chain_update(&outer).chain_update(inner_hash).finalize();
    mac.iter().map(|b| format!("{b:02x}")).collect()
}

// --- The community's settings for it -----------------------------------------------

/// For a community made now (nothing in its store yet), the page is on; for one
/// that existed before the web view did, off until an admin turns it on. Called
/// once at opening, before anyone is let in.
pub fn settle_new(store: &Store) -> anyhow::Result<()> {
    let fresh = store.member_count()? == 0 && store.channels()?.is_empty();
    if store.setting(PAGE)?.is_none() {
        store.set_setting(PAGE, if fresh { "1" } else { "0" })?;
    }
    // Every channel everyone may see is public, for a community made now (asked
    // 2026-11-17); one from before shows those its admins chose, until told.
    if store.setting(PUBLIC)?.is_none() {
        store.set_setting(PUBLIC, if fresh { "everyone" } else { "chosen" })?;
    }
    Ok(())
}

pub fn public_choice(store: &Store) -> String {
    store.setting(PUBLIC).ok().flatten().filter(|v| PUBLIC_CHOICES.contains(&v.as_str())).unwrap_or_else(|| "chosen".to_string())
}

/// The channels public on the web, each with when from (0 for its whole
/// history): those an admin chose, and with `everyone`, every open text channel
/// everyone may see -- each for as long as they may.
pub fn public_channels(community: &crate::community::Community) -> anyhow::Result<std::collections::HashMap<String, i64>> {
    let mut public = community.store.web_channels()?;
    if public_choice(&community.store) == "everyone" {
        for channel in community.store.channels()? {
            if !channel.closed && channel.voice.is_none() && community.everyone_sees(&channel)? {
                public.entry(channel.id).or_insert(0);
            }
        }
    }
    Ok(public)
}

pub fn page_on(store: &Store) -> bool {
    store.setting(PAGE).ok().flatten().as_deref() == Some("1")
}

pub fn outside_pictures(store: &Store) -> bool {
    store.setting(OUTSIDE).ok().flatten().as_deref() == Some("1")
}

pub fn voice_choice(store: &Store) -> String {
    store.setting(VOICE).ok().flatten().filter(|v| VOICE_CHOICES.contains(&v.as_str())).unwrap_or_else(|| DEFAULT_VOICE.to_string())
}

/// The community's own secret for the web view: pairwise ids, picture addresses.
/// Kept sealed with the service's seal key where it has one, bound to the
/// community (`sealing`: its keys, which go with it wherever it is moved, WYV-124)
/// (security review WYV-32).
pub fn secret(store: &Store, sealing: &crate::sealing::Sealing) -> anyhow::Result<String> {
    const WHAT: &str = "web-secret";
    if let Some(kept) = store.setting(SECRET)? {
        let (secret, again) = sealing.open(&kept, WHAT)?;
        if again {
            store.set_setting(SECRET, &sealing.seal(&secret, WHAT)?)?;
        }
        return Ok(secret);
    }
    store.set_setting(SECRET, &sealing.seal(&store::secret(), WHAT)?)?;
    let kept = store.setting(SECRET)?.unwrap_or_default();
    Ok(sealing.open(&kept, WHAT)?.0)
}

/// The web secret, where one is kept, opened and sealed again where it should be:
/// at start-up, beside the identity, so the start that adopts unsealed keys
/// adopts it too, and one that doesn't open stops the server (WYV-111).
pub fn settle_secret(store: &Store, sealing: &crate::sealing::Sealing) -> anyhow::Result<()> {
    if store.setting(SECRET)?.is_some() {
        secret(store, sealing).map_err(|e| e.context("the community's web secret"))?;
    }
    Ok(())
}

/// `community`'s `web`, for a server with a web view.
pub fn community_body(web_url: &str, store: &Store) -> serde_json::Value {
    let mut body = serde_json::json!({ "url": web_url, "page": page_on(store), "voice": voice_choice(store), "outside_pictures": outside_pictures(store), "public_channels": public_choice(store) });
    if page_on(store) {
        body["sign_in"] = serde_json::json!(web_url.trim_end_matches('/'));
    }
    body
}

/// A channel's name as it goes in its address: letters and digits as they are,
/// anything else as `-`.
pub fn slug(name: &str) -> String {
    let slug: String = name.chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '.' { c } else { '-' }).collect();
    let slug = slug.trim_matches('-').to_string();
    let encoded: String = url::form_urlencoded::byte_serialize(slug.as_bytes()).collect();
    if encoded.is_empty() { "-".to_string() } else { encoded }
}

/// A channel's page, from the web view's root.
pub fn channel_path(id: &str, name: &str) -> String {
    format!("ch/{}/{}", url::form_urlencoded::byte_serialize(id.as_bytes()).collect::<String>(), slug(name))
}

// --- Serving ---------------------------------------------------------------------------

/// The web view's routes, for [`crate::server::router`] to merge: at the root,
/// for a proxy that takes the web view's path off, and under that path too, for
/// one that doesn't.
pub fn router(state: Arc<AppState>, web: &Web) -> Router {
    let routes = || pages::routes().merge(posting::routes()).merge(sign_in::routes()).merge(oidc::routes());
    let mut router = routes();
    let prefix = web.path.trim_end_matches('/');
    if !prefix.is_empty() {
        // At its path without the `/`, a page's links would go a level up: to the path with it.
        let bare = format!("{}/", prefix.rsplit('/').next().unwrap_or_default());
        router = router.nest(prefix, routes()).route(prefix, axum::routing::get(move || async move { as_page(axum::response::Redirect::permanent(&bare).into_response()) })).route(&format!("{prefix}/"), axum::routing::get(pages::home));
    }
    router.layer(axum::middleware::from_fn_with_state(state.clone(), guard)).with_state(state)
}

/// Before any route of the web view: there has to be one, the rate limit holds,
/// and a `POST` comes from the web view's own pages (or is the identity
/// provider's, which websites' servers call).
async fn guard(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let Some(web) = &state.web else { return StatusCode::NOT_FOUND.into_response() };
    // By the server's trusted proxies, an IPv6 client by its /64 (WYV-98, WYV-106).
    if let Some(client) = state.clients.of(&request) {
        if !web.allowed(client, wyvrn_service::now()) {
            return page_error(StatusCode::TOO_MANY_REQUESTS, "Slow down", "Too many requests from here just now. Try again in a minute.");
        }
    }
    let path = request.uri().path().to_string();
    let open = path.contains("/oauth/") || path.ends_with("/.well-known/openid-configuration") || path.ends_with("/.well-known/oauth-authorization-server");
    if request.method() == Method::POST && !open && !same_origin(web, request.headers()) {
        return page_error(StatusCode::FORBIDDEN, "Not from here", "That came from another website, so it was refused.");
    }
    next.run(request).await
}

/// Whether a `POST` came from the web view's own origin: `Origin`, or failing
/// that `Referer`, and a browser that sends neither is refused.
pub fn same_origin(web: &Web, headers: &HeaderMap) -> bool {
    let own = web.origin();
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        return origin == own;
    }
    headers.get(header::REFERER).and_then(|v| v.to_str().ok()).and_then(|r| url::Url::parse(r).ok()).is_some_and(|r| r.origin().ascii_serialization() == own)
}

/// Outermost, around the whole server: pages get their security headers and
/// lose the DIDComm endpoint's CORS; the identity provider's open endpoints get
/// theirs, without credentials.
pub async fn headers(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let page = response.extensions().get::<WebPage>().is_some();
    let open = response.extensions().get::<OpenToAll>().is_some();
    if !page && !open {
        return response;
    }
    let hsts = state.web.as_ref().is_some_and(|web| web.config.hsts);
    let headers = response.headers_mut();
    let cors: Vec<_> = headers.keys().filter(|name| name.as_str().starts_with("access-control-")).cloned().collect();
    for name in cors {
        headers.remove(name);
    }
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("strict-origin-when-cross-origin"));
    headers.insert("cross-origin-resource-policy", HeaderValue::from_static(if open { "cross-origin" } else { "same-origin" }));
    if page {
        headers.entry(header::CONTENT_SECURITY_POLICY).or_insert(HeaderValue::from_static(CSP));
        headers.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
    } else {
        headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"));
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("authorization, content-type"));
        headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST, OPTIONS"));
    }
    if hsts {
        headers.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
    }
    response
}

/// Marks a response as a page.
pub fn as_page(mut response: Response) -> Response {
    response.extensions_mut().insert(WebPage);
    response
}

/// A page saying something went wrong, with a status.
pub fn page_error(status: StatusCode, title: &str, said: &str) -> Response {
    let mut response = pages::message_page(status, title, said);
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_is_hmac() {
        // RFC 4231, test case 2.
        assert_eq!(keyed("Jefe", "what do ya want for nothing?"), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn slugs_keep_letters_and_take_the_rest_out() {
        assert_eq!(slug("design-review"), "design-review");
        assert_eq!(slug("にほんご"), "%E3%81%AB%E3%81%BB%E3%82%93%E3%81%94");
        assert_eq!(slug("Lounge & Chat!"), "Lounge---Chat");
        assert_eq!(slug("!!!"), "-");
    }
}
