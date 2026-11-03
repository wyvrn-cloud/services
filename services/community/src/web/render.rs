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

/// The web addresses written bare in `text` (not in Markdown's own link syntax),
/// each with where it starts and ends, as GitHub's Markdown links them: `http://`
/// or `https://` up to a space or `<`, less what ends a sentence after it (and a
/// closing bracket that opened before it).
pub fn bare_links(text: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("http").map(|at| from + at) {
        let rest = &text[at..];
        let scheme = if rest.starts_with("https://") { 8 } else if rest.starts_with("http://") { 7 } else { 0 };
        // Only at the start of a word.
        let starts_word = at == 0 || !text[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric());
        if scheme == 0 || !starts_word {
            from = at + 4;
            continue;
        }
        let mut end = at + rest.find(|c: char| c.is_whitespace() || c == '<').unwrap_or(rest.len());
        loop {
            let candidate = &text[at..end];
            let last = candidate.chars().next_back();
            let unbalanced = last == Some(')') && candidate.matches(')').count() > candidate.matches('(').count();
            if matches!(last, Some('.' | ',' | ':' | ';' | '!' | '?' | '"' | '\'' | '*' | '_' | '~')) || unbalanced {
                end -= last.map_or(1, char::len_utf8);
            } else {
                break;
            }
        }
        if end > at + scheme {
            found.push((at, end));
        }
        from = end.max(at + scheme);
    }
    found
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
    // Inside a link (or code), an address written bare stays as it is.
    let mut in_link = 0usize;
    let mut in_code = 0usize;
    // Text the parser hands over in pieces (it stops at `_` and `*`, which might
    // have been emphasis) is joined again, so that an address in it is whole.
    let mut joined: Vec<Event> = Vec::new();
    for event in Parser::new_ext(text, options) {
        match (joined.last_mut(), event) {
            (Some(Event::Text(before)), Event::Text(more)) => *before = CowStr::from(format!("{before}{more}")),
            (_, event) => joined.push(event),
        }
    }
    for event in joined {
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
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code += 1;
                events.push(Event::Start(Tag::CodeBlock(kind)));
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = in_code.saturating_sub(1);
                events.push(Event::End(TagEnd::CodeBlock));
            }
            Event::Text(said) if in_link == 0 && in_code == 0 && said.contains("http") => {
                let mut at = 0;
                for (start, end) in bare_links(&said) {
                    if start > at {
                        events.push(Event::Text(CowStr::from(said[at..start].to_string())));
                    }
                    let address = &said[start..end];
                    events.push(Event::Html(CowStr::from(format!("<a href=\"{}\" rel=\"nofollow ugc noopener noreferrer\">{}</a>", escape(address), escape(address)))));
                    at = end;
                }
                if at < said.len() {
                    events.push(Event::Text(CowStr::from(said[at..].to_string())));
                }
            }
            Event::Start(Tag::Link { link_type, dest_url, .. }) => {
                in_link += 1;
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
                in_link = in_link.saturating_sub(1);
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

/// The most emoji a post can be made of and still be drawn large, as the app has it.
pub const GIANT_EMOJI_LIMIT: usize = 25;

/// How many emoji a post is, if it is nothing else: emoji and custom emoji (a
/// picture `custom` says is one), with only space between them, and no more than
/// [`GIANT_EMOJI_LIMIT`]; `0` for anything else. As the app's emojiOnly.ts.
pub fn emoji_only_count(text: &str, custom: &dyn Fn(&str) -> bool) -> usize {
    let mut count = 0;
    let mut inside: Option<String> = None;
    for event in Parser::new(text) {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => inside = Some(dest_url.to_string()),
            Event::End(TagEnd::Image) => match inside.take() {
                Some(address) if custom(&address) => count += 1,
                _ => return 0,
            },
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) | Event::SoftBreak => {}
            Event::Text(_) if inside.is_some() => {}
            Event::Text(said) => match emoji_in(&said) {
                Some(n) => count += n,
                None => return 0,
            },
            _ => return 0,
        }
        if count > GIANT_EMOJI_LIMIT {
            return 0;
        }
    }
    count
}

