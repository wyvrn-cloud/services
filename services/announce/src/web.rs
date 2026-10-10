//! The web pages: every post, newest first, for anyone to read.

use pulldown_cmark::{html, Event, Options, Parser};
use time::{format_description::well_known::Rfc3339, macros::format_description, OffsetDateTime};

use crate::channel::{Channel, ChannelInfo};
use crate::store::Post;

/// How many posts a page of the log shows.
pub const PAGE_SIZE: usize = 20;

pub fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// A post's Markdown as HTML. HTML written into a post is shown as the text it is,
/// never run: a post is an admin's, but an admin's account can be someone else's for
/// a day.
pub fn render_markdown(content: &str) -> String {
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(content, options).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        other => other,
    });
    let mut rendered = String::new();
    html::push_html(&mut rendered, events);
    rendered
}

/// What to call a post that has no title of its own: its first line, without the
/// Markdown.
pub fn title_of(content: &str) -> String {
    let line = content.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
    let plain: String = line
        .trim_start_matches('#')
        .trim()
        .chars()
        .filter(|c| !matches!(c, '*' | '_' | '`'))
        .collect();
    let mut title: String = plain.chars().take(80).collect();
    if plain.chars().count() > 80 {
        title.push('…');
    }
    title
}

fn date(unix: i64) -> String {
    OffsetDateTime::from_unix_timestamp(unix)
        .ok()
        .and_then(|at| at.format(format_description!("[day padding:none] [month repr:long] [year]")).ok())
        .unwrap_or_default()
}

fn rfc3339(unix: i64) -> String {
    OffsetDateTime::from_unix_timestamp(unix).ok().and_then(|at| at.format(&Rfc3339).ok()).unwrap_or_default()
}

const STYLE: &str = r#"
:root { color-scheme: light dark; --text: #1c1b22; --dim: #62616b; --bg: #fbfbfd; --card: #fff; --border: #dddce4; --accent: #5b4bd6; }
@media (prefers-color-scheme: dark) { :root { --text: #ececf1; --dim: #a09fab; --bg: #15141a; --card: #1e1d25; --border: #34333e; --accent: #a99cff; } }
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--text); font: 16px/1.6 system-ui, -apple-system, "Segoe UI", sans-serif; }
main { max-width: 720px; margin: 0 auto; padding: 32px 16px 64px; }
header h1 { margin: 0 0 4px; font-size: 1.7rem; }
header p { margin: 0; color: var(--dim); }
a { color: var(--accent); }
.subscribe { margin: 20px 0 28px; padding: 12px 14px; border: 1px solid var(--border); border-radius: 10px; background: var(--card); font-size: .92rem; }
.subscribe summary { cursor: pointer; font-weight: 600; }
.subscribe code { display: block; margin-top: 8px; padding: 8px; overflow-wrap: anywhere; background: var(--bg); border-radius: 6px; font-size: .8rem; }
article { margin: 0 0 20px; padding: 16px 18px; border: 1px solid var(--border); border-radius: 10px; background: var(--card); overflow-wrap: anywhere; }
article .meta { margin: 0 0 6px; color: var(--dim); font-size: .85rem; }
article .meta a { color: inherit; text-decoration: none; }
article .meta a:hover { text-decoration: underline; }
article :is(h1, h2, h3) { line-height: 1.3; margin: .8em 0 .4em; }
article h1 { font-size: 1.35rem; } article h2 { font-size: 1.15rem; } article h3 { font-size: 1rem; }
article > :nth-child(2) { margin-top: 0; }
article > :last-child { margin-bottom: 0; }
article img { max-width: 100%; }
article pre { padding: 10px; overflow-x: auto; background: var(--bg); border-radius: 6px; }
article code { font-size: .9em; }
article blockquote { margin: 0; padding-left: 12px; border-left: 3px solid var(--border); color: var(--dim); }
article table { border-collapse: collapse; } article :is(td, th) { padding: 4px 10px; border: 1px solid var(--border); }
nav { display: flex; justify-content: space-between; margin-top: 24px; }
.empty { color: var(--dim); }
footer { margin-top: 40px; color: var(--dim); font-size: .85rem; }
"#;

fn page(info: &ChannelInfo, title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{title}</title>\
<meta name=\"description\" content=\"{description}\">\
<link rel=\"alternate\" type=\"application/atom+xml\" title=\"{name}\" href=\"/feed.xml\">\
<style>{STYLE}</style></head><body><main>{body}\
<footer><a href=\"/feed.xml\">Feed</a></footer></main></body></html>\n",
        title = escape(title),
        description = escape(&info.description),
        name = escape(&info.name),
    )
}

fn article(channel: &Channel, post: &Post) -> String {
    let author = channel.author_of(post).map(|name| format!(" · {}", escape(name))).unwrap_or_default();
    format!(
        "<article id=\"post-{seq}\"><p class=\"meta\"><a href=\"/posts/{seq}\"><time datetime=\"{iso}\">{date}</time></a>{author}</p>{content}</article>",
        seq = post.seq,
        iso = rfc3339(post.posted_at),
        date = date(post.posted_at),
        content = render_markdown(&post.content),
    )
}

fn header(info: &ChannelInfo, did: &str, linked: bool) -> String {
    let name = if linked { format!("<a href=\"/\">{}</a>", escape(&info.name)) } else { escape(&info.name) };
    format!(
        "<header><h1>{name}</h1><p>{description}</p></header>\
<details class=\"subscribe\"><summary>Get these in Wyvrn</summary>\
Add this channel as a contact, by its DID. New posts then arrive in the app as they are made.\
<code>{did}</code></details>",
        description = escape(&info.description),
        did = escape(did),
    )
}

