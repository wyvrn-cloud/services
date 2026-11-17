//! What an AutoMod rule finds in a post, and the post with it censored.
//!
//! A rule finds:
//!
//! - its own **words**, however they are disguised: each letter may be written as
//!   something that looks like it (`$` for s, `1` and `!` for i, `0` for o, a
//!   Cyrillic `ѕ`...), repeated (`shiiit`), with a space or a dot between letters
//!   (`s h i t`, `s.h.i.t`), and with `*`, `#`, `%` or `@` standing for any
//!   letter after the first (`sh#t`). The word is still found only whole, and a
//!   letter only stands for its own look-alikes, so "shit" never finds "ship".
//! - the **filters** it has on (`filters`: profanity, slurs, sexual words, insults,
//!   each from mild words up or from moderate or only the worst), from rustrict's
//!   lists, which catch the same disguises and more;
//! - its **patterns** (regular expressions);
//! - **links**: any link at all (`links`), except to `allowed_domains`; and links to
//!   `blocked_domains` or to the domain lists it uses (`domain_lists`: adult sites,
//!   link shorteners), even written without `https://` or as `example[.]com`.
//!   A blocked domain is blocked even if it is allowed too; an allowed domain is let
//!   through a domain list;
//! - more mentions than `max_mentions`.
//!
//! What it finds inside one of its `allowed_words` (a town, a name, "shiitake") it
//! lets be. Censored, words become dots and a link `<UNSAFE URL BLOCKED>`.

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;
use rustrict::{Censor, Type};
use serde_json::Value;

/// What censored words become, one a letter.
pub const DOT: char = '•';
/// What a censored link becomes.
pub const LINK_BLOCKED: &str = "<UNSAFE URL BLOCKED>";
/// The largest compiled pattern a rule may have (Unicode classes such as `\w` take
/// most of it).
const PATTERN_SIZE: usize = 1 << 21;

/// The filters a rule may turn on: its key, what it is called, and rustrict's kind.
pub const FILTERS: [(&str, &str, Type); 4] = [
    ("profanity", "Swearing", Type::PROFANE),
    ("slurs", "Slurs and hate", Type::OFFENSIVE),
    ("sexual", "Sexual words", Type::SEXUAL),
    ("insults", "Insults", Type::MEAN),
];

/// How strict a filter is: its key, and what it is called.
pub const STRICTNESS: [(&str, &str); 3] = [("mild", "Strict: mild words too"), ("moderate", "Moderate"), ("severe", "Only the worst")];

fn level(strictness: &str) -> Option<Type> {
    match strictness {
        "mild" => Some(Type::MILD_OR_HIGHER),
        "moderate" => Some(Type::MODERATE_OR_HIGHER),
        "severe" => Some(Type::SEVERE),
        _ => None,
    }
}

/// The domain lists a rule may use: its key, what it is called, and the domains
/// (a link is to one if it is that domain or under it; "xxx" is a whole ending).
pub const DOMAIN_LISTS: [(&str, &str, &[&str]); 2] = [
    (
        "adult",
        "Adult sites",
        &[
            "xxx", "porn", "sex", "adult", "pornhub.com", "xvideos.com", "xnxx.com", "xhamster.com", "redtube.com", "youporn.com", "tube8.com", "spankbang.com",
            "eporner.com", "brazzers.com", "onlyfans.com", "fansly.com", "manyvids.com", "chaturbate.com", "stripchat.com", "livejasmin.com", "bongacams.com",
            "cam4.com", "camsoda.com", "myfreecams.com", "motherless.com", "rule34.xxx", "e-hentai.org", "nhentai.net", "hanime.tv", "literotica.com",
        ],
    ),
    (
        "shorteners",
        "Link shorteners (they hide where a link goes)",
        &[
            "bit.ly", "tinyurl.com", "t.co", "goo.gl", "ow.ly", "is.gd", "v.gd", "buff.ly", "rebrand.ly", "cutt.ly", "shorturl.at", "tiny.cc", "rb.gy", "t.ly",
            "bl.ink", "s.id", "adf.ly", "shorte.st", "bc.vc", "ouo.io", "lnkd.in", "trib.al", "soo.gd", "clck.ru", "qr.ae", "tinyurl.is", "shrtco.de",
        ],
    ),
];

