//! The pages: the community's own, a channel's, a post's and a thread's, with
//! the sitemap, the theme, and the pictures and files they show.
//!
//! Who sees what: anyone sees the page and the channels made public, from when
//! they were made public; a member signed in sees every open channel, as members
//! do (community/1.0's Endnote 2). A closed channel is on no page.

use std::collections::HashMap;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use serde_json::{json, Value};

use super::render::{self, escape};
use super::{as_page, channel_path, page_error, page_on, voice_choice, Web};
use crate::server::AppState;
use crate::store::{Channel, Member, Post};

/// How many posts a channel's page shows at once, and how many around a post.
const PAGE_POSTS: usize = 50;
const AROUND: usize = 10;
/// How far back the pages go, in pages of posts.
const MAX_BEFORE_PAGES: i64 = 400;
/// The largest file shown from the community's own store.
const MAX_MEDIA_BYTES: u64 = 200 * 1024 * 1024;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/ch/{id}", get(channel_bare))
        .route("/ch/{id}/{name}", get(channel))
        .route("/ch/{id}/m/{seq}", get(post_page))
        .route("/ch/{id}/t/{thread}", get(thread_page))
        .route("/m/{post}", get(short_link))
        .route("/sitemap.xml", get(sitemap))
        .route("/robots.txt", get(robots))
        .route("/theme.css", get(theme_css))
        .route("/assets/{file}", get(asset))
        .route("/fonts/{file}", get(font))
        .route("/icon", get(icon))
        .route("/p/{token}", get(picture))
        .route("/media/{post}/{n}", get(media))
        .route("/x/{file}", get(outside_file))
}

// --- Who is looking -------------------------------------------------------------------

/// Who is looking: nobody in particular, or a member signed in.
pub struct Viewer {
    pub member: Option<Member>,
    pub csrf: Option<String>,
}

impl Viewer {
    pub fn of(state: &AppState, headers: &HeaderMap) -> Self {
        match super::sign_in::session_of(state, headers) {
            Some((member, csrf)) => Self { member: Some(member), csrf: Some(csrf) },
            None => Self { member: None, csrf: None },
        }
    }

    fn signed_in(&self) -> bool {
        self.member.is_some()
    }
}

/// The channels a viewer may read, each with when from (`None`: all of it).
fn readable(state: &AppState, viewer: &Viewer) -> anyhow::Result<HashMap<String, Option<i64>>> {
    let public = state.community.store.web_channels()?;
    Ok(state
        .community
        .store
        .channels()?
        .into_iter()
        .filter(|c| !c.closed && c.voice.is_none())
        .filter_map(|c| {
            if viewer.signed_in() {
                Some((c.id.clone(), None))
            } else {
                public.get(&c.id).map(|since| (c.id.clone(), Some(*since)))
            }
        })
        .collect())
}

fn shows(post: &Post, since: Option<i64>) -> bool {
    since.is_none_or(|since| post.posted_at >= since)
}

// --- What the templates are given -------------------------------------------------------

pub struct Site {
    pub name: String,
    /// Its first letter, for where it has no picture.
    pub letter: String,
    pub description: String,
    pub has_icon: bool,
    /// From this page to the web view's root: `./`, `../../` and so on.
    pub root: String,
    /// Changes when the theme does, so the stylesheet is fetched again.
    pub theme: String,
    pub signed_in: Option<String>,
    pub csrf: String,
    /// Opens the community in the app.
    pub in_app: String,
    pub canonical: String,
}

pub struct ChannelLink {
    pub name: String,
    pub href: String,
    pub topic: String,
    pub current: bool,
    pub forum: bool,
    pub public: bool,
}

pub struct VoiceLink {
    pub name: String,
    /// How many are in it, where that may be shown.
    pub count: Option<usize>,
    pub names: Vec<String>,
}

pub struct Group {
    pub name: Option<String>,
    pub channels: Vec<ChannelLink>,
    pub voice: Vec<VoiceLink>,
}

