//! A picture's width and height on its media item (chat-message/1.0's `media`:
//! `width` and `height`, both or neither), so a member's app keeps its room before
//! it has the picture, and the channel doesn't jump as pictures come in. A member's
//! app gives them for what it sends; here they are measured for what the community
//! keeps itself (a Discord server's history brought in), and, once, for the
//! pictures of posts kept before they were ([`fill`]).

use serde_json::Value;

use crate::files::{Carried, Files};
use crate::store::Store;

/// Posts read a page at a time: each page's changes written together, and nothing
/// held between pages.
const PAGE: usize = 200;

/// The largest file opened to be measured.
const MOST: u64 = 256 << 20;

/// A picture's width and height as it is shown (turned as its EXIF says), read
/// from its header: PNG, JPEG, GIF or WebP. None for anything else, or anything
/// that can't be read.
pub fn of(bytes: &[u8]) -> Option<(u32, u32)> {
    use image::metadata::Orientation;
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_decoder().ok()?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let turned = matches!(decoder.orientation().unwrap_or(Orientation::NoTransforms), Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH);
    Some(if turned { (height, width) } else { (width, height) })
}

/// Whether a file of `media_type` is one measured.
fn is_picture(media_type: &str) -> bool {
    media_type.starts_with("image/") && media_type != "image/svg+xml"
}

/// `item`, the media item of `plain` (a file of `media_type`), given its width and
/// height if it is a picture they can be read of. Whether it was.
pub fn put(item: &mut Value, media_type: &str, plain: &[u8]) -> bool {
    let Some((width, height)) = is_picture(media_type).then(|| of(plain)).flatten() else { return false };
    item["width"] = width.into();
    item["height"] = height.into();
    true
}

/// What [`fill`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Filled {
    /// Posts read that carry files.
    pub posts_read: usize,
    /// Posts whose pictures were given their sizes (or would be, in a dry run).
    pub posts: usize,
    /// Pictures given their sizes.
    pub pictures: usize,
    /// Pictures kept elsewhere, or no longer here: left as they are.
    pub not_here: usize,
    /// Pictures kept here that couldn't be opened or measured: left as they are.
    pub unreadable: usize,
    /// Posts changed meanwhile by something else: left for another run.
    pub changed_meanwhile: usize,
}

