//! The community as an OpenID Connect provider (community-sign-in/1.0): the
//! authorization code flow with PKCE (S256) and nothing else; ES256 id tokens
//! signed with a key of the community's own; pairwise `sub`; consent remembered
//! per website and set of scopes; websites registered by an admin, by themselves
//! (RFC 7591) or known by their address (IndieAuth), the last two off until an
//! admin turns them on, and shown as unverified.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use askama::Template;
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use wyvrn_service::now;
use wyvrn_sign_in::keys::SecretKey;

use super::pages::{site, Site, Viewer};
use super::sign_in::{form_ok, private_page};
use super::store::{Client, Code, Token};
use super::{as_page, keyed, page_error, page_on, OpenToAll, Web};
use crate::server::AppState;
use crate::store::Member;

pub const SCOPES: [&str; 4] = ["openid", "did", "profile", "roles"];
const CODE_FOR: i64 = 60;
const TOKEN_FOR: i64 = 3600;
const ID_TOKEN_FOR: i64 = 600;
/// A new signing key at least this often; the one before stays published a day.
const KEY_FOR: i64 = 90 * 24 * 3600;
const OLD_KEY_KEPT: i64 = 24 * 3600;
/// How many websites may register themselves from one address in an hour.
const REGISTRATIONS_PER_HOUR: i64 = 5;
const MAX_REDIRECTS: usize = 10;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/.well-known/oauth-authorization-server", get(discovery))
        .route("/oauth/authorize", get(authorize).post(decide))
        .route("/oauth/token", post(token).options(preflight))
        .route("/oauth/userinfo", get(userinfo).post(userinfo).options(preflight))
        .route("/oauth/jwks", get(jwks))
        .route("/oauth/revoke", post(revoke).options(preflight))
        .route("/oauth/register", post(register).options(preflight))
        .route("/sites", get(sites))
        .route("/sites/revoke", post(revoke_site))
}

fn open(mut response: Response) -> Response {
    response.extensions_mut().insert(OpenToAll);
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn preflight() -> Response {
    open(StatusCode::NO_CONTENT.into_response())
}

/// An OAuth error, as JSON.
fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    open((status, Json(json!({ "error": error, "error_description": description }))).into_response())
}

fn on(state: &AppState) -> Option<&Web> {
    state.web.as_deref().filter(|_| page_on(&state.community.store))
}

fn setting_on(state: &AppState, name: &str) -> bool {
    state.community.store.setting(name).ok().flatten().as_deref() == Some("1")
}

// --- Discovery and keys -------------------------------------------------------------

async fn discovery(State(state): State<Arc<AppState>>) -> Response {
    let Some(web) = on(&state) else { return oauth_error(StatusCode::NOT_FOUND, "not_found", "There's no identity provider here.") };
    let issuer = web.issuer();
    let mut document = json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "userinfo_endpoint": format!("{issuer}/oauth/userinfo"),
        "jwks_uri": format!("{issuer}/oauth/jwks"),
        "revocation_endpoint": format!("{issuer}/oauth/revoke"),
        "scopes_supported": SCOPES,
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code"],
        "subject_types_supported": ["pairwise"],
        "id_token_signing_alg_values_supported": ["ES256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
        "revocation_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
        "code_challenge_methods_supported": ["S256"],
        "claims_supported": ["iss", "sub", "aud", "exp", "iat", "nonce", "did", "name", "picture", "roles"],
        "service_documentation": "https://wyvrn-cloud.github.io/protocols/profiles/community-sign-in/1.0/",
    });
    if setting_on(&state, super::DYNAMIC) {
        document["registration_endpoint"] = json!(format!("{issuer}/oauth/register"));
    }
    if setting_on(&state, super::INDIEAUTH) {
        document["client_id_metadata_document_supported"] = json!(true);
    }
    let mut response = open(Json(document).into_response());
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
    response
}