pub fn strings(value: &Value) -> Vec<String> {
    value.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// A pattern as a rule gives it, compiled with a size limit, or why it can't be.
pub fn compile(pattern: &str) -> Result<Regex, String> {
    regex::RegexBuilder::new(pattern).case_insensitive(true).size_limit(PATTERN_SIZE).build().map_err(|error| error.to_string())
}

/// What else a letter is written as, to disguise a word.
fn look_alikes(letter: char) -> &'static str {
    match letter {
        'a' => "4@àáâãäåαа∆",
        'b' => "8ß",
        'c' => "(¢©çс",
        'e' => "3€èéêëе£",
        'g' => "96",
        'h' => "#",
        'i' => "1!|lìíîïіı",
        'l' => "1|!",
        'o' => "0°òóôõöøо",
        'p' => "р",
        's' => "$5§ѕ",
        't' => "7+",
        'u' => "vùúûüμ",
        'x' => "×х",
        'y' => "у",
        _ => "",
    }
}

/// A rule's words as one pattern that finds any of them, disguised or not, as a
/// whole word; the word found is its group `w`.
fn words_pattern(words: &[String]) -> String {
    let each: Vec<String> = words
        .iter()
        .map(|word| {
            let mut out = String::new();
            let mut letters = 0;
            for piece in word.to_lowercase().chars() {
                if piece == '*' {
                    out.push_str(r"[\p{L}\p{N}]*");
                    continue;
                }
                if !piece.is_alphanumeric() {
                    out.push_str(&regex::escape(&piece.to_string()));
                    continue;
                }
                if letters > 0 {
                    // Between letters: a space, dot, dash or the like, once.
                    out.push_str(r"[\s._\-~|]?");
                }
                let mut class: String = std::iter::once(piece).chain(look_alikes(piece).chars()).map(|c| regex::escape(&c.to_string())).collect();
                if letters > 0 {
                    class.push_str(r"*#%@");
                }
                out.push_str(&format!("[{class}]+"));
                letters += 1;
            }
            out
        })
        .collect();
    format!(r"(?i)(?:^|[^\p{{L}}\p{{N}}])(?P<w>{})(?:$|[^\p{{L}}\p{{N}}])", each.join("|"))
}

/// The byte ranges in `content` of each word found, disguised or not.
fn find_words(words: &[String], content: &str) -> Vec<Range<usize>> {
    if words.is_empty() {
        return Vec::new();
    }
    let Ok(re) = compile(&words_pattern(words)) else { return Vec::new() };
    let mut found = Vec::new();
    let mut at = 0;
    while at <= content.len() {
        let Some(word) = re.captures_at(content, at).and_then(|c| c.name("w")) else { break };
        found.push(word.range());
        // From the word's end, so the character after it may begin the next.
        at = word.end().max(at + 1);
        while !content.is_char_boundary(at) && at < content.len() {
            at += 1;
        }
    }
    found
}

/// The byte ranges in `content` of what the filters at `threshold` find.
fn find_filtered(threshold: Type, content: &str) -> Vec<Range<usize>> {
    let mut censor = Censor::from_str(content);
    censor.with_censor_threshold(threshold).with_censor_first_character_threshold(threshold).with_censor_replacement(DOT);
    let censored: Vec<char> = censor.censor().chars().collect();
    let original: Vec<(usize, char)> = content.char_indices().collect();
    // Each character of `content` and how many it is censored as: one, mostly, but
    // accents and marks rustrict leaves out are none.
    let widths: Vec<usize> = if censored.len() == original.len() {
        vec![1; original.len()]
    } else {
        original.iter().map(|(_, c)| Censor::from_str(&c.to_string()).with_censor_threshold(Type::NONE).censor().chars().count()).collect()
    };
    if widths.iter().sum::<usize>() != censored.len() {
        // (Out of step: the whole post, rather than the wrong words.)
        let whole = 0..content.len();
        return if censored.contains(&DOT) { vec![whole] } else { Vec::new() };
    }
    let mut found: Vec<Range<usize>> = Vec::new();
    let mut out = 0;
    for (n, (start, c)) in original.iter().enumerate() {
        let end = start + c.len_utf8();
        let dotted = (out..out + widths[n]).any(|i| censored[i] == DOT) && *c != DOT;
        // A mark left out goes with the letter before it.
        let with_last = widths[n] == 0 && found.last().is_some_and(|last| last.end == *start);
        if dotted || with_last {
            match found.last_mut() {
                Some(last) if last.end == *start => last.end = end,
                _ => found.push(*start..end),
            }
        }
        out += widths[n];
    }
    found
}

