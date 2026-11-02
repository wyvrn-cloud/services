//! The web view (community/1.0's "On the web") served for real: its pages,
//! who sees what, its headers, its addresses behind a proxy, and the identity
//! provider (community-sign-in/1.0) from a website's side.
//!
//! Signing in itself (the Digital Credentials API, a phone answering) can't be
//! run here: a session is made in the store, as a sign-in ends by making one.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{body::Bytes, extract::State, routing::post, Router};
use base64::Engine;
use didcomm_agent::{Agent, Identity};
use reqwest::header;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use wyvrn_community::{
    community::Community,
    server::{self, AppState},
    store::{Role, Store},
    web::{self, store::Client, Web},
};

const C: &str = "https://wyvrn.app/community/1.0/";
const W: &str = "https://didcomm.org/workflow/1.0/";

struct Peer {
    agent: Arc<Agent>,
    server: String,
}

impl Peer {
    async fn start(server: &Running) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let agent = Arc::new(Agent::with_endpoint(Identity::generate().unwrap(), &url).unwrap());
        let app = Router::new()
            .route(
                "/",
                post(|State(agent): State<Arc<Agent>>, body: Bytes| async move {
                    let _ = agent.receive(&body).await;
                }),
            )
            .with_state(agent.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { agent, server: server.did.clone() }
    }

    fn did(&self) -> String {
        self.agent.did()
    }

    async fn ask(&self, message_type: &str, body: Value) -> Value {
        let message = json!({ "type": message_type, "body": body, "return_route": "all" });
        self.agent.send(&self.server, &message).await.expect("sent").expect("an answer").message
    }

    /// Says something the server doesn't answer (a profile).
    async fn tell(&self, message_type: &str, body: Value) {
        let message = json!({ "type": message_type, "body": body });
        let _ = self.agent.send(&self.server, &message).await.expect("sent");
    }

    async fn community(&self, kind: &str, body: Value) -> Value {
        self.ask(&format!("{C}{kind}"), body).await
    }
}

struct Running {
    did: String,
    /// The server's own address.
    url: String,
    state: Arc<AppState>,
    http: reqwest::Client,
}

impl Running {
    async fn get(&self, path: &str) -> reqwest::Response {
        self.http.get(format!("{}{path}", self.url)).send().await.unwrap()
    }

    async fn text(&self, path: &str) -> (u16, String) {
        let response = self.get(path).await;
        (response.status().as_u16(), response.text().await.unwrap())
    }

    /// A session for `did`, as signing in ends by making one: its cookie and form token.
    fn session(&self, did: &str) -> (String, String) {
        let store = &self.state.community.store;
        let token = web::store::secret();
        let csrf = web::store::secret();
        let mkey = store.member_key(did).unwrap();
        store.add_session(&token, &mkey, &csrf, wyvrn_service::now(), wyvrn_service::now() + 3600).unwrap();
        (format!("wyvrn_web={token}"), csrf)
    }

    fn channel_id(&self, name: &str) -> String {
        self.state.community.store.channel_named(name).unwrap().unwrap().id
    }
}

struct Options<'a> {
    name: &'a str,
    web_url: Option<&'a str>,
    fresh: bool,
    limit: u32,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Self { name: "The Workshop", web_url: None, fresh: true, limit: 10_000 }
    }
}