/// The key id tokens are signed with now, made (and the old ones let go) as they
/// come due.
fn signing_key(state: &AppState) -> anyhow::Result<(String, SecretKey)> {
    let store = &state.community.store;
    let at = now();
    let keys = store.oidc_keys()?;
    let current = keys.first().filter(|(_, _, made)| *made > at - KEY_FOR).cloned();
    let (kid, jwk) = match current {
        Some((kid, jwk, _)) => (kid, jwk),
        None => {
            let key = SecretKey::generate_p256();
            let kid = uuid::Uuid::new_v4().simple().to_string()[..16].to_string();
            let jwk = key.to_jwk().to_string();
            store.add_oidc_key(&kid, &jwk, at)?;
            (kid, jwk)
        }
    };
    // A key replaced more than a day ago is published no longer.
    let mut newer_made: Option<i64> = None;
    for (old, _, made) in store.oidc_keys()? {
        if old != kid && newer_made.is_some_and(|newer| newer < at - OLD_KEY_KEPT) {
            store.remove_oidc_key(&old)?;
        }
        newer_made = Some(made);
    }
    Ok((kid, SecretKey::from_jwk(&serde_json::from_str(&jwk)?)?))
}

async fn jwks(State(state): State<Arc<AppState>>) -> Response {
    if on(&state).is_none() {
        return oauth_error(StatusCode::NOT_FOUND, "not_found", "There's no identity provider here.");
    }
    let _ = signing_key(&state);
    let keys: Vec<Value> = state
        .community
        .store
        .oidc_keys()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(kid, jwk, _)| {
            let key = SecretKey::from_jwk(&serde_json::from_str(&jwk).ok()?).ok()?;
            let mut public = key.public().to_jwk();
            public["kid"] = json!(kid);
            public["use"] = json!("sig");
            public["alg"] = json!("ES256");
            Some(public)
        })
        .collect();
    let mut response = open(Json(json!({ "keys": keys })).into_response());
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=600"));
    response
}

// --- Websites ------------------------------------------------------------------------

/// The website a `client_id` names: one registered (by an admin or itself), or
/// one known by its address, its metadata fetched, where an admin allows those.
async fn client_named(state: &AppState, client_id: &str) -> Option<Client> {
    if let Some(client) = state.community.store.client(client_id).ok().flatten() {
        if client.kind == "dynamic" && !setting_on(state, super::DYNAMIC) {
            return None;
        }
        return Some(client);
    }
    if client_id.starts_with("https://") && setting_on(state, super::INDIEAUTH) {
        return indieauth_client(client_id).await;
    }
    None
}

/// Where pairwise ids are made for: the host of a website's redirect addresses
/// (or of its own address, for one known by it).
fn sector(client: &Client) -> String {
    let host = |u: &str| url::Url::parse(u).ok().and_then(|u| u.host_str().map(str::to_string));
    if client.kind == "indieauth" {
        return host(&client.id).unwrap_or_else(|| client.id.clone());
    }
    let hosts: BTreeSet<String> = client.redirect_uris.iter().filter_map(|u| host(u)).collect();
    if hosts.len() == 1 { hosts.into_iter().next().unwrap_or_default() } else { client.id.clone() }
}

/// What a website may be sent back to: `https:`, or `http:` to the machine
/// itself, and nothing after a `#`.
pub fn redirect_allowed(uri: &str) -> bool {
    let Ok(parsed) = url::Url::parse(uri) else { return false };
    if parsed.fragment().is_some() {
        return false;
    }
    match parsed.scheme() {
        "https" => parsed.host_str().is_some(),
        "http" => matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")) || parsed.host_str().is_some_and(|h| h.ends_with(".localhost")),
        _ => false,
    }
}

type Cached = HashMap<String, (i64, Option<Client>)>;
static INDIEAUTH_CACHE: Mutex<Option<Cached>> = Mutex::new(None);

/// A website known by its address: its client metadata document, fetched from a
/// public address, small, soon and without redirects elsewhere, and kept a day.
async fn indieauth_client(client_id: &str) -> Option<Client> {
    let at = now();
    if let Some((fetched, client)) = INDIEAUTH_CACHE.lock().unwrap_or_else(|p| p.into_inner()).as_ref().and_then(|c| c.get(client_id)).cloned() {
        if fetched > at - 24 * 3600 {
            return client;
        }
    }
    let client = fetch_metadata(client_id).await.unwrap_or_else(|why| {
        tracing::info!(client_id, %why, "a website known by its address couldn't be read");
        None
    });
    let mut cache = INDIEAUTH_CACHE.lock().unwrap_or_else(|p| p.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    if cache.len() > 1000 {
        cache.clear();
    }
    cache.insert(client_id.to_string(), (at, client.clone()));
    client
}

pub(super) fn public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => !(v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast() || v4.is_documentation() || v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]) || v4.octets()[0] >= 224),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public_address(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback() || v6.is_unspecified() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80 || v6.is_multicast())
        }
    }
}

