//! A time written in a message as Discord and the app write one (`<t:1767225600>`,
//! `<t:1767225600:F>`), shown on a service's web page as a `<time>` saying it in
//! UTC, which [`SCRIPT`] (or the community's web.js) then writes in the reader's
//! own zone and language.

/// What a timestamp written in a post says (`<t:seconds>` or `<t:seconds:style>`,
/// the style one of Discord's `tTdDfFR`, `f` when none): its seconds and style.
pub fn parse(written: &str) -> Option<(i64, char)> {
    let inside = written.strip_prefix("<t:")?.strip_suffix('>')?;
    let (seconds, style) = match inside.split_once(':') {
        Some((seconds, style)) => {
            let mut chars = style.chars();
            let style = chars.next().filter(|c| "tTdDfFR".contains(*c) && chars.next().is_none())?;
            (seconds, style)
        }
        None => (inside, 'f'),
    };
    if seconds.is_empty() || seconds.len() > 13 || !seconds.trim_start_matches('-').chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((seconds.parse().ok()?, style))
}

/// A piece of text: words as they are, or a time written in them.
#[derive(Debug, PartialEq)]
pub enum Piece<'a> {
    Text(&'a str),
    Time(i64, char),
}

/// `text` cut into its words and the times written in it. What looks like a time
/// and isn't one (`<t:soon>`) stays words.
pub fn pieces(text: &str) -> Vec<Piece<'_>> {
    let mut pieces = Vec::new();
    let mut rest = text;
    let mut words_from = 0;
    let mut at_in = 0;
    while let Some(at) = rest[at_in..].find("<t:").map(|i| at_in + i) {
        match rest[at..].find('>').and_then(|end| parse(&rest[at..at + end + 1]).map(|t| (t, end))) {
            Some(((seconds, style), end)) => {
                if at > words_from {
                    pieces.push(Piece::Text(&rest[words_from..at]));
                }
                pieces.push(Piece::Time(seconds, style));
                rest = &rest[at + end + 1..];
                words_from = 0;
                at_in = 0;
            }
            None => at_in = at + 3,
        }
    }
    if words_from < rest.len() {
        pieces.push(Piece::Text(&rest[words_from..]));
    }
    pieces
}

/// A timestamp as a page draws it: a `<time class="timestamp">` saying it in UTC.
pub fn html(seconds: i64, style: char) -> String {
    let escape = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let Some(when) = chrono::DateTime::from_timestamp(seconds, 0) else {
        return escape(&format!("<t:{seconds}:{style}>"));
    };
    let shown = match style {
        't' => when.format("%H:%M UTC").to_string(),
        'T' => when.format("%H:%M:%S UTC").to_string(),
        'd' => when.format("%Y-%m-%d").to_string(),
        'D' => when.format("%B %-d, %Y").to_string(),
        'F' => when.format("%A, %B %-d, %Y %H:%M UTC").to_string(),
        _ => when.format("%B %-d, %Y %H:%M UTC").to_string(),
    };
    format!("<time class=\"timestamp\" datetime=\"{}\" data-style=\"{style}\">{}</time>", when.to_rfc3339_opts(chrono::SecondsFormat::Secs, true), escape(&shown))
}

/// A page's script that writes each `time.timestamp` in the reader's zone and
/// language, as the app does (`R` relative), the whole of it on hover.
pub const SCRIPT: &str = r#"(function () {
  function relative(when) {
    var diff = Math.round((when.getTime() - Date.now()) / 1000)
    var units = [['year', 31536000], ['month', 2592000], ['week', 604800], ['day', 86400], ['hour', 3600], ['minute', 60], ['second', 1]]
    var format = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' })
    for (var i = 0; i < units.length; i++) {
      if (Math.abs(diff) >= units[i][1] || units[i][0] === 'second') return format.format(Math.round(diff / units[i][1]), units[i][0])
    }
  }
  Array.prototype.forEach.call(document.querySelectorAll('time.timestamp'), function (time) {
    var when = new Date(time.getAttribute('datetime'))
    if (isNaN(when.getTime())) return
    var as = function (options) { return new Intl.DateTimeFormat(undefined, options).format(when) }
    var style = time.getAttribute('data-style')
    var shown
    if (style === 't') shown = as({ hour: 'numeric', minute: '2-digit' })
    else if (style === 'T') shown = as({ hour: 'numeric', minute: '2-digit', second: '2-digit' })
    else if (style === 'd') shown = as({ dateStyle: 'short' })
    else if (style === 'D') shown = as({ dateStyle: 'long' })
    else if (style === 'F') shown = as({ dateStyle: 'full', timeStyle: 'short' })
    else if (style === 'R') shown = relative(when)
    else shown = as({ dateStyle: 'long', timeStyle: 'short' })
    time.textContent = shown
    time.title = as({ dateStyle: 'full', timeStyle: 'short' })
  })
})()"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_found_in_words_and_what_only_looks_like_one_stays_words() {
        assert_eq!(parse("<t:1767225600:F>"), Some((1767225600, 'F')));
        assert_eq!(parse("<t:1767225600>"), Some((1767225600, 'f')));
        assert_eq!(parse("<t:soon>"), None);
        assert_eq!(parse("<t:1:Q>"), None);
        assert_eq!(pieces("At <t:soon> or <t:1767225600:t>, ok"), vec![Piece::Text("At <t:soon> or "), Piece::Time(1767225600, 't'), Piece::Text(", ok")]);
        assert_eq!(pieces("<t:5>"), vec![Piece::Time(5, 'f')]);
        assert!(html(1767225600, 'F').contains("datetime=\"2026-01-01T00:00:00Z\""));
    }
}
