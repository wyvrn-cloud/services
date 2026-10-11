//! What an uploaded picture is, read from its bytes: nothing a maker says about a
//! file is taken on trust, least of all its type and size.

use base64::Engine;
use sha2::{Digest, Sha256};

/// A picture this service will host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub media_type: &'static str,
    /// The extension its file is served under.
    pub extension: &'static str,
    pub width: u32,
    pub height: u32,
    pub animated: bool,
    /// The hex SHA-256 of the file: what a sticker is known by.
    pub sha256: String,
}

impl Image {
    /// The name the file is served under: the start of its hash, so the same
    /// picture is the same file and a file never changes under its name.
    pub fn file(&self) -> String {
        format!("{}.{}", self.id(), self.extension)
    }

    pub fn id(&self) -> &str {
        &self.sha256[..32]
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn u16_le(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?)))
}

fn u24_le(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 3)?;
    Some(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16)
}

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

/// Reads `bytes` as a PNG, a GIF or a WebP. `Err` says, for a person, why it
/// isn't one this service takes.
pub fn inspect(bytes: &[u8]) -> Result<Image, String> {
    let not_a_picture = || "That isn't a PNG, GIF or WebP picture.".to_string();
    let (media_type, extension, width, height, animated) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let size = |at: usize| bytes.get(at..at + 4).and_then(|b| b.try_into().ok()).map(u32::from_be_bytes);
        let (width, height) = size(16).zip(size(20)).ok_or_else(not_a_picture)?;
        // An animated PNG says so before its first frame's data.
        let animated = bytes.windows(4).position(|w| w == b"IDAT").is_some_and(|data| contains(&bytes[..data], b"acTL"));
        ("image/png", "png", width, height, animated)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        let (width, height) = u16_le(bytes, 6).zip(u16_le(bytes, 8)).ok_or_else(not_a_picture)?;
        ("image/gif", "gif", width, height, contains(bytes, b"NETSCAPE2.0"))
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        match bytes.get(12..16) {
            // Lossy: a frame header, with the size 14 bits each after its start code.
            Some(b"VP8 ") => {
                let (width, height) = u16_le(bytes, 26).zip(u16_le(bytes, 28)).ok_or_else(not_a_picture)?;
                ("image/webp", "webp", width & 0x3fff, height & 0x3fff, false)
            }
            // Lossless: 14 bits each, less one, packed after a signature byte.
            Some(b"VP8L") => {
                let b = bytes.get(21..25).ok_or_else(not_a_picture)?;
                let width = 1 + (u32::from(b[0]) | (u32::from(b[1]) & 0x3f) << 8);
                let height = 1 + (u32::from(b[1]) >> 6 | u32::from(b[2]) << 2 | (u32::from(b[3]) & 0x0f) << 10);
                ("image/webp", "webp", width, height, false)
            }
            // Extended: flags (one says "animated"), then the canvas, less one.
            Some(b"VP8X") => {
                let flags = *bytes.get(20).ok_or_else(not_a_picture)?;
                let (width, height) = u24_le(bytes, 24).zip(u24_le(bytes, 27)).ok_or_else(not_a_picture)?;
                ("image/webp", "webp", width + 1, height + 1, flags & 0x02 != 0)
            }
            _ => return Err(not_a_picture()),
        }
    } else {
        return Err(not_a_picture());
    };
    if width == 0 || height == 0 {
        return Err(not_a_picture());
    }
    Ok(Image { media_type, extension, width, height, animated, sha256: hex(&Sha256::digest(bytes)) })
}

/// The bytes of a `data:` URL (`data:image/webp;base64,…`), which is how a
/// picture arrives in a workflow's input.
pub fn from_data_url(url: &str) -> Result<Vec<u8>, String> {
    let (header, data) = url.strip_prefix("data:").and_then(|rest| rest.split_once(',')).ok_or("No picture was sent.")?;
    if !header.ends_with(";base64") {
        return Err("The picture has to be sent base64-encoded.".to_string());
    }
    base64::engine::general_purpose::STANDARD
        .decode(data.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(data.trim_end_matches('=')))
        .map_err(|_| "The picture didn't arrive whole.".to_string())
}