async fn fetch_metadata(client_id: &str) -> Result<Option<Client>, String> {
    let parsed = url::Url::parse(client_id).map_err(|e| e.to_string())?;
    if parsed.scheme() != "https" || parsed.fragment().is_some() || !parsed.username().is_empty() || parsed.password().is_some() {
        return Ok(None);
    }
    let host = parsed.host_str().ok_or("no host")?.to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);
    // Looked up once, and only a public address used: never this machine's network.
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port)).await.map_err(|e| e.to_string())?.collect();
    let Some(address) = addresses.iter().find(|a| public_address(a.ip())).copied() else { return Err("not a public address".into()) };
    if addresses.iter().any(|a| !public_address(a.ip())) {
        return Err("it also names a private address".into());
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(5)).redirect(reqwest::redirect::Policy::none()).resolve(&host, address).build().map_err(|e| e.to_string())?;
    let mut response = client.get(client_id).header(header::ACCEPT, "application/json").send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        body.extend_from_slice(&chunk);
        if body.len() > 64 * 1024 {
            return Err("over 64 KiB".into());
        }
    }
    let metadata: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    if metadata["client_id"].as_str().is_some_and(|id| id != client_id) {
        return Err("its client_id isn't its address".into());
    }
    let origin = parsed.origin().ascii_serialization();
    let mut redirect_uris: Vec<String> = metadata["redirect_uris"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|u| redirect_allowed(u)).take(MAX_REDIRECTS).map(str::to_string).collect();
    if redirect_uris.is_empty() {
        redirect_uris = Vec::new();
    }
    let name = metadata["client_name"].as_str().map(|n| n.chars().filter(|c| !c.is_control()).take(80).collect::<String>()).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| host.clone());
    Ok(Some(Client { id: client_id.to_string(), kind: "indieauth".into(), name, secret: None, redirect_uris, created: now(), created_by: Some(origin) }))
}

/// Whether `uri` is one `client` may be sent back to: exactly one of its own, or
/// for one known by its address and listing none, on its own origin.
fn redirect_of(client: &Client, uri: &str) -> bool {
    if client.redirect_uris.iter().any(|u| u == uri) {
        return true;
    }
    client.kind == "indieauth"
        && client.redirect_uris.is_empty()
        && redirect_allowed(uri)
        && url::Url::parse(uri).ok().map(|u| u.origin().ascii_serialization()) == url::Url::parse(&client.id).ok().map(|u| u.origin().ascii_serialization())
}

// --- Authorization -----------------------------------------------------------------------

#[derive(Deserialize, Clone, Default)]
struct Asking {
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    redirect_uri: String,
    #[serde(default)]
    response_type: String,
    #[serde(default)]
    scope: String,
    state: Option<String>,
    nonce: Option<String>,
    #[serde(default)]
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: String,
    prompt: Option<String>,
}

/// Sends the browser back to the website with `fields` (an error, or the code).
fn back_to(redirect_uri: &str, fields: &[(&str, &str)], state: Option<&str>) -> Response {
    let Ok(mut url) = url::Url::parse(redirect_uri) else { return page_error(StatusCode::BAD_REQUEST, "Can't go back", "That website's address isn't one to go back to.") };
    {
        let mut query = url.query_pairs_mut();
        for (name, value) in fields {
            query.append_pair(name, value);
        }
        if let Some(state) = state {
            query.append_pair("state", state);
        }
    }
    let mut response = (StatusCode::SEE_OTHER, [(header::LOCATION, url.as_str().to_string())]).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    as_page(response)
}

