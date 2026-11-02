//! Signing in to the web view, as wyvrn-verify's own page does: website-sign-in/1.0
//! through the Digital Credentials API. The request is signed with the
//! community's `did:web` (or, for a community without one, unsigned and bound to
//! the page's origin); the answer is checked with wyvrn-sign-in; the DID it names
//! is pinged from the community and has to answer; and the person has to be a
//! member. Then a session: a random token in an `HttpOnly` cookie limited to the
//! web view's path, kept (hashed) in the store so it can be ended.
//!
//! The credential names the person's public address. A member is known by the DID
//! they write from, so the member is found by key, as `wyvrn_service::named`
//! recognises an address in a setting: the one whose document shares a
//! key-agreement key with the address's.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use askama::Template;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::oneshot;
use wyvrn_service::now;
use wyvrn_sign_in::keys::SecretKey;
use wyvrn_sign_in::openid4vp::{self, Asking};
use wyvrn_sign_in::verify::{self, Lookups};

use super::pages::{site, Viewer};
use super::{as_page, page_error, page_on, Web};
use crate::server::AppState;
use crate::store::Member;

pub const PING: &str = "https://didcomm.org/trust-ping/2.0/ping";
pub const PING_RESPONSE: &str = "https://didcomm.org/trust-ping/2.0/ping-response";
const REQUEST_FOR: i64 = 300;
const PING_WAIT: Duration = Duration::from_secs(15);
/// How long a session lasts.
pub const SESSION_FOR: i64 = 14 * 24 * 3600;
const COOKIE: &str = "wyvrn_web";

/// A request made and not yet answered.
struct Asked {
    asking: Asking,
    /// The key binding's audience: the signed request's `client_id`, or
    /// `origin:` and the page's origin.
    audience: String,
    at: i64,
}

/// Requests and pings under way.
#[derive(Default)]
pub struct Signing {
    requests: Mutex<HashMap<String, Asked>>,
    pings: Mutex<HashMap<String, oneshot::Sender<()>>>,
    /// Public addresses already matched to members: address → member DID.
    matched: Mutex<HashMap<String, String>>,
}

impl Signing {
    fn keep(&self, asked: Asked) -> String {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let mut requests = self.requests.lock().unwrap_or_else(|p| p.into_inner());
        requests.retain(|_, a| a.at > asked.at - REQUEST_FOR);
        requests.insert(id.clone(), asked);
        id
    }

    fn take(&self, id: &str, now: i64) -> Option<Asked> {
        self.requests.lock().unwrap_or_else(|p| p.into_inner()).remove(id).filter(|a| a.at > now - REQUEST_FOR)
    }

    /// A ping's answer came (`thid`): whoever waits on it is told. Whether anyone was.
    pub fn answered(&self, thid: &str) -> bool {
        self.pings.lock().unwrap_or_else(|p| p.into_inner()).remove(thid).is_some_and(|waiting| waiting.send(()).is_ok())
    }
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/sign-in", get(sign_in_page))
        .route("/sign-in/request", get(request))
        .route("/sign-in/response", post(response))
        .route("/sign-out", post(sign_out))
}

// --- Sessions -------------------------------------------------------------------------

fn cookie_value(headers: &HeaderMap) -> Option<String> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).filter_map(|c| c.trim().strip_prefix(&format!("{COOKIE}="))).map(str::to_string).next()
}

/// The member a request's session is, and its form token, while they are a member.
pub fn session_of(state: &AppState, headers: &HeaderMap) -> Option<(Member, String)> {
    let token = cookie_value(headers)?;
    let store = &state.community.store;
    let (mkey, csrf) = store.session(&token, now()).ok()??;
    let member = store.member_by_key(&mkey).ok()??;
    if store.banned(&member.did, now()).ok().flatten().is_some() {
        return None;
    }
    Some((member, csrf))
}

fn cookie(web: &Web, token: &str, max_age: i64) -> String {
    format!("{COOKIE}={token}; Path={}; HttpOnly;{} SameSite=Lax; Max-Age={max_age}", web.path, if web.secure() { " Secure;" } else { "" })
}

/// Whether a form's token is the session's.
pub fn form_ok(state: &AppState, headers: &HeaderMap, given: &str) -> Option<Member> {
    let (member, csrf) = session_of(state, headers)?;
    (!given.is_empty() && constant_eq(given, &csrf)).then_some(member)
}