/// Every post's pictures kept here that have no width and height yet, opened and
/// measured, and each post that gained them marked changed, so members' apps take
/// them up as they do an edit. A page of posts at a time, the database held only
/// for each page's writes; told after each page (`progress`). A picture that can't
/// be read is left as it is; run again, it changes nothing more. `dry_run`: only
/// counted.
pub fn fill(store: &Store, files: &Files, dry_run: bool, mut progress: impl FnMut(&Filled)) -> anyhow::Result<Filled> {
    let mut filled = Filled::default();
    let mut after = String::new();
    loop {
        let page = store.post_extras_with_media_after(&after, PAGE)?;
        let Some((last, _)) = page.last() else { break };
        after = last.clone();
        // Opened and measured first, the database not held.
        let mut measured = Vec::new();
        for (post, was) in &page {
            let Some(mut extra) = serde_json::from_str::<Value>(was).ok().filter(|e| e["media"].is_array()) else { continue };
            filled.posts_read += 1;
            let mut gained = 0;
            for n in 0..extra["media"].as_array().map_or(0, Vec::len) {
                let item = &extra["media"][n];
                if item.get("width").is_some() || item.get("height").is_some() {
                    continue;
                }
                let media_type = extra["attachments"].as_array().into_iter().flatten().find(|a| a["id"] == item["attachment"]).and_then(|a| a["media_type"].as_str()).unwrap_or_default();
                if !is_picture(&media_type.to_ascii_lowercase()) {
                    continue;
                }
                let size = match files.carried_now(&extra, n, MOST) {
                    Carried::Opened { bytes, .. } => of(&bytes),
                    Carried::NotHere => {
                        filled.not_here += 1;
                        continue;
                    }
                    Carried::Unreadable => None,
                };
                let Some((width, height)) = size else {
                    filled.unreadable += 1;
                    continue;
                };
                extra["media"][n]["width"] = width.into();
                extra["media"][n]["height"] = height.into();
                gained += 1;
            }
            if gained > 0 {
                filled.pictures += gained;
                measured.push((post, was, extra));
            }
        }
        if dry_run {
            filled.posts += measured.len();
        } else if !measured.is_empty() {
            store.db.grouped(|| -> anyhow::Result<()> {
                for (post, was, extra) in &measured {
                    if store.replace_post_extra(post, was, extra)? {
                        filled.posts += 1;
                    } else {
                        filled.changed_meanwhile += 1;
                    }
                }
                Ok(())
            })?;
        }
        progress(&filled);
        if page.len() < PAGE {
            break;
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, RgbImage};
    use serde_json::json;

    fn picture(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(RgbImage::new(width, height)).write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    #[test]
    fn a_pictures_size_is_read_from_its_header() {
        for (format, width, height) in [(ImageFormat::Png, 3, 2), (ImageFormat::Jpeg, 17, 9), (ImageFormat::Gif, 5, 7), (ImageFormat::WebP, 12, 4)] {
            assert_eq!(of(&picture(format, width, height)), Some((width, height)), "{format:?}");
        }
        let png = picture(ImageFormat::Png, 640, 480);
        assert_eq!(of(&png), Some((640, 480)));
        // Nothing else is a picture.
        assert_eq!(of(b"not a picture"), None);
        assert_eq!(of(b""), None);
        let mut item = json!({ "attachment": "a" });
        assert!(!put(&mut item, "video/mp4", &png));
        assert!(!put(&mut item, "image/png", b"\x89PNG broken"));
        assert_eq!(item, json!({ "attachment": "a" }));
        assert!(put(&mut item, "image/png", &png));
        assert_eq!(item, json!({ "attachment": "a", "width": 640, "height": 480 }));
    }

    #[test]
    fn a_jpeg_turned_by_its_exif_is_as_wide_as_it_is_shown() {
        let jpeg = picture(ImageFormat::Jpeg, 8, 4);
        // An APP1 segment, Exif, saying "turned 90°" (orientation 6), after SOI.
        let tiff: Vec<u8> = [b"II*\0".as_slice(), &8u32.to_le_bytes(), &1u16.to_le_bytes(), &0x0112u16.to_le_bytes(), &3u16.to_le_bytes(), &1u32.to_le_bytes(), &6u16.to_le_bytes(), &[0, 0], &0u32.to_le_bytes()].concat();
        let payload: Vec<u8> = [b"Exif\0\0".as_slice(), &tiff].concat();
        let segment: Vec<u8> = [[0xFF, 0xE1].as_slice(), &((payload.len() + 2) as u16).to_be_bytes(), &payload].concat();
        let turned: Vec<u8> = [&jpeg[..2], segment.as_slice(), &jpeg[2..]].concat();
        assert_eq!(of(&jpeg), Some((8, 4)));
        assert_eq!(of(&turned), Some((4, 8)));
    }

    /// A store with a channel, and files kept (in a directory of their own).
    fn community() -> (Store, Files, std::path::PathBuf) {
        let store = Store::in_memory().unwrap();
        store.add_channel("general", "general", "", "did:example:owner", 0).unwrap();
        store.admit("did:example:importer", crate::store::Role::Member, 0).unwrap();
        let files = Files::scratch(&crate::config::FilesConfig::default(), "https://community.example/").unwrap();
        let dir = files.host.store.path("");
        (store, files, dir)
    }

    /// A post carrying `kept` (name, media type, bytes), as the importer keeps one.
    fn post_with(store: &Store, files: &Files, id: &str, kept: &[(&str, &str, Vec<u8>)]) {
        let author = store.member("did:example:importer").unwrap().unwrap();
        let post = store.add_post("general", id, "", &author, None, None, 100).unwrap();
        let (mut media, mut attachments) = (Vec::new(), Vec::new());
        for (name, media_type, plain) in kept {
            let (sealed, ciphering) = crate::discord_history::seal(plain);
            let (file, url, hash) = files.keep_any("import:discord:1", name, media_type, &sealed, 100).unwrap();
            attachments.push(json!({ "id": file, "media_type": media_type, "filename": name, "byte_count": sealed.len(), "data": { "links": [url], "hash": hash } }));
            media.push(json!({ "attachment": file, "ciphering": ciphering }));
        }
        store.set_post_extra(&post.id, &json!({ "media": media, "attachments": attachments })).unwrap();
    }

    fn media_of(store: &Store, id: &str) -> Value {
        store.post_by_id(id).unwrap().unwrap().extra.unwrap()["media"].clone()
    }

    #[test]
    fn pictures_kept_before_are_measured_once_and_what_cant_be_read_is_left() {
        let (store, files, dir) = community();
        post_with(&store, &files, "two", &[("a.png", "image/png", picture(ImageFormat::Png, 30, 20)), ("b.txt", "text/plain", b"words".to_vec()), ("c.webp", "image/webp", picture(ImageFormat::WebP, 6, 9))]);
        post_with(&store, &files, "broken", &[("d.jpg", "image/jpeg", b"not really a jpeg".to_vec())]);
        // One already measured (as a member's app sends it) is left as it was.
        post_with(&store, &files, "sent", &[("e.gif", "image/gif", picture(ImageFormat::Gif, 4, 4))]);
        let mut extra = store.post_by_id("sent").unwrap().unwrap().extra.unwrap();
        extra["media"][0]["width"] = json!(400);
        extra["media"][0]["height"] = json!(400);
        store.set_post_extra("sent", &extra).unwrap();
        let rev = store.latest_rev().unwrap();

        let counted = fill(&store, &files, true, |_| {}).unwrap();
        assert_eq!((counted.posts_read, counted.posts, counted.pictures, counted.unreadable), (3, 1, 2, 1));
        assert_eq!(store.latest_rev().unwrap(), rev);
        assert!(media_of(&store, "two")[0].get("width").is_none());

        let mut told = 0;
        let filled = fill(&store, &files, false, |_| told += 1).unwrap();
        assert_eq!(filled, Filled { posts_read: 3, posts: 1, pictures: 2, not_here: 0, unreadable: 1, changed_meanwhile: 0 });
        assert!(told > 0);
        let media = media_of(&store, "two");
        assert_eq!((media[0]["width"].clone(), media[0]["height"].clone()), (json!(30), json!(20)));
        assert!(media[1].get("width").is_none());
        assert_eq!((media[2]["width"].clone(), media[2]["height"].clone()), (json!(6), json!(9)));
        // Its key is as it was: the app still opens it.
        assert!(media[0]["ciphering"]["parameters"]["key"].is_string());
        // Sent as changed, as an edit is; nothing else is.
        let changed = store.posts_changed_after(rev, 10).unwrap();
        assert_eq!(changed.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["two"]);
        assert!(media_of(&store, "broken")[0].get("width").is_none());
        assert_eq!(media_of(&store, "sent")[0]["width"], json!(400));

        // Again: nothing more.
        let rev = store.latest_rev().unwrap();
        let again = fill(&store, &files, false, |_| {}).unwrap();
        assert_eq!((again.posts, again.pictures, again.unreadable), (0, 0, 1));
        assert_eq!(store.latest_rev().unwrap(), rev);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_post_changed_meanwhile_is_left_for_another_run() {
        let (store, files, dir) = community();
        post_with(&store, &files, "p", &[("a.png", "image/png", picture(ImageFormat::Png, 2, 2))]);
        let was = store.post_extras_with_media_after("", 10).unwrap().remove(0).1;
        store.set_post_extra("p", &json!({ "media": [], "attachments": [] })).unwrap();
        assert!(!store.replace_post_extra("p", &was, &json!({ "media": "overwritten" })).unwrap());
        assert_eq!(media_of(&store, "p"), json!([]));
        let _ = std::fs::remove_dir_all(dir);
    }
}