/// What a request asks, checked: the website, where to go back to, and the
/// scopes it may be given. An error before the website and its address are known
/// is shown here, never sent anywhere; one after goes back to it.
#[allow(clippy::result_large_err)] // a page to show, made once per request
async fn checked(state: &AppState, asking: &Asking) -> Result<(Client, Vec<String>), Response> {
    let Some(client) = client_named(state, &asking.client_id).await else {
        return Err(page_error(StatusCode::BAD_REQUEST, "Unknown website", "The website that sent you here isn't one this community knows."));
    };
    if !redirect_of(&client, &asking.redirect_uri) {
        return Err(page_error(StatusCode::BAD_REQUEST, "Wrong address", "The website asked to send you back somewhere it didn't register, so you've been stopped here."));
    }
    let back = |error: &str, description: &str| back_to(&asking.redirect_uri, &[("error", error), ("error_description", description), ("iss", &state.web.as_ref().map(|w| w.issuer()).unwrap_or_default())], asking.state.as_deref());
    if asking.response_type != "code" {
        return Err(back("unsupported_response_type", "Only the code flow is supported."));
    }
    if asking.code_challenge_method != "S256" || asking.code_challenge.len() < 43 || asking.code_challenge.len() > 128 {
        return Err(back("invalid_request", "PKCE with S256 is required."));
    }
    let scopes: Vec<String> = SCOPES.iter().filter(|s| asking.scope.split_whitespace().any(|w| w == **s)).map(|s| s.to_string()).collect();
    if !scopes.iter().any(|s| s == "openid") {
        return Err(back("invalid_scope", "The openid scope is required."));
    }
    Ok((client, scopes))
}

fn query_of(asking: &Asking) -> String {
    let mut out = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in [("client_id", Some(&asking.client_id)), ("redirect_uri", Some(&asking.redirect_uri)), ("response_type", Some(&asking.response_type)), ("scope", Some(&asking.scope)), ("state", asking.state.as_ref()), ("nonce", asking.nonce.as_ref()), ("code_challenge", Some(&asking.code_challenge)), ("code_challenge_method", Some(&asking.code_challenge_method))] {
        if let Some(value) = value {
            out.append_pair(name, value);
        }
    }
    out.finish()
}

#[derive(Template)]
#[template(path = "web/consent.html")]
struct ConsentPage {
    site: Site,
    title: String,
    client_name: String,
    client_address: Option<String>,
    verified: bool,
    tells: Vec<String>,
    fields: Vec<(String, String)>,
}

fn tells(scope: &str) -> &'static str {
    match scope {
        "openid" => "That you're a member of this community",
        "did" => "Your DID, which lets it message you in Wyvrn",
        "profile" => "Your name and picture",
        "roles" => "Your roles in this community",
        _ => "",
    }
}

