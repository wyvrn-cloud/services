//! The pages and the picture, over HTTP.
//!
//! | Path | What it answers |
//! | --- | --- |
//! | `/dl/<did>` | A page saying who a public address is, in OpenGraph tags. |
//! | `/dl?_oob=…` | The same for a contact invite. |
//! | `/card/a/<did>.png` | An address's card. |
//! | `/card/i/<id>.png` | An invite's card, for as long as it is remembered. |
//!
//! The `/dl` paths are the web app's own: a person's browser is to get the app
//! there, and only what fetches a link to show it gets this. Telling the two apart
//! is the proxy's part (by `User-Agent`); this service answers whoever it is sent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::render::{Renderer, HEIGHT, WIDTH};
use crate::who::{self, Fetch, Who};

/// How long an address is taken to be who it was: a card is asked for in bursts
/// (a link posted somewhere busy), and a profile doesn't change by the minute.
const ADDRESS_FOR: Duration = Duration::from_secs(5 * 60);
/// An address that couldn't be read is asked again sooner.
const FAILURE_FOR: Duration = Duration::from_secs(30);
/// An invite's card is asked for by whoever was just given its page. It can't be
/// drawn again from its address, which doesn't hold the invite.
const INVITE_FOR: Duration = Duration::from_secs(60 * 60);
const MAX_REMEMBERED: usize = 512;
/// An invite with a picture in it is long, and no longer than this.
const MAX_INVITE_CHARS: usize = 120_000;

struct Remembered {
    until: Instant,
    who: Result<Arc<Who>, String>,
    png: Option<Arc<Vec<u8>>>,
}

/// Who something remembered is (or why that couldn't be read), and its card if drawn.
type Recalled = (Result<Arc<Who>, String>, Option<Arc<Vec<u8>>>);

pub struct AppState {
    pub config: Config,
    pub renderer: Arc<Renderer>,
    pub fetch: Arc<dyn Fetch>,
    remembered: Mutex<HashMap<String, Remembered>>,
}

impl AppState {
    pub fn new(config: Config, fetch: Arc<dyn Fetch>) -> Arc<Self> {
        let renderer = crate::render::shared(&config.site_name);
        Arc::new(Self { config, renderer, fetch, remembered: Mutex::new(HashMap::new()) })
    }

    fn remembered(&self) -> std::sync::MutexGuard<'_, HashMap<String, Remembered>> {
        self.remembered.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn recall(&self, key: &str) -> Option<Recalled> {
        let remembered = self.remembered();
        remembered.get(key).filter(|entry| entry.until > Instant::now()).map(|entry| (entry.who.clone(), entry.png.clone()))
    }

    fn remember(&self, key: String, who: Result<Arc<Who>, String>, lasting: Duration) {
        let mut remembered = self.remembered();
        if remembered.len() >= MAX_REMEMBERED {
            let now = Instant::now();
            remembered.retain(|_, entry| entry.until > now);
            // Still full of what hasn't run out: the soonest to run out goes.
            if remembered.len() >= MAX_REMEMBERED {
                if let Some(oldest) = remembered.iter().min_by_key(|(_, entry)| entry.until).map(|(key, _)| key.clone()) {
                    remembered.remove(&oldest);
                }
            }
        }
        remembered.insert(key, Remembered { until: Instant::now() + lasting, who, png: None });
    }

    /// Who an address is: as remembered, or looked up now.
    async fn address(&self, did: &str) -> Result<Arc<Who>, String> {
        let key = format!("a:{did}");
        if let Some((who, _)) = self.recall(&key) {
            return who;
        }
        let who = who::of_address(did, self.fetch.as_ref(), &self.config).await.map(Arc::new);
        if let Err(why) = &who {
            tracing::info!(%did, %why, "an address couldn't be read");
        }
        self.remember(key, who.clone(), if who.is_ok() { ADDRESS_FOR } else { FAILURE_FOR });
        who
    }

    /// The card remembered under `key`, drawn if it hasn't been yet.
    async fn card(&self, key: &str) -> Option<Arc<Vec<u8>>> {
        let (who, png) = self.recall(key)?;
        if png.is_some() {
            return png;
        }
        let who = who.ok()?;
        let renderer = self.renderer.clone();
        // Drawing is work, not waiting: off the threads that answer requests.
        let png = match tokio::task::spawn_blocking(move || renderer.png(&who)).await {
            Ok(Ok(png)) => Arc::new(png),
            Ok(Err(error)) => {
                tracing::warn!(%error, key, "a card couldn't be drawn");
                return None;
            }
            Err(_) => return None,
        };
        if let Some(entry) = self.remembered().get_mut(key) {
            entry.png = Some(png.clone());
        }
        Some(png)
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/dl", get(invite_page))
        .route("/dl/{*did}", get(address_page))
        .route("/card/a/{*did}", get(address_card))
        .route("/card/i/{id}", get(invite_card))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state)
}

fn attribute(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Where the links are, as whoever followed one sees it: `https://chat.example`.
fn site(state: &AppState, headers: &HeaderMap) -> String {
    if let Some(url) = &state.config.public_url {
        return url.clone();
    }
    let said = |name: &str| headers.get(name).and_then(|value| value.to_str().ok()).map(|value| value.split(',').next().unwrap_or_default().trim().to_string());
    let host = said("x-forwarded-host").or_else(|| said("host")).filter(|host| !host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']')));
    let scheme = said("x-forwarded-proto").filter(|scheme| scheme == "http" || scheme == "https").unwrap_or_else(|| "https".to_string());
    format!("{scheme}://{}", host.unwrap_or_else(|| "localhost".to_string()))
}

