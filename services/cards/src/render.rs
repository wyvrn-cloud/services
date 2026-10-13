//! The picture: a card drawn from who a link is to.
//!
//! Written as SVG and rendered to a PNG, 1200 by 630, which is the size every
//! place that shows a link's picture large expects. The text is set in the fonts
//! kept with this crate (`fonts/`), and anything they have no letters for in
//! whatever the machine has installed.

use std::sync::Arc;

use resvg::{tiny_skia, usvg};

use crate::who::{Mark, Who};

pub const WIDTH: u32 = 1200;
pub const HEIGHT: u32 = 630;

const REGULAR: &[u8] = include_bytes!("../fonts/DejaVuSans.ttf");
const BOLD: &[u8] = include_bytes!("../fonts/DejaVuSans-Bold.ttf");
const FAMILY: &str = "DejaVu Sans";

/// The app's own colours, as it has them in the dark.
const GROUND: &str = "#16161a";
const TEXT: &str = "#ececee";
const DIM: &str = "#98989f";
const ACCENT: &str = "#4a63e7";
const CAUTION: &str = "#d9a441";

/// The colours the app draws a placeholder picture in (its `identicon.ts`).
const PALETTE: [&str; 12] = ["#e57373", "#f06292", "#ba68c8", "#9575cd", "#7986cb", "#64b5f6", "#4fc3f7", "#4dd0e1", "#4db6ac", "#81c784", "#aed581", "#ffb74d"];

const BANNER_HEIGHT: u32 = 220;
const MARGIN: f32 = 60.0;

