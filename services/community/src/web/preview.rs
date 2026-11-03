//! Previews of the links in public posts (community/1.0's "On the web"), as the app
//! shows them: a page's title, description and pictures, and for a YouTube video its
//! channel and title. The server reads each linked page itself -- from the public
//! web only, with limits on time and size -- keeps what it found, and shows the
//! pictures from its own address (`i/`, outside.rs), so a visitor's browser contacts
//! nobody but this server until they follow a link.
//!
//! Read once and kept (`web_previews`); a link that gave nothing is read again after
//! a day. A page is built after waiting a short while for links not read yet; one
//! still being read is a plain link until the next time.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::outside::{self, fetch_public, AGENT};
use crate::server::AppState;
use crate::store::Post;

/// The most links of one post that get a preview, as the app has it.
pub const MOST_PER_POST: usize = 5;
/// How long a page is held back for links not read yet.
const WAIT: Duration = Duration::from_millis(2500);
/// A link that gave nothing is read again after this long.
const RETRY_SECS: i64 = 24 * 60 * 60;
/// The most of a page read for its preview.
const MOST_PAGE: usize = 768 * 1024;
const MOST_TITLE: usize = 200;
const MOST_DESCRIPTION: usize = 500;
const MOST_PICTURES: usize = 4;

/// What was found for a link.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Preview {
    /// `page`, `youtube`, or `none` (nothing to show).
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Pictures, by their own addresses: shown through this server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pictures: Vec<String>,
    /// A YouTube video's channel, and its page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_url: Option<String>,
}

impl Preview {
    fn none() -> Self {
        Self { kind: "none".into(), ..Self::default() }
    }

    pub fn shows_something(&self) -> bool {
        self.kind != "none" && (self.title.is_some() || self.description.is_some() || !self.pictures.is_empty())
    }
}

/// The links in a post that may have a preview, in order, each once: addresses
/// written bare and Markdown links, not in code; never a picture's, a GIF (shown as
/// itself), or anything but the web.
pub fn links_in(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut add = |address: &str| {
        let address = address.trim();
        let web = url::Url::parse(address).is_ok_and(|u| matches!(u.scheme(), "https" | "http") && u.host_str().is_some());
        if web && !outside::is_gif(address) && !found.iter().any(|known| known == address) && found.len() < MOST_PER_POST {
            found.push(address.to_string());
        }
    };
    let (mut in_code, mut in_link, mut in_image) = (0usize, 0usize, 0usize);
    for event in Parser::new(text) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => in_code += 1,
            Event::End(TagEnd::CodeBlock) => in_code = in_code.saturating_sub(1),
            Event::Start(Tag::Image { .. }) => in_image += 1,
            Event::End(TagEnd::Image) => in_image = in_image.saturating_sub(1),
            Event::Start(Tag::Link { dest_url, .. }) => {
                in_link += 1;
                if in_image == 0 {
                    add(&dest_url);
                }
            }
            Event::End(TagEnd::Link) => in_link = in_link.saturating_sub(1),
            Event::Text(said) if in_code == 0 && in_link == 0 && in_image == 0 => {
                for (start, end) in super::render::bare_links(&said) {
                    add(&said[start..end]);
                }
            }
            _ => {}
        }
    }
    found
}