pub struct Media {
    /// `image`, `video`, `audio`, `sticker` or `file`.
    pub kind: &'static str,
    pub href: String,
    pub name: String,
    pub size: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

pub struct PostView {
    pub id: String,
    pub anchor: String,
    pub href: String,
    pub author: String,
    pub picture: Option<String>,
    pub initial: String,
    pub when: String,
    pub iso: String,
    pub html: String,
    pub title: Option<String>,
    pub edited: bool,
    pub reply_author: Option<String>,
    pub reply_snippet: Option<String>,
    pub media: Vec<Media>,
    /// Nothing but custom emoji: drawn large, as the app does.
    pub emoji_only: bool,
    pub reactions: Vec<(String, usize)>,
    pub thread_href: Option<String>,
    pub thread_count: i64,
    pub thread_name: Option<String>,
    pub pinned: bool,
    pub focus: bool,
}

#[derive(Template)]
#[template(path = "web/home.html")]
struct HomePage {
    site: Site,
    groups: Vec<Group>,
    members: i64,
    title: String,
    description: String,
    json_ld: String,
    public_count: usize,
    noindex: bool,
}

#[derive(Template)]
#[template(path = "web/channel.html")]
struct ChannelPage {
    site: Site,
    groups: Vec<Group>,
    title: String,
    description: String,
    json_ld: String,
    channel_name: String,
    topic: String,
    posts: Vec<PostView>,
    older: Option<String>,
    newer: Option<String>,
    heading: String,
    back: Option<String>,
    noindex: bool,
}

#[derive(Template)]
#[template(path = "web/message.html")]
struct MessagePage {
    title: String,
    said: String,
    style: &'static str,
}

/// The message page's own style, allowed by its hash: it is shown at any depth,
/// so it can't link the stylesheet.
const MESSAGE_STYLE: &str = "body{font:16px/1.5 system-ui,sans-serif;margin:0;display:grid;place-items:center;min-height:100vh;background:#f6f6f8;color:#1a1a1e}@media (prefers-color-scheme:dark){body{background:#16161a;color:#ececee}}main{max-width:32rem;padding:2rem}h1{font-size:1.5rem}";

/// A page with only something to say: an error, a sign-in's outcome.
pub fn message_page(status: StatusCode, title: &str, said: &str) -> Response {
    use base64::Engine;
    use sha2::Digest;
    let page = MessagePage { title: title.to_string(), said: said.to_string(), style: MESSAGE_STYLE };
    let mut response = (status, Html(page.render().unwrap_or_else(|_| escape(said)))).into_response();
    let hash = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(MESSAGE_STYLE.as_bytes()));
    if let Ok(value) = HeaderValue::from_str(&format!("default-src 'none'; style-src 'sha256-{hash}'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'")) {
        response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, value);
    }
    as_page(response)
}

// --- Building them ------------------------------------------------------------------------

fn web_of(state: &AppState) -> Option<&Web> {
    state.web.as_deref()
}

/// The page is off, or there is no web view: nothing here.
fn nothing() -> Response {
    page_error(StatusCode::NOT_FOUND, "Not here", "There's nothing at this address.")
}

pub fn site(state: &AppState, viewer: &Viewer, root: &str, canonical: String) -> Site {
    let changed = state.community.changed();
    let name = changed.name.clone().unwrap_or_else(|| state.community.config.name.clone());
    let description = changed.description.clone().unwrap_or_else(|| state.community.config.description.clone());
    let did = state.voice.speaking_as().unwrap_or_else(|| state.agent.did());
    let theme = state.community.store.setting(super::THEME).ok().flatten().unwrap_or_default() + &state.community.store.setting(super::CSS).ok().flatten().unwrap_or_default();
    let in_app = format!("wyvrn://community?did={}", url::form_urlencoded::byte_serialize(did.as_bytes()).collect::<String>());
    Site {
        letter: name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default(),
        name,
        description,
        has_icon: changed.picture.is_some(),
        root: root.to_string(),
        theme: super::keyed("theme", &theme)[..12].to_string(),
        signed_in: viewer.member.as_ref().map(|m| m.name.clone().unwrap_or_else(|| "Member".to_string())),
        csrf: viewer.csrf.clone().unwrap_or_default(),
        in_app,
        canonical,
    }
}

/// The channels and voice channels, by category, as the side of every page shows them.
fn groups(state: &AppState, viewer: &Viewer, root: &str, current: Option<&str>) -> anyhow::Result<Vec<Group>> {
    let store = &state.community.store;
    let readable = readable(state, viewer)?;
    let public = store.web_channels()?;
    let voice_choice = voice_choice(store);
    let rooms = state.rooms.in_rooms();
    let mut by_category: Vec<Group> = Vec::new();
    let mut place: HashMap<Option<String>, usize> = HashMap::new();
    let categories = store.categories()?;
    let named = |id: &Option<String>| id.as_ref().and_then(|id| categories.iter().find(|c| c.id == *id)).map(|c| c.name.clone());
    for channel in store.channels()?.into_iter().filter(|c| !c.closed) {
        let at = *place.entry(channel.category.clone()).or_insert_with(|| {
            by_category.push(Group { name: named(&channel.category), channels: Vec::new(), voice: Vec::new() });
            by_category.len() - 1
        });
        if channel.voice.is_some() {
            let present = rooms.get(&channel.id).cloned().unwrap_or_default();
            let (count, names) = match (voice_choice.as_str(), viewer.signed_in()) {
                ("off", _) => continue,
                ("members", false) => continue,
                ("counts", false) => (Some(present.len()), Vec::new()),
                _ => (Some(present.len()), present.iter().map(|did| name_of(state, did)).collect()),
            };
            by_category[at].voice.push(VoiceLink { name: channel.name.clone(), count, names });
        } else if readable.contains_key(&channel.id) {
            by_category[at].channels.push(ChannelLink {
                href: format!("{root}{}", channel_path(&channel.id, &channel.name)),
                name: channel.name.clone(),
                topic: channel.topic.clone(),
                current: current == Some(channel.id.as_str()),
                forum: channel.forum.is_some(),
                public: public.contains_key(&channel.id),
            });
        }
    }
    by_category.retain(|g| !g.channels.is_empty() || !g.voice.is_empty());
    Ok(by_category)
}