async fn authorize(State(state): State<Arc<AppState>>, Query(asking): Query<Asking>, headers: HeaderMap) -> Response {
    let Some(web) = on(&state) else { return page_error(StatusCode::NOT_FOUND, "Not here", "There's nothing at this address.") };
    let (client, scopes) = match checked(&state, &asking).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    let viewer = Viewer::of(&state, &headers);
    let prompt_none = asking.prompt.as_deref() == Some("none");
    let Some(member) = viewer.member.clone() else {
        if prompt_none {
            return back_to(&asking.redirect_uri, &[("error", "login_required"), ("iss", &web.issuer())], asking.state.as_deref());
        }
        let back = format!("oauth/authorize?{}", query_of(&asking));
        let to = format!("../sign-in?return_to={}", url::form_urlencoded::byte_serialize(back.as_bytes()).collect::<String>());
        return as_page(Redirect::to(&to).into_response());
    };
    let store = &state.community.store;
    let mkey = match store.member_key(&member.did) {
        Ok(mkey) => mkey,
        Err(error) => return page_error(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong", &error.to_string()),
    };
    let allowed: BTreeSet<String> = store.consent(&mkey, &client.id).ok().flatten().unwrap_or_default().split_whitespace().map(str::to_string).collect();
    let covered = scopes.iter().all(|s| allowed.contains(s));
    if covered && asking.prompt.as_deref() != Some("consent") {
        return grant(&state, web, &client, &mkey, &scopes, &asking);
    }
    if prompt_none {
        return back_to(&asking.redirect_uri, &[("error", "consent_required"), ("iss", &web.issuer())], asking.state.as_deref());
    }
    let mut fields: Vec<(String, String)> = url::form_urlencoded::parse(query_of(&asking).as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    fields.push(("csrf".to_string(), viewer.csrf.clone().unwrap_or_default()));
    let page = ConsentPage {
        site: site(&state, &viewer, "../", web.absolute("oauth/authorize")),
        title: format!("Sign in to {}", client.name),
        client_address: (!client.verified()).then(|| client.redirect_uris.first().cloned().or_else(|| Some(client.id.clone())).and_then(|u| url::Url::parse(&u).ok()).map(|u| u.origin().ascii_serialization())).flatten(),
        client_name: client.name.clone(),
        verified: client.verified(),
        tells: scopes.iter().map(|s| tells(s).to_string()).collect(),
        fields,
    };
    let mut response = private_page(page.render());
    // The answer is sent back to the website: let the form go there after its redirect.
    let origin = url::Url::parse(&asking.redirect_uri).map(|u| u.origin().ascii_serialization()).unwrap_or_default();
    let csp = super::CSP.replace("form-action 'self'", &format!("form-action 'self' {origin}"));
    if let Ok(value) = HeaderValue::from_str(&csp) {
        response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, value);
    }
    response
}

#[derive(Deserialize)]
struct Decision {
    #[serde(flatten)]
    asking: Asking,
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    decision: String,
}

/// The consent page's answer.
async fn decide(State(state): State<Arc<AppState>>, headers: HeaderMap, Form(decision): Form<Decision>) -> Response {
    let Some(web) = on(&state) else { return page_error(StatusCode::NOT_FOUND, "Not here", "There's nothing at this address.") };
    let Some(member) = form_ok(&state, &headers, &decision.csrf) else {
        return page_error(StatusCode::FORBIDDEN, "Sign in again", "Your session ended, or that form wasn't from here. Go back to the website and try again.");
    };
    let asking = decision.asking;
    let (client, scopes) = match checked(&state, &asking).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    if decision.decision != "allow" {
        return back_to(&asking.redirect_uri, &[("error", "access_denied"), ("iss", &web.issuer())], asking.state.as_deref());
    }
    let store = &state.community.store;
    let Ok(mkey) = store.member_key(&member.did) else { return page_error(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong", "Try again.") };
    if let Err(error) = store.set_consent(&mkey, &client.id, &scopes.join(" "), now()) {
        return page_error(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong", &error.to_string());
    }
    tracing::info!(client = %client.id, member = %member.did, scopes = %scopes.join(" "), "a member allowed a website");
    grant(&state, web, &client, &mkey, &scopes, &asking)
}

/// A code for the website, and back to it.
fn grant(state: &AppState, web: &Web, client: &Client, mkey: &str, scopes: &[String], asking: &Asking) -> Response {
    let code = super::store::secret();
    let held = Code { mkey: mkey.to_string(), client: client.id.clone(), redirect_uri: asking.redirect_uri.clone(), scopes: scopes.join(" "), nonce: asking.nonce.clone(), challenge: asking.code_challenge.clone() };
    if let Err(error) = state.community.store.add_code(&code, &held, now() + CODE_FOR) {
        return page_error(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong", &error.to_string());
    }
    back_to(&asking.redirect_uri, &[("code", &code), ("iss", &web.issuer())], asking.state.as_deref())
}

// --- Tokens --------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct TokenAsk {
    #[serde(default)]
    grant_type: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    redirect_uri: String,
    #[serde(default)]
    client_id: String,
    client_secret: Option<String>,
    #[serde(default)]
    code_verifier: String,
    #[serde(default)]
    token: String,
}

/// The website a token request is from, authenticated: by its secret (basic or
/// in the form), or, for one without a secret, by its id alone (PKCE carries it).
#[allow(clippy::result_large_err)]
async fn authenticated(state: &AppState, headers: &HeaderMap, form: &TokenAsk) -> Result<Client, Response> {
    let basic = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Basic ")).and_then(|b| base64::engine::general_purpose::STANDARD.decode(b.trim()).ok()).and_then(|b| String::from_utf8(b).ok()).and_then(|pair| {
        let (id, secret) = pair.split_once(':')?;
        let decode = |s: &str| url::form_urlencoded::parse(format!("x={s}").as_bytes()).next().map(|(_, v)| v.into_owned()).unwrap_or_default();
        Some((decode(id), decode(secret)))
    });
    let (id, secret) = match basic {
        Some((id, secret)) => (id, Some(secret)),
        None => (form.client_id.clone(), form.client_secret.clone()),
    };
    let bad = || oauth_error(StatusCode::UNAUTHORIZED, "invalid_client", "That website isn't known, or its secret is wrong.");
    let Some(client) = client_named(state, &id).await else { return Err(bad()) };
    match (&client.secret, secret) {
        (Some(hash), Some(given)) if super::sign_in::constant_eq(hash, &super::store::hashed(&given)) => Ok(client),
        (None, None) => Ok(client),
        (None, Some(given)) if given.is_empty() => Ok(client),
        _ => Err(bad()),
    }
}

async fn token(State(state): State<Arc<AppState>>, headers: HeaderMap, Form(form): Form<TokenAsk>) -> Response {
    let Some(web) = on(&state) else { return oauth_error(StatusCode::NOT_FOUND, "not_found", "There's no identity provider here.") };
    if form.grant_type != "authorization_code" {
        return oauth_error(StatusCode::BAD_REQUEST, "unsupported_grant_type", "Only authorization_code.");
    }
    let client = match authenticated(&state, &headers, &form).await {
        Ok(client) => client,
        Err(response) => return response,
    };
    let store = &state.community.store;
    let at = now();
    let Some(code) = store.take_code(&form.code, at).ok().flatten() else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "That code is unknown, used or too old.");
    };
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(form.code_verifier.as_bytes()));
    if code.client != client.id || code.redirect_uri != form.redirect_uri || form.code_verifier.len() < 43 || !super::sign_in::constant_eq(&challenge, &code.challenge) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "That code wasn't given for this.");
    }
    let Some(member) = store.member_by_key(&code.mkey).ok().flatten().filter(|m| store.banned(&m.did, at).ok().flatten().is_none()) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "They aren't a member any more.");
    };
    let access = super::store::secret();
    if let Err(error) = store.add_token(&access, &Token { mkey: code.mkey.clone(), client: client.id.clone(), scopes: code.scopes.clone() }, &form.code, at + TOKEN_FOR) {
        return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &error.to_string());
    }
    let mut payload = json!({ "iss": web.issuer(), "sub": sub(&state, &client, &code.mkey), "aud": client.id, "iat": at, "exp": at + ID_TOKEN_FOR });
    if let Some(nonce) = &code.nonce {
        payload["nonce"] = json!(nonce);
    }
    if let Value::Object(claims) = claims(&state, web, &client, &member, &code.mkey, &code.scopes) {
        for (name, value) in claims {
            payload[name] = value;
        }
    }
    let (kid, key) = match signing_key(&state) {
        Ok(found) => found,
        Err(error) => return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &error.to_string()),
    };
    let id_token = wyvrn_sign_in::jws::sign(&json!({ "alg": "ES256", "typ": "JWT", "kid": kid }), &payload, &key);
    let mut response = open(Json(json!({ "access_token": access, "token_type": "Bearer", "expires_in": TOKEN_FOR, "id_token": id_token, "scope": code.scopes })).into_response());
    response.headers_mut().insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