pub fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Where to go after signing in: a path inside the web view, from its root, and
/// nothing else (no scheme, no host, no `//`).
pub fn return_to(given: Option<&str>) -> String {
    match given {
        Some(path) if !path.is_empty() && !path.starts_with('/') && !path.starts_with('\\') && !path.contains("://") && !path.contains('\\') && !path.chars().any(|c| c.is_control()) && !path.contains("..") => path.to_string(),
        _ => String::new(),
    }
}

// --- The page --------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "web/sign_in.html")]
struct SignInPage {
    site: super::pages::Site,
    title: String,
    then: String,
}

#[derive(Deserialize)]
struct ReturnTo {
    return_to: Option<String>,
}

async fn sign_in_page(State(state): State<Arc<AppState>>, Query(query): Query<ReturnTo>, headers: HeaderMap) -> Response {
    let Some(web) = state.web.as_deref() else { return StatusCode::NOT_FOUND.into_response() };
    if !page_on(&state.community.store) {
        return page_error(StatusCode::NOT_FOUND, "Not here", "There's nothing at this address.");
    }
    let then = return_to(query.return_to.as_deref());
    let viewer = Viewer::of(&state, &headers);
    if viewer.member.is_some() {
        return as_page(Redirect::to(&format!("./{then}")).into_response());
    }
    let page = SignInPage { site: site(&state, &viewer, "./", web.absolute("sign-in")), title: "Sign in".to_string(), then: format!("./{then}") };
    private_page(page.render())
}