/// The YouTube video `address` is a link to: a watch page, a short, a `youtu.be`
/// link, a live or an embed (as the app's youTube.ts).
pub fn youtube_id(address: &str) -> Option<String> {
    let url = url::Url::parse(address).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let id = if host == "youtu.be" {
        url.path_segments()?.next()?.to_string()
    } else if matches!(host.as_str(), "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtube-nocookie.com" | "www.youtube-nocookie.com") {
        let path: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        match path.as_slice() {
            ["watch", ..] => url.query_pairs().find(|(k, _)| k == "v")?.1.to_string(),
            ["shorts" | "embed" | "live" | "v", id, ..] => id.to_string(),
            _ => return None,
        }
    } else {
        return None;
    };
    (id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')).then_some(id)
}

/// Where a link to a post on X (Twitter) is read from: vxtwitter, which answers a
/// preview where x.com only sends a script, and answers only fetchers it knows
/// (`facebookexternalhit`, as the app's link_preview.rs tells it).
fn twitter_source(address: &str) -> Option<String> {
    let url = url::Url::parse(address).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let host = host.trim_start_matches("www.").trim_start_matches("mobile.");
    matches!(host, "x.com" | "twitter.com" | "vxtwitter.com" | "fixvx.com").then(|| format!("https://vxtwitter.com{}", url.path()))
}

fn key(address: &str) -> String {
    Sha256::digest(address.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>()[..40].to_string()
}

/// The previews of every link in `posts`: those kept, and those read now, waiting
/// at most a moment for them. What isn't read in time goes on being read, and is
/// shown the next time.
pub async fn prepare(state: &Arc<AppState>, posts: &[&Post]) -> HashMap<String, Preview> {
    let now = crate::gates::now();
    let store = &state.community.store;
    let mut known = HashMap::new();
    let mut tasks = tokio::task::JoinSet::new();
    let mut asked = std::collections::HashSet::new();
    for post in posts {
        for address in links_in(&post.content) {
            if !asked.insert(address.clone()) {
                continue;
            }
            match store.link_preview(&key(&address)) {
                Ok(Some((data, at))) => {
                    let preview: Preview = serde_json::from_str(&data).unwrap_or_else(|_| Preview::none());
                    let stale = preview.kind == "none" && now - at > RETRY_SECS;
                    if !stale {
                        known.insert(address, preview);
                        continue;
                    }
                }
                Ok(None) => {}
                Err(_) => continue,
            }
            if !reading(&address, now) {
                continue;
            }
            let state = state.clone();
            // Spawned, so that one not read in time is still kept for next time.
            let task = tokio::spawn(async move {
                let preview = read(&address).await;
                if let Ok(data) = serde_json::to_string(&preview) {
                    let _ = state.community.store.keep_link_preview(&key(&address), &address, &data, crate::gates::now());
                }
                done_reading(&address);
                (address, preview)
            });
            tasks.spawn(async move { task.await.ok() });
        }
    }
    let _ = tokio::time::timeout(WAIT, async {
        while let Some(done) = tasks.join_next().await {
            if let Ok(Some((address, preview))) = done {
                known.insert(address, preview);
            }
        }
    })
    .await;
    known
}

/// Links being read now, so that two pages at once don't read one twice.
static READING: std::sync::Mutex<Option<HashMap<String, i64>>> = std::sync::Mutex::new(None);

fn reading(address: &str, now: i64) -> bool {
    let mut reading = READING.lock().unwrap_or_else(|p| p.into_inner());
    let reading = reading.get_or_insert_with(HashMap::new);
    if reading.get(address).is_some_and(|at| now - at < 60) {
        return false;
    }
    if reading.len() > 1000 {
        reading.clear();
    }
    reading.insert(address.to_string(), now);
    true
}

fn done_reading(address: &str) {
    if let Some(reading) = READING.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
        reading.remove(address);
    }
}

/// What `address` comes to: a YouTube video's details, or a page's preview.
pub async fn read(address: &str) -> Preview {
    if let Some(id) = youtube_id(address) {
        return read_youtube(&id).await.unwrap_or_else(Preview::none);
    }
    let twitter = twitter_source(address);
    let (source, agent) = match &twitter {
        Some(source) => (source.clone(), "facebookexternalhit/1.1"),
        None => (address.to_string(), AGENT),
    };
    let Ok(url) = url::Url::parse(&source) else { return Preview::none() };
    let mut preview = match fetch_public(&url, MOST_PAGE, agent, 3).await {
        Ok(fetched) if fetched.content_type.as_deref().is_none_or(|t| t.starts_with("text/html") || t.starts_with("application/xhtml")) => {
            let html = String::from_utf8_lossy(&fetched.bytes);
            parse_page(&html, &fetched.url).unwrap_or_else(Preview::none)
        }
        _ => Preview::none(),
    };
    // vxtwitter names itself (with the post's counts); the post is X's.
    if twitter.is_some() && preview.kind == "page" {
        preview.site = Some("X".into());
    }
    preview
}

/// A YouTube video's title and channel, from YouTube's oEmbed, and its picture.
async fn read_youtube(id: &str) -> Option<Preview> {
    let url = url::Url::parse(&format!("https://www.youtube.com/oembed?format=json&url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3D{id}")).ok()?;
    let fetched = fetch_public(&url, 64 * 1024, AGENT, 1).await.ok()?;
    let oembed: serde_json::Value = serde_json::from_slice(&fetched.bytes).ok()?;
    let text = |field: &str, most: usize| oembed[field].as_str().map(|v| clean(v, most)).filter(|v| !v.is_empty());
    let channel_url = oembed["author_url"]
        .as_str()
        .and_then(|u| url::Url::parse(u).ok())
        .filter(|u| u.scheme() == "https" && matches!(u.host_str(), Some("www.youtube.com" | "youtube.com")))
        .map(|u| u.to_string());
    Some(Preview {
        kind: "youtube".into(),
        site: Some("YouTube".into()),
        title: text("title", MOST_TITLE),
        description: None,
        pictures: vec![format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")],
        channel: text("author_name", 100),
        channel_url,
    })
}

/// One line of text from a page, cut short.
fn clean(text: &str, most: usize) -> String {
    let text = entities(text);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= most {
        text
    } else {
        format!("{}…", text.chars().take(most.saturating_sub(1)).collect::<String>().trim_end())
    }
}

/// HTML's character references in an attribute or a title, as the characters.
fn entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ if name.starts_with("#x") || name.starts_with("#X") => u32::from_str_radix(&name[2..], 16).ok().and_then(char::from_u32),
            _ if name.starts_with('#') => name[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The attributes of a tag (`name="value"`, `name='value'`, `name=value`), names in lower case.
fn attributes(tag: &str) -> HashMap<String, String> {
    let mut found = HashMap::new();
    let bytes = tag.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        while at < bytes.len() && !(bytes[at].is_ascii_alphabetic()) {
            at += 1;
        }
        let start = at;
        while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'-' | b':' | b'_')) {
            at += 1;
        }
        let name = tag[start..at].to_ascii_lowercase();
        while at < bytes.len() && bytes[at] == b' ' {
            at += 1;
        }
        if at >= bytes.len() || bytes[at] != b'=' {
            continue;
        }
        at += 1;
        while at < bytes.len() && bytes[at] == b' ' {
            at += 1;
        }
        let value = match bytes.get(at) {
            Some(&quote @ (b'"' | b'\'')) => {
                let end = tag[at + 1..].find(quote as char).map_or(tag.len(), |e| at + 1 + e);
                let value = &tag[at + 1..end];
                at = end + 1;
                value
            }
            _ => {
                let end = tag[at..].find(|c: char| c.is_whitespace() || c == '>').map_or(tag.len(), |e| at + e);
                let value = &tag[at..end];
                at = end;
                value
            }
        };
        found.entry(name).or_insert_with(|| value.to_string());
    }
    found
}

