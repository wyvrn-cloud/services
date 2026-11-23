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
        .route("/ch/{id}/feed.atom", get(feed))
        .route("/search", get(search))
        .route("/widget", get(widget))
        .route("/widget.json", get(widget_json))
        .route("/sitemap.xml", get(sitemap))
        .route("/robots.txt", get(robots))
        .route("/theme.css", get(theme_css))
        .route("/assets/{file}", get(asset))
        .route("/fonts/{file}", get(font))
        .route("/icon", get(icon))
        .route("/p/{token}", get(picture))
        .route("/media/{post}/{n}", get(media))
        .route("/x/{file}", get(outside_file))
        .route("/i/{name}", get(preview_picture))
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
    let public = crate::web::public_channels(&state.community)?;
    Ok(state
        .community
        .store
        .channels()?
        .into_iter()
        .filter(|c| !c.closed && c.voice.is_none())
        .filter(|c| web_sees(state, viewer, c))
        .filter_map(|c| {
            if viewer.signed_in() {
                Some((c.id.clone(), None))
            } else {
                public.get(&c.id).map(|since| (c.id.clone(), Some(*since)))
            }
        })
        .collect())
}

/// Whether a viewer may see a channel at all: a member, as in the app; anyone
/// else, if `everyone` may (a public channel `everyone` may not see isn't public).
fn web_sees(state: &AppState, viewer: &Viewer, channel: &crate::store::Channel) -> bool {
    let community = &state.community;
    match &viewer.member {
        Some(member) => community.powers(&member.did).and_then(|powers| community.sees(&member.did, &powers, channel)).unwrap_or(false),
        None => community.everyone_sees(channel).unwrap_or(false),
    }
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
    /// A share card's picture (an absolute address), where the page has one of its own.
    pub og_image: Option<String>,
    /// `summary`, or `summary_large_image` where there is a picture of its own.
    pub card: &'static str,
    /// The page's channel's Atom feed, where it is public.
    pub feed: Option<String>,
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

/// One reaction to a post: the emoji (a custom emoji's picture, through this
/// server, where it is one) and how many chose it.
pub struct ReactionView {
    /// The reaction as kept: what toggling it from the web sends.
    pub raw: String,
    pub text: String,
    pub picture: Option<String>,
    pub count: usize,
}

/// A link's preview under a post: a page's, or a YouTube video's.
/// A webhook's embed (webhooks.rs), as a card: its colour down the side, its
/// pictures through this server, as link previews' are.
pub struct EmbedView {
    pub color: Option<String>,
    pub author: Option<String>,
    pub title: Option<String>,
    pub href: Option<String>,
    pub html: String,
    pub fields: Vec<EmbedField>,
    pub image: Option<String>,
    pub thumbnail: Option<String>,
    pub footer: Option<String>,
}

pub struct EmbedField {
    pub name: String,
    pub html: String,
    pub inline: bool,
}

pub struct Card {
    /// `page` or `youtube`.
    pub kind: &'static str,
    pub href: String,
    pub site: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub channel: Option<String>,
    pub channel_href: Option<String>,
    /// Its pictures, at this server.
    pub pictures: Vec<String>,
}

pub struct PostView {
    pub id: String,
    /// Its place in its channel: what replying and reacting from the web name.
    pub seq: i64,
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
    /// Posted by a webhook (webhooks.rs): shown as from an app, not a member.
    pub app: bool,
    pub embeds: Vec<EmbedView>,
    /// Nothing but emoji: the class that draws it large (larger the fewer), as the
    /// app does; empty otherwise.
    pub giant: &'static str,
    pub reactions: Vec<ReactionView>,
    /// Previews of the links it says (preview.rs).
    pub cards: Vec<Card>,
    pub thread_href: Option<String>,
    pub thread_count: i64,
    pub thread_name: Option<String>,
    /// The forum section its thread is in, with those that section is inside ("Help › Android").
    pub section: Option<String>,
    pub pinned: bool,
    pub focus: bool,
    /// Carries on the run of posts above it (as the app's cozy layout has it): drawn
    /// without its author's picture and name again. See [`mark_runs`].
    pub continued: bool,
    /// The time of day alone, shown beside a continued post.
    pub clock: String,
    run: Run,
}

/// What decides whether a post carries on the run above it.
#[derive(Default)]
struct Run {
    author: String,
    at: i64,
    starts: bool,
}

/// How long after the one before a post may come and still be part of its run, as
/// the app has it (wyvrn-chat's runs.ts).
const RUN_GAP_SECS: i64 = 7 * 60;

/// Marks each post that carries on the run of the one above it: the same author,
/// within seven minutes, on the same day (UTC, as the page tells the time). A reply,
/// a post with a title, and the post a link points to start a run of their own.
fn mark_runs(views: &mut [PostView]) {
    for n in 1..views.len() {
        let (before, rest) = views.split_at_mut(n);
        let (previous, post) = (&before[n - 1].run, &mut rest[0]);
        let gap = post.run.at - previous.at;
        post.continued = !post.run.starts && post.run.author == previous.author && (0..RUN_GAP_SECS).contains(&gap) && post.run.at.div_euclid(86_400) == previous.at.div_euclid(86_400);
    }
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
    /// For a member signed in who may post here: the form, and what it answers.
    compose: Option<Compose>,
    /// Where a post's reactions are changed, for a member signed in (`<this><seq>/react`).
    react_base: Option<String>,
    /// The emoji a post can be given in one tap (posting.rs).
    quick: &'static [&'static str],
    /// Where the page hears that the channel moved on (posting.rs's `live`).
    live: Option<String>,
    /// A gallery: its posts as a grid of their first picture or video.
    gallery: bool,
}

