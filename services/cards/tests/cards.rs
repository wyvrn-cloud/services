//! The service as a link's fetcher meets it: a page of tags, and the picture they
//! name -- for an address looked up from files a test hands it.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use didcomm_agent::Identity;
use tower::ServiceExt;
use wyvrn_cards::{config::Config, server, who::Fetch};
use wyvrn_service::address::{build_agent, DidMethod, ProfileConfig};

/// A host's files, and how many times they were asked for.
#[derive(Default)]
struct Hosts {
    files: HashMap<String, String>,
    asked: AtomicUsize,
}

impl Fetch for Hosts {
    fn get<'a>(&'a self, url: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<String>, String>> + Send + 'a>> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(self.files.get(url).cloned()) })
    }
}

/// A 1x1 PNG.
const DOT: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// A service that is `did:web:updates.example`, with a profile: its files.
async fn updates(pictures: bool) -> Hosts {
    let dir = std::env::temp_dir().join(format!("wyvrn-cards-test-{}-{pictures}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Pictures a test run is given (to look at the card they make), or a dot.
    let picture = |name: &str| {
        let given = std::env::var("CARDS_TEST_PICTURES").ok().map(|from| std::path::PathBuf::from(from).join(name)).filter(|path| path.exists());
        given.or_else(|| {
            let path = dir.join(name).with_extension("png");
            std::fs::write(&path, base64::engine::general_purpose::STANDARD.decode(DOT).unwrap()).unwrap();
            Some(path)
        })
    };
    let profile = ProfileConfig {
        name: Some("Wyvrn Updates".into()),
        description: Some("Release notes & news, <as> they happen.".into()),
        picture: pictures.then(|| picture("picture.png")).flatten(),
        banner: pictures.then(|| picture("banner.jpg")).flatten(),
        background: pictures.then(|| picture("background.jpg")).flatten(),
    };
    let (_, published) = build_agent(Identity::generate().unwrap(), "https://updates.example/", DidMethod::Web).unwrap();
    let published = published.unwrap().with_profile(&profile).unwrap();
    let mut hosts = Hosts::default();
    hosts.files.insert("https://updates.example/.well-known/did.json".into(), published.document.to_string());
    hosts.files.insert("https://updates.example/whois.vp".into(), published.whois(1_800_000_000).await.unwrap().unwrap().to_string());
    hosts
}

async fn get(state: &Arc<server::AppState>, path: &str) -> (StatusCode, String, Vec<u8>) {
    let request = Request::builder().uri(path).header("host", "chat.example").header("x-forwarded-proto", "https").body(Body::empty()).unwrap();
    let response = server::router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let kind = response.headers().get("content-type").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
    (status, kind, axum::body::to_bytes(response.into_body(), 4 << 20).await.unwrap().to_vec())
}

fn config() -> Config {
    Config { official_domains: vec!["updates.example".into()], did_host: "id.example".into(), ..Config::default() }
}

#[tokio::test]
async fn a_link_to_an_address_says_who_it_is_and_names_its_card() {
    let hosts = Arc::new(updates(true).await);
    let state = server::AppState::new(config(), hosts.clone());

    let (status, kind, body) = get(&state, "/dl/did:web:updates.example").await;
    let html = String::from_utf8(body).unwrap();
    assert_eq!((status, kind.as_str()), (StatusCode::OK, "text/html; charset=utf-8"));
    assert!(html.contains(r#"<meta property="og:title" content="Wyvrn Updates">"#), "{html}");
    // What the profile says is text, wherever it is put.
    assert!(html.contains(r#"<meta property="og:description" content="updates.example · Release notes &amp; news, &lt;as&gt; they happen.">"#), "{html}");
    assert!(html.contains(r#"<meta property="og:url" content="https://chat.example/dl/did:web:updates.example">"#), "{html}");
    assert!(html.contains(r#"<meta property="og:image" content="https://chat.example/card/a/did:web:updates.example.png">"#), "{html}");
    assert!(html.contains(r#"<meta name="twitter:card" content="summary_large_image">"#));

    let (status, kind, png) = get(&state, "/card/a/did:web:updates.example.png").await;
    assert_eq!((status, kind.as_str()), (StatusCode::OK, "image/png"));
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    if let Ok(to) = std::env::var("CARDS_TEST_OUT") {
        std::fs::write(std::path::Path::new(&to).join("address.png"), &png).unwrap();
    }

    // Looked up once, however many times it is asked for: the page, the card, the card again.
    let asked = hosts.asked.load(Ordering::SeqCst);
    get(&state, "/card/a/did:web:updates.example.png").await;
    get(&state, "/dl/did:web:updates.example").await;
    assert_eq!(hosts.asked.load(Ordering::SeqCst), asked);
}

#[tokio::test]
async fn an_address_that_cannot_be_read_is_still_a_link_and_has_no_card() {
    let state = server::AppState::new(config(), Arc::new(Hosts::default()));
    let (status, _, body) = get(&state, "/dl/did:web:id.example:nobody").await;
    let html = String::from_utf8(body).unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(r#"<meta property="og:title" content="nobody via id.example">"#), "{html}");
    assert!(!html.contains("og:image"));
    assert_eq!(get(&state, "/card/a/did:web:id.example:nobody.png").await.0, StatusCode::NOT_FOUND);
    // And what isn't an address is nothing at all.
    assert_eq!(get(&state, "/dl/did:peer:4zQmExample").await.0, StatusCode::NOT_FOUND);
    assert_eq!(get(&state, "/dl/%3Cscript%3E").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_link_carrying_an_invite_says_who_it_is_from() {
    let state = server::AppState::new(config(), Arc::new(Hosts::default()));
    let invitation = serde_json::json!({
        "type": "https://didcomm.org/out-of-band/2.0/invitation", "id": "1", "from": "did:peer:4zQmExampleOfAPeerDidThatGoesOnForSomeWay",
        "body": { "goal_code": "app.wyvrn.contact" },
        "attachments": [{ "id": "profile", "data": { "json": {
            "type": "https://didcomm.org/user-profile/1.0/profile",
            "body": { "profile": { "displayName": "Alice", "description": "Hello there", "displayPicture": "#pfp" } },
            "attachments": [{ "id": "pfp", "media_type": "image/png", "data": { "base64": DOT } }],
        }}}],
    });
    let oob = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(invitation.to_string());
    let (status, _, body) = get(&state, &format!("/dl?_oob={oob}")).await;
    let html = String::from_utf8(body).unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(r#"<meta property="og:title" content="Alice">"#), "{html}");
    assert!(html.contains(r#"<meta property="og:description" content="Contact invite · Hello there">"#), "{html}");
    let image = html.split(r#"<meta property="og:image" content="https://chat.example"#).nth(1).unwrap().split('"').next().unwrap();
    let (status, kind, png) = get(&state, image).await;
    assert_eq!((status, kind.as_str()), (StatusCode::OK, "image/png"));
    if let Ok(to) = std::env::var("CARDS_TEST_OUT") {
        std::fs::write(std::path::Path::new(&to).join("invite.png"), &png).unwrap();
    }
    // A card nobody was given the page for isn't there to be had.
    assert_eq!(get(&state, "/card/i/00000000000000000000000000000000.png").await.0, StatusCode::NOT_FOUND);
    assert_eq!(get(&state, "/dl?_oob=bm90IGFuIGludml0ZQ").await.0, StatusCode::OK);
    assert_eq!(get(&state, "/dl").await.0, StatusCode::NOT_FOUND);
}