async fn start(options: Options<'_>) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let web_line = options.web_url.map(|u| format!("public_url = \"{u}\"\n")).unwrap_or_default();
    let config: wyvrn_community::config::Config = toml::from_str(&format!(
        "[server]\nbind = \"unused\"\npublic_url = \"{url}\"\n[identity]\npath = \"unused\"\n[store]\npath = \"unused\"\n[community]\nname = {}\nmembers_may_create_channels = true\n[web]\n{web_line}requests_per_minute = {}\n",
        toml::Value::String(options.name.to_string()),
        options.limit
    ))
    .unwrap();
    let (agent, did_document) = server::build_agent(&config, Identity::generate().unwrap()).unwrap();
    let did = agent.did();
    let store = Store::in_memory().unwrap();
    if !options.fresh {
        store.add_channel("old", "older", "", "someone", 1).unwrap();
    }
    let web = Web::new(&config).unwrap().map(Arc::new);
    web::settle_new(&store).unwrap();
    let mut community = Community::new(store, config.community.clone(), Some(wyvrn_service::profile::Kept::scratch().unwrap())).unwrap();
    community.web_url = web.as_ref().map(|w| w.url.to_string());
    let mut state = AppState::new(agent, Default::default(), community, did_document, didcomm_quickstart::default_resolver());
    state.web = web;
    let state = Arc::new(state);
    let app = server::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap() });
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    Running { did, url, state, http }
}

/// A community with an admin and a member, a channel `general` and one `secret`.
async fn with_channels(server: &Running) -> (Peer, Peer) {
    let admin = Peer::start(server).await;
    let member = Peer::start(server).await;
    server.state.community.store.set_member(&admin.did(), Role::Admin, 1).unwrap();
    server.state.community.store.set_member(&member.did(), Role::Member, 1).unwrap();
    for name in ["general", "secret"] {
        let answer = admin.community("create-channel", json!({ "name": name })).await;
        assert_eq!(answer["type"], format!("{C}community"), "{answer}");
    }
    (admin, member)
}

async fn say(peer: &Peer, server: &Running, channel: &str, text: &str) -> Value {
    let answer = peer.community("post", json!({ "channel": server.channel_id(channel), "id": uuid::Uuid::new_v4().to_string(), "content": text })).await;
    assert_eq!(answer["type"], format!("{C}posted"), "{answer}");
    answer
}

#[tokio::test]
async fn the_page_is_on_for_a_new_community_and_off_for_one_from_before() {
    let new = start(Options::default()).await;
    let (status, html) = new.text("").await;
    assert_eq!(status, 200);
    assert!(html.contains("<h1>The Workshop</h1>"), "{html}");

    let old = start(Options { fresh: false, ..Options::default() }).await;
    let (status, text) = old.text("").await;
    assert_eq!(status, 200);
    assert!(text.starts_with("This is a Wyvrn community server"), "{text}");
    assert_eq!(old.text("sitemap.xml").await.0, 404);
}

#[tokio::test]
async fn pages_are_hardened_and_the_didcomm_endpoint_keeps_its_cors() {
    let server = start(Options::default()).await;
    let page = server.http.get(&server.url).header(header::ORIGIN, "https://evil.example").send().await.unwrap();
    let headers = page.headers();
    assert!(headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("frame-ancestors 'none'"));
    assert!(headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("script-src 'self'"));
    assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none(), "a page sends no CORS");
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS).is_none());

    for path in ["assets/web.css", "assets/web.js", "theme.css", "robots.txt", "sitemap.xml", "sign-in", "fonts/inter-latin.woff2", "ch/nope/x"] {
        let response = server.get(path).await;
        assert_eq!(response.headers()[header::X_FRAME_OPTIONS], "DENY", "{path}");
        assert!(response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none(), "{path}");
    }

    // DIDComm's preflight still answers any origin.
    let preflight = server.http.request(reqwest::Method::OPTIONS, &server.url).header(header::ORIGIN, "https://app.example").header("access-control-request-method", "POST").send().await.unwrap();
    assert_eq!(preflight.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
}