fn name_of(state: &AppState, did: &str) -> String {
    state.community.store.member(did).ok().flatten().and_then(|m| m.name).unwrap_or_else(|| "A member".to_string())
}

/// A member's picture address, as the public pages show it.
fn picture_href(state: &AppState, did: &str, root: &str) -> Option<String> {
    let store = &state.community.store;
    store.member_picture(did).ok().flatten()?;
    let secret = super::secret(store).ok()?;
    let token = store.picture_token(&store.member_key(did).ok()?, "pages", &secret).ok()?;
    Some(format!("{root}p/{token}"))
}

fn post_view(state: &AppState, post: &Post, channel: &Channel, root: &str, focus: Option<i64>) -> PostView {
    let ch = format!("{root}ch/{}", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>());
    let author = post.author_name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "A member".to_string());
    let extra = post.extra.clone().unwrap_or(Value::Null);
    let attachments: HashMap<&str, &Value> = extra["attachments"].as_array().into_iter().flatten().filter_map(|a| Some((a["id"].as_str()?, a))).collect();
    // Pack files (custom emoji, stickers) are shown through this server (outside.rs).
    let secret = super::secret(&state.community.store).ok();
    let through_here = |address: &str| secret.as_deref().and_then(|secret| super::outside::href(secret, root, address));
    let media = extra["media"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(n, item)| {
            if item["kind"] == "sticker" {
                let sticker = &item["sticker"];
                let (pack, file) = (sticker["pack"].as_str()?, sticker["file"].as_str()?);
                // The name and the hash it was sent with have to agree, as the app has it.
                let (stem, _) = super::outside::pack_file(file)?;
                sticker["sha256"].as_str().filter(|hash| hash.starts_with(stem))?;
                return Some(Media {
                    kind: "sticker",
                    href: through_here(&format!("{}/{file}", pack.trim_end_matches('/')))?,
                    name: sticker["emoji"].as_str().map(|e| e.chars().take(64).collect()).unwrap_or_else(|| "Sticker".to_string()),
                    size: String::new(),
                    width: sticker["width"].as_i64(),
                    height: sticker["height"].as_i64(),
                });
            }
            let attachment = attachments.get(item["attachment"].as_str()?)?;
            let media_type = attachment["media_type"].as_str().unwrap_or("application/octet-stream");
            let kind = if inline_type(media_type) {
                match media_type.split('/').next() {
                    Some("image") => "image",
                    Some("video") => "video",
                    Some("audio") => "audio",
                    _ => "file",
                }
            } else {
                "file"
            };
            Some(Media {
                kind,
                href: format!("{root}media/{}/{n}", url::form_urlencoded::byte_serialize(post.id.as_bytes()).collect::<String>()),
                name: attachment["filename"].as_str().unwrap_or("file").chars().take(120).collect(),
                size: attachment["byte_count"].as_u64().map(render::size).unwrap_or_default(),
                width: item["width"].as_i64(),
                height: item["height"].as_i64(),
            })
        })
        .collect();
    let thread = post.grew.as_ref();
    PostView {
        id: post.id.clone(),
        anchor: format!("m{}", post.seq),
        href: format!("{ch}/m/{}", post.seq),
        initial: author.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into()),
        picture: picture_href(state, &post.author_did, root),
        author,
        when: render::when(post.posted_at),
        iso: render::iso(post.posted_at),
        html: if super::outside::is_gif(&post.content) {
            super::outside::gif_html(&post.content)
        } else {
            render::markdown_with(&post.content, &|address, _| {
                let file = url::Url::parse(address).ok()?.path_segments()?.next_back()?.to_string();
                super::outside::pack_file(&file)?;
                through_here(address)
            })
        },
        title: extra["title"].as_str().map(str::to_string),
        edited: post.edited_at.is_some(),
        reply_author: post.reply_to.as_ref().and(post.reply_author.as_deref()).map(|did| name_of(state, did)),
        reply_snippet: post.reply_snippet.clone(),
        media,
        emoji_only: render::only_pictures(&post.content, 25),
        reactions: post.reactions.iter().map(|(emoji, who)| (if emoji.starts_with("![") { emoji.split(']').next().unwrap_or("").trim_start_matches("![").to_string() } else { emoji.clone() }, who.len())).collect(),
        thread_href: thread.map(|t| format!("{ch}/t/{}", url::form_urlencoded::byte_serialize(t.id.as_bytes()).collect::<String>())),
        thread_count: thread.map_or(0, |t| t.count),
        thread_name: thread.and_then(|t| t.name.clone()),
        pinned: post.pinned,
        focus: focus == Some(post.seq),
    }
}

#[derive(Deserialize)]
struct OutsideQuery {
    from: String,
    key: String,
}