pub fn private_page(html: Result<String, askama::Error>) -> Response {
    match html {
        Ok(html) => {
            let mut response = Html(html).into_response();
            response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response.headers_mut().insert("x-robots-tag", HeaderValue::from_static("noindex"));
            as_page(response)
        }
        Err(error) => {
            tracing::error!(%error, "a page couldn't be made");
            page_error(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong", "This page couldn't be made just now.")
        }
    }
}

fn refusal(status: StatusCode, why: impl std::fmt::Display) -> Response {
    let mut response = (status, Json(json!({ "error": why.to_string() }))).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    as_page(response)
}

// --- The request, and its answer ---------------------------------------------------------

/// A request for website-sign-in/1.0's credential, for the page to hand the browser.
async fn request(State(state): State<Arc<AppState>>) -> Response {
    let Some(web) = state.web.as_deref() else { return StatusCode::NOT_FOUND.into_response() };
    if !page_on(&state.community.store) {
        return refusal(StatusCode::NOT_FOUND, "There's no web view here.");
    }
    let asking = Asking::new();
    let query = openid4vp::dcql(&[(openid4vp::QUERY_ID, wyvrn_sign_in::VCT)]);
    let origin = web.origin();
    let signed = state.did_document.as_ref().and_then(|published| {
        let key = serde_json::from_str::<Value>(published.signing_jwk()).ok().and_then(|jwk| SecretKey::from_jwk(&jwk).ok())?;
        Some((published.did().to_string(), key))
    });
    let (request, audience) = match signed {
        Some((did, key)) => (asking.signed(&query, &did, &format!("{did}#key-1"), std::slice::from_ref(&origin), &key), format!("{}{did}", openid4vp::DID_PREFIX)),
        None => (asking.unsigned(&query), format!("origin:{origin}")),
    };
    let id = web.sign_in.keep(Asked { asking, audience, at: now() });
    let mut response = Json(json!({ "id": id, "request": request })).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    as_page(response)
}

#[derive(Deserialize)]
struct Answer {
    id: String,
    data: Value,
}

async fn response(State(state): State<Arc<AppState>>, Json(answer): Json<Answer>) -> Response {
    let Some(web) = state.web.as_deref() else { return StatusCode::NOT_FOUND.into_response() };
    let at = now();
    let Some(asked) = web.sign_in.take(&answer.id, at) else {
        return refusal(StatusCode::NOT_FOUND, "That sign-in took too long. Try again.");
    };
    let vp_token = match asked.asking.open(&answer.data) {
        Ok(vp_token) => vp_token,
        Err(e) => return refusal(StatusCode::BAD_REQUEST, e),
    };
    let Some(presentation) = openid4vp::presentation(&vp_token, openid4vp::QUERY_ID) else {
        return refusal(StatusCode::BAD_REQUEST, "No sign-in credential in the answer.");
    };
    let lookups = Resolver::new();
    let trusted = trusted_issuers(web, presentation, &lookups).await;
    let expect = verify::Expect { audience: &asked.audience, nonce: &asked.asking.nonce, now: at, trusted_issuers: &trusted };
    let signed_in = match verify::check(presentation, &expect, &lookups).await {
        Ok(signed_in) => signed_in,
        Err(e) => return refusal(StatusCode::BAD_REQUEST, e),
    };
    if let Err(e) = verify::reachable(&signed_in.document, &signed_in.did) {
        return refusal(StatusCode::BAD_REQUEST, e);
    }
    if let Err(why) = ping(&state, web, &signed_in.did).await {
        return refusal(StatusCode::BAD_REQUEST, why);
    }
    let Some(member) = member_for(&state, web, &signed_in.did, &signed_in.document).await else {
        return refusal(StatusCode::FORBIDDEN, "You signed in, but you aren't a member of this community. Join it in Wyvrn first.");
    };
    if state.community.store.banned(&member.did, at).ok().flatten().is_some() {
        return refusal(StatusCode::FORBIDDEN, "You can't sign in to this community.");
    }
    let store = &state.community.store;
    let (token, csrf) = (super::store::secret(), super::store::secret());
    let mkey = match store.member_key(&member.did) {
        Ok(mkey) => mkey,
        Err(error) => return refusal(StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    if let Err(error) = store.add_session(&token, &mkey, &csrf, at, at + SESSION_FOR) {
        return refusal(StatusCode::INTERNAL_SERVER_ERROR, error);
    }
    tracing::info!(member = %member.did, "a member signed in to the web view");
    let mut response = Json(json!({ "name": member.name })).into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie(web, &token, SESSION_FOR)) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    as_page(response)
}

/// The issuers trusted for this presentation: those configured, and one that is
/// a `did:webvh` whose document says it is also one of them (a verification
/// service speaks as its public address once it holds one).
async fn trusted_issuers(web: &Web, presentation: &str, lookups: &Resolver) -> Vec<String> {
    let mut trusted = web.config.sign_in_issuers.clone();
    let issuer = wyvrn_sign_in::sdjwt::SdJwt::parse(presentation).ok().and_then(|sd| sd.credential.claim("iss").map(str::to_string));
    if let Some(issuer) = issuer.filter(|i| !trusted.contains(i) && i.starts_with("did:webvh:")) {
        if let Ok(document) = lookups.document(&issuer).await {
            let also: Vec<&str> = document["alsoKnownAs"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            if also.iter().any(|a| trusted.iter().any(|t| t == a)) {
                trusted.push(issuer);
            }
        }
    }
    trusted
}

/// Pings `did` from the community, and waits for its answer.
async fn ping(state: &AppState, web: &Web, did: &str) -> Result<(), String> {
    let id = uuid::Uuid::new_v4().to_string();
    let (told, waiting) = oneshot::channel();
    web.sign_in.pings.lock().unwrap_or_else(|p| p.into_inner()).insert(id.clone(), told);
    let message = json!({ "id": id, "type": PING, "body": { "response_requested": true } });
    if let Err(error) = state.voice.send(&state.agent, did, &message).await {
        web.sign_in.pings.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
        return Err(format!("Your app couldn't be reached: {error}"));
    }
    match tokio::time::timeout(PING_WAIT, waiting).await {
        Ok(Ok(())) => Ok(()),
        _ => {
            web.sign_in.pings.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
            Err("Your app didn't answer. Open Wyvrn on your phone and try again.".to_string())
        }
    }
}

/// The member `did` (a public address, usually) is: themselves, or the member
/// whose document shares a key-agreement key with it.
async fn member_for(state: &AppState, web: &Web, did: &str, document: &Value) -> Option<Member> {
    let store = &state.community.store;
    if let Some(member) = store.member(did).ok().flatten() {
        return Some(member);
    }
    let remembered = web.sign_in.matched.lock().unwrap_or_else(|p| p.into_inner()).get(did).cloned();
    if let Some(member) = remembered.and_then(|m| store.member(&m).ok().flatten()) {
        return Some(member);
    }
    let keys = wyvrn_hosted_did::keys::keys_for(document, "keyAgreement");
    if keys.is_empty() {
        return None;
    }
    for member in store.members().ok()? {
        let Ok(theirs) = state.resolver.resolve(&member.did).await else { continue };
        if wyvrn_hosted_did::keys::keys_for(&theirs, "keyAgreement").iter().any(|k| keys.contains(k)) {
            web.sign_in.matched.lock().unwrap_or_else(|p| p.into_inner()).insert(did.to_string(), member.did.clone());
            return Some(member);
        }
    }
    None
}

#[derive(Deserialize)]
struct Csrf {
    #[serde(default)]
    csrf: String,
}

async fn sign_out(State(state): State<Arc<AppState>>, headers: HeaderMap, Form(form): Form<Csrf>) -> Response {
    let Some(web) = state.web.as_deref() else { return StatusCode::NOT_FOUND.into_response() };
    if form_ok(&state, &headers, &form.csrf).is_some() {
        if let Some(token) = cookie_value(&headers) {
            let _ = state.community.store.end_session(&token);
        }
    }
    let mut response = Redirect::to("./").into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie(web, "", 0)) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    as_page(response)
}

// --- Resolving, for the checks ---------------------------------------------------------------

/// DIDs resolved and status lists fetched over HTTPS, for wyvrn-sign-in's checks.
pub struct Resolver(reqwest::Client);

impl Resolver {
    pub fn new() -> Self {
        Self(reqwest::Client::builder().timeout(Duration::from_secs(15)).build().unwrap_or_default())
    }
}

impl Default for Resolver {
    fn default() -> Self {
        Self::new()
    }
}

impl wyvrn_hosted_did::resolve::Fetch for Resolver {
    fn get(&self, url: &str) -> impl std::future::Future<Output = Result<Option<Vec<u8>>, String>> + Send {
        let request = self.0.get(url);
        async move {
            let response = request.send().await.map_err(|e| e.to_string())?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !response.status().is_success() {
                return Err(format!("HTTP {}", response.status()));
            }
            Ok(Some(response.bytes().await.map_err(|e| e.to_string())?.to_vec()))
        }
    }
}

#[async_trait::async_trait]
impl Lookups for Resolver {
    async fn document(&self, did: &str) -> wyvrn_sign_in::Result<Value> {
        let lookup = |why: String| wyvrn_sign_in::Error::Lookup(why);
        if let Some(document) = did.starts_with("did:peer:4").then(|| wyvrn_hosted_did::verify::peer4_document(did)).flatten() {
            return Ok(document);
        }
        let resolved = wyvrn_hosted_did::resolve::resolve(did, self).await.map_err(|e| lookup(format!("{did} couldn't be resolved: {e}")))?;
        if did.starts_with("did:webvh:") && resolved.standing != wyvrn_hosted_did::resolve::Standing::Verified {
            return Err(lookup(format!("{did}'s history doesn't verify")));
        }
        if resolved.deactivated {
            return Err(lookup(format!("{did} has been given up")));
        }
        Ok(resolved.document)
    }

    async fn status_list(&self, uri: &str) -> wyvrn_sign_in::Result<String> {
        let response = self.0.get(uri).send().await.map_err(|e| wyvrn_sign_in::Error::Lookup(e.to_string()))?;
        if !response.status().is_success() {
            return Err(wyvrn_sign_in::Error::Lookup(format!("HTTP {} for {uri}", response.status())));
        }
        response.text().await.map_err(|e| wyvrn_sign_in::Error::Lookup(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_to_stays_inside_the_web_view() {
        assert_eq!(return_to(Some("ch/abc/general")), "ch/abc/general");
        assert_eq!(return_to(Some("oauth/authorize?client_id=x")), "oauth/authorize?client_id=x");
        for bad in ["/elsewhere", "//evil.example", "https://evil.example", "\\\\evil", "../up", "a\nb"] {
            assert_eq!(return_to(Some(bad)), "", "{bad}");
        }
        assert_eq!(return_to(None), "");
    }

    #[test]
    fn tokens_compare_whole() {
        assert!(constant_eq("abc", "abc"));
        assert!(!constant_eq("abc", "abd"));
        assert!(!constant_eq("abc", "ab"));
    }
}