#[tokio::test]
async fn only_public_channels_show_and_what_is_said_is_only_ever_text() {
    let server = start(Options { name: "The <b>\"Workshop\"</b>", ..Options::default() }).await;
    let (admin, member) = with_channels(&server).await;
    member.tell("https://didcomm.org/user-profile/1.0/profile", json!({ "profile": { "displayName": "<img src=x onerror=alert(1)>" } })).await;
    say(&member, &server, "general", "before it was public").await;
    say(&member, &server, "secret", "nobody outside sees this").await;
    let general = server.channel_id("general");
    let secret = server.channel_id("secret");

    // Nothing is public yet.
    let (_, home) = server.text("").await;
    assert!(home.contains("The &#60;b&#62;&#34;Workshop&#34;&#60;/b&#62;") || home.contains("The &lt;b&gt;&quot;Workshop&quot;&lt;/b&gt;"), "{home}");
    assert!(!home.contains("<b>\"Workshop\""));
    assert!(!home.contains(&general));
    assert_eq!(server.text(&format!("ch/{general}/general")).await.0, 404);

    // (Times are to the second: the post above is in an earlier one.)
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    // Public from now on: a member may not, an admin may.
    let refused = member.community("update-channel", json!({ "channel": general, "web": { "public": true } })).await;
    assert_eq!(refused["body"]["code"], "e.p.trust.not-authorized");
    let answer = admin.community("update-channel", json!({ "channel": general, "web": { "public": true } })).await;
    let listed = answer["body"]["channels"].as_array().unwrap().iter().find(|c| c["id"] == general.as_str()).unwrap().clone();
    assert_eq!(listed["web"]["public"], true);
    assert!(listed["url"].as_str().unwrap().ends_with(&format!("ch/{general}/general")));
    assert_eq!(answer["body"]["web"]["page"], true);
    assert_eq!(answer["body"]["web"]["voice"], "counts");

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    say(&member, &server, "general", "**hello** <script>alert(1)</script> [x](javascript:alert(2))").await;
    say(&member, &server, "secret", "still private").await;

    let (status, page) = server.text(&format!("ch/{general}/general")).await;
    assert_eq!(status, 200);
    assert!(page.contains("<strong>hello</strong>"));
    assert!(!page.contains("<script>alert"));
    assert!(!page.contains("javascript:"));
    assert!(!page.contains("<img src=x"), "a name is text");
    assert!(page.contains("&lt;img src=x onerror=alert(1)&gt;") || page.contains("&#60;img"), "{page}");
    assert!(!page.contains("before it was public"), "posts from before it was public stay off it");
    assert!(!page.contains(&member.did()), "authors by name, never by DID");

    // The private channel: as if it weren't there, and never listed.
    assert_eq!(server.text(&format!("ch/{secret}/secret")).await.0, 404);
    let (_, home) = server.text("").await;
    assert!(home.contains(&format!("ch/{general}/general")));
    assert!(!home.contains(&secret));
    let (_, sitemap) = server.text("sitemap.xml").await;
    assert!(!sitemap.contains(&secret));

    // With its history, the earlier post shows too.
    admin.community("update-channel", json!({ "channel": general, "web": { "public": true, "history": true } })).await;
    assert!(server.text(&format!("ch/{general}/general")).await.1.contains("before it was public"));

    // A member signed in sees the private channel, and the page isn't cached for anyone.
    let (cookie, _) = server.session(&member.did());
    let response = server.http.get(format!("{}ch/{secret}/secret", server.url)).header(header::COOKIE, cookie).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.headers()[header::CACHE_CONTROL].to_str().unwrap().contains("private"));
    assert_eq!(response.headers()["x-robots-tag"], "noindex");
    assert!(response.text().await.unwrap().contains("nobody outside sees this"));
}