/// A pack file (a custom emoji, a sticker) a page named: kept here, or fetched once
/// and kept if it is what its name says (outside.rs). Only addresses this server
/// signed are fetched.
async fn outside_file(State(state): State<Arc<AppState>>, Path(file): Path<String>, Query(asked): Query<OutsideQuery>) -> Response {
    use base64::Engine;
    let store = &state.community.store;
    if !page_on(store) {
        return nothing();
    }
    let Some((stem, _)) = super::outside::pack_file(&file) else { return nothing() };
    let Ok(secret) = super::secret(store) else { return nothing() };
    if !super::outside::signed(&secret, &file, &asked.from, &asked.key) {
        return nothing();
    }
    let kept = match store.outside_picture(stem) {
        Ok(Some(kept)) => Some(kept),
        Ok(None) => match super::outside::fetch(&asked.from, crate::gates::now()).await {
            Ok((media_type, bytes)) => {
                let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
                if let Err(error) = store.keep_outside_picture(stem, media_type, &data, crate::gates::now()) {
                    tracing::warn!(%error, "couldn't keep a pack file for the pages");
                }
                Some((media_type.to_string(), data))
            }
            Err(why) => {
                tracing::debug!(file = %file, %why, "a pack file a page names couldn't be had");
                None
            }
        },
        Err(_) => None,
    };
    let Some((media_type, data)) = kept else { return nothing() };
    let mut response = picture_response(&media_type, &data);
    // Named for its hash: it never changes.
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    response
}

/// A picture, a video or a sound a page may show where it is; anything else is
/// a file to save. (Never SVG, which can carry script.)
fn inline_type(media_type: &str) -> bool {
    matches!(
        media_type.to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/avif" | "video/mp4" | "video/webm" | "audio/mpeg" | "audio/ogg" | "audio/webm" | "audio/mp4" | "audio/wav"
    )
}

fn discussion(post: &Post, web: &Web, channel: &Channel, name: &str) -> Value {
    json!({
        "@type": "DiscussionForumPosting",
        "url": web.absolute(&format!("ch/{}/m/{}", channel.id, post.seq)),
        "author": { "@type": "Person", "name": name },
        "datePublished": render::iso(post.posted_at),
        "text": render::excerpt(&post.content, 500),
        "commentCount": post.grew.as_ref().map_or(0, |t| t.count),
    })
}