/// The filters a rule has on, as one threshold; none if it has none on.
fn threshold(rule: &Value) -> Option<Type> {
    FILTERS
        .iter()
        .filter_map(|(key, _, kind)| rule["filters"][key].as_str().and_then(level).map(|level| *kind & level))
        .reduce(|a, b| a | b)
}

static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\b(?:https?://|www\.)[^\s<>"]+"#).expect("a valid pattern"));
/// A domain written out without `https://`, perhaps disguised: `example[.]com`,
/// `example dot com`.
static BARE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b[a-z0-9][a-z0-9-]*(?:(?:\.|\[\.\]|\(\.\)|\[dot\]|\(dot\)| dot )[a-z0-9-]+)+(?:/[^\s<>"]*)?"#).expect("a valid pattern")
});
static DISGUISED_DOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\[\.\]|\(\.\)|\[dot\]|\(dot\)| dot ").expect("a valid pattern"));

/// The host a link is to, in lower case and without `www.`.
fn host_of(link: &str) -> String {
    let link = DISGUISED_DOT.replace_all(link, ".").to_lowercase();
    let rest = link.split_once("://").map(|(_, rest)| rest).unwrap_or(&link);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default().split(':').next().unwrap_or_default();
    host.trim_end_matches('.').trim_start_matches("www.").to_string()
}

fn under(host: &str, domain: &str) -> bool {
    let domain = domain.trim().trim_start_matches("www.").trim_start_matches('.').to_lowercase();
    !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
}

/// A link without what ends the sentence it is in.
fn trimmed(content: &str, mut range: Range<usize>) -> Range<usize> {
    while range.end > range.start && content[..range.end].ends_with(['.', ',', ')', '!', '?', ';', ':', '\'', '"']) {
        range.end -= 1;
    }
    range
}

/// The links in `content` the rule doesn't let be, each with its host.
fn find_links(rule: &Value, content: &str) -> Vec<(Range<usize>, String)> {
    let blocked = strings(&rule["blocked_domains"]);
    let allowed = strings(&rule["allowed_domains"]);
    let lists: Vec<&str> = strings(&rule["domain_lists"]).iter().filter_map(|key| DOMAIN_LISTS.iter().find(|(k, _, _)| k == key)).flat_map(|(_, _, domains)| domains.iter().copied()).collect();
    let any_link = rule["links"] == true;
    if blocked.is_empty() && lists.is_empty() && !any_link {
        return Vec::new();
    }
    let bad = |host: &str, written_as_link: bool| {
        blocked.iter().any(|d| under(host, d)) || (!allowed.iter().any(|d| under(host, d)) && (lists.iter().any(|d| under(host, d)) || (any_link && written_as_link)))
    };
    let mut found: Vec<(Range<usize>, String)> = Vec::new();
    for link in LINK.find_iter(content) {
        let range = trimmed(content, link.range());
        let host = host_of(&content[range.clone()]);
        if bad(&host, true) {
            found.push((range, host));
        }
    }
    // Written out bare, a domain is only looked for in the lists.
    for link in BARE.find_iter(content) {
        let range = trimmed(content, link.range());
        if LINK.find_iter(content).any(|l| l.start() <= range.start && range.start < l.end()) {
            continue;
        }
        let host = host_of(&content[range.clone()]);
        if host.contains('.') && bad(&host, false) {
            found.push((range, host));
        }
    }
    found.sort_by_key(|(range, _)| range.start);
    found
}

/// What censoring puts in place of something found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cover {
    Dots,
    Link,
}