/// A member's id at a website: different at each, the same through changes of DID.
fn sub(state: &AppState, client: &Client, mkey: &str) -> String {
    let secret = super::secret(&state.community.store).unwrap_or_default();
    keyed(&secret, &format!("sub\n{}\n{mkey}", sector(client)))[..40].to_string()
}

/// The claims `scopes` give, beyond `sub`.
fn claims(state: &AppState, web: &Web, client: &Client, member: &Member, mkey: &str, scopes: &str) -> Value {
    let store = &state.community.store;
    let has = |s: &str| scopes.split_whitespace().any(|w| w == s);
    let mut claims = json!({});
    if has("did") {
        claims["did"] = json!(member.did);
    }
    if has("profile") {
        claims["name"] = json!(member.name.clone().unwrap_or_else(|| "A member".to_string()));
        if store.member_picture(&member.did).ok().flatten().is_some() {
            if let Ok(token) = super::secret(store).and_then(|secret| Ok(store.picture_token(mkey, &client.id, &secret)?)) {
                claims["picture"] = json!(web.absolute(&format!("p/{token}")));
            }
        }
    }
    if has("roles") {
        let held: BTreeSet<String> = store.member_roles(&member.did).unwrap_or_default().into_iter().collect();
        let roles: Vec<Value> = store.roles().unwrap_or_default().into_iter().filter(|r| held.contains(&r.id)).map(|r| json!({ "id": r.id, "name": r.name })).collect();
        claims["roles"] = json!(roles);
    }
    claims
}