fn page(html: Result<String, askama::Error>, private: bool) -> Response {
    let html = match html {
        Ok(html) => html,
        Err(error) => {
            tracing::error!(%error, "a page couldn't be made");
            return page_error(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong", "This page couldn't be made just now.");
        }
    };
    let mut response = Html(html).into_response();
    let headers = response.headers_mut();
    if private {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
        headers.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    } else {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=60"));
    }
    headers.insert(header::VARY, HeaderValue::from_static("Cookie"));
    as_page(response)
}

// --- The pages ----------------------------------------------------------------------------

/// The community's own page.
pub async fn home(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some(web) = web_of(&state) else { return nothing() };
    if !page_on(&state.community.store) {
        return nothing();
    }
    let viewer = Viewer::of(&state, &headers);
    // At the root of the server or of the web view: the same page, linked from
    // the web view's root as `./`.
    let site = site(&state, &viewer, "./", web.absolute(""));
    let groups = match groups(&state, &viewer, "./", None) {
        Ok(groups) => groups,
        Err(error) => return internal(error),
    };
    let public_count = groups.iter().map(|g| g.channels.len()).sum();
    let members = state.community.store.member_count().unwrap_or(0);
    let json_ld = render::json_ld(&json!({ "@context": "https://schema.org", "@type": "WebSite", "name": site.name, "url": web.absolute(""), "description": site.description }));
    let title = site.name.clone();
    let description = if site.description.is_empty() { format!("{} on Wyvrn", site.name) } else { render::excerpt(&site.description, 200) };
    let private = viewer.signed_in();
    page(HomePage { site, groups, members, title, description, json_ld, public_count, noindex: false }.render(), private)
}

/// `/ch/<id>`: to the channel's page under its name.
async fn channel_bare(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match state.community.store.channel(&id) {
        Ok(Some(channel)) => as_page(Redirect::permanent(&format!("../{}", channel_path(&channel.id, &channel.name))).into_response()),
        _ => nothing(),
    }
}

#[derive(Deserialize)]
struct Before {
    before: Option<i64>,
}

/// A channel's posts, newest last, a page at a time.
async fn channel(State(state): State<Arc<AppState>>, Path((id, name)): Path<(String, String)>, Query(paging): Query<Before>, headers: HeaderMap) -> Response {
    let Some(web) = web_of(&state) else { return nothing() };
    if !page_on(&state.community.store) {
        return nothing();
    }
    let viewer = Viewer::of(&state, &headers);
    let Some((channel, since)) = visible_channel(&state, &viewer, &id) else { return nothing() };
    // The id is what counts: a channel renamed is at its new name.
    let wanted = super::slug(&channel.name);
    if name != wanted && url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>() != wanted {
        return as_page(Redirect::permanent(&format!("../../{}", channel_path(&channel.id, &channel.name))).into_response());
    }
    let root = "../../";
    let store = &state.community.store;
    let before = paging.before.filter(|b| *b > 0);
    if before.is_some_and(|b| b < channel.latest_seq - PAGE_POSTS as i64 * MAX_BEFORE_PAGES) {
        return page_error(StatusCode::NOT_FOUND, "Too far back", "The web shows a channel's more recent posts. Open it in Wyvrn to read further back.");
    }
    let mut posts = match store.channel_posts_before(&channel.id, None, before, PAGE_POSTS + 1) {
        Ok(posts) => posts,
        Err(error) => return internal(error.into()),
    };
    let more = posts.len() > PAGE_POSTS;
    posts.truncate(PAGE_POSTS);
    posts.retain(|p| shows(p, since));
    let older = (more && posts.last().is_some_and(|p| shows_any_before(&state, &channel, p.seq, since))).then(|| format!("?before={}", posts.last().map_or(0, |p| p.seq)));
    let newer = before.map(|b| format!("?before={}", b + PAGE_POSTS as i64)).filter(|_| before.is_some_and(|b| b <= channel.latest_seq));
    posts.reverse();
    let shown: Vec<&Post> = posts.iter().filter(|p| !p.deleted).collect();
    let canonical = web.absolute(&match before {
        Some(b) => format!("{}?before={b}", channel_path(&channel.id, &channel.name)),
        None => channel_path(&channel.id, &channel.name),
    });
    let views: Vec<PostView> = shown.iter().map(|p| post_view(&state, p, &channel, root, None)).collect();
    let json_ld = render::json_ld(&json!({ "@context": "https://schema.org", "@type": "CollectionPage", "name": channel.name, "url": canonical, "hasPart": shown.iter().take(20).map(|p| discussion(p, web, &channel, &p.author_name.clone().unwrap_or_default())).collect::<Vec<_>>() }));
    let mut site = site(&state, &viewer, root, canonical);
    site.in_app.push_str(&format!("&channel={}", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>()));
    let groups = match groups(&state, &viewer, root, Some(&channel.id)) {
        Ok(groups) => groups,
        Err(error) => return internal(error),
    };
    let description = if channel.topic.is_empty() { format!("#{} in {}", channel.name, site.name) } else { render::excerpt(&channel.topic, 200) };
    let private = since.is_none() && !is_public(&state, &channel.id);
    let title = format!("#{} · {}", channel.name, site.name);
    page(
        ChannelPage { heading: format!("#{}", channel.name), channel_name: channel.name.clone(), topic: channel.topic.clone(), posts: views, older, newer, title, description, json_ld, site, groups, back: None, noindex: private }.render(),
        private || viewer.signed_in(),
    )
}

fn is_public(state: &AppState, channel: &str) -> bool {
    state.community.store.web_channels().map(|c| c.contains_key(channel)).unwrap_or(false)
}

fn shows_any_before(state: &AppState, channel: &Channel, seq: i64, since: Option<i64>) -> bool {
    state.community.store.channel_posts_before(&channel.id, None, Some(seq), 1).ok().and_then(|p| p.into_iter().next()).is_some_and(|p| shows(&p, since))
}

/// The channel, if the viewer may read it, and from when.
fn visible_channel(state: &AppState, viewer: &Viewer, id: &str) -> Option<(Channel, Option<i64>)> {
    let since = *readable(state, viewer).ok()?.get(id)?;
    Some((state.community.store.channel(id).ok()??, since))
}

/// One post on a page of its own, with those around it.
async fn post_page(State(state): State<Arc<AppState>>, Path((id, seq)): Path<(String, i64)>, headers: HeaderMap) -> Response {
    let Some(web) = web_of(&state) else { return nothing() };
    if !page_on(&state.community.store) {
        return nothing();
    }
    let viewer = Viewer::of(&state, &headers);
    let Some((channel, since)) = visible_channel(&state, &viewer, &id) else { return nothing() };
    let store = &state.community.store;
    let Some(post) = store.post_at(&channel.id, seq).ok().flatten() else {
        return nothing();
    };
    if !shows(&post, since) {
        return nothing();
    }
    if post.deleted {
        return page_error(StatusCode::GONE, "Taken back", "This post was taken back, or removed.");
    }
    let root = "../../../";
    let (around, _, _) = match store.channel_posts_around(&channel.id, post.thread.as_deref(), post.posted_at, AROUND) {
        Ok(found) => found,
        Err(error) => return internal(error.into()),
    };
    let views: Vec<PostView> = around.iter().filter(|p| !p.deleted && shows(p, since)).map(|p| post_view(&state, p, &channel, root, Some(seq))).collect();
    let author = post.author_name.clone().unwrap_or_else(|| "A member".to_string());
    let canonical = web.absolute(&format!("ch/{}/m/{seq}", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>()));
    let mut ld = discussion(&post, web, &channel, &author);
    ld["@context"] = json!("https://schema.org");
    let mut site = site(&state, &viewer, root, canonical);
    site.in_app.push_str(&format!("&channel={}&seq={seq}", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>()));
    let groups = match groups(&state, &viewer, root, Some(&channel.id)) {
        Ok(groups) => groups,
        Err(error) => return internal(error),
    };
    let excerpt = render::excerpt(&post.content, 200);
    let title = format!("{author} in #{} · {}", channel.name, site.name);
    let private = since.is_none() && !is_public(&state, &channel.id);
    page(
        ChannelPage {
            heading: format!("#{}", channel.name),
            channel_name: channel.name.clone(),
            topic: String::new(),
            posts: views,
            older: None,
            newer: None,
            title,
            description: if excerpt.is_empty() { format!("A post in #{}", channel.name) } else { excerpt },
            json_ld: render::json_ld(&ld),
            back: Some(format!("{root}{}#m{seq}", channel_path(&channel.id, &channel.name))),
            site,
            groups,
            noindex: private,
        }
        .render(),
        private || viewer.signed_in(),
    )
}

/// A thread, or a forum post, from the post it grew from.
async fn thread_page(State(state): State<Arc<AppState>>, Path((id, thread)): Path<(String, String)>, headers: HeaderMap) -> Response {
    let Some(web) = web_of(&state) else { return nothing() };
    if !page_on(&state.community.store) {
        return nothing();
    }
    let viewer = Viewer::of(&state, &headers);
    let Some((channel, since)) = visible_channel(&state, &viewer, &id) else { return nothing() };
    let store = &state.community.store;
    let Some(root_post) = store.post_by_id(&thread).ok().flatten().filter(|p| p.channel == channel.id && shows(p, since)) else { return nothing() };
    let root = "../../../";
    let mut posts = match store.channel_posts_before(&channel.id, Some(&thread), None, 200) {
        Ok(posts) => posts,
        Err(error) => return internal(error.into()),
    };
    posts.reverse();
    let mut views = vec![post_view(&state, &root_post, &channel, root, None)];
    views.extend(posts.iter().filter(|p| !p.deleted && shows(p, since)).map(|p| post_view(&state, p, &channel, root, None)));
    let name = root_post.grew.as_ref().and_then(|t| t.name.clone()).or_else(|| root_post.extra.as_ref().and_then(|e| e["title"].as_str().map(str::to_string))).unwrap_or_else(|| render::excerpt(&root_post.content, 60));
    let canonical = web.absolute(&format!("ch/{}/t/{}", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>(), url::form_urlencoded::byte_serialize(thread.as_bytes()).collect::<String>()));
    let mut ld = discussion(&root_post, web, &channel, &root_post.author_name.clone().unwrap_or_default());
    ld["@context"] = json!("https://schema.org");
    ld["headline"] = json!(name);
    ld["url"] = json!(canonical);
    let site = site(&state, &viewer, root, canonical);
    let groups = match groups(&state, &viewer, root, Some(&channel.id)) {
        Ok(groups) => groups,
        Err(error) => return internal(error),
    };
    let private = since.is_none() && !is_public(&state, &channel.id);
    page(
        ChannelPage {
            heading: name.clone(),
            channel_name: channel.name.clone(),
            topic: String::new(),
            posts: views,
            older: None,
            newer: None,
            title: format!("{name} · #{} · {}", channel.name, site.name),
            description: render::excerpt(&root_post.content, 200),
            json_ld: render::json_ld(&ld),
            back: Some(format!("{root}{}", channel_path(&channel.id, &channel.name))),
            site,
            groups,
            noindex: private,
        }
        .render(),
        private || viewer.signed_in(),
    )
}

/// `/m/<post id>`: to the post's page.
async fn short_link(State(state): State<Arc<AppState>>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    if !page_on(&state.community.store) {
        return nothing();
    }
    let viewer = Viewer::of(&state, &headers);
    let Some(post) = state.community.store.post_by_id(&id).ok().flatten() else { return nothing() };
    let Some((channel, since)) = visible_channel(&state, &viewer, &post.channel) else { return nothing() };
    if !shows(&post, since) {
        return nothing();
    }
    let to = format!("../ch/{}/m/{}", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>(), post.seq);
    as_page(Redirect::to(&to).into_response())
}

// --- For search engines --------------------------------------------------------------------

async fn sitemap(State(state): State<Arc<AppState>>) -> Response {
    let Some(web) = web_of(&state) else { return nothing() };
    if !page_on(&state.community.store) {
        return nothing();
    }
    let store = &state.community.store;
    let public = store.web_channels().unwrap_or_default();
    let mut urls = vec![(web.absolute(""), None::<i64>)];
    for channel in store.channels().unwrap_or_default().into_iter().filter(|c| !c.closed && public.contains_key(&c.id)) {
        let latest = store.channel_posts_before(&channel.id, None, None, 1).ok().and_then(|p| p.into_iter().next()).map(|p| p.posted_at);
        urls.push((web.absolute(&channel_path(&channel.id, &channel.name)), latest));
    }
    let body: String = urls
        .iter()
        .map(|(loc, last)| match last {
            Some(at) => format!("<url><loc>{}</loc><lastmod>{}</lastmod></url>", escape(loc), render::iso(*at)),
            None => format!("<url><loc>{}</loc></url>", escape(loc)),
        })
        .collect();
    let xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">{body}</urlset>\n");
    as_page(([(header::CONTENT_TYPE, "application/xml; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=600")], xml).into_response())
}