/// The page: nothing but what a link's preview is read from, and for a person who
/// ends up here anyway, the link.
fn page(state: &AppState, link: &str, title: &str, description: &str, image: Option<&str>) -> Response {
    let site_name = attribute(&state.config.site_name);
    let (title, description, link) = (attribute(title), attribute(description), attribute(link));
    let mut tags = format!(
        r##"<meta property="og:type" content="profile">
<meta property="og:site_name" content="{site_name}">
<meta property="og:title" content="{title}">
<meta property="og:description" content="{description}">
<meta property="og:url" content="{link}">
<meta name="description" content="{description}">
<meta name="theme-color" content="#4a63e7">
<meta name="twitter:title" content="{title}">
<meta name="twitter:description" content="{description}">
"##
    );
    match image {
        Some(image) => {
            let image = attribute(image);
            tags.push_str(&format!(
                r#"<meta property="og:image" content="{image}">
<meta property="og:image:type" content="image/png">
<meta property="og:image:width" content="{WIDTH}">
<meta property="og:image:height" content="{HEIGHT}">
<meta property="og:image:alt" content="{title}">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:image" content="{image}">
"#
            ));
        }
        None => tags.push_str("<meta name=\"twitter:card\" content=\"summary\">\n"),
    }
    let html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>{title}</title>\n{tags}</head>\n<body>\n<h1>{title}</h1>\n<p>{description}</p>\n<p><a href=\"{link}\">Open in {site_name}</a></p>\n</body>\n</html>\n"
    );
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=300")], html).into_response()
}

/// The DID a path names after `prefix`, as it was written: a port in one is `%3A`
/// and stays so. One that arrives with all its colons encoded is read as meant.
fn did_in(path: &str, prefix: &str, suffix: &str) -> Option<String> {
    let named = path.strip_prefix(prefix)?.strip_suffix(suffix)?;
    let named = if named.starts_with("did:") { named.to_string() } else { named.replace("%3A", ":").replace("%3a", ":") };
    who::is_address(&named).then_some(named)
}

async fn address_page(State(state): State<Arc<AppState>>, uri: Uri, headers: HeaderMap) -> Response {
    let Some(did) = did_in(uri.path(), "/dl/", "") else { return StatusCode::NOT_FOUND.into_response() };
    let site = site(&state, &headers);
    let link = format!("{site}/dl/{did}");
    match state.address(&did).await {
        Ok(who) => {
            // A name is its own word, so where it is hosted is said beside it.
            let description = match (&who.name, &who.description) {
                (Some(_), Some(description)) => format!("{} · {description}", who.label),
                (Some(_), None) => format!("{} · A public address on {}", who.label, state.config.site_name),
                (None, Some(description)) => description.clone(),
                (None, None) => format!("A public address on {}", state.config.site_name),
            };
            page(&state, &link, who.title(), &description, Some(&format!("{site}/card/a/{did}.png")))
        }
        // Still a link to somewhere: said plainly, with no picture of anyone.
        Err(_) => page(&state, &link, &who::written(&did), &format!("A public address on {}", state.config.site_name), None),
    }
}

async fn address_card(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let Some(did) = did_in(uri.path(), "/card/a/", ".png") else { return StatusCode::NOT_FOUND.into_response() };
    if state.address(&did).await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    png(state.card(&format!("a:{did}")).await, ADDRESS_FOR)
}

async fn invite_page(State(state): State<Arc<AppState>>, uri: Uri, headers: HeaderMap) -> Response {
    let oob = uri.query().unwrap_or_default().split('&').find_map(|pair| pair.strip_prefix("_oob=")).map(|oob| oob.replace("%3D", "").replace("%3d", ""));
    let Some(oob) = oob.filter(|oob| !oob.is_empty() && oob.len() <= MAX_INVITE_CHARS) else { return StatusCode::NOT_FOUND.into_response() };
    let site = site(&state, &headers);
    let link = format!("{site}/dl?_oob={oob}");
    let Some(who) = who::of_invite(&oob) else {
        return page(&state, &link, "Invite", &format!("An invite on {}", state.config.site_name), None);
    };
    let id: String = Sha256::digest(oob.as_bytes()).iter().take(16).map(|byte| format!("{byte:02x}")).collect();
    let key = format!("i:{id}");
    if state.recall(&key).is_none() {
        state.remember(key, Ok(Arc::new(who.clone())), INVITE_FOR);
    }
    let description = match &who.description {
        Some(description) => format!("Contact invite · {description}"),
        None => format!("An invite to add them as a contact on {}", state.config.site_name),
    };
    page(&state, &link, who.title(), &description, Some(&format!("{site}/card/i/{id}.png")))
}

async fn invite_card(State(state): State<Arc<AppState>>, axum::extract::Path(id): axum::extract::Path<String>) -> Response {
    let Some(id) = id.strip_suffix(".png").filter(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())) else { return StatusCode::NOT_FOUND.into_response() };
    png(state.card(&format!("i:{id}")).await, INVITE_FOR)
}

fn png(card: Option<Arc<Vec<u8>>>, lasting: Duration) -> Response {
    match card {
        Some(card) => ([(header::CONTENT_TYPE, "image/png".to_string()), (header::CACHE_CONTROL, format!("public, max-age={}", lasting.as_secs()))], card.as_ref().clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