async fn userinfo(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let Some(web) = on(&state) else { return oauth_error(StatusCode::NOT_FOUND, "not_found", "There's no identity provider here.") };
    let given = request.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(|t| t.trim().to_string());
    let unauthorized = || {
        let mut response = oauth_error(StatusCode::UNAUTHORIZED, "invalid_token", "That token is unknown, taken back or too old.");
        response.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer error=\"invalid_token\""));
        response
    };
    let Some(given) = given else { return unauthorized() };
    let store = &state.community.store;
    let at = now();
    let Some(held) = store.token(&given, at).ok().flatten() else { return unauthorized() };
    let Some(member) = store.member_by_key(&held.mkey).ok().flatten().filter(|m| store.banned(&m.did, at).ok().flatten().is_none()) else { return unauthorized() };
    let Some(client) = client_named(&state, &held.client).await else { return unauthorized() };
    let mut body = claims(&state, web, &client, &member, &held.mkey, &held.scopes);
    body["sub"] = json!(sub(&state, &client, &held.mkey));
    open(Json(body).into_response())
}

async fn revoke(State(state): State<Arc<AppState>>, headers: HeaderMap, Form(form): Form<TokenAsk>) -> Response {
    if on(&state).is_none() {
        return oauth_error(StatusCode::NOT_FOUND, "not_found", "There's no identity provider here.");
    }
    let client = match authenticated(&state, &headers, &form).await {
        Ok(client) => client,
        Err(response) => return response,
    };
    let store = &state.community.store;
    if store.token(&form.token, now()).ok().flatten().is_some_and(|t| t.client == client.id) {
        let _ = store.revoke_token(&form.token);
    }
    open(StatusCode::OK.into_response())
}

// --- Websites registering themselves ------------------------------------------------------------

#[derive(Deserialize)]
struct Registration {
    #[serde(default)]
    redirect_uris: Vec<String>,
    #[serde(default)]
    client_name: String,
    token_endpoint_auth_method: Option<String>,
}

async fn register(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let Some(web) = on(&state) else { return oauth_error(StatusCode::NOT_FOUND, "not_found", "There's no identity provider here.") };
    if !setting_on(&state, super::DYNAMIC) {
        return oauth_error(StatusCode::FORBIDDEN, "access_denied", "Websites can't register themselves here.");
    }
    let from = web.client(&request).map(|a| a.to_string()).unwrap_or_default();
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), 64 * 1024).await else { return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata", "Too long.") };
    let Ok(asked) = serde_json::from_slice::<Registration>(&bytes) else { return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata", "That isn't client metadata.") };
    let store = &state.community.store;
    let at = now();
    if store.clients_registered_since(&from, at - 3600).unwrap_or(0) >= REGISTRATIONS_PER_HOUR {
        return oauth_error(StatusCode::TOO_MANY_REQUESTS, "access_denied", "Too many websites registered from here just now.");
    }
    if asked.redirect_uris.is_empty() || asked.redirect_uris.len() > MAX_REDIRECTS || !asked.redirect_uris.iter().all(|u| redirect_allowed(u)) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_redirect_uri", "Redirect addresses are https:, or http: to localhost, with no #.");
    }
    let name: String = asked.client_name.chars().filter(|c| !c.is_control()).take(80).collect();
    if name.trim().is_empty() {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata", "A client_name is needed.");
    }
    let public = asked.token_endpoint_auth_method.as_deref() == Some("none");
    let secret = (!public).then(super::store::secret);
    let client = Client { id: format!("dyn-{}", uuid::Uuid::new_v4().simple()), kind: "dynamic".into(), name: name.trim().to_string(), secret: secret.as_deref().map(super::store::hashed), redirect_uris: asked.redirect_uris.clone(), created: at, created_by: Some(from) };
    if let Err(error) = store.add_client(&client) {
        return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &error.to_string());
    }
    tracing::info!(client = %client.id, name = %client.name, "a website registered itself");
    let mut body = json!({ "client_id": client.id, "client_name": client.name, "redirect_uris": client.redirect_uris, "client_id_issued_at": at, "token_endpoint_auth_method": if public { "none" } else { "client_secret_basic" }, "grant_types": ["authorization_code"], "response_types": ["code"] });
    if let Some(secret) = secret {
        body["client_secret"] = json!(secret);
        body["client_secret_expires_at"] = json!(0);
    }
    open((StatusCode::CREATED, Json(body)).into_response())
}