#[tokio::test]
async fn permalinks_follow_renames_and_a_post_taken_back_is_gone() {
    let server = start(Options::default()).await;
    let (admin, member) = with_channels(&server).await;
    let general = server.channel_id("general");
    admin.community("update-channel", json!({ "channel": general, "web": { "public": true, "history": true } })).await;
    let posted = say(&member, &server, "general", "the first").await;
    let seq = posted["body"]["seq"].as_i64().unwrap();

    let (status, page) = server.text(&format!("ch/{general}/m/{seq}")).await;
    assert_eq!(status, 200);
    assert!(page.contains("class=\"post focus\""));
    assert!(page.contains(&format!("<link rel=\"canonical\" href=\"{}ch/{general}/m/{seq}\">", server.url)));
    assert!(page.contains("DiscussionForumPosting"));

    // The short link goes there.
    let short = server.get(&format!("m/{}", posted["body"]["id"].as_str().unwrap())).await;
    assert_eq!(short.status(), 303);
    assert!(short.headers()[header::LOCATION].to_str().unwrap().ends_with(&format!("ch/{general}/m/{seq}")));

    // Renamed: its old name's address is moved for good.
    admin.community("update-channel", json!({ "channel": general, "name": "lobby" })).await;
    let moved = server.get(&format!("ch/{general}/general")).await;
    assert_eq!(moved.status(), 308);
    assert!(moved.headers()[header::LOCATION].to_str().unwrap().ends_with(&format!("ch/{general}/lobby")));

    // Taken back: gone.
    member.ask("https://wyvrn.app/message-edits/1.0/delete", json!({ "to": { "id": posted["body"]["id"] } })).await;
    let (status, _) = server.text(&format!("ch/{general}/m/{seq}")).await;
    assert!(status == 410 || status == 404, "{status}");
}

#[tokio::test]
async fn absolute_addresses_come_from_the_settings_never_the_request() {
    let server = start(Options { web_url: Some("https://example.com/forum/"), ..Options::default() }).await;
    let (admin, _) = with_channels(&server).await;
    let general = server.channel_id("general");
    admin.community("update-channel", json!({ "channel": general, "web": { "public": true } })).await;

    // A proxy that takes the prefix off, and one that doesn't: the same page.
    for path in [format!("ch/{general}/general"), format!("forum/ch/{general}/general")] {
        let response = server.http.get(format!("{}{path}", server.url)).header(header::HOST, "evil.example").header("x-forwarded-host", "evil.example").header("x-forwarded-prefix", "/evil").send().await.unwrap();
        assert_eq!(response.status(), 200, "{path}");
        let page = response.text().await.unwrap();
        assert!(page.contains(&format!("<link rel=\"canonical\" href=\"https://example.com/forum/ch/{general}/general\">")), "{page}");
        assert!(!page.contains("evil.example"));
        assert!(page.contains("href=\"../../assets/web.css\""), "links inside are relative");
    }
    let bare = server.get("forum").await;
    assert_eq!(bare.status(), 308);
    assert_eq!(bare.headers()[header::LOCATION], "forum/");
    assert_eq!(server.text("forum/").await.0, 200);
    let (_, sitemap) = server.text("forum/sitemap.xml").await;
    assert!(sitemap.contains(&format!("<loc>https://example.com/forum/ch/{general}/general</loc>")));
    assert!(sitemap.contains("<loc>https://example.com/forum/</loc>"));
    let (_, robots) = server.text("robots.txt").await;
    assert!(robots.contains("Sitemap: https://example.com/forum/sitemap.xml"));
    assert!(robots.contains("Disallow: /forum/oauth/"));

    // A form posted from another origin than the web view's is refused.
    let refused = server.http.post(format!("{}sign-out", server.url)).header(header::ORIGIN, server.url.trim_end_matches('/')).form(&[("csrf", "x")]).send().await.unwrap();
    assert_eq!(refused.status(), 403);
    let fine = server.http.post(format!("{}sign-out", server.url)).header(header::ORIGIN, "https://example.com").form(&[("csrf", "x")]).send().await.unwrap();
    assert_eq!(fine.status(), 303);

    // The identity provider's issuer is the web view's address.
    let discovery: Value = server.http.get(format!("{}.well-known/openid-configuration", server.url)).send().await.unwrap().json().await.unwrap();
    assert_eq!(discovery["issuer"], "https://example.com/forum");
    assert_eq!(discovery["authorization_endpoint"], "https://example.com/forum/oauth/authorize");
}