/// How many emoji `text` is, space aside, or `None` if it is anything else. An emoji
/// with its modifiers (skin tone, the emoji form, a keycap, tags) and the emoji joined
/// to it (a family, a profession) is one; so is a flag, a pair of regional indicators.
fn emoji_in(text: &str) -> Option<usize> {
    let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    let (mut count, mut at) = (0, 0);
    let regional = |c: char| ('\u{1F1E6}'..='\u{1F1FF}').contains(&c);
    while at < chars.len() {
        let c = chars[at];
        if regional(c) {
            if !chars.get(at + 1).is_some_and(|next| regional(*next)) {
                return None;
            }
            at += 2;
        } else if matches!(c, '0'..='9' | '#' | '*') {
            // A keycap: the character, perhaps the emoji form, and the keycap mark.
            at += 1;
            if chars.get(at) == Some(&'\u{FE0F}') {
                at += 1;
            }
            if chars.get(at) != Some(&'\u{20E3}') {
                return None;
            }
            at += 1;
        } else if is_emoji(c) {
            at += 1;
        } else {
            return None;
        }
        count += 1;
        loop {
            match chars.get(at).copied() {
                Some('\u{FE0F}' | '\u{20E3}' | '\u{1F3FB}'..='\u{1F3FF}' | '\u{E0020}'..='\u{E007F}') => at += 1,
                Some('\u{200D}') if chars.get(at + 1).is_some_and(|next| is_emoji(*next)) => at += 2,
                _ => break,
            }
        }
    }
    Some(count)
}

/// Whether `c` is a character drawn as an emoji (or one that becomes one with the
/// emoji form after it).
fn is_emoji(c: char) -> bool {
    matches!(c,
        '\u{1F000}'..='\u{1FAFF}' | '\u{2600}'..='\u{27BF}' | '\u{2300}'..='\u{23FF}' | '\u{2B00}'..='\u{2BFF}' |
        '\u{2190}'..='\u{21FF}' | '\u{25A0}'..='\u{25FF}' | '\u{2934}' | '\u{2935}' | '\u{3030}' | '\u{303D}' |
        '\u{3297}' | '\u{3299}' | '\u{00A9}' | '\u{00AE}' | '\u{203C}' | '\u{2049}' | '\u{2122}' | '\u{2139}')
}

/// The size a post of `count` emoji alone is drawn at, larger the fewer there are,
/// as the app does; nothing for `0`.
pub fn giant_class(count: usize) -> &'static str {
    match count {
        0 => "",
        1..=3 => " giant-emoji few",
        4..=10 => " giant-emoji some",
        _ => " giant-emoji many",
    }
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
    fn an_address_written_bare_is_a_link_but_not_in_code_or_a_link() {
        let html = markdown("see https://www.youtube.com/watch?v=CD_wOuLtjUc, then (https://example.com/a_(b)) `https://code.example` [x](https://linked.example)");
        assert!(html.contains("<a href=\"https://www.youtube.com/watch?v=CD_wOuLtjUc\" rel=\"nofollow ugc noopener noreferrer\">https://www.youtube.com/watch?v=CD_wOuLtjUc</a>,"), "{html}");
        assert!(html.contains(">https://example.com/a_(b)</a>)"), "{html}");
        assert!(html.contains("<code>https://code.example</code>"), "{html}");
        assert_eq!(html.matches("https://linked.example").count(), 1, "{html}");
        assert_eq!(bare_links("xhttps://no.example and http:// nothing"), vec![]);
        assert!(!markdown("https://a.example/<script>").contains("<script>"));
    }

    #[test]
    fn a_post_of_emoji_alone_is_counted_as_the_app_counts_it() {
        let custom = |address: &str| address.ends_with("0123456789abcdef0123456789abcdef.webp");
        let emoji = "![party](https://pack.example/0123456789abcdef0123456789abcdef.webp)";
        assert_eq!(emoji_only_count("🎉", &custom), 1);
        assert_eq!(emoji_only_count("👍🏽 ❤️ 👨‍👩‍👧 🇨🇦 #️⃣", &custom), 5);
        assert_eq!(emoji_only_count(&format!("{emoji} {emoji} 🎉"), &custom), 3);
        assert_eq!(emoji_only_count("hi 🎉", &custom), 0);
        assert_eq!(emoji_only_count("![x](https://elsewhere.example/a.png)", &custom), 0);
        assert_eq!(emoji_only_count(&"🎉".repeat(26), &custom), 0);
        assert_eq!((giant_class(1), giant_class(4), giant_class(11), giant_class(0)), (" giant-emoji few", " giant-emoji some", " giant-emoji many", ""));
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