#[cfg(test)]
pub mod samples {
    //! The smallest files of each kind that say what they are.

    pub fn png(width: u32, height: u32, animated: bool) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        if animated {
            bytes.extend(b"\x00\x00\x00\x08acTL\x00\x00\x00\x02\x00\x00\x00\x00\x00\x00\x00\x00");
        }
        bytes.extend(b"\x00\x00\x00\x00IDAT\x00\x00\x00\x00");
        bytes
    }

    pub fn gif(width: u16, height: u16, animated: bool) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend(width.to_le_bytes());
        bytes.extend(height.to_le_bytes());
        bytes.extend([0, 0, 0]);
        if animated {
            bytes.extend(b"\x21\xff\x0bNETSCAPE2.0\x03\x01\x00\x00\x00");
        }
        bytes
    }

    pub fn webp_extended(width: u32, height: u32, animated: bool) -> Vec<u8> {
        let mut bytes = b"RIFF\x00\x00\x00\x00WEBPVP8X\x0a\x00\x00\x00".to_vec();
        bytes.extend([if animated { 0x02 } else { 0 }, 0, 0, 0]);
        bytes.extend(&(width - 1).to_le_bytes()[..3]);
        bytes.extend(&(height - 1).to_le_bytes()[..3]);
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_is_what_its_bytes_say() {
        let png = inspect(&samples::png(512, 300, false)).unwrap();
        assert_eq!((png.media_type, png.width, png.height, png.animated), ("image/png", 512, 300, false));
        assert!(inspect(&samples::png(100, 100, true)).unwrap().animated);
        let gif = inspect(&samples::gif(64, 48, true)).unwrap();
        assert_eq!((gif.media_type, gif.width, gif.height, gif.animated), ("image/gif", 64, 48, true));
        assert!(!inspect(&samples::gif(64, 48, false)).unwrap().animated);
        let webp = inspect(&samples::webp_extended(512, 512, true)).unwrap();
        assert_eq!((webp.media_type, webp.extension, webp.width, webp.height, webp.animated), ("image/webp", "webp", 512, 512, true));
        // Lossy and lossless WebP keep their size differently.
        let lossy = b"RIFF\x00\x00\x00\x00WEBPVP8 \x00\x00\x00\x00\x00\x00\x00\x9d\x01\x2a\x00\x02\x80\x01";
        assert_eq!(inspect(lossy).map(|i| (i.width, i.height)), Ok((512, 384)));
        let lossless = b"RIFF\x00\x00\x00\x00WEBPVP8L\x00\x00\x00\x00\x2f\xff\xc1\x5f\x00";
        assert_eq!(inspect(lossless).map(|i| (i.width, i.height)), Ok((512, 384)));
    }

    #[test]
    fn a_file_is_named_for_what_is_in_it() {
        let image = inspect(&samples::png(10, 10, false)).unwrap();
        assert_eq!(image.sha256.len(), 64);
        assert_eq!(image.file(), format!("{}.png", &image.sha256[..32]));
        assert_ne!(image.sha256, inspect(&samples::png(10, 11, false)).unwrap().sha256);
    }

    #[test]
    fn anything_else_is_refused() {
        for not in [&b"<svg xmlns='http://www.w3.org/2000/svg'/>"[..], b"\xff\xd8\xff\xe0 a jpeg", b"", b"RIFF\x00\x00\x00\x00WAVE", b"\x89PNG\r\n\x1a\n"] {
            assert!(inspect(not).is_err());
        }
    }

    #[test]
    fn a_picture_arrives_as_a_data_url() {
        assert_eq!(from_data_url("data:image/png;base64,aGVsbG8=").unwrap(), b"hello");
        assert_eq!(from_data_url("data:image/png;base64,aGVsbG8").unwrap(), b"hello");
        assert!(from_data_url("https://example.com/cat.png").is_err());
        assert!(from_data_url("data:image/png,raw").is_err());
    }
}
