//! Pictures from elsewhere that a public page shows (community/1.0's "On the web").
//!
//! - **Custom emoji and stickers** are files in a pack at some other address, each
//!   named for the start of its SHA-256 hash (stickers/1.0). A page never has the
//!   visitor's browser fetch them: it names them at this server (`x/<file>`), which
//!   fetches each once, keeps it only if its hash starts with its name and it is a
//!   PNG, GIF or WebP, and serves it from then on. An address at `x/` is signed
//!   with the web view's secret, so this server only ever fetches what one of its
//!   own pages named -- never an address a visitor makes up -- and only from a
//!   public address over HTTPS.
//! - **GIFs** are KLIPY's, whose terms require each viewer to fetch them from
//!   KLIPY (no proxy or cache of ours): a post that is only a GIF's address shows
//!   it from there, and the page's content policy allows those hosts for pictures.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::render::escape;

/// KLIPY's picture hosts, as the app has them (wyvrn-chat's gifs.ts).
pub const GIF_HOSTS: [&str; 3] = ["static.klipy.com", "static1.klipy.com", "static2.klipy.com"];

/// The most a pack file may be, as the app holds it.
pub const MAX_BYTES: usize = 2 * 1024 * 1024;

/// A file that couldn't be fetched isn't asked for again for this long.
const FAILED_FOR_SECS: i64 = 10 * 60;

/// For tests only: pack files at loopback and private addresses are fetched too,
/// over plain HTTP. A call in code, never a setting.
static PRIVATE_HOSTS: AtomicBool = AtomicBool::new(false);

pub fn allow_private_hosts() {
    PRIVATE_HOSTS.store(true, Ordering::Relaxed);
}

/// Whether `text` is nothing but the address of a GIF at KLIPY.
pub fn is_gif(text: &str) -> bool {
    let text = text.trim();
    !text.contains(char::is_whitespace)
        && url::Url::parse(text).is_ok_and(|url| url.scheme() == "https" && url.host_str().is_some_and(|host| GIF_HOSTS.contains(&host)))
}

/// A GIF post's picture, from KLIPY, telling KLIPY nothing of the page it is on.
pub fn gif_html(address: &str) -> String {
    format!("<img class=\"gif\" src=\"{}\" alt=\"GIF\" loading=\"lazy\" referrerpolicy=\"no-referrer\">", escape(address.trim()))
}

/// A pack file's name, if `name` is one: 32 hex digits (the start of its hash) and
/// `.png`, `.gif` or `.webp`.
pub fn pack_file(name: &str) -> Option<(&str, &str)> {
    let (stem, extension) = name.split_once('.')?;
    let hex = stem.len() == 32 && stem.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    (hex && matches!(extension, "png" | "gif" | "webp")).then_some((stem, extension))
}

/// Whether `address` may be fetched as a pack file: HTTPS (or, for tests, HTTP at
/// a loopback host), with no credentials, ending in a pack file's name.
fn fetchable(address: &str) -> Option<url::Url> {
    let url = url::Url::parse(address).ok()?;
    let local = PRIVATE_HOSTS.load(Ordering::Relaxed) && url.scheme() == "http";
    if (url.scheme() != "https" && !local) || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return None;
    }
    pack_file(url.path_segments()?.next_back()?)?;
    Some(url)
}

fn signature(secret: &str, address: &str) -> String {
    super::keyed(secret, &format!("outside\n{address}"))[..32].to_string()
}

/// Where a page shows the pack file at `address`: this server's `x/<file>`, signed.
/// `None` for anything that isn't a pack file it would fetch.
pub fn href(secret: &str, root: &str, address: &str) -> Option<String> {
    let url = fetchable(address)?;
    let file = url.path_segments()?.next_back()?.to_string();
    let from: String = url::form_urlencoded::byte_serialize(url.as_str().as_bytes()).collect();
    Some(format!("{root}x/{file}?from={from}&key={}", signature(secret, url.as_str())))
}

/// Whether `key` is this server's signature on `address` for `file`.
pub fn signed(secret: &str, file: &str, address: &str, key: &str) -> bool {
    let Some(url) = fetchable(address) else { return false };
    url.path_segments().and_then(|mut segments| segments.next_back()) == Some(file) && signature(secret, url.as_str()) == key
}

/// What a picture's first bytes say it is: PNG, GIF or WebP, and nothing else.
pub fn picture_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// When each file that couldn't be had was last tried, by its name's hash part.
static FAILED: Mutex<Option<HashMap<String, i64>>> = Mutex::new(None);