/// The log: `posts` newest first, with a way on to older and back to newer ones.
pub fn index_page(channel: &Channel, did: &str, posts: &[Post], older: Option<i64>, newer: bool) -> String {
    let mut body = header(&channel.info, did, false);
    if posts.is_empty() {
        body.push_str("<p class=\"empty\">Nothing has been posted yet.</p>");
    }
    for post in posts {
        body.push_str(&article(channel, post));
    }
    if older.is_some() || newer {
        body.push_str("<nav>");
        body.push_str(if newer { "<a href=\"/\">← Newest</a>" } else { "<span></span>" });
        if let Some(before) = older {
            body.push_str(&format!("<a href=\"/?before={before}\">Older →</a>"));
        }
        body.push_str("</nav>");
    }
    page(&channel.info, &channel.info.name, &body)
}

pub fn post_page(channel: &Channel, did: &str, post: &Post) -> String {
    let body = format!("{}{}", header(&channel.info, did, true), article(channel, post));
    page(&channel.info, &format!("{} · {}", title_of(&post.content), channel.info.name), &body)
}

/// The newest posts as an Atom feed.
pub fn feed(channel: &Channel, did: &str, posts: &[Post]) -> String {
    let base = channel.info.url.trim_end_matches('/');
    let updated = posts.first().map(|post| rfc3339(post.posted_at)).unwrap_or_else(|| rfc3339(0));
    let mut feed = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<feed xmlns=\"http://www.w3.org/2005/Atom\">\
<title>{name}</title><subtitle>{description}</subtitle><id>{id}</id>\
<link href=\"{base}/\"/><link rel=\"self\" href=\"{base}/feed.xml\"/><updated>{updated}</updated>",
        name = escape(&channel.info.name),
        description = escape(&channel.info.description),
        id = escape(did),
        base = escape(base),
    );
    for post in posts {
        let author = channel.author_of(post).unwrap_or(&channel.info.name);
        feed.push_str(&format!(
            "<entry><title>{title}</title><id>urn:uuid:{id}</id><link href=\"{base}/posts/{seq}\"/>\
<updated>{at}</updated><author><name>{author}</name></author><content type=\"html\">{content}</content></entry>",
            title = escape(&title_of(&post.content)),
            id = escape(&post.id),
            base = escape(base),
            seq = post.seq,
            at = rfc3339(post.posted_at),
            author = escape(author),
            content = escape(&render_markdown(&post.content)),
        ));
    }
    feed.push_str("</feed>\n");
    feed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Role, Store};

    fn channel() -> Channel {
        let store = Store::in_memory().unwrap();
        store.set_role("did:a", Role::Admin, 1).unwrap();
        Channel {
            store,
            info: ChannelInfo {
                name: "Wyvrn <Updates>".into(),
                description: "Release notes & more".into(),
                url: "https://updates.example/".into(),
                max_post_length: 1000,
                ..ChannelInfo::default()
            },
        }
    }

    fn post(channel: &Channel, content: &str) -> Post {
        let author = channel.store.member("did:a").unwrap().unwrap();
        channel.store.add_post(&uuid::Uuid::new_v4().to_string(), content, &author, 1_791_600_000).unwrap()
    }

    #[test]
    fn markdown_is_rendered_and_html_in_a_post_is_only_ever_text() {
        let rendered = render_markdown("# 0.0.9\n\n- **Fixed** a thing\n\n<script>alert(1)</script> <b onclick=x>hi</b>");
        assert!(rendered.contains("<h1>0.0.9</h1>"));
        assert!(rendered.contains("<strong>Fixed</strong>"));
        assert!(!rendered.contains("<script"));
        assert!(!rendered.contains("<b onclick"));
        assert!(rendered.contains("&lt;script&gt;"));
    }

    #[test]
    fn a_post_is_called_by_its_first_line() {
        assert_eq!(title_of("\n## Version **0.0.9**\n\nNotes"), "Version 0.0.9");
        assert_eq!(title_of(&"x".repeat(200)).chars().count(), 81);
        assert_eq!(title_of(""), "");
    }

    #[test]
    fn the_log_page_shows_each_post_and_escapes_what_the_channel_is_called() {
        let channel = channel();
        let posts = vec![post(&channel, "Second"), post(&channel, "First")];
        let page = index_page(&channel, "did:web:updates.example", &posts, Some(1), false);
        assert!(page.contains("Wyvrn &lt;Updates&gt;"));
        assert!(page.contains("Release notes &amp; more"));
        assert!(page.contains("did:web:updates.example"));
        assert!(page.contains("href=\"/posts/1\""));
        assert!(page.contains("href=\"/?before=1\""));
        assert!(page.find("Second").unwrap() < page.find("First").unwrap());
        // Nowhere to write anything.
        assert!(!page.contains("<form") && !page.contains("<input") && !page.contains("<textarea"));
    }

    #[test]
    fn an_empty_log_says_so() {
        let channel = channel();
        assert!(index_page(&channel, "did:x", &[], None, false).contains("Nothing has been posted yet."));
    }

    #[test]
    fn the_feed_lists_posts_with_their_pages() {
        let channel = channel();
        let posts = vec![post(&channel, "# Hello\n\nworld")];
        let feed = feed(&channel, "did:web:updates.example", &posts);
        assert!(feed.contains("<title>Hello</title>"));
        assert!(feed.contains("https://updates.example/posts/1"));
        assert!(feed.contains("&lt;h1&gt;Hello&lt;/h1&gt;"));
    }
}