async fn robots(State(state): State<Arc<AppState>>) -> Response {
    let Some(web) = web_of(&state) else { return nothing() };
    let text = if page_on(&state.community.store) {
        format!("User-agent: *\nDisallow: {0}sign-in\nDisallow: {0}oauth/\nDisallow: {0}sites\nDisallow: {0}media/\nAllow: {0}\nSitemap: {1}\n", web.path, web.absolute("sitemap.xml"))
    } else {
        format!("User-agent: *\nDisallow: {}\n", web.path)
    };
    as_page(([(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=600")], text).into_response())
}

// --- The look ----------------------------------------------------------------------------------

/// The community's theme: its colours as CSS variables, then its own CSS
/// (`?plain` leaves that out: the sign-in and consent pages).
async fn theme_css(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let plain = request.uri().query().is_some_and(|q| q.split('&').any(|p| p == "plain"));
    let store = &state.community.store;
    let theme = super::flows::Theme::load(store);
    let mut css = theme.variables();
    if !plain {
        if let Some(own) = store.setting(super::CSS).ok().flatten().filter(|c| !c.trim().is_empty()) {
            css.push_str("\n/* The community's own */\n");
            css.push_str(&own);
        }
    }
    as_page(([(header::CONTENT_TYPE, "text/css; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=300")], css).into_response())
}

const STYLE: &str = include_str!("../../web/web.css");
const SCRIPT: &str = include_str!("../../web/web.js");

async fn asset(Path(file): Path<String>) -> Response {
    let (body, kind) = match file.as_str() {
        "web.css" => (STYLE, "text/css; charset=utf-8"),
        "web.js" => (SCRIPT, "text/javascript; charset=utf-8"),
        _ => return nothing(),
    };
    as_page(([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, "public, max-age=3600")], body).into_response())
}

/// The fonts an admin may choose from, served from here (nothing from elsewhere).
async fn font(Path(file): Path<String>) -> Response {
    let bytes: &'static [u8] = match file.as_str() {
        "inter-latin.woff2" => include_bytes!("../../web/fonts/inter-latin-wght-normal.woff2"),
        "inter-latin-ext.woff2" => include_bytes!("../../web/fonts/inter-latin-ext-wght-normal.woff2"),
        "serif-latin.woff2" => include_bytes!("../../web/fonts/source-serif-4-latin-wght-normal.woff2"),
        "serif-latin-ext.woff2" => include_bytes!("../../web/fonts/source-serif-4-latin-ext-wght-normal.woff2"),
        "mono-latin.woff2" => include_bytes!("../../web/fonts/jetbrains-mono-latin-wght-normal.woff2"),
        "mono-latin-ext.woff2" => include_bytes!("../../web/fonts/jetbrains-mono-latin-ext-wght-normal.woff2"),
        _ => return nothing(),
    };
    as_page(([(header::CONTENT_TYPE, "font/woff2"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], bytes).into_response())
}

/// The community's picture.
async fn icon(State(state): State<Arc<AppState>>) -> Response {
    if !page_on(&state.community.store) {
        return nothing();
    }
    let Some(picture) = state.community.changed().picture else { return nothing() };
    picture_response(&picture.media_type, &picture.base64)
}

fn picture_response(media_type: &str, base64_data: &str) -> Response {
    use base64::Engine;
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(base64_data.trim_end_matches('=')).or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(base64_data.trim_end_matches('='))) else {
        return nothing();
    };
    let media_type = if inline_type(media_type) && media_type.starts_with("image/") { media_type.to_string() } else { "application/octet-stream".to_string() };
    let mut response = ([(header::CACHE_CONTROL, "public, max-age=3600")], bytes).into_response();
    if let Ok(value) = HeaderValue::from_str(&media_type) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    as_page(response)
}

/// A member's picture, by the address the pages (or a website) were given.
async fn picture(State(state): State<Arc<AppState>>, Path(token): Path<String>) -> Response {
    let store = &state.community.store;
    let Some(member) = store.picture_holder(&token).ok().flatten().and_then(|mkey| store.member_by_key(&mkey).ok().flatten()) else { return nothing() };
    match store.member_picture(&member.did).ok().flatten() {
        Some((media_type, data, _)) => picture_response(&media_type, &data),
        None => nothing(),
    }
}

/// A file a post carries, from the community's own store, opened with the key
/// the post carries: for a post the viewer may read. Pictures, videos and sounds
/// are shown; anything else is a file to save.
async fn media(State(state): State<Arc<AppState>>, Path((post_id, n)): Path<(String, usize)>, headers: HeaderMap) -> Response {
    if !page_on(&state.community.store) {
        return nothing();
    }
    let viewer = Viewer::of(&state, &headers);
    let Some(post) = state.community.store.post_by_id(&post_id).ok().flatten().filter(|p| !p.deleted) else { return nothing() };
    let Some((_, since)) = visible_channel(&state, &viewer, &post.channel) else { return nothing() };
    if !shows(&post, since) {
        return nothing();
    }
    let extra = post.extra.unwrap_or(Value::Null);
    let Some(item) = extra["media"].as_array().and_then(|m| m.get(n)) else { return nothing() };
    let Some(attachment) = extra["attachments"].as_array().into_iter().flatten().find(|a| a["id"] == item["attachment"]) else { return nothing() };
    let (Some(files), Some(link)) = (&state.community.files, attachment["data"]["links"].as_array().and_then(|l| l.first()).and_then(Value::as_str)) else { return nothing() };
    // Only what is kept here: a link elsewhere is never fetched.
    let Some(web) = web_of(&state) else { return nothing() };
    let own = format!("{}f/", web.server_url);
    let Some(file_id) = link.strip_prefix(&own).filter(|id| !id.is_empty() && !id.contains('/')) else { return nothing() };
    let Ok(stored) = files.host.stored(file_id, wyvrn_service::now()) else { return nothing() };
    if stored.byte_count > MAX_MEDIA_BYTES {
        return nothing();
    }
    let Ok(bytes) = tokio::fs::read(files.host.store.path(file_id)).await else { return nothing() };
    let bytes = match item.get("ciphering").filter(|c| !c.is_null()) {
        Some(ciphering) => match open(&bytes, ciphering) {
            Some(plain) => plain,
            None => return page_error(StatusCode::UNPROCESSABLE_ENTITY, "Can't be opened", "This file isn't what the post says it is."),
        },
        None => bytes,
    };
    let media_type = attachment["media_type"].as_str().unwrap_or("application/octet-stream");
    let shown = inline_type(media_type);
    let name: String = attachment["filename"].as_str().unwrap_or("file").chars().filter(|c| !c.is_control() && *c != '"' && *c != '\\').take(120).collect();
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(if shown { media_type } else { "application/octet-stream" }).unwrap_or(HeaderValue::from_static("application/octet-stream")));
    let disposition = format!("{}; filename=\"{}\"", if shown { "inline" } else { "attachment" }, name.replace(|c: char| !c.is_ascii(), "_"));
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(if since.is_some() { "public, max-age=3600" } else { "private, max-age=3600" }));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; sandbox"));
    as_page(response)
}