/// The form a member signed in posts with.
pub struct Compose {
    pub action: String,
    /// The post it answers, by its place: who wrote it, and what it said.
    pub reply: Option<(i64, String, String)>,
    pub placeholder: String,
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
        og_image: None,
        card: "summary",
        feed: None,
    }
}

/// The channels and voice channels, by category, as the side of every page shows them.
fn groups(state: &AppState, viewer: &Viewer, root: &str, current: Option<&str>) -> anyhow::Result<Vec<Group>> {
    let store = &state.community.store;
    let readable = readable(state, viewer)?;
    let public = crate::web::public_channels(&state.community)?;
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
            if !web_sees(state, viewer, &channel) {
                continue;
            }
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

/// Who is in each voice channel, as the side of `viewer`'s pages shows it (counts,
/// names, or nothing, by the community's choice): what a live page watches for a
/// change in (posting.rs's `live`). Empty where nothing of voice is shown.
pub fn voice_shown(state: &AppState, viewer: &Viewer) -> String {
    let Ok(groups) = groups(state, viewer, "", None) else { return String::new() };
    let shown: Vec<Value> = groups.iter().flat_map(|g| g.voice.iter()).map(|room| json!([room.name, room.count, room.names])).collect();
    if shown.is_empty() {
        String::new()
    } else {
        Value::Array(shown).to_string()
    }
}

fn name_of(state: &AppState, did: &str) -> String {
    state.community.store.member(did).ok().flatten().and_then(|m| m.name).unwrap_or_else(|| "A member".to_string())
}

/// The picture a member is shown with here: the one they chose for this community
/// (community/1.0's Profiles, `here`), else their profile's. `(media type, base64)`.
fn shown_picture(store: &crate::store::Store, did: &str) -> Option<(String, String)> {
    if let Some(picture) = store.member_here(did).ok().flatten().and_then(|here| here.picture) {
        return Some(picture);
    }
    store.member_picture(did).ok().flatten().map(|(media_type, data, _)| (media_type, data))
}

/// A member's picture address, as the public pages show it.
fn picture_href(state: &AppState, did: &str, root: &str) -> Option<String> {
    let store = &state.community.store;
    shown_picture(store, did)?;
    let secret = super::secret(store, &state.community.sealing()).ok()?;
    let token = store.picture_token(&store.member_key(did).ok()?, "pages", &secret).ok()?;
    Some(format!("{root}p/{token}"))
}

/// A custom emoji written as Markdown (`![name](<pack>/<hash>.png)`): its name and address.
fn custom_emoji(written: &str) -> Option<(String, String)> {
    let inner = written.strip_prefix("![")?;
    let (name, rest) = inner.split_once("](")?;
    let address = rest.strip_suffix(')')?.trim_start_matches('<').trim_end_matches('>');
    let file = url::Url::parse(address).ok()?.path_segments()?.next_back()?.to_string();
    super::outside::pack_file(&file)?;
    Some((name.trim_matches(':').to_string(), address.to_string()))
}

fn post_view(state: &AppState, post: &Post, channel: &Channel, root: &str, focus: Option<i64>, previews: &HashMap<String, super::preview::Preview>) -> PostView {
    let ch = format!("{root}ch/{}", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>());
    let author = post.author_name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "A member".to_string());
    let extra = post.extra.clone().unwrap_or(Value::Null);
    let attachments: HashMap<&str, &Value> = extra["attachments"].as_array().into_iter().flatten().filter_map(|a| Some((a["id"].as_str()?, a))).collect();
    // Pack files (custom emoji, stickers) are shown through this server (outside.rs).
    let secret = super::secret(&state.community.store, &state.community.sealing()).ok();
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
    // (A private thread isn't linked, nor named, on the web.)
    let thread = post.grew.as_ref().filter(|t| !t.private);
    PostView {
        id: post.id.clone(),
        seq: post.seq,
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
        app: extra["webhook"].is_object(),
        embeds: extra["embeds"]
            .as_array()
            .into_iter()
            .flatten()
            .take(10)
            .map(|embed| {
                let text = |value: &Value| value.as_str().map(str::to_string).filter(|t| !t.trim().is_empty());
                let picture = |value: &Value| value.as_str().and_then(|address| secret.as_deref().and_then(|secret| super::outside::picture_href(secret, root, address)));
                EmbedView {
                    color: embed["color"].as_u64().filter(|c| *c <= 0xFF_FF_FF).map(|c| format!("#{c:06x}")),
                    author: text(&embed["author"]["name"]),
                    title: text(&embed["title"]),
                    href: text(&embed["url"]).filter(|u| u.starts_with("https://") || u.starts_with("http://")),
                    html: text(&embed["description"]).map(|d| render::markdown(&d)).unwrap_or_default(),
                    fields: embed["fields"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|field| Some(EmbedField { name: text(&field["name"])?, html: render::markdown(field["value"].as_str().unwrap_or_default()), inline: field["inline"] == true }))
                        .collect(),
                    image: picture(&embed["image"]["url"]),
                    thumbnail: picture(&embed["thumbnail"]["url"]),
                    footer: text(&embed["footer"]["text"]),
                }
            })
            .collect(),
        giant: render::giant_class(render::emoji_only_count(&post.content, &|address| {
            url::Url::parse(address).ok().and_then(|u| u.path_segments()?.next_back().map(str::to_string)).is_some_and(|file| super::outside::pack_file(&file).is_some())
        })),
        reactions: post
            .reactions
            .iter()
            .map(|(emoji, who)| match custom_emoji(emoji) {
                Some((name, address)) => ReactionView { raw: emoji.clone(), text: format!(":{name}:"), picture: through_here(&address), count: who.len() },
                None => ReactionView { raw: emoji.clone(), text: emoji.clone(), picture: None, count: who.len() },
            })
            .collect(),
        cards: super::preview::links_in(&post.content)
            .into_iter()
            .filter_map(|address| {
                let preview = previews.get(&address).filter(|p| p.shows_something())?;
                Some(Card {
                    kind: if preview.kind == "youtube" { "youtube" } else { "page" },
                    site: preview.site.clone(),
                    title: preview.title.clone(),
                    description: preview.description.clone(),
                    channel: preview.channel.clone(),
                    channel_href: preview.channel_url.clone(),
                    pictures: preview.pictures.iter().filter_map(|picture| secret.as_deref().and_then(|secret| super::outside::picture_href(secret, root, picture))).collect(),
                    href: address,
                })
            })
            .collect(),
        thread_href: thread.map(|t| format!("{ch}/t/{}", url::form_urlencoded::byte_serialize(t.id.as_bytes()).collect::<String>())),
        thread_count: thread.map_or(0, |t| t.count),
        thread_name: thread.and_then(|t| t.name.clone()),
        section: thread.and_then(|t| t.section.as_deref()).and_then(|id| crate::board::section_path(channel.sections.as_ref(), id)),
        pinned: post.pinned,
        focus: focus == Some(post.seq),
        continued: false,
        clock: chrono::DateTime::from_timestamp(post.posted_at, 0).map(|t| t.format("%H:%M").to_string()).unwrap_or_default(),
        run: Run { author: post.author_did.clone(), at: post.posted_at, starts: post.reply_to.is_some() || extra["title"].is_string() || focus == Some(post.seq) },
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
    let Ok(secret) = super::secret(store, &state.community.sealing()) else { return nothing() };
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

/// A picture from a link's preview (or a YouTube video's), named by a page: kept
/// here, or fetched once and kept. Only addresses this server signed are fetched.
async fn preview_picture(State(state): State<Arc<AppState>>, Path(name): Path<String>, Query(asked): Query<OutsideQuery>) -> Response {
    use base64::Engine;
    let store = &state.community.store;
    if !page_on(store) {
        return nothing();
    }
    let Ok(secret) = super::secret(store, &state.community.sealing()) else { return nothing() };
    if !super::outside::picture_signed(&secret, &name, &asked.from, &asked.key) {
        return nothing();
    }
    let kept = match store.outside_picture(&name) {
        Ok(Some(kept)) => Some(kept),
        Ok(None) => match super::outside::fetch_preview_picture(&asked.from, crate::gates::now()).await {
            Ok((media_type, bytes)) => {
                let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let _ = store.keep_outside_picture(&name, media_type, &data, crate::gates::now());
                Some((media_type.to_string(), data))
            }
            Err(_) => None,
        },
        Err(_) => None,
    };
    let Some((media_type, data)) = kept else { return nothing() };
    let mut response = picture_response(&media_type, &data);
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=86400"));
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
        "commentCount": post.grew.as_ref().filter(|t| !t.private).map_or(0, |t| t.count),
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
async fn channel_bare(State(state): State<Arc<AppState>>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let viewer = Viewer::of(&state, &headers);
    match state.community.store.channel(&id) {
        Ok(Some(channel)) if readable(&state, &viewer).is_ok_and(|readable| readable.contains_key(&channel.id)) => as_page(Redirect::permanent(&format!("../{}", channel_path(&channel.id, &channel.name))).into_response()),
        _ => nothing(),
    }
}

#[derive(Deserialize)]
struct Before {
    before: Option<i64>,
    /// The post a member signed in is answering (`?reply=<seq>`).
    reply: Option<i64>,
    /// In a wiki: the page shown (`?page=<id>`).
    page: Option<String>,
}

/// A page of a wiki, in the list of its pages.
struct WikiEntry {
    title: String,
    href: String,
    depth: usize,
    current: bool,
}

#[derive(Template)]
#[template(path = "web/wiki.html")]
struct WikiView {
    site: Site,
    groups: Vec<Group>,
    title: String,
    description: String,
    json_ld: String,
    noindex: bool,
    heading: String,
    topic: String,
    pages: Vec<WikiEntry>,
    page_title: Option<String>,
    html: String,
}

/// One of the community's events, as a calendar channel's page shows it.
struct EventView {
    title: String,
    /// When it (next) starts and ends, as `<time>`s the page writes in the reader's zone.
    starts: String,
    ends: String,
    repeats: bool,
    cancelled: bool,
    happening: bool,
    /// Where: a place, or a voice channel by name (linked where the web shows it).
    place: Option<String>,
    voice: Option<String>,
    voice_href: Option<String>,
    going: i64,
    maybe: i64,
    html: String,
}

#[derive(Template)]
#[template(path = "web/calendar.html")]
struct CalendarView {
    site: Site,
    groups: Vec<Group>,
    title: String,
    description: String,
    json_ld: String,
    noindex: bool,
    heading: String,
    topic: String,
    events: Vec<EventView>,
}

/// A calendar channel on the web: the community's events this viewer may see (an
/// event in a voice channel only where the web shows that channel), happening or
/// to come first, then those just past, as the app lists them.
fn calendar_page(state: &AppState, viewer: &Viewer, channel: &Channel, private: bool) -> Response {
    let Some(web) = web_of(state) else { return nothing() };
    let root = "../../";
    let readable = match readable(state, viewer) {
        Ok(readable) => readable,
        Err(error) => return internal(error),
    };
    let store = &state.community.store;
    let now = chrono::Utc::now().timestamp();
    let listed = match state.community.events_listed(now) {
        Ok(listed) => listed,
        Err(error) => return internal(error),
    };
    let mut events = Vec::new();
    let mut schema = Vec::new();
    for (start, end, event) in listed {
        let voice = event.data["channel"].as_str();
        if voice.is_some_and(|id| !readable.contains_key(id)) {
            continue;
        }
        let voice_channel = voice.and_then(|id| store.channel(id).ok().flatten());
        let title = event.data["title"].as_str().unwrap_or("An event").to_string();
        let place = event.data["location"].as_str().map(str::to_string);
        schema.push(json!({ "@type": "Event", "name": title, "startDate": render::iso(start), "endDate": render::iso(end), "eventStatus": if event.cancelled { "https://schema.org/EventCancelled" } else { "https://schema.org/EventScheduled" }, "location": match (&place, &voice_channel) {
            (Some(place), _) => json!({ "@type": "Place", "name": place }),
            _ => json!({ "@type": "VirtualLocation", "url": web.absolute("") }),
        } }));
        let rsvps = store.event_rsvps(&event.id).unwrap_or_default();
        events.push(EventView {
            title,
            starts: render::time_html(start, 'F'),
            ends: render::time_html(end, 't'),
            repeats: event.data["recurrence"].is_object(),
            cancelled: event.cancelled,
            happening: !event.cancelled && start <= now && now < end,
            place,
            voice: voice_channel.as_ref().map(|c| c.name.clone()),
            voice_href: voice_channel.as_ref().filter(|c| readable.contains_key(&c.id)).map(|c| format!("{root}{}", channel_path(&c.id, &c.name))),
            going: rsvps.iter().filter(|(_, s)| s == "accepted").count() as i64,
            maybe: rsvps.iter().filter(|(_, s)| s == "tentative").count() as i64,
            html: render::markdown(event.data["description"].as_str().unwrap_or_default()),
        });
    }
    let site = site(state, viewer, root, web.absolute(&channel_path(&channel.id, &channel.name)));
    let groups = match groups(state, viewer, root, Some(&channel.id)) {
        Ok(groups) => groups,
        Err(error) => return internal(error),
    };
    let title = format!("#{} · {}", channel.name, site.name);
    page(
        CalendarView {
            json_ld: render::json_ld(&json!({ "@context": "https://schema.org", "@type": "CollectionPage", "name": title, "hasPart": schema })),
            description: if channel.topic.is_empty() { format!("Events in {}", site.name) } else { render::excerpt(&channel.topic, 200) },
            site,
            groups,
            title,
            noindex: private,
            heading: format!("📅 {}", channel.name),
            topic: channel.topic.clone(),
            events,
        }
        .render(),
        private || viewer.signed_in(),
    )
}

/// A wiki on the web: its pages as a tree, and the one chosen as it now reads.
fn wiki_page(state: &AppState, viewer: &Viewer, channel: &Channel, chosen: Option<&str>, private: bool) -> Response {
    let Some(web) = web_of(state) else { return nothing() };
    let root = "../../";
    let pages: Vec<crate::store::WikiPage> = match state.community.store.wiki_pages(&channel.id) {
        Ok(pages) => pages.into_iter().filter(|p| !p.deleted).collect(),
        Err(error) => return internal(error.into()),
    };
    // Depth first, each under its parent, in their order.
    fn walk(pages: &[crate::store::WikiPage], parent: Option<&str>, depth: usize, out: &mut Vec<(usize, usize)>) {
        for (n, page) in pages.iter().enumerate().filter(|(_, p)| p.parent.as_deref() == parent) {
            out.push((n, depth));
            if depth < 8 {
                walk(pages, Some(&page.id), depth + 1, out);
            }
        }
    }
    let mut order = Vec::new();
    walk(&pages, None, 0, &mut order);
    let shown = chosen.and_then(|id| pages.iter().find(|p| p.id == id)).or_else(|| order.first().map(|(n, _)| &pages[*n]));
    let path = channel_path(&channel.id, &channel.name);
    let entries = order
        .iter()
        .map(|(n, depth)| WikiEntry { title: pages[*n].title.clone(), href: format!("?page={}", url::form_urlencoded::byte_serialize(pages[*n].id.as_bytes()).collect::<String>()), depth: *depth, current: shown.is_some_and(|s| s.id == pages[*n].id) })
        .collect();
    let html = shown.map(|p| render::markdown(&state.community.wiki_text(&p.id).unwrap_or_default())).unwrap_or_default();
    let canonical = web.absolute(&match shown {
        Some(p) => format!("{path}?page={}", p.id),
        None => path.clone(),
    });
    let site = site(state, viewer, root, canonical);
    let groups = match groups(state, viewer, root, Some(&channel.id)) {
        Ok(groups) => groups,
        Err(error) => return internal(error),
    };
    let title = match shown {
        Some(p) => format!("{} · #{} · {}", p.title, channel.name, site.name),
        None => format!("#{} · {}", channel.name, site.name),
    };
    let description = shown.map(|p| render::excerpt(&state.community.wiki_text(&p.id).unwrap_or_default(), 200)).unwrap_or_else(|| format!("#{} in {}", channel.name, site.name));
    page(
        WikiView {
            json_ld: render::json_ld(&json!({ "@context": "https://schema.org", "@type": "WebPage", "name": title })),
            site,
            groups,
            title,
            description,
            noindex: private,
            heading: format!("📚 {}", channel.name),
            topic: channel.topic.clone(),
            pages: entries,
            page_title: shown.map(|p| p.title.clone()),
            html,
        }
        .render(),
        private || viewer.signed_in(),
    )
}

/// A channel's posts, newest last, a page at a time.
async fn channel(State(state): State<Arc<AppState>>, Path((id, name)): Path<(String, String)>, Query(paging): Query<Before>, headers: HeaderMap) -> Response {
    let reply = paging.reply;
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
    // A calendar: the community's events, not posts.
    if channel.calendar {
        let private = since.is_none() && !is_public(&state, &channel.id);
        return calendar_page(&state, &viewer, &channel, private);
    }
    // A wiki: its pages, not posts.
    if channel.wiki {
        let private = since.is_none() && !is_public(&state, &channel.id);
        return wiki_page(&state, &viewer, &channel, paging.page.as_deref(), private);
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
    let previews = super::preview::prepare(&state, &shown).await;
    let mut views: Vec<PostView> = shown.iter().map(|p| post_view(&state, p, &channel, root, None, &previews)).collect();
    mark_runs(&mut views);
    let json_ld = render::json_ld(&json!({ "@context": "https://schema.org", "@type": "CollectionPage", "name": channel.name, "url": canonical, "hasPart": shown.iter().take(20).map(|p| discussion(p, web, &channel, &p.author_name.clone().unwrap_or_default())).collect::<Vec<_>>() }));
    let mut site = site(&state, &viewer, root, canonical);
    site.in_app.push_str(&format!("&channel={}", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>()));
    if is_public(&state, &channel.id) {
        site.feed = Some(web.absolute(&format!("ch/{}/feed.atom", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>())));
    }
    let groups = match groups(&state, &viewer, root, Some(&channel.id)) {
        Ok(groups) => groups,
        Err(error) => return internal(error),
    };
    let description = if channel.topic.is_empty() { format!("#{} in {}", channel.name, site.name) } else { render::excerpt(&channel.topic, 200) };
    let private = since.is_none() && !is_public(&state, &channel.id);
    let title = format!("#{} · {}", channel.name, site.name);
    page(
        ChannelPage {
            heading: format!("#{}", channel.name),
            channel_name: channel.name.clone(),
            topic: channel.topic.clone(),
            posts: views,
            older,
            newer,
            title,
            description,
            json_ld,
            site,
            groups,
            back: None,
            noindex: private,
            compose: compose_for(&state, &viewer, &channel, root, reply),
            react_base: react_base(&viewer, &channel, root),
            quick: super::posting::QUICK_REACTIONS,
            // The newest page only: an older one doesn't change at its foot.
            live: before.is_none().then(|| format!("{root}ch/{}/live", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>())),
            gallery: channel.gallery,
        }
        .render(),
        private || viewer.signed_in(),
    )
}

/// The form to post with, for a member signed in who may post in `channel`.
fn compose_for(state: &AppState, viewer: &Viewer, channel: &Channel, root: &str, reply: Option<i64>) -> Option<Compose> {
    let member = viewer.member.as_ref()?;
    if channel.forum.is_some() || !state.community.may_in(&member.did, channel, "post").unwrap_or(false) {
        return None;
    }
    let answering = reply.and_then(|seq| state.community.store.post_at(&channel.id, seq).ok().flatten()).filter(|p| !p.deleted).map(|p| (p.seq, p.author_name.clone().unwrap_or_else(|| "A member".into()), render::excerpt(&p.content, 100)));
    Some(Compose { action: format!("{root}ch/{}/post", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>()), reply: answering, placeholder: format!("Post in #{}", channel.name) })
}

/// Where reactions go, for a member signed in.
fn react_base(viewer: &Viewer, channel: &Channel, root: &str) -> Option<String> {
    viewer.member.as_ref()?;
    Some(format!("{root}ch/{}/m/", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>()))
}

fn is_public(state: &AppState, channel: &str) -> bool {
    crate::web::public_channels(&state.community).map(|c| c.contains_key(channel)).unwrap_or(false)
}

fn shows_any_before(state: &AppState, channel: &Channel, seq: i64, since: Option<i64>) -> bool {
    state.community.store.channel_posts_before(&channel.id, None, Some(seq), 1).ok().and_then(|p| p.into_iter().next()).is_some_and(|p| shows(&p, since))
}

/// The channel, if the viewer may read it, and from when.
pub(super) fn visible_channel(state: &AppState, viewer: &Viewer, id: &str) -> Option<(Channel, Option<i64>)> {
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
    let Some(post) = store.post_at(&channel.id, seq).ok().flatten().filter(|p| !in_private_thread(&state, p)) else {
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
    let listed: Vec<&Post> = around.iter().filter(|p| !p.deleted && shows(p, since)).collect();
    let previews = super::preview::prepare(&state, &listed).await;
    let mut views: Vec<PostView> = listed.iter().map(|p| post_view(&state, p, &channel, root, Some(seq), &previews)).collect();
    mark_runs(&mut views);
    let author = post.author_name.clone().unwrap_or_else(|| "A member".to_string());
    let canonical = web.absolute(&format!("ch/{}/m/{seq}", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>()));
    let mut ld = discussion(&post, web, &channel, &author);
    ld["@context"] = json!("https://schema.org");
    let mut site = site(&state, &viewer, root, canonical);
    site.in_app.push_str(&format!("&channel={}&seq={seq}", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>()));
    // Its share card: the post's own picture, large, where it is public and has one.
    if since.is_some() && first_picture(&post).is_some() {
        site.og_image = Some(web.absolute(&format!("media/{}/0", url::form_urlencoded::byte_serialize(post.id.as_bytes()).collect::<String>())));
        site.card = "summary_large_image";
    }
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
            compose: None,
            react_base: react_base(&viewer, &channel, root),
            quick: super::posting::QUICK_REACTIONS,
            live: None,
            gallery: false,
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
    // A private thread is never on the web, signed in or not.
    if store.is_private_thread(&thread).unwrap_or(true) {
        return nothing();
    }
    let root = "../../../";
    let mut posts = match store.channel_posts_before(&channel.id, Some(&thread), None, 200) {
        Ok(posts) => posts,
        Err(error) => return internal(error.into()),
    };
    posts.reverse();
    let listed: Vec<&Post> = std::iter::once(&root_post).chain(posts.iter().filter(|p| !p.deleted && shows(p, since))).collect();
    let previews = super::preview::prepare(&state, &listed).await;
    let mut views: Vec<PostView> = listed.iter().map(|p| post_view(&state, p, &channel, root, None, &previews)).collect();
    mark_runs(&mut views);
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
            // In a forum, the section it is in: where it sits on the board.
            topic: root_post.grew.as_ref().and_then(|t| t.section.as_deref()).and_then(|id| crate::board::section_path(channel.sections.as_ref(), id)).unwrap_or_default(),
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
            compose: None,
            react_base: react_base(&viewer, &channel, root),
            quick: super::posting::QUICK_REACTIONS,
            live: None,
            gallery: false,
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
    let Some(post) = state.community.store.post_by_id(&id).ok().flatten().filter(|p| !in_private_thread(&state, p)) else { return nothing() };
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
    let public = crate::web::public_channels(&state.community).unwrap_or_default();
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

/// The start of the hash of the stylesheet and the script together: put in their
/// addresses, so that a browser holding an older copy fetches them again after an
/// update, and a copy can be kept for good.
static ASSETS_VERSION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(format!("{STYLE}\n{SCRIPT}").as_bytes());
    hash.iter().take(6).map(|b| format!("{b:02x}")).collect()
});

impl Site {
    /// What the stylesheet's and the script's addresses carry ([`ASSETS_VERSION`]).
    pub fn assets(&self) -> &'static str {
        ASSETS_VERSION.as_str()
    }
}

#[derive(Deserialize)]
struct Versioned {
    v: Option<String>,
}

async fn asset(Path(file): Path<String>, Query(versioned): Query<Versioned>) -> Response {
    let (body, kind) = match file.as_str() {
        "web.css" => (STYLE, "text/css; charset=utf-8"),
        "web.js" => (SCRIPT, "text/javascript; charset=utf-8"),
        _ => return nothing(),
    };
    // Asked for by the version they are (`?v=`), so kept for good; asked for without
    // it (a page from before), kept for an hour.
    let kept = if versioned.v.as_deref() == Some(ASSETS_VERSION.as_str()) { "public, max-age=31536000, immutable" } else { "public, max-age=3600" };
    as_page(([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, kept)], body).into_response())
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
    match shown_picture(store, &member.did) {
        Some((media_type, data)) => picture_response(&media_type, &data),
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
    let Some(post) = state.community.store.post_by_id(&post_id).ok().flatten().filter(|p| !p.deleted && !in_private_thread(&state, p)) else { return nothing() };
    let Some((_, since)) = visible_channel(&state, &viewer, &post.channel) else { return nothing() };
    if !shows(&post, since) {
        return nothing();
    }
    let extra = post.extra.unwrap_or(Value::Null);
    let Some(files) = &state.community.files else { return nothing() };
    let (bytes, media_type, name) = match files.carried(&extra, n, MAX_MEDIA_BYTES).await {
        crate::files::Carried::Opened { bytes, media_type, name } => (bytes, media_type, name),
        crate::files::Carried::NotHere => return nothing(),
        crate::files::Carried::Unreadable => return page_error(StatusCode::UNPROCESSABLE_ENTITY, "Can't be opened", "This file isn't what the post says it is."),
    };
    let media_type = media_type.as_str();
    let shown = inline_type(media_type);
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
        assert_eq!(crate::files::open_sealed(ciphertext, &ciphering).as_deref(), Some(b"a picture".as_slice()));
        let mut changed = ciphertext.to_vec();
        changed[0] ^= 1;
        assert_eq!(crate::files::open_sealed(&changed, &ciphering), None, "changed at the host: refused");
        assert_eq!(crate::files::open_sealed(ciphertext, &json!({ "algorithm": "rot13" })), None);
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

/// A post in a private thread: never on the web, whoever asks (community/1.0's
/// "A private thread").
fn in_private_thread(state: &AppState, post: &Post) -> bool {
    post.thread.as_deref().is_some_and(|thread| state.community.store.is_private_thread(thread).unwrap_or(true))
}

/// A post's first picture, if it carries one.
fn first_picture(post: &Post) -> Option<()> {
    let extra = post.extra.as_ref()?;
    let first = extra["media"].as_array()?.first()?;
    let attachment = extra["attachments"].as_array()?.iter().find(|a| a["id"] == first["attachment"])?;
    attachment["media_type"].as_str().filter(|t| t.starts_with("image/")).map(|_| ())
}

/// Someone not signed in: what the feed and the widget show is what anyone sees.
fn anyone() -> Viewer {
    Viewer { member: None, csrf: None }
}

/// `/ch/{id}/feed.atom`: a public channel's newest posts, as an Atom feed.
async fn feed(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(web) = web_of(&state) else { return nothing() };
    if !page_on(&state.community.store) {
        return nothing();
    }
    let Some((channel, since)) = visible_channel(&state, &anyone(), &id) else { return nothing() };
    let posts = match state.community.store.channel_posts_by_time(&channel.id, None, None, 50) {
        Ok(posts) => posts,
        Err(error) => return internal(error.into()),
    };
    let shown: Vec<&Post> = posts.iter().filter(|p| !p.deleted && shows(p, since)).collect();
    let previews = HashMap::new();
    let name = site(&state, &anyone(), "", String::new()).name;
    let page = web.absolute(&channel_path(&channel.id, &channel.name));
    let at = |secs: i64| chrono::DateTime::from_timestamp(secs, 0).map(|t| t.to_rfc3339()).unwrap_or_default();
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<feed xmlns=\"http://www.w3.org/2005/Atom\">\n<title>#{} · {}</title>\n<link href=\"{}\"/>\n<link rel=\"self\" href=\"{}\"/>\n<id>{}</id>\n<updated>{}</updated>\n",
        escape(&channel.name),
        escape(&name),
        escape(&page),
        escape(&web.absolute(&format!("ch/{}/feed.atom", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>()))),
        escape(&page),
        at(shown.first().map_or(channel.created_at, |p| p.posted_at)),
    );
    for post in shown {
        let view = post_view(&state, post, &channel, "", None, &previews);
        let link = web.absolute(&format!("ch/{}/m/{}", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>(), post.seq));
        let title = render::excerpt(&post.content, 80);
        xml.push_str(&format!(
            "<entry>\n<id>{link}</id>\n<link href=\"{link}\"/>\n<title>{}</title>\n<author><name>{}</name></author>\n<updated>{}</updated>\n<content type=\"html\">{}</content>\n</entry>\n",
            escape(if title.is_empty() { "A post" } else { &title }),
            escape(&view.author),
            at(post.edited_at.unwrap_or(post.posted_at)),
            escape(&view.html),
            link = escape(&link),
        ));
    }
    xml.push_str("</feed>\n");
    let mut response = ([(header::CONTENT_TYPE, "application/atom+xml; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=300")], xml).into_response();
    response.extensions_mut().insert(super::OpenToAll);
    response
}

#[derive(Deserialize, Default)]
pub struct Searched {
    q: Option<String>,
}

/// `/search?q=`: the posts that say it, in what the viewer may read here.
async fn search(State(state): State<Arc<AppState>>, Query(asked): Query<Searched>, headers: HeaderMap) -> Response {
    let Some(web) = web_of(&state) else { return nothing() };
    if !page_on(&state.community.store) {
        return nothing();
    }
    let viewer = Viewer::of(&state, &headers);
    let query = asked.q.unwrap_or_default().chars().take(100).collect::<String>();
    let words: Vec<String> = query.split_whitespace().take(8).map(str::to_string).collect();
    let readable = match readable(&state, &viewer) {
        Ok(readable) => readable,
        Err(error) => return internal(error),
    };
    let within: Vec<String> = readable.keys().cloned().collect();
    let found = if query.trim().chars().count() >= 2 && !within.is_empty() {
        match state.community.store.search(&words, None, None, Some(&within), None, 30) {
            Ok(found) => found,
            Err(error) => return internal(error.into()),
        }
    } else {
        Vec::new()
    };
    let shown: Vec<&Post> = found.iter().filter(|p| !in_private_thread(&state, p) && readable.get(&p.channel).is_some_and(|since| shows(p, *since))).collect();
    let previews = super::preview::prepare(&state, &shown).await;
    let channels: HashMap<String, Channel> = state.community.store.channels().unwrap_or_default().into_iter().map(|c| (c.id.clone(), c)).collect();
    let root = "./";
    let mut views: Vec<PostView> = shown.iter().filter_map(|p| channels.get(&p.channel).map(|c| post_view(&state, p, c, root, None, &previews))).collect();
    mark_runs(&mut views);
    let canonical = web.absolute("search");
    let site = site(&state, &viewer, root, canonical);
    let groups = match groups(&state, &viewer, root, None) {
        Ok(groups) => groups,
        Err(error) => return internal(error),
    };
    let heading = if query.is_empty() { "Search".to_string() } else { format!("Search: {query}") };
    page(
        ChannelPage {
            title: format!("{heading} · {}", site.name),
            description: format!("Posts in {} that say what was searched for.", site.name),
            json_ld: String::new(),
            channel_name: String::new(),
            topic: if query.is_empty() { "Search what is public here.".into() } else if views.is_empty() { "Nothing here says that.".into() } else { format!("{} found", views.len()) },
            posts: views,
            older: None,
            newer: None,
            heading,
            back: None,
            site,
            groups,
            noindex: true,
            compose: None,
            react_base: None,
            quick: super::posting::QUICK_REACTIONS,
            live: None,
            gallery: false,
        }
        .render(),
        true,
    )
}

#[derive(Deserialize, Default)]
pub struct WidgetAsked {
    channel: Option<String>,
}

/// What the widget shows: the community, how many are here, its public channels,
/// how to join, and a public channel's newest posts.
fn widget_body(state: &AppState, web: &Web, asked: &WidgetAsked) -> anyhow::Result<Value> {
    let anyone = anyone();
    let site = site(state, &anyone, "", String::new());
    let readable = readable(state, &anyone)?;
    let mut channels: Vec<Channel> = state.community.store.channels()?.into_iter().filter(|c| readable.contains_key(&c.id)).collect();
    channels.sort_by_key(|c| c.created_at);
    let online = state.community.seen_online()?.len();
    // Only voice channels everyone may see are counted (WYV-89): who is in a
    // private one is that channel's members' to know.
    let in_voice: usize = state
        .rooms
        .in_rooms()
        .into_iter()
        .filter(|(channel, _)| state.community.store.channel(channel).ok().flatten().is_some_and(|c| state.community.everyone_sees(&c).unwrap_or(false)))
        .map(|(_, dids)| dids.len())
        .sum();
    let chosen = asked.channel.as_deref().and_then(|id| channels.iter().find(|c| c.id == id)).or_else(|| channels.first());
    let mut recent = Vec::new();
    if let Some(channel) = chosen {
        let since = readable.get(&channel.id).copied().flatten();
        for post in state.community.store.channel_posts_by_time(&channel.id, None, None, 10)?.into_iter().filter(|p| !p.deleted && shows(p, since)).take(5) {
            recent.push(json!({
                "author": post.author_name.clone().unwrap_or_else(|| "A member".into()),
                "text": render::excerpt(&post.content, 140),
                "at": post.posted_at,
                "url": web.absolute(&format!("ch/{}/m/{}", url::form_urlencoded::byte_serialize(channel.id.as_bytes()).collect::<String>(), post.seq)),
            }));
        }
    }
    Ok(json!({
        "name": site.name,
        "description": site.description,
        "icon": site.has_icon.then(|| web.absolute("icon")),
        "url": web.absolute(""),
        "join": site.in_app,
        "online": online,
        "in_voice": in_voice,
        "channels": channels.iter().map(|c| json!({ "id": c.id, "name": c.name, "url": web.absolute(&channel_path(&c.id, &c.name)) })).collect::<Vec<_>>(),
        "recent": { "channel": chosen.map(|c| c.name.clone()), "posts": recent },
    }))
}

/// `/widget.json`: the widget's facts, for any site to show its own way.
async fn widget_json(State(state): State<Arc<AppState>>, Query(asked): Query<WidgetAsked>) -> Response {
    let Some(web) = web_of(&state) else { return nothing() };
    if !page_on(&state.community.store) {
        return nothing();
    }
    match widget_body(&state, web, &asked) {
        Ok(body) => {
            let mut response = ([(header::CACHE_CONTROL, "public, max-age=60")], axum::Json(body)).into_response();
            response.extensions_mut().insert(super::OpenToAll);
            response
        }
        Err(error) => internal(error),
    }
}

/// The widget's own style, allowed by its hash.
const WIDGET_STYLE: &str = "body{margin:0;font:14px/1.4 system-ui,sans-serif;background:#fff;color:#1a1a1e}@media (prefers-color-scheme:dark){body{background:#16161a;color:#ececee}a{color:#8ab4ff}}.w{padding:12px;display:grid;gap:10px}.h{display:flex;gap:10px;align-items:center}.h img,.l{width:40px;height:40px;border-radius:12px;object-fit:cover}.l{display:grid;place-items:center;background:#4a63e7;color:#fff;font-weight:700}.n{font-weight:700;font-size:16px}.s{opacity:.75;font-size:12px}ul{margin:0;padding:0;list-style:none}li{padding:4px 0;border-top:1px solid #8884}.p b{font-weight:600}.j{display:inline-block;padding:8px 14px;border-radius:8px;background:#4a63e7;color:#fff;text-decoration:none;font-weight:600;text-align:center}";

/// `/widget`: the widget, for any site to put in a frame
/// (`<iframe src=".../widget" width="320" height="480">`).
async fn widget(State(state): State<Arc<AppState>>, Query(asked): Query<WidgetAsked>) -> Response {
    use base64::Engine;
    use sha2::Digest;
    let Some(web) = web_of(&state) else { return nothing() };
    if !page_on(&state.community.store) {
        return nothing();
    }
    let body = match widget_body(&state, web, &asked) {
        Ok(body) => body,
        Err(error) => return internal(error),
    };
    let text = |v: &Value| escape(v.as_str().unwrap_or_default());
    let icon = match body["icon"].as_str() {
        Some(icon) => format!("<img src=\"{}\" alt=\"\">", escape(icon)),
        None => format!("<span class=\"l\">{}</span>", escape(&body["name"].as_str().unwrap_or_default().chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default())),
    };
    let channels: String = body["channels"].as_array().into_iter().flatten().take(8).map(|c| format!("<li><a href=\"{}\" target=\"_blank\" rel=\"noopener\">#{}</a></li>", text(&c["url"]), text(&c["name"]))).collect();
    let posts: String = body["recent"]["posts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| format!("<li class=\"p\"><b>{}</b> <a href=\"{}\" target=\"_blank\" rel=\"noopener\">{}</a></li>", text(&p["author"]), text(&p["url"]), text(&p["text"])))
        .collect();
    let recent_heading = body["recent"]["channel"].as_str().map(|c| format!("<div class=\"s\">Lately in #{}</div>", escape(c))).unwrap_or_default();
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{name}</title><style>{style}</style></head><body><div class=\"w\"><div class=\"h\">{icon}<div><div class=\"n\">{name}</div><div class=\"s\">{online} online · {voice} in voice</div></div></div><a class=\"j\" href=\"{join}\" target=\"_blank\" rel=\"noopener\">Join in Wyvrn</a><ul>{channels}</ul>{recent_heading}<ul>{posts}</ul><div class=\"s\"><a href=\"{url}\" target=\"_blank\" rel=\"noopener\">Open on the web</a></div></div></body></html>",
        name = text(&body["name"]),
        style = WIDGET_STYLE,
        online = body["online"],
        voice = body["in_voice"],
        join = text(&body["join"]),
        url = text(&body["url"]),
    );
    let hash = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(WIDGET_STYLE.as_bytes()));
    let mut response = Html(html).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=60"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if let Ok(value) = HeaderValue::from_str(&format!("default-src 'none'; style-src 'sha256-{hash}'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors *")) {
        headers.insert(header::CONTENT_SECURITY_POLICY, value);
    }
    response
}