/// Draws cards. Made once: reading the fonts is the slow part.
pub struct Renderer {
    options: usvg::Options<'static>,
    site_name: String,
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Nothing XML has no way to say.
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// How wide `text` is set at `size`, near enough to fit it by: letters the font
/// kept here doesn't have (and so are drawn in another) are taken as a full square.
fn width(text: &str, size: f32, bold: bool) -> f32 {
    let Ok(face) = ttf_parser::Face::parse(if bold { BOLD } else { REGULAR }, 0) else { return text.chars().count() as f32 * size * 0.6 };
    let unit = size / face.units_per_em() as f32;
    text.chars().map(|c| face.glyph_index(c).and_then(|glyph| face.glyph_hor_advance(glyph)).map_or(size, |advance| advance as f32 * unit)).sum()
}

/// `text`, cut short with an ellipsis if it is wider than `max`.
fn fitted(text: &str, size: f32, bold: bool, max: f32) -> String {
    if width(text, size, bold) <= max {
        return text.to_string();
    }
    let mut kept: Vec<char> = text.chars().collect();
    while !kept.is_empty() {
        kept.pop();
        let cut = format!("{}…", kept.iter().collect::<String>().trim_end());
        if width(&cut, size, bold) <= max {
            return cut;
        }
    }
    String::new()
}

/// `text` with its middle left out if it is wider than `max`: a DID is told apart
/// by how it starts and how it ends.
fn fitted_ends(text: &str, size: f32, max: f32) -> String {
    if width(text, size, false) <= max {
        return text.to_string();
    }
    let all: Vec<char> = text.chars().collect();
    let mut keep = all.len() / 2;
    while keep > 4 {
        keep -= 1;
        let cut = format!("{}…{}", all[..keep].iter().collect::<String>(), all[all.len() - keep / 2..].iter().collect::<String>());
        if width(&cut, size, false) <= max {
            return cut;
        }
    }
    fitted(text, size, false, max)
}

/// `text` as lines no wider than `max`, at most `lines` of them, the last cut
/// short if there was more to say.
fn wrapped(text: &str, size: f32, max: f32, lines: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut words = text.split_whitespace().peekable();
    while let Some(word) = words.next() {
        let tried = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if width(&tried, size, false) <= max {
            line = tried;
            continue;
        }
        if out.len() + 1 == lines {
            // No room for another line: this one takes what it can of the rest.
            out.push(fitted(&format!("{tried} {}", words.collect::<Vec<_>>().join(" ")), size, false, max));
            return out;
        }
        if !line.is_empty() {
            out.push(std::mem::take(&mut line));
        }
        // (A word too long for a line by itself is cut to one.)
        line = fitted(word, size, false, max);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// The app's placeholder picture for a DID with none: the same squares in the
/// same colour as the app draws beside it, so the card is of the same party.
fn identicon(seed: &str, x: f32, y: f32, size: f32) -> String {
    // (The app hashes the text as JavaScript has it: UTF-16.)
    let hash = seed.encode_utf16().fold(0u32, |hash, unit| hash.wrapping_mul(31).wrapping_add(unit as u32));
    let colour = PALETTE[(hash % PALETTE.len() as u32) as usize];
    let cell = size / 5.0;
    let mut cells = String::new();
    for row in 0..5u32 {
        for col in 0..3u32 {
            if (hash >> (row * 3 + col)) & 1 == 0 {
                continue;
            }
            for at in [col, 4 - col] {
                cells.push_str(&format!(r#"<rect x="{}" y="{}" width="{cell}" height="{cell}"/>"#, x + at as f32 * cell, y + row as f32 * cell));
            }
        }
    }
    format!(r##"<rect x="{x}" y="{y}" width="{size}" height="{size}" fill="#eeeeee"/><g fill="{colour}">{cells}</g>"##)
}

fn mark(mark: Mark) -> (&'static str, &'static str, bool) {
    match mark {
        Mark::Official => ("Official", ACCENT, true),
        Mark::Verified => ("History verified", TEXT, false),
        Mark::HostsWord => ("On its host’s word", CAUTION, false),
        Mark::Ended => ("Ended by its owner", CAUTION, false),
    }
}

impl Renderer {
    pub fn new(site_name: &str) -> Self {
        let mut options = usvg::Options::default();
        let fonts = options.fontdb_mut();
        fonts.load_font_data(REGULAR.to_vec());
        fonts.load_font_data(BOLD.to_vec());
        // For what those two have no letters for: other scripts, emoji.
        fonts.load_system_fonts();
        fonts.set_sans_serif_family(FAMILY);
        options.font_family = FAMILY.to_string();
        Self { options, site_name: site_name.to_string() }
    }

    /// The card as SVG.
    pub fn svg(&self, who: &Who) -> String {
        let (w, h) = (WIDTH as f32, HEIGHT as f32);
        let mut svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" viewBox="0 0 {WIDTH} {HEIGHT}" font-family="{FAMILY}">
<defs>
<clipPath id="banner"><rect width="{WIDTH}" height="{BANNER_HEIGHT}"/></clipPath>
<clipPath id="picture"><circle cx="160" cy="{BANNER_HEIGHT}" r="92"/></clipPath>
<linearGradient id="strip" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{ACCENT}"/><stop offset="1" stop-color="#232a5c"/></linearGradient>
<linearGradient id="shade" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{GROUND}" stop-opacity="0.35"/><stop offset="1" stop-color="{GROUND}" stop-opacity="0.84"/></linearGradient>
</defs>
<rect width="{WIDTH}" height="{HEIGHT}" fill="{GROUND}"/>
"##
        );
        if let Some(background) = &who.background {
            // Behind everything, shaded so that what is written over it can be read.
            svg.push_str(&format!(r#"<image width="{WIDTH}" height="{HEIGHT}" preserveAspectRatio="xMidYMid slice" href="{background}"/><rect width="{WIDTH}" height="{HEIGHT}" fill="url(#shade)"/>"#));
        }
        match &who.banner {
            Some(banner) => svg.push_str(&format!(r#"<image width="{WIDTH}" height="{BANNER_HEIGHT}" preserveAspectRatio="xMidYMid slice" clip-path="url(#banner)" href="{banner}"/>"#)),
            // With neither, a strip of the app's own colour where a banner would be.
            None if who.background.is_none() => svg.push_str(&format!(r#"<rect width="{WIDTH}" height="{BANNER_HEIGHT}" fill="url(#strip)"/>"#)),
            None => {}
        }

        // The picture, in a ring of the ground's colour where it overlaps the banner.
        svg.push_str(&format!(r#"<circle cx="160" cy="{BANNER_HEIGHT}" r="100" fill="{GROUND}"/><g clip-path="url(#picture)">"#));
        let top = BANNER_HEIGHT as f32 - 92.0;
        match &who.picture {
            Some(picture) => svg.push_str(&format!(r#"<image x="68" y="{top}" width="184" height="184" preserveAspectRatio="xMidYMid slice" href="{picture}"/>"#)),
            None => svg.push_str(&identicon(&who.did, 68.0, top, 184.0)),
        }
        svg.push_str("</g>\n");

        // What is said of it, beside the picture.
        let mut x = 290.0;
        for (words, colour, filled) in who.marks.iter().copied().map(mark) {
            let wide = width(words, 24.0, filled) + 36.0;
            let (fill, ink) = if filled { (colour, "#ffffff") } else { (GROUND, colour) };
            svg.push_str(&format!(
                r#"<rect x="{x}" y="{y}" width="{wide}" height="44" rx="22" fill="{fill}" fill-opacity="{opacity}" stroke="{colour}" stroke-width="2"/><text x="{tx}" y="{ty}" font-size="24" font-weight="{weight}" fill="{ink}">{words}</text>"#,
                y = BANNER_HEIGHT + 18,
                opacity = if filled { "1" } else { "0.6" },
                tx = x + 18.0,
                ty = BANNER_HEIGHT + 49,
                weight = if filled { "700" } else { "400" },
                words = escape(words),
            ));
            x += wide + 14.0;
        }

        let room = w - 2.0 * MARGIN;
        svg.push_str(&format!(r#"<text x="{MARGIN}" y="392" font-size="56" font-weight="700" fill="{TEXT}">{}</text>"#, escape(&fitted(who.title(), 56.0, true, room))));
        // Under a name that is its own word, what it is that can't be made up.
        let mut y = 392.0;
        if who.name.is_some() {
            y += 46.0;
            svg.push_str(&format!(r#"<text x="{MARGIN}" y="{y}" font-size="28" fill="{DIM}">{}</text>"#, escape(&fitted(&who.label, 28.0, false, room))));
        }
        if let Some(description) = &who.description {
            y += 12.0;
            for line in wrapped(description, 28.0, room, 2) {
                y += 40.0;
                svg.push_str(&format!(r#"<text x="{MARGIN}" y="{y}" font-size="28" fill="{TEXT}">{}</text>"#, escape(&line)));
            }
        }

        // Along the bottom: the DID, and whose link this is.
        let site = fitted(&self.site_name, 30.0, true, 300.0);
        let site_width = width(&site, 30.0, true);
        svg.push_str(&format!(
            r#"<text x="{MARGIN}" y="{base}" font-size="20" fill="{DIM}">{did}</text><text x="{sx}" y="{base}" font-size="30" font-weight="700" fill="{TEXT}">{site}</text>"#,
            base = h - 36.0,
            did = escape(&fitted_ends(&who.did, 20.0, room - site_width - 40.0)),
            sx = w - MARGIN - site_width,
            site = escape(&site),
        ));
        svg.push_str("</svg>");
        svg
    }

    /// The card as a PNG.
    pub fn png(&self, who: &Who) -> anyhow::Result<Vec<u8>> {
        let tree = usvg::Tree::from_str(&self.svg(who), &self.options)?;
        let mut pixels = tiny_skia::Pixmap::new(WIDTH, HEIGHT).ok_or_else(|| anyhow::anyhow!("no room to draw a card"))?;
        resvg::render(&tree, tiny_skia::Transform::identity(), &mut pixels.as_mut());
        Ok(pixels.encode_png()?)
    }
}

/// A [`Renderer`] to share between requests.
pub fn shared(site_name: &str) -> Arc<Renderer> {
    Arc::new(Renderer::new(site_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn someone() -> Who {
        Who {
            did: "did:web:id.example:alice".into(),
            label: "alice at id.example".into(),
            name: Some("Alice <&> \"Friends\"".into()),
            description: Some("Writes about wyverns, dragons and other things with wings, at more length than fits on one line of a card by a good way, and then some more besides to be sure of it.".into()),
            picture: None,
            banner: None,
            background: None,
            marks: vec![Mark::Official, Mark::Verified],
        }
    }

    #[test]
    fn text_is_cut_to_fit() {
        assert_eq!(fitted("short", 28.0, false, 500.0), "short");
        let cut = fitted("a name that goes on for a good deal longer than there is room for", 28.0, false, 300.0);
        assert!(cut.ends_with('…') && width(&cut, 28.0, false) <= 300.0, "{cut}");
        let did = fitted_ends("did:webvh:QmRa6b5qfr9fPWTi49QZUaT3Avt9qC4zXsyPpTr4QJth7R:id.wyvrn.app:frostyfrog", 20.0, 400.0);
        assert!(did.starts_with("did:webvh:") && did.ends_with("frostyfrog") && did.contains('…'), "{did}");
    }

    #[test]
    fn a_description_takes_two_lines_and_no_more() {
        let lines = wrapped(&someone().description.unwrap(), 28.0, 1080.0, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'));
        assert!(lines.iter().all(|line| width(line, 28.0, false) <= 1080.0));
        assert_eq!(wrapped("one line", 28.0, 1080.0, 2), ["one line"]);
    }

    #[test]
    fn what_a_profile_says_is_text_in_the_card_and_nothing_else() {
        let svg = Renderer::new("Wyvrn").svg(&someone());
        assert!(svg.contains("Alice &lt;&amp;&gt; &quot;Friends&quot;"));
        assert!(!svg.contains("<&>"));
    }

    #[test]
    fn a_card_is_a_png_of_the_size_a_link_preview_wants() {
        let png = Renderer::new("Wyvrn").png(&someone()).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // Width and height, in the header.
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), WIDTH);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), HEIGHT);
    }
}