fn failed_lately(stem: &str, now: i64) -> bool {
    let failed = FAILED.lock().unwrap_or_else(|p| p.into_inner());
    failed.as_ref().and_then(|failed| failed.get(stem)).is_some_and(|at| now - at < FAILED_FOR_SECS)
}

fn note_failed(stem: &str, now: i64) {
    let mut failed = FAILED.lock().unwrap_or_else(|p| p.into_inner());
    let failed = failed.get_or_insert_with(HashMap::new);
    if failed.len() > 10_000 {
        failed.clear();
    }
    failed.insert(stem.to_string(), now);
}

/// The pack file at `address` fetched, and its type, if it is what its name says:
/// its SHA-256 starts with the name, it is a PNG, GIF or WebP, and not over
/// [`MAX_BYTES`]. Fetched only from a public address (looked up once and used as
/// looked up), with no redirects, within ten seconds.
pub async fn fetch(address: &str, now: i64) -> Result<(&'static str, Vec<u8>), String> {
    let url = fetchable(address).ok_or("not a pack file's address")?;
    let file = url.path_segments().and_then(|mut segments| segments.next_back()).unwrap_or_default().to_string();
    let (stem, _) = pack_file(&file).ok_or("not a pack file")?;
    if failed_lately(stem, now) {
        return Err("tried lately".into());
    }
    let got = fetch_checked(&url, stem).await;
    if got.is_err() {
        note_failed(stem, now);
    }
    got
}

async fn fetch_checked(url: &url::Url, stem: &str) -> Result<(&'static str, Vec<u8>), String> {
    let host = url.host_str().ok_or("no host")?.to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port)).await.map_err(|e| e.to_string())?.collect();
    let private_allowed = PRIVATE_HOSTS.load(Ordering::Relaxed);
    let public = |address: &SocketAddr| private_allowed || super::oidc::public_address(address.ip());
    let address = addresses.iter().find(|a| public(a)).copied().ok_or("not a public address")?;
    if addresses.iter().any(|a| !public(a)) {
        return Err("it also names a private address".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .resolve(&host, address)
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = client.get(url.as_str()).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > MAX_BYTES {
            return Err("over 2 MiB".into());
        }
    }
    let hash: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if !hash.starts_with(stem) {
        return Err("not what its name says".into());
    }
    let media_type = picture_type(&bytes).ok_or("not a PNG, GIF or WebP")?;
    Ok((media_type, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_post_that_is_a_klipy_address_is_a_gif() {
        assert!(is_gif(" https://static.klipy.com/ii/abc/7a/be/t0TOtOCr.webp "));
        assert!(!is_gif("look https://static.klipy.com/ii/abc/x.webp"));
        assert!(!is_gif("http://static.klipy.com/x.webp"));
        assert!(!is_gif("https://klipy.com.evil.example/x.webp"));
        assert!(gif_html("https://static.klipy.com/a\"b.webp").contains("a&quot;b"));
    }

    #[test]
    fn a_pack_file_is_named_for_its_hash_and_signed_where_a_page_names_it() {
        let file = "955b9a2099ec16abd916638c1309866e.webp";
        assert_eq!(pack_file(file), Some(("955b9a2099ec16abd916638c1309866e", "webp")));
        assert!(pack_file("955b9a2099ec16abd916638c1309866e.svg").is_none());
        assert!(pack_file("955B9A2099EC16ABD916638C1309866E.webp").is_none());
        assert!(pack_file("party.webp").is_none());

        let address = format!("https://stickers.example/pack/{file}");
        let shown = href("secret", "/", &address).unwrap();
        assert!(shown.starts_with(&format!("/x/{file}?from=")));
        let key = shown.rsplit("key=").next().unwrap();
        assert!(signed("secret", file, &address, key));
        assert!(!signed("other secret", file, &address, key));
        assert!(!signed("secret", file, "https://elsewhere.example/955b9a2099ec16abd916638c1309866e.webp", key));
        // Not HTTPS, credentials, or not a pack file: nothing to show.
        assert!(href("secret", "/", &format!("http://stickers.example/{file}")).is_none());
        assert!(href("secret", "/", &format!("https://me:pw@stickers.example/{file}")).is_none());
        assert!(href("secret", "/", "https://stickers.example/pack/party.webp").is_none());
    }

    #[test]
    fn only_pngs_gifs_and_webps_are_pictures() {
        assert_eq!(picture_type(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(picture_type(b"GIF89a..."), Some("image/gif"));
        assert_eq!(picture_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(picture_type(b"<svg xmlns="), None);
    }
}