/// A page's preview from its `<head>`: OpenGraph, then Twitter's card, then its own
/// title and description; its `og:image`s (up to four), else its `twitter:image`.
pub fn parse_page(html: &str, page: &url::Url) -> Option<Preview> {
    let head = &html[..html.find("</head>").unwrap_or(html.len()).min(html.len())];
    let mut tags: Vec<(String, String)> = Vec::new();
    let mut rest = head;
    while let Some(at) = rest.find("<meta") {
        rest = &rest[at + 5..];
        let end = rest.find('>').unwrap_or(rest.len());
        let found = attributes(&rest[..end]);
        if let (Some(name), Some(content)) = (found.get("property").or_else(|| found.get("name")), found.get("content")) {
            tags.push((name.to_ascii_lowercase(), content.clone()));
        }
        rest = &rest[end..];
    }
    let first = |names: &[&str]| names.iter().find_map(|name| tags.iter().find(|(key, value)| key == name && !value.trim().is_empty()).map(|(_, value)| value.clone()));
    let page_title = head.find("<title").and_then(|at| {
        let after = &head[at..];
        let open = after.find('>')? + 1;
        let close = after.find("</title>")?;
        (open <= close).then(|| after[open..close].to_string())
    });
    let title = first(&["og:title", "twitter:title"]).or(page_title).map(|t| clean(&t, MOST_TITLE)).filter(|t| !t.is_empty());
    let description = first(&["og:description", "twitter:description", "description"]).map(|d| clean(&d, MOST_DESCRIPTION)).filter(|d| !d.is_empty());
    let site = first(&["og:site_name"]).map(|s| clean(&s, 100)).filter(|s| !s.is_empty()).or_else(|| page.host_str().map(|h| h.trim_start_matches("www.").to_string()));
    let absolute = |address: &str| page.join(entities(address.trim()).as_str()).ok().filter(|u| matches!(u.scheme(), "https" | "http")).map(|u| u.to_string());
    let mut pictures: Vec<String> = tags.iter().filter(|(key, _)| key == "og:image" || key == "og:image:url" || key == "og:image:secure_url").filter_map(|(_, value)| absolute(value)).collect();
    if pictures.is_empty() {
        pictures = tags.iter().filter(|(key, _)| key == "twitter:image" || key == "twitter:image:src").filter_map(|(_, value)| absolute(value)).collect();
    }
    pictures.dedup();
    pictures.truncate(MOST_PICTURES);
    let preview = Preview { kind: "page".into(), site, title, description, pictures, channel: None, channel_url: None };
    preview.shows_something().then_some(preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_links_of_a_post_are_its_bare_and_markdown_links_not_code_pictures_or_gifs() {
        let text = "see https://example.com/a, and [this](https://example.org/b) and `https://code.example/c`\n\n```\nhttps://block.example/d\n```\n![pic](https://pics.example/e.png) https://static.klipy.com/ii/x.webp https://example.com/a";
        assert_eq!(links_in(text), vec!["https://example.com/a".to_string(), "https://example.org/b".to_string()]);
        let many: String = (0..9).map(|n| format!("https://example.com/{n} ")).collect();
        assert_eq!(links_in(&many).len(), MOST_PER_POST);
    }

    #[test]
    fn youtube_and_twitter_links_are_known() {
        assert_eq!(youtube_id("https://www.youtube.com/watch?v=CD_wOuLtjUc").as_deref(), Some("CD_wOuLtjUc"));
        assert_eq!(youtube_id("https://youtu.be/oBtnrXiK_0Q?is=HK5SzYtPB_n0TMVF").as_deref(), Some("oBtnrXiK_0Q"));
        assert_eq!(youtube_id("https://www.youtube.com/shorts/dQw4w9WgXcQ").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(youtube_id("https://www.youtube.com/@channel"), None);
        assert_eq!(youtube_id("https://youtube.com.evil.example/watch?v=dQw4w9WgXcQ"), None);
        assert_eq!(twitter_source("https://x.com/MiraLunaMocha/status/2116911756807393310?s=20").as_deref(), Some("https://vxtwitter.com/MiraLunaMocha/status/2116911756807393310"));
        assert_eq!(twitter_source("https://mobile.twitter.com/a/status/1").as_deref(), Some("https://vxtwitter.com/a/status/1"));
        assert_eq!(twitter_source("https://example.com/x.com/a"), None);
    }

    #[test]
    fn a_page_says_what_it_is_in_its_head() {
        let page = url::Url::parse("https://news.example/story/1").unwrap();
        let html = r#"<html><head><title>Fallback &amp; title</title>
            <meta property="og:site_name" content="News Example">
            <meta property='og:title' content="A &quot;story&quot;">
            <meta name="description" content="What   happened.">
            <meta property="og:image" content="/pictures/one.jpg">
            <meta property="og:image" content="https://cdn.example/two.png">
            <meta property="og:image" content="javascript:alert(1)">
            </head><body><meta property="og:title" content="not in the head"></body></html>"#;
        let preview = parse_page(html, &page).unwrap();
        assert_eq!(preview.site.as_deref(), Some("News Example"));
        assert_eq!(preview.title.as_deref(), Some("A \"story\""));
        assert_eq!(preview.description.as_deref(), Some("What happened."));
        assert_eq!(preview.pictures, vec!["https://news.example/pictures/one.jpg".to_string(), "https://cdn.example/two.png".to_string()]);
        // Only its own title: still something to show, named for its site.
        let bare = parse_page("<head><title>Just a title</title></head>", &page).unwrap();
        assert_eq!((bare.title.as_deref(), bare.site.as_deref()), (Some("Just a title"), Some("news.example")));
        assert!(parse_page("<head></head>", &page).is_none());
    }

    #[test]
    fn vxtwitters_answer_is_a_preview() {
        let page = url::Url::parse("https://vxtwitter.com/MiraLunaMocha/status/2116911756807393310").unwrap();
        let html = r#"<head><meta name="og:title" content="Mira Luna 🌙✨ (@MiraLunaMocha)" /><meta property="og:image" content="https://pbs.twimg.com/media/HWDH6sRWwAAfCGA.png?name=orig" /><meta property="og:description" content="You got Steam Points for paying for Steamworks submission??? 😂" /></head>"#;
        let preview = parse_page(html, &page).unwrap();
        assert_eq!(preview.title.as_deref(), Some("Mira Luna 🌙✨ (@MiraLunaMocha)"));
        assert_eq!(preview.pictures.len(), 1);
    }

    /// The real sites, read as a page would: run with `--ignored` (it goes out to the internet).
    #[tokio::test]
    #[ignore]
    async fn real_links_have_previews() {
        for address in ["https://www.youtube.com/watch?v=CD_wOuLtjUc", "https://x.com/MiraLunaMocha/status/2116911756807393310", "https://www.rust-lang.org/"] {
            let preview = read(address).await;
            println!("{address}\n  {preview:?}");
            assert!(preview.shows_something(), "{address}");
        }
    }
}