#[tokio::test]
async fn the_theme_is_variables_then_the_override_but_never_on_sign_in() {
    let server = start(Options::default()).await;
    let store = &server.state.community.store;
    let theme = web::flows::Theme { accent_light: "#ff8800".into(), ..Default::default() };
    store.set_setting(web::THEME, &serde_json::to_string(&theme).unwrap()).unwrap();
    store.set_setting(web::CSS, ".post { outline: 1px solid hotpink; }").unwrap();
    let (_, css) = server.text("theme.css").await;
    assert!(css.contains("--accent: #ff8800"));
    assert!(css.contains("hotpink"));
    let (_, plain) = server.text("theme.css?plain").await;
    assert!(plain.contains("--accent: #ff8800"));
    assert!(!plain.contains("hotpink"), "no override on the sign-in and consent pages");
    let (_, sign_in) = server.text("sign-in").await;
    assert!(sign_in.contains("theme.css?plain"));
    assert!(sign_in.contains("id=\"signin\""));
}

#[tokio::test]
async fn admins_turn_the_page_off_and_choose_who_sees_voice() {
    let server = start(Options::default()).await;
    let (admin, member) = with_channels(&server).await;
    let refused = member.community("set-web", json!({ "page": false })).await;
    assert_eq!(refused["body"]["code"], "e.p.trust.not-authorized");
    let bad = admin.community("set-web", json!({ "voice": "everyone" })).await;
    assert_eq!(bad["body"]["code"], "e.p.msg.invalid-web");
    let answer = admin.community("set-web", json!({ "page": false, "voice": "names" })).await;
    assert_eq!(answer["body"]["web"]["page"], false);
    assert_eq!(answer["body"]["web"]["voice"], "names");
    let (_, text) = server.text("").await;
    assert!(text.starts_with("This is a Wyvrn community server"));
    assert_eq!(server.text("sign-in").await.0, 404);
    assert_eq!(server.text(".well-known/openid-configuration").await.0, 404);
}

