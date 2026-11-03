//! Posts' Markdown as HTML for the web, as announcement channels render theirs:
//! HTML written into a post is shown as text, never run. Beyond that, a link goes
//! only to `http(s):` or `mailto:` and is marked as the poster's (`nofollow ugc`),
//! and a picture from elsewhere is a link to it, never fetched by the page.

use pulldown_cmark::{html, CowStr, Event, LinkType, Options, Parser, Tag, TagEnd};

/// `text` with `&`, `<`, `>`, `"` and `'` written as entities.
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

/// Whether a link may be followed from a page: the web, or mail.
fn followable(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
}

/// A post's Markdown, as HTML that runs nothing and fetches nothing.
pub fn markdown(text: &str) -> String {
    markdown_with(text, &|_, _| None)
}

/// [`markdown`], except that a picture `shown` gives an address for (from its own
/// address and its words) is drawn there, small, as a custom emoji is.
pub fn markdown_with(text: &str, shown: &dyn Fn(&str, &str) -> Option<String>) -> String {
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let mut events = Vec::new();
    // Inside a picture, its words are gathered for the link that stands for it.
    let mut picture: Option<(String, String)> = None;
    let mut dropped_links = 0usize;
    for event in Parser::new_ext(text, options) {
        if let Some((_, words)) = picture.as_mut() {
            match event {
                Event::End(TagEnd::Image) => {
                    let (address, words) = picture.take().unwrap_or_default();
                    if let Some(source) = shown(&address, &words) {
                        let name = words.trim().trim_matches(':');
                        let name = if name.is_empty() { "emoji".to_string() } else { format!(":{name}:") };
                        events.push(Event::Html(CowStr::from(format!("<img class=\"emoji\" src=\"{}\" alt=\"{}\" title=\"{}\" loading=\"lazy\">", escape(&source), escape(&name), escape(&name)))));
                        continue;
                    }
                    let words = if words.trim().is_empty() { "picture".to_string() } else { words };
                    if followable(&address) {
                        events.push(Event::Html(CowStr::from(format!("<a href=\"{}\" rel=\"nofollow ugc noopener noreferrer\">{}</a>", escape(&address), escape(&words)))));
                    } else {
                        events.push(Event::Text(CowStr::from(words)));
                    }
                }
                Event::Text(said) | Event::Code(said) => words.push_str(&said),
                _ => {}
            }
            continue;
        }
        match event {
            // What a poster wrote as HTML is shown as they wrote it.
            Event::Html(raw) | Event::InlineHtml(raw) => events.push(Event::Text(raw)),
            Event::Start(Tag::Image { dest_url, .. }) => picture = Some((dest_url.to_string(), String::new())),
            Event::Start(Tag::Link { link_type, dest_url, .. }) => {
                let address = match link_type {
                    LinkType::Email => format!("mailto:{dest_url}"),
                    _ => dest_url.to_string(),
                };
                if followable(&address) {
                    events.push(Event::Html(CowStr::from(format!("<a href=\"{}\" rel=\"nofollow ugc noopener noreferrer\">", escape(&address)))));
                } else {
                    dropped_links += 1;
                }
            }
            Event::End(TagEnd::Link) => {
                if dropped_links > 0 {
                    dropped_links -= 1;
                } else {
                    events.push(Event::Html(CowStr::from("</a>")));
                }
            }
            other => events.push(other),
        }
    }
    let mut rendered = String::new();
    html::push_html(&mut rendered, events.into_iter());
    rendered
}

/// Whether `text` says nothing but pictures (custom emoji), up to `most` of them:
/// a post an app draws large.
pub fn only_pictures(text: &str, most: usize) -> bool {
    let mut pictures = 0;
    let mut inside = 0;
    for event in Parser::new(text) {
        match event {
            Event::Start(Tag::Image { .. }) => {
                pictures += 1;
                inside += 1;
            }
            Event::End(TagEnd::Image) => inside -= 1,
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) | Event::SoftBreak => {}
            Event::Text(said) if inside > 0 || said.trim().is_empty() => {}
            _ => return false,
        }
    }
    (1..=most).contains(&pictures)
}

/// Up to `most` characters of what a post says, as plain text on one line: a
/// page's description.
pub fn excerpt(text: &str, most: usize) -> String {
    let mut plain = String::new();
    for event in Parser::new(text) {
        match event {
            Event::Text(said) | Event::Code(said) | Event::Html(said) | Event::InlineHtml(said) => plain.push_str(&said),
            Event::SoftBreak | Event::HardBreak | Event::End(_) => plain.push(' '),
            _ => {}
        }
    }
    let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if plain.chars().count() <= most {
        return plain;
    }
    let cut: String = plain.chars().take(most.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// A time as people read it: `1 Nov 2026, 14:05 UTC`.
pub fn when(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0).map(|t| t.format("%-d %b %Y, %H:%M UTC").to_string()).unwrap_or_default()
}

/// A time as machines read it (`<time datetime>`, schema.org, the sitemap).
pub fn iso(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0).map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap_or_default()
}

/// The size of a file, as a person reads it.
pub fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1024 * 1024 * 1024 => format!("{:.1} GB", b as f64 / (1024.0 * 1024.0 * 1024.0)),
        b if b >= 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        b if b >= 1024 => format!("{} KB", b / 1024),
        b => format!("{b} bytes"),
    }
}

/// JSON for inside a `<script type="application/ld+json">`: nothing in it can
/// end the script.
pub fn json_ld(value: &serde_json::Value) -> String {
    value.to_string().replace('<', "\\u003c").replace('>', "\\u003e").replace('&', "\\u0026")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_in_a_post_is_only_ever_text() {
        let rendered = markdown("**hi** <script>alert(1)</script> <b onclick=x>there</b>");
        assert!(rendered.contains("<strong>hi</strong>"));
        assert!(!rendered.contains("<script"));
        assert!(!rendered.contains("<b "));
        assert!(rendered.contains("&lt;script&gt;"));
    }

    #[test]
    fn links_go_only_to_the_web_and_are_the_posters() {
        let rendered = markdown("[ok](https://example.com/a?b=\"c\") [bad](javascript:alert(1)) [data](data:text/html,x) <https://auto.example>");
        assert!(rendered.contains("<a href=\"https://example.com/a?b=&quot;c&quot;\" rel=\"nofollow ugc noopener noreferrer\">ok</a>"));
        assert!(!rendered.contains("javascript:"));
        assert!(!rendered.contains("data:text"));
        assert!(rendered.contains("bad"));
        assert!(rendered.contains("href=\"https://auto.example\""));
    }

    #[test]
    fn a_picture_from_elsewhere_is_a_link_to_it() {
        let rendered = markdown("look ![a cat](https://cats.example/cat.png) ![x](javascript:1)");
        assert!(!rendered.contains("<img"));
        assert!(rendered.contains("<a href=\"https://cats.example/cat.png\" rel=\"nofollow ugc noopener noreferrer\">a cat</a>"));
        assert!(!rendered.contains("javascript"));
    }

    #[test]
    fn excerpts_are_plain_and_short() {
        assert_eq!(excerpt("# Title\n\nSome **bold** text", 100), "Title Some bold text");
        assert_eq!(excerpt("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn json_ld_cannot_end_its_script() {
        assert!(!json_ld(&serde_json::json!({ "a": "</script><script>x" })).contains("</script>"));
    }
}