/// What a rule found in a post.
#[derive(Debug, Clone)]
pub struct Found {
    /// The first thing found, said for an alert: `the word “…”`.
    pub what: String,
    /// Where each thing found is in the post, and what censoring puts there.
    /// Empty for what can't be censored (too many mentions).
    pub spans: Vec<(Range<usize>, Cover)>,
}

/// What `rule` finds in `content`, if anything.
pub fn inspect(rule: &Value, content: &str, mentions: usize) -> Option<Found> {
    let mut spans: Vec<(Range<usize>, Cover)> = Vec::new();
    let mut what: Option<String> = None;
    let mut note = |text: String| {
        if what.is_none() {
            what = Some(text)
        }
    };
    // What the allowed words let be: whatever is found inside one.
    let allowed: Vec<Range<usize>> = find_words(&strings(&rule["allowed_words"]).into_iter().map(|w| w.replace('*', "")).collect::<Vec<_>>(), content);
    let let_be = |range: &Range<usize>| allowed.iter().any(|a| a.start <= range.start && range.end <= a.end);

    for (range, host) in find_links(rule, content) {
        note(format!("a link to {host}"));
        spans.push((range, Cover::Link));
    }
    // A link's address is never dotted: that would only break it (a GIF on
    // tenor.com/…/oh-shit-cat.gif no longer loads), and an address is full of
    // letters that are no word (Xw9a55QkDicK). A rule may instead cover the whole
    // link when the words in its address are what it looks for.
    let links: Vec<Range<usize>> = LINK.find_iter(content).map(|link| trimmed(content, link.range())).collect();
    if rule["link_words"] == true {
        for link in &links {
            if spans.iter().any(|(s, _)| s.start < link.end && link.start < s.end) {
                continue;
            }
            let readable = address_words(&content[link.clone()]);
            let allowed_here = find_words(&strings(&rule["allowed_words"]).into_iter().map(|w| w.replace('*', "")).collect::<Vec<_>>(), &readable);
            if let Some(word) = find_text(rule, &readable).into_iter().find(|range| !allowed_here.iter().any(|a| a.start <= range.start && range.end <= a.end)) {
                note(format!("a link with “{}” in it", &readable[word]));
                spans.push((link.clone(), Cover::Link));
            }
        }
    }
    let mut text = find_text(rule, content);
    text.sort_by_key(|range| range.start);
    for range in text.into_iter().filter(|range| !let_be(range)) {
        let overlaps = |s: &Range<usize>| s.start < range.end && range.start < s.end;
        if spans.iter().any(|(s, _)| overlaps(s)) || links.iter().any(overlaps) {
            continue;
        }
        note(format!("“{}”", &content[range.clone()]));
        spans.push((range, Cover::Dots));
    }
    if rule["max_mentions"].as_u64().is_some_and(|most| most > 0 && mentions as u64 > most) {
        note(format!("{mentions} mentions"));
    }
    spans.sort_by_key(|(range, _)| range.start);
    what.map(|what| Found { what, spans })
}

/// What a rule's words, filters and patterns find in `content`.
fn find_text(rule: &Value, content: &str) -> Vec<Range<usize>> {
    let mut text: Vec<Range<usize>> = find_words(&strings(&rule["words"]), content);
    if let Some(threshold) = threshold(rule) {
        text.extend(find_filtered(threshold, content));
    }
    for pattern in strings(&rule["patterns"]) {
        if let Ok(re) = compile(&pattern) {
            text.extend(re.find_iter(content).filter(|m| !m.is_empty()).map(|m| m.range()));
        }
    }
    text
}