#[tokio::test]
async fn a_members_picture_is_kept_and_shown_beside_their_posts() {
    let server = start(Options::default()).await;
    let (admin, member) = with_channels(&server).await;
    let general = server.channel_id("general");
    admin.community("update-channel", json!({ "channel": general, "web": { "public": true, "history": true } })).await;
    // A 1x1 PNG.
    let png = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap();
    let message = json!({
        "type": "https://didcomm.org/user-profile/1.0/profile",
        "body": { "profile": { "displayName": "Ada", "displayPicture": "#pfp" } },
        "attachments": [{ "id": "pfp", "media_type": "image/png", "data": { "base64": base64::engine::general_purpose::STANDARD.encode(&png) } }],
        "return_route": "all"
    });
    let _ = member.agent.send(&member.server, &message).await;
    say(&member, &server, "general", "hello").await;
    let (_, page) = server.text(&format!("ch/{general}/general")).await;
    let at = page.find("src=\"../../p/").expect("a picture beside the post");
    let token: String = page[at + 13..].chars().take_while(|c| *c != '"').collect();
    let picture = server.get(&format!("p/{token}")).await;
    assert_eq!(picture.status(), 200);
    assert_eq!(picture.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(picture.bytes().await.unwrap().to_vec(), png);
    assert_eq!(server.text("p/not-a-token").await.0, 404);
}

#[tokio::test]
async fn too_many_requests_from_one_address_are_slowed() {
    let server = start(Options { limit: 3, ..Options::default() }).await;
    for _ in 0..3 {
        assert_eq!(server.text("robots.txt").await.0, 200);
    }
    assert_eq!(server.text("robots.txt").await.0, 429);
}

#[tokio::test]
async fn a_sign_in_request_is_signed_by_the_community_for_its_origin() {
    let server = start(Options::default()).await;
    let asked: Value = server.get("sign-in/request").await.json().await.unwrap();
    match &server.state.did_document {
        // A did:web community signs its request, for its origin alone.
        Some(published) => {
            assert_eq!(asked["request"]["protocol"], "openid4vp-v1-signed");
            let compact = asked["request"]["data"]["request"].as_str().unwrap();
            let jws = wyvrn_sign_in::jws::Jws::parse(compact).unwrap();
            assert_eq!(jws.payload["expected_origins"], json!([server.url.trim_end_matches('/')]));
            assert_eq!(jws.payload["client_id"], format!("decentralized_identifier:{}", published.did()));
        }
        // One without (plain HTTP, here) asks unsigned: the browser vouches for the origin.
        None => {
            assert_eq!(asked["request"]["protocol"], "openid4vp-v1-unsigned");
            assert!(asked["request"]["data"]["nonce"].as_str().is_some_and(|n| n.len() >= 16));
        }
    }
    let answer = server.http.post(format!("{}sign-in/response", server.url)).header(header::ORIGIN, server.url.trim_end_matches('/')).json(&json!({ "id": "nope", "data": {} })).send().await.unwrap();
    assert_eq!(answer.status(), 404);
}

/// A website's PKCE pair.
fn pkce() -> (String, String) {
    let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn query_of(url: &str) -> std::collections::HashMap<String, String> {
    url::Url::parse(url).unwrap().query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

#[tokio::test]
async fn a_website_signs_a_member_in_with_the_community() {
    let server = start(Options::default()).await;
    let (_admin, member) = with_channels(&server).await;
    member.tell("https://didcomm.org/user-profile/1.0/profile", json!({ "profile": { "displayName": "Ada" } })).await;
    let store = &server.state.community.store;
    let secret = "wiki-secret";
    store.add_client(&Client { id: "wiki".into(), kind: "registered".into(), name: "The Wiki".into(), secret: Some(web::store::hashed(secret)), redirect_uris: vec!["https://wiki.example/cb".into()], created: 1, created_by: None }).unwrap();
    let (verifier, challenge) = pkce();
    let authorize = format!("oauth/authorize?client_id=wiki&redirect_uri={}&response_type=code&scope=openid%20profile%20roles%20did&state=s1&nonce=n1&code_challenge={challenge}&code_challenge_method=S256", url::form_urlencoded::byte_serialize(b"https://wiki.example/cb").collect::<String>());

    // A redirect address it didn't register is stopped here, not followed.
    let wrong = server.get(&authorize.replace("wiki.example%2Fcb", "evil.example%2Fcb")).await;
    assert_eq!(wrong.status(), 400);
    // Not signed in: to the sign-in page, and back here after.
    let to_sign_in = server.get(&authorize).await;
    assert_eq!(to_sign_in.status(), 303);
    assert!(to_sign_in.headers()[header::LOCATION].to_str().unwrap().starts_with("../sign-in?return_to=oauth%2Fauthorize"));

    // Signed in: the consent page, saying who asks and what it is told.
    let (cookie, csrf) = server.session(&member.did());
    let consent = server.http.get(format!("{}{authorize}", server.url)).header(header::COOKIE, &cookie).send().await.unwrap();
    assert_eq!(consent.status(), 200);
    assert!(consent.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("form-action 'self' https://wiki.example"));
    let page = consent.text().await.unwrap();
    assert!(page.contains("The Wiki"));
    assert!(page.contains("Verified"));
    assert!(page.contains("Your roles in this community"));

    // Allowed: back to the website with a code.
    let fields = [("client_id", "wiki"), ("redirect_uri", "https://wiki.example/cb"), ("response_type", "code"), ("scope", "openid profile roles did"), ("state", "s1"), ("nonce", "n1"), ("code_challenge", challenge.as_str()), ("code_challenge_method", "S256"), ("csrf", csrf.as_str()), ("decision", "allow")];
    let forged = server.http.post(format!("{}oauth/authorize", server.url)).header(header::COOKIE, &cookie).header(header::ORIGIN, server.url.trim_end_matches('/')).form(&[("csrf", "wrong"), ("decision", "allow")]).send().await.unwrap();
    assert_eq!(forged.status(), 403);
    let allowed = server.http.post(format!("{}oauth/authorize", server.url)).header(header::COOKIE, &cookie).header(header::ORIGIN, server.url.trim_end_matches('/')).form(&fields).send().await.unwrap();
    assert_eq!(allowed.status(), 303);
    let back = query_of(allowed.headers()[header::LOCATION].to_str().unwrap());
    assert_eq!(back["state"], "s1");
    assert_eq!(back["iss"], server.url.trim_end_matches('/'));
    let code = back["code"].clone();

    // The website's server exchanges it: a wrong verifier is refused.
    let token_at = format!("{}oauth/token", server.url);
    let wrong_verifier = server.http.post(&token_at).basic_auth("wiki", Some(secret)).form(&[("grant_type", "authorization_code"), ("code", code.as_str()), ("redirect_uri", "https://wiki.example/cb"), ("code_verifier", "x".repeat(50).as_str())]).send().await.unwrap();
    assert_eq!(wrong_verifier.status(), 400);
    // (That used the code: a website that got it wrong starts again.)
    let (verifier2, challenge2) = pkce();
    let fields2: Vec<(&str, &str)> = fields.iter().map(|(k, v)| if *k == "code_challenge" { (*k, challenge2.as_str()) } else { (*k, *v) }).collect();
    let _ = verifier;
    let allowed = server.http.post(format!("{}oauth/authorize", server.url)).header(header::COOKIE, &cookie).header(header::ORIGIN, server.url.trim_end_matches('/')).form(&fields2).send().await.unwrap();
    let code = query_of(allowed.headers()[header::LOCATION].to_str().unwrap())["code"].clone();
    let tokens: Value = server.http.post(&token_at).basic_auth("wiki", Some(secret)).form(&[("grant_type", "authorization_code"), ("code", code.as_str()), ("redirect_uri", "https://wiki.example/cb"), ("code_verifier", verifier2.as_str())]).send().await.unwrap().json().await.unwrap();
    let id_token = tokens["id_token"].as_str().unwrap_or_else(|| panic!("{tokens}"));
    let access = tokens["access_token"].as_str().unwrap();

    // The id token checks against the published keys, and says what was allowed.
    let jwks: Value = server.get("oauth/jwks").await.json().await.unwrap();
    let jws = wyvrn_sign_in::jws::Jws::parse(id_token).unwrap();
    let key = jwks["keys"].as_array().unwrap().iter().find(|k| k["kid"].as_str() == jws.kid()).unwrap();
    jws.verify(&wyvrn_sign_in::keys::PublicKey::from_jwk(key).unwrap()).unwrap();
    assert_eq!(jws.payload["iss"], server.url.trim_end_matches('/'));
    assert_eq!(jws.payload["aud"], "wiki");
    assert_eq!(jws.payload["nonce"], "n1");
    assert_eq!(jws.payload["name"], "Ada");
    assert_eq!(jws.payload["did"], member.did());
    assert!(jws.payload["roles"].is_array());
    let sub = jws.payload["sub"].as_str().unwrap().to_string();
    assert_ne!(sub, member.did());

    // The code once only: used again, it gets nothing, and what it got is taken back.
    let again = server.http.post(&token_at).basic_auth("wiki", Some(secret)).form(&[("grant_type", "authorization_code"), ("code", code.as_str()), ("redirect_uri", "https://wiki.example/cb"), ("code_verifier", verifier2.as_str())]).send().await.unwrap();
    assert_eq!(again.status(), 400);
    let info = server.http.get(format!("{}oauth/userinfo", server.url)).bearer_auth(access).send().await.unwrap();
    assert_eq!(info.status(), 401);

    // Signing in again is a redirect and back: consent is remembered.
    let (verifier3, challenge3) = pkce();
    let again = server.http.get(format!("{}{}", server.url, authorize.replace(&challenge, &challenge3))).header(header::COOKIE, &cookie).send().await.unwrap();
    assert_eq!(again.status(), 303);
    let code = query_of(again.headers()[header::LOCATION].to_str().unwrap())["code"].clone();
    let tokens: Value = server.http.post(&token_at).form(&[("grant_type", "authorization_code"), ("code", code.as_str()), ("redirect_uri", "https://wiki.example/cb"), ("code_verifier", verifier3.as_str()), ("client_id", "wiki"), ("client_secret", secret)]).send().await.unwrap().json().await.unwrap();
    let access = tokens["access_token"].as_str().unwrap();
    let info: Value = server.http.get(format!("{}oauth/userinfo", server.url)).bearer_auth(access).send().await.unwrap().json().await.unwrap();
    assert_eq!(info["sub"], sub, "the same id at the same website");
    assert_eq!(info["name"], "Ada");

    // Taken back by the member: its token stops working.
    let mkey = store.member_key(&member.did()).unwrap();
    assert!(store.revoke_consent(&mkey, "wiki").unwrap());
    assert_eq!(server.http.get(format!("{}oauth/userinfo", server.url)).bearer_auth(access).send().await.unwrap().status(), 401);

    // A wrong secret is no website at all.
    let bad = server.http.post(&token_at).basic_auth("wiki", Some("nope")).form(&[("grant_type", "authorization_code"), ("code", "x")]).send().await.unwrap();
    assert_eq!(bad.status(), 401);
}

#[tokio::test]
async fn websites_register_themselves_only_where_an_admin_allows_and_are_unverified() {
    let server = start(Options::default()).await;
    let (admin, member) = with_channels(&server).await;
    let register = |body: Value| server.http.post(format!("{}oauth/register", server.url)).json(&body).send();
    assert_eq!(register(json!({ "redirect_uris": ["https://app.example/cb"], "client_name": "App" })).await.unwrap().status(), 403);

    // The admin turns it on, through the `web` workflow.
    let id = uuid::Uuid::new_v4().to_string();
    let started = admin.ask(&format!("{W}start"), json!({ "template_id": "web", "instance_id": id })).await;
    assert_eq!(started["body"]["state"], "form", "{started}");
    let saved = admin.ask(&format!("{W}advance"), json!({ "instance_id": id, "event": "save", "input": { "dynamic": true, "radius": "8", "font": "serif", "accent_light": "#123456", "css": "" } })).await;
    assert_eq!(saved["body"]["state"], "form", "{saved}");
    let blocked = admin.ask(&format!("{W}advance"), json!({ "instance_id": id, "event": "save", "input": { "css": "@import url(https://evil.example/x.css);" } })).await;
    assert!(blocked["type"].as_str().unwrap().ends_with("problem-report"), "{blocked}");
    let refused = member.ask(&format!("{W}start"), json!({ "template_id": "web", "instance_id": uuid::Uuid::new_v4().to_string() })).await;
    assert!(refused["type"].as_str().unwrap().ends_with("problem-report"));

    assert!(register(json!({ "redirect_uris": ["http://app.example/cb"], "client_name": "App" })).await.unwrap().status().is_client_error(), "only https, or localhost");
    let registered = register(json!({ "redirect_uris": ["https://app.example/cb"], "client_name": "App", "token_endpoint_auth_method": "none" })).await.unwrap();
    assert_eq!(registered.status(), 201);
    let registered: Value = registered.json().await.unwrap();
    assert!(registered.get("client_secret").is_none());
    let client_id = registered["client_id"].as_str().unwrap();

    let (cookie, _) = server.session(&member.did());
    let (_, challenge) = pkce();
    let consent = server.http.get(format!("{}oauth/authorize?client_id={client_id}&redirect_uri=https%3A%2F%2Fapp.example%2Fcb&response_type=code&scope=openid&code_challenge={challenge}&code_challenge_method=S256", server.url)).header(header::COOKIE, &cookie).send().await.unwrap();
    let page = consent.text().await.unwrap();
    assert!(page.contains("Unverified"), "{page}");
    assert!(page.contains("https://app.example"));

    // And the theme saved shows in its stylesheet.
    let (_, css) = server.text("theme.css").await;
    assert!(css.contains("--radius: 8px"));
    assert!(css.contains("Source Serif 4"));
    assert!(css.contains("--accent: #123456"));
}