/// A file encrypted as wyvrn-chat encrypts what it puts at a host: AES-256-GCM,
/// the tag apart from the ciphertext (chat-message/1.0's `ciphering`).
fn open(ciphertext: &[u8], ciphering: &Value) -> Option<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit};
    if ciphering["algorithm"] != "aes-256-gcm" {
        return None;
    }
    let hex = |name: &str| -> Option<Vec<u8>> {
        let text = ciphering["parameters"][name].as_str()?;
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
    };
    let (key, iv, tag) = (hex("key")?, hex("iv")?, hex("tag")?);
    if key.len() != 32 || iv.len() != 12 || tag.len() != 16 {
        return None;
    }
    let cipher = aes_gcm::Aes256Gcm::new_from_slice(&key).ok()?;
    let mut sealed = ciphertext.to_vec();
    sealed.extend_from_slice(&tag);
    cipher.decrypt(aes_gcm::Nonce::from_slice(&iv), sealed.as_slice()).ok()
}

fn internal(error: anyhow::Error) -> Response {
    tracing::error!(%error, "a page couldn't be made");
    page_error(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong", "This page couldn't be made just now.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_opens_with_the_key_its_post_carries() {
        use aes_gcm::aead::{Aead, KeyInit};
        let key = [7u8; 32];
        let iv = [9u8; 12];
        let sealed = aes_gcm::Aes256Gcm::new_from_slice(&key).unwrap().encrypt(aes_gcm::Nonce::from_slice(&iv), b"a picture".as_slice()).unwrap();
        let (ciphertext, tag) = sealed.split_at(sealed.len() - 16);
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let ciphering = json!({ "algorithm": "aes-256-gcm", "parameters": { "key": hex(&key), "iv": hex(&iv), "tag": hex(tag) } });
        assert_eq!(open(ciphertext, &ciphering).as_deref(), Some(b"a picture".as_slice()));
        let mut changed = ciphertext.to_vec();
        changed[0] ^= 1;
        assert_eq!(open(&changed, &ciphering), None, "changed at the host: refused");
        assert_eq!(open(ciphertext, &json!({ "algorithm": "rot13" })), None);
    }

    #[test]
    fn only_pictures_videos_and_sounds_are_shown_where_they_are() {
        assert!(inline_type("image/png"));
        assert!(inline_type("video/mp4"));
        assert!(!inline_type("image/svg+xml"));
        assert!(!inline_type("text/html"));
        assert!(!inline_type("application/pdf"));
    }
}