/// The words in a link's address, one space between each: what is between its
/// slashes, dashes, dots and the like, leaving out what looks like an id rather
/// than a word (a digit in it, or capitals in the middle: xT9IgG50Fb7Mi0prBC).
fn address_words(link: &str) -> String {
    link.split(|c: char| !c.is_alphabetic() && !c.is_numeric())
        .filter(|piece| !piece.is_empty())
        .filter(|piece| !piece.chars().any(char::is_numeric))
        .filter(|piece| {
            let rest: Vec<char> = piece.chars().skip(1).collect();
            rest.iter().all(|c| c.is_lowercase()) || rest.iter().all(|c| c.is_uppercase())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `content` with what was found covered: words as dots (spaces and line breaks
/// kept), links as `<UNSAFE URL BLOCKED>`.
pub fn censor(content: &str, spans: &[(Range<usize>, Cover)]) -> String {
    let mut out = String::with_capacity(content.len());
    let mut at = 0;
    for (range, cover) in spans {
        if range.start < at {
            continue;
        }
        out.push_str(&content[at..range.start]);
        match cover {
            Cover::Link => out.push_str(LINK_BLOCKED),
            Cover::Dots => out.extend(content[range.clone()].chars().map(|c| if c.is_whitespace() { c } else { DOT })),
        }
        at = range.end;
    }
    out.push_str(&content[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn censored(rule: &Value, content: &str) -> String {
        censor(content, &inspect(rule, content, 0).map(|f| f.spans).unwrap_or_default())
    }

    #[test]
    fn a_link_is_never_dotted_but_may_be_covered_whole_for_the_words_in_its_address() {
        let family = json!({ "filters": { "profanity": "mild", "sexual": "mild", "slurs": "mild", "insults": "moderate" }, "words": ["shit"] });
        // Dots would break it, so a link is left as it is...
        for link in [
            "https://media.tenor.com/AbC123/oh-shit-cat.gif",
            "https://media.tenor.com/Xw9a55QkDicK/cockatoo-dancing.gif",
            "https://tenor.com/view/scunthorpe-united-hell-yeah-gif-1234",
            "www.giphy.com/gifs/cat-funny-bitch-please-3o7TKSjRrfIPjeiVyM",
        ] {
            assert_eq!(censored(&family, link), link);
            assert_eq!(censored(&family, &format!("look: {link}, shit")), format!("look: {link}, ••••"));
        }
        // ...and the words around it, a caption among them, are still looked at.
        assert_eq!(censored(&family, "![oh shit cat](https://media.giphy.com/media/xT9IgG50Fb7Mi0prBC/giphy.gif)"), "![oh •••• cat](https://media.giphy.com/media/xT9IgG50Fb7Mi0prBC/giphy.gif)");

        // A rule that would rather may cover the whole link, for a word in its address:
        let whole = json!({ "filters": { "profanity": "mild", "sexual": "mild" }, "words": ["scam"], "allowed_words": ["hell"], "link_words": true });
        assert_eq!(censored(&whole, "lol https://media.tenor.com/AbC123/oh-shit-cat.gif!"), "lol <UNSAFE URL BLOCKED>!");
        assert_eq!(censored(&whole, "https://example.com/free-scam-here"), "<UNSAFE URL BLOCKED>");
        assert_eq!(inspect(&whole, "https://media.tenor.com/AbC123/oh-shit-cat.gif", 0).unwrap().what, "a link with “shit” in it");
        // ...but not for the letters of an id, nor for a word it is told to let be.
        for fine in [
            "https://media.tenor.com/Xw9a55QkDicK/cockatoo-dancing.gif",
            "https://media.giphy.com/media/l0HlBO7eyXzSZkJri/giphy.gif",
            "https://media.giphy.com/media/AsSdIcKfOoBaR/giphy.gif",
            "https://tenor.com/view/scunthorpe-united-hell-yeah-gif-1234",
            "https://shipping.example.com/worship",
        ] {
            assert!(inspect(&whole, fine, 0).is_none(), "{fine} is let be");
        }
    }

    #[test]
    fn a_rules_own_words_are_found_however_they_are_disguised() {
        let rule = json!({ "words": ["shit", "scam"] });
        for disguised in ["shit", "Sh#t", "sh*t", "$hit", "sh1t", "sh!t", "shiiiit", "s h i t", "s.h.i.t", "ѕhit", "SHIT!", "a scam?"] {
            assert!(inspect(&rule, disguised, 0).is_some(), "{disguised} is found");
        }
        for innocent in ["ship", "Ship it!", "shipping", "sh!p", "sh1p", "Sh#p", "s h i p", "worship", "she hit me", "his hit", "scampi", "#hit", "shi"] {
            assert!(inspect(&rule, innocent, 0).is_none(), "{innocent} is let be");
        }
        assert_eq!(censored(&rule, "oh sh#t, a scam"), "oh ••••, a ••••");
        assert_eq!(censored(&rule, "shit shit"), "•••• ••••");
        assert_eq!(censored(&rule, "s h i t happens"), "• • • • happens");
    }

    #[test]
    fn a_word_with_a_star_finds_any_ending() {
        let rule = json!({ "words": ["free*"] });
        assert!(inspect(&rule, "Get FREEBIES now", 0).is_some());
        assert!(inspect(&rule, "carefree", 0).is_none());
    }

    #[test]
    fn filters_find_by_how_bad_and_the_allowed_words_are_let_be() {
        let strict = json!({ "filters": { "profanity": "mild" } });
        let worst = json!({ "filters": { "profanity": "severe" } });
        assert_eq!(censored(&strict, "what the f#ck"), "what the ••••");
        assert_eq!(censored(&strict, "damn, that hurt"), "••••, that hurt");
        assert!(inspect(&worst, "damn, that hurt", 0).is_none());
        for innocent in ["ship it", "shipping", "worship", "class", "assess", "therapist", "button", "Scunthorpe", "push it"] {
            assert!(inspect(&strict, innocent, 0).is_none(), "{innocent} is let be");
        }
        assert!(inspect(&strict, "shiitake soup", 0).is_some());
        let allowing = json!({ "filters": { "profanity": "mild" }, "allowed_words": ["shiitake"] });
        assert!(inspect(&allowing, "shiitake soup", 0).is_none());
        assert_eq!(censored(&allowing, "shiitake, sh#t"), "shiitake, ••••");
        assert!(inspect(&json!({}), "sh#t", 0).is_none());
        // Accented, the words are found, and nothing else loses its accents.
        assert_eq!(censored(&strict, "café shït"), "café ••••");
    }

    #[test]
    fn links_are_found_by_where_they_go() {
        let rule = json!({ "links": true, "allowed_domains": ["wyvrn.app"] });
        assert!(inspect(&rule, "see https://docs.wyvrn.app/x and www.wyvrn.app", 0).is_none());
        assert_eq!(inspect(&rule, "see https://evil.example/x", 0).map(|f| f.what).as_deref(), Some("a link to evil.example"));
        assert_eq!(censored(&rule, "see https://evil.example/x."), "see <UNSAFE URL BLOCKED>.");
        // Any link means one written as a link: "e.g." and "node.js" aren't.
        assert!(inspect(&rule, "e.g. node.js", 0).is_none());

        let lists = json!({ "domain_lists": ["adult", "shorteners"], "blocked_domains": ["bad.example"], "allowed_domains": ["bit.ly"] });
        assert!(inspect(&lists, "https://pornhub.com/x", 0).is_some());
        assert!(inspect(&lists, "go to pornhub[.]com", 0).is_some());
        assert!(inspect(&lists, "go to pornhub dot com", 0).is_some());
        assert!(inspect(&lists, "anything.xxx", 0).is_some());
        assert_eq!(censored(&lists, "watch www.xvideos.com/v/1 now"), "watch <UNSAFE URL BLOCKED> now");
        assert!(inspect(&lists, "https://tinyurl.com/abc", 0).is_some());
        assert!(inspect(&lists, "https://bit.ly/abc", 0).is_none(), "allowed past a list");
        assert!(inspect(&lists, "sub.bad.example", 0).is_some(), "blocked, bare");
        assert!(inspect(&lists, "https://wyvrn.app and sex ed", 0).is_none());
        let both = json!({ "blocked_domains": ["bad.example"], "allowed_domains": ["bad.example"] });
        assert!(inspect(&both, "https://bad.example", 0).is_some(), "blocked wins");
    }

    #[test]
    fn patterns_and_mentions() {
        let rule = json!({ "patterns": [r"\d{4}-\d{4}"], "max_mentions": 3 });
        assert_eq!(censored(&rule, "call 1234-5678"), "call •••••••••");
        assert!(inspect(&rule, "hi all", 4).is_some_and(|f| f.spans.is_empty()));
        assert!(inspect(&rule, "hi all", 3).is_none());
        assert!(compile("(unclosed").is_err());
    }
}