// --- A member's websites ---------------------------------------------------------------------

#[derive(Template)]
#[template(path = "web/sites.html")]
struct SitesPage {
    site: Site,
    title: String,
    sites: Vec<(String, String, bool, String, String)>,
}

/// The websites a member let sign them in, and taking any back.
async fn sites(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some(web) = on(&state) else { return page_error(StatusCode::NOT_FOUND, "Not here", "There's nothing at this address.") };
    let viewer = Viewer::of(&state, &headers);
    let Some(member) = viewer.member.clone() else { return as_page(Redirect::to("sign-in?return_to=sites").into_response()) };
    let store = &state.community.store;
    let mkey = store.member_key(&member.did).unwrap_or_default();
    let mut listed = Vec::new();
    for (client_id, scopes, at) in store.consents_of(&mkey).unwrap_or_default() {
        let client = client_named(&state, &client_id).await;
        let name = client.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| client_id.clone());
        let what = scopes.split_whitespace().map(tells).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("; ");
        listed.push((client_id, name, client.as_ref().is_some_and(Client::verified), what, super::render::when(at)));
    }
    private_page(SitesPage { site: site(&state, &viewer, "./", web.absolute("sites")), title: "Websites you sign in to".into(), sites: listed }.render())
}

#[derive(Deserialize)]
struct RevokeSite {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    client: String,
}

async fn revoke_site(State(state): State<Arc<AppState>>, headers: HeaderMap, Form(form): Form<RevokeSite>) -> Response {
    let Some(member) = form_ok(&state, &headers, &form.csrf) else { return page_error(StatusCode::FORBIDDEN, "Sign in again", "Your session ended, or that form wasn't from here.") };
    let store = &state.community.store;
    if let Ok(mkey) = store.member_key(&member.did) {
        let _ = store.revoke_consent(&mkey, &form.client);
    }
    as_page(Redirect::to("../sites").into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_are_https_or_this_machine() {
        assert!(redirect_allowed("https://wiki.example/cb"));
        assert!(redirect_allowed("http://localhost:3000/cb"));
        assert!(redirect_allowed("http://app.localhost/cb"));
        assert!(!redirect_allowed("http://wiki.example/cb"));
        assert!(!redirect_allowed("https://wiki.example/cb#x"));
        assert!(!redirect_allowed("javascript:alert(1)"));
        assert!(!redirect_allowed("myapp://cb"));
    }

    #[test]
    fn private_addresses_are_not_public() {
        for private in ["10.0.0.1", "192.168.1.1", "127.0.0.1", "169.254.1.1", "100.64.0.1", "::1", "fd00::1", "fe80::1", "::ffff:10.0.0.1", "0.0.0.0"] {
            assert!(!public_address(private.parse().unwrap()), "{private}");
        }
        assert!(public_address("93.184.216.34".parse().unwrap()));
        assert!(public_address("2606:4700::1111".parse().unwrap()));
    }

    #[test]
    fn a_website_known_by_its_address_goes_back_only_where_it_says() {
        let listed = Client { id: "https://app.example/".into(), kind: "indieauth".into(), name: "App".into(), secret: None, redirect_uris: vec!["https://app.example/cb".into()], created: 0, created_by: None };
        assert!(redirect_of(&listed, "https://app.example/cb"));
        assert!(!redirect_of(&listed, "https://app.example/other"));
        let unlisted = Client { redirect_uris: vec![], ..listed.clone() };
        assert!(redirect_of(&unlisted, "https://app.example/anything"));
        assert!(!redirect_of(&unlisted, "https://evil.example/cb"));
        let registered = Client { kind: "registered".into(), redirect_uris: vec![], ..listed };
        assert!(!redirect_of(&registered, "https://app.example/anything"));
    }

    #[test]
    fn pairwise_sectors_are_hosts() {
        let client = Client { id: "c1".into(), kind: "registered".into(), name: "W".into(), secret: None, redirect_uris: vec!["https://wiki.example/a".into(), "https://wiki.example/b".into()], created: 0, created_by: None };
        assert_eq!(sector(&client), "wiki.example");
        let two = Client { redirect_uris: vec!["https://a.example/".into(), "https://b.example/".into()], ..client };
        assert_eq!(sector(&two), "c1");
    }
}
