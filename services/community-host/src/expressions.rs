//! A Discord server's own custom emoji and stickers brought into one community's
//! packs, from a folder `scripts/discord_expressions.py` fetched them into
//! (`manifest.json` and the pictures beside it).
//!
//! Written straight into the community's sticker store, as its own packs (owned by
//! the community's DID, as those made through its workflows are), so it is for the
//! host's operator, not a member: the workflows take one picture at a time and hold
//! them to the usual sizes; this takes them as the folder has them, at their
//! original size if they were fetched so, and says which are over those sizes.
//!
//! - Emoji go in packs of their own, still ones and animated ones apart, as Discord
//!   lists them; stickers in another. The community's pack limits are raised to fit
//!   what it already has and what comes in (never lowered).
//! - A Discord name becomes a shortcode Wyvrn takes (`a-z`, `0-9`, `_`, 2 to 32
//!   long), each one once in the community: `PepeLaugh` is `pepelaugh`, and a second
//!   `pepelaugh` (`pepeLaugh`) is `pepelaugh_2`.
//! - Run again, a picture the community's packs already hold is left out, so it can
//!   be run once more after a partial run, or with emoji added on Discord since.
//! - `imported.json`, written beside the manifest, gives each Discord emoji and
//!   sticker id the pack, shortcode and file it became: what an importer of the
//!   server's history needs to draw `<:name:id>`.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;
use serde_json::{json, Value};
use wyvrn_stickers::config::ServiceConfig;
use wyvrn_stickers::images::{inspect, Image};
use wyvrn_stickers::processor::{is_emoji, is_name, is_shortcode};
use wyvrn_stickers::store::{pack_scope, Kind, Pack, Store};

use crate::config::Limits;

/// One picture the fetch wrote down.
#[derive(Debug, Clone, Deserialize)]
pub struct Fetched {
    pub id: String,
    pub name: String,
    /// Its file in the folder.
    pub file: String,
    #[serde(default)]
    pub animated: bool,
    /// A sticker's tags on Discord: the emoji it stands for, or words.
    #[serde(default)]
    pub tags: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub guild: Guild,
    #[serde(default)]
    pub emoji: Vec<Fetched>,
    #[serde(default)]
    pub stickers: Vec<Fetched>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Guild {
    pub id: String,
    pub name: String,
}

/// A pack to be made, and what goes in it.
#[derive(Debug, Clone)]
pub struct Planned {
    pub name: String,
    pub title: String,
    pub kind: Kind,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub discord_id: String,
    pub discord_name: String,
    pub image: Image,
    pub bytes: Vec<u8>,
    /// The emoji it stands for: a sticker's from its tags, else 🙂; none for a
    /// custom emoji.
    pub emoji: String,
    pub shortcode: Option<String>,
    /// Over what the workflows let a member upload: kept as it is all the same.
    pub oversize: bool,
}

/// What an import would do, or did.
#[derive(Debug, Default)]
pub struct Plan {
    pub packs: Vec<Planned>,
    /// The same picture as one already in the community's packs, or as another
    /// coming in: not kept twice, but given what that one became. (Discord id,
    /// Discord name, its picture's hash.)
    pub already: Vec<(String, String, String)>,
    /// Couldn't be taken, and why.
    pub refused: Vec<(String, String)>,
    /// The limits the community needs, where they are more than it has.
    pub limits: Option<Limits>,
}

/// `text` as a shortcode Wyvrn takes: lower case, anything but a-z, 0-9 and _ as
/// _, at most 32 long, at least 2.
pub fn shortcode_of(text: &str) -> String {
    let mut code: String = text.chars().map(|c| c.to_ascii_lowercase()).map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '_' }).collect();
    code.truncate(32);
    while code.len() < 2 {
        code.push('_');
    }
    code
}

/// The first of `tags` that is an emoji, as a sticker's stands-for.
fn emoji_of(tags: &str) -> String {
    tags.split([',', ' ']).map(str::trim).find(|tag| is_emoji(tag)).unwrap_or("🙂").to_string()
}

/// A pack name from `text`: lower case, a-z, 0-9 and single hyphens, 3 to 32 long.
pub fn pack_name_of(text: &str) -> String {
    let mut name = String::new();
    for c in text.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            name.push(c);
        } else if !name.is_empty() && !name.ends_with('-') {
            name.push('-');
        }
    }
    name.truncate(24);
    let name = name.trim_end_matches('-').to_string();
    if name.len() < 3 { format!("server-{name}").trim_end_matches('-').to_string() } else { name }
}

/// The next of `base`, `base-2`, `base-3`... not in `taken` and a name packs may have.
fn free_name(base: &str, taken: &mut HashSet<String>, reserved: &[String]) -> String {
    let mut n = 1;
    loop {
        let name = if n == 1 { base.to_string() } else { format!("{base}-{n}") };
        if is_name(&name) && !reserved.contains(&name) && taken.insert(name.clone()) {
            return name;
        }
        n += 1;
    }
}

fn free_shortcode(base: &str, taken: &mut HashSet<String>) -> String {
    let mut n = 1;
    loop {
        let code = if n == 1 {
            base.to_string()
        } else {
            let suffix = format!("_{n}");
            format!("{}{suffix}", &base[..base.len().min(32 - suffix.len())])
        };
        if is_shortcode(&code) && taken.insert(code.clone()) {
            return code;
        }
        n += 1;
    }
}

/// Works out what bringing `manifest` (its pictures in `folder`) into `store`
/// would do, under `limits` (the community's now) and `prefix` (the start of the
/// new packs' names; the server's name if `None`). Nothing is written.
pub fn plan(manifest: &Manifest, folder: &Path, store: &Store, owner: &str, limits: &Limits, prefix: Option<&str>) -> anyhow::Result<Plan> {
    let sizes = ServiceConfig::default();
    let existing = store.packs(None)?;
    let mut held: HashSet<String> = HashSet::new();
    let mut codes: HashSet<String> = HashSet::new();
    let (mut emoji_packs, mut sticker_packs) = (0, 0);
    for pack in &existing {
        if pack.owner == owner && !pack.removed {
            match pack.kind {
                Kind::Emoji => emoji_packs += 1,
                Kind::Sticker => sticker_packs += 1,
            }
        }
        for sticker in store.stickers(&pack_scope(&pack.name))? {
            held.insert(sticker.sha256.clone());
            if let Some(code) = sticker.shortcode {
                codes.insert(code);
            }
        }
    }
    let mut names: HashSet<String> = existing.iter().map(|pack| pack.name.clone()).collect();
    let reserved = store.reserved_names()?.unwrap_or(sizes.reserved_names.clone());

    let mut plan = Plan::default();
    let mut read = |fetched: &Fetched, kind: Kind| -> anyhow::Result<Option<Item>> {
        let path = folder.join(&fetched.file);
        anyhow::ensure!(!fetched.file.contains("..") && !fetched.file.starts_with('/'), "{}: a file outside the folder", fetched.file);
        let bytes = std::fs::read(&path).with_context(|| format!("{}", path.display()))?;
        let image = match inspect(&bytes) {
            Ok(image) => image,
            Err(why) => {
                plan.refused.push((fetched.name.clone(), why));
                return Ok(None);
            }
        };
        if !held.insert(image.sha256.clone()) {
            plan.already.push((fetched.id.clone(), fetched.name.clone(), image.sha256.clone()));
            return Ok(None);
        }
        let (px, max_bytes) = match kind {
            Kind::Emoji => (sizes.max_emoji_px, sizes.max_emoji_bytes),
            Kind::Sticker => (sizes.max_sticker_px, sizes.max_sticker_bytes),
        };
        let oversize = image.width > px || image.height > px || bytes.len() > max_bytes;
        let (emoji, shortcode) = match kind {
            Kind::Emoji => (String::new(), Some(free_shortcode(&shortcode_of(&fetched.name), &mut codes))),
            Kind::Sticker => (emoji_of(&fetched.tags), None),
        };
        Ok(Some(Item { discord_id: fetched.id.clone(), discord_name: fetched.name.clone(), image, bytes, emoji, shortcode, oversize }))
    };
    let mut still = Vec::new();
    let mut animated = Vec::new();
    for fetched in &manifest.emoji {
        if let Some(item) = read(fetched, Kind::Emoji)? {
            if item.image.animated || fetched.animated { animated.push(item) } else { still.push(item) }
        }
    }
    let mut stickers = Vec::new();
    for fetched in &manifest.stickers {
        if let Some(item) = read(fetched, Kind::Sticker)? {
            stickers.push(item);
        }
    }

    // Each group in one pack where the per-pack limit can be raised to hold it
    // (Discord's own most: 250 emoji of a kind, 60 stickers), else split.
    let emoji_per_pack = limits.emoji_per_pack.max(still.len().max(animated.len()).min(250));
    let stickers_per_pack = limits.stickers_per_pack.max(stickers.len().min(250));
    let prefix = prefix.map(pack_name_of).unwrap_or_else(|| pack_name_of(&manifest.guild.name));
    let title = &manifest.guild.name;
    let mut add = |items: Vec<Item>, kind: Kind, per: usize, base: &str, title: String| {
        let chunks: Vec<Vec<Item>> = items.chunks(per.max(1)).map(<[Item]>::to_vec).collect();
        let several = chunks.len() > 1;
        for (n, items) in chunks.into_iter().enumerate() {
            let title = if several { format!("{title} {}", n + 1) } else { title.clone() };
            plan.packs.push(Planned { name: free_name(&format!("{prefix}-{base}"), &mut names, &reserved), title, kind, items });
        }
    };
    add(still, Kind::Emoji, emoji_per_pack, "emoji", format!("{title} emoji"));
    add(animated, Kind::Emoji, emoji_per_pack, "animated", format!("{title} animated emoji"));
    add(stickers, Kind::Sticker, stickers_per_pack, "stickers", format!("{title} stickers"));

    let new_emoji = plan.packs.iter().filter(|pack| pack.kind == Kind::Emoji).count();
    let new_stickers = plan.packs.len() - new_emoji;
    let mut raised = limits.clone();
    raised.emoji_packs = limits.emoji_packs.max(emoji_packs + new_emoji);
    raised.emoji_per_pack = emoji_per_pack;
    raised.sticker_packs = limits.sticker_packs.max(sticker_packs + new_stickers);
    raised.stickers_per_pack = stickers_per_pack;
    if (raised.emoji_packs, raised.emoji_per_pack, raised.sticker_packs, raised.stickers_per_pack) != (limits.emoji_packs, limits.emoji_per_pack, limits.sticker_packs, limits.stickers_per_pack) {
        plan.limits = Some(raised);
    }
    Ok(plan)
}

/// Makes `plan`'s packs in `store`, owned by `owner`, and gives back what each
/// Discord id became (`imported.json`), those not kept twice included.
pub fn write(plan: &Plan, store: &Store, owner: &str, now: i64) -> anyhow::Result<Value> {
    let mut became = serde_json::Map::new();
    for planned in &plan.packs {
        let draft = format!("import:{}", planned.name);
        for item in &planned.items {
            store.add_sticker(&draft, &item.image, &item.bytes, &item.emoji, item.shortcode.as_deref())?;
        }
        let pack = Pack {
            name: planned.name.clone(),
            kind: planned.kind,
            title: planned.title.clone(),
            owner: owner.to_string(),
            reuse: true,
            icon: planned.items.first().map(|item| item.image.id().to_string()),
            version: 1,
            created: now,
            updated: now,
            removed: false,
        };
        anyhow::ensure!(store.publish(&pack, &draft)?, "a pack called {} was made meanwhile; run it again", planned.name);
        for item in &planned.items {
            became.insert(
                item.discord_id.clone(),
                json!({ "kind": planned.kind.as_str(), "discord_name": item.discord_name, "pack": planned.name, "shortcode": item.shortcode, "file": item.image.file() }),
            );
        }
    }
    // A picture kept once stands for every Discord id that had it.
    if !plan.already.is_empty() {
        let mut kept = std::collections::HashMap::new();
        for pack in store.packs(Some(owner))?.into_iter().filter(|pack| !pack.removed) {
            for sticker in store.stickers(&pack_scope(&pack.name))? {
                kept.entry(sticker.sha256.clone()).or_insert_with(|| (pack.kind, pack.name.clone(), sticker.shortcode.clone(), sticker.file()));
            }
        }
        for (id, name, sha256) in &plan.already {
            if let Some((kind, pack, shortcode, file)) = kept.get(sha256) {
                became.insert(id.clone(), json!({ "kind": kind.as_str(), "discord_name": name, "pack": pack, "shortcode": shortcode, "file": file, "same_as_another": true }));
            }
        }
    }
    Ok(Value::Object(became))
}

/// What `plan` comes to, for a person to read.
pub fn describe(plan: &Plan) -> String {
    let mut lines = Vec::new();
    for pack in &plan.packs {
        let oversize = pack.items.iter().filter(|item| item.oversize).count();
        let kind = if pack.kind == Kind::Emoji { "emoji" } else { "stickers" };
        let over = if oversize > 0 { format!(", {oversize} over the usual size, kept as they are") } else { String::new() };
        lines.push(format!("pack {} ({:?}): {} {kind}{over}", pack.name, pack.title, pack.items.len()));
        for item in pack.items.iter().filter(|item| item.shortcode.as_deref().is_some_and(|code| code != item.discord_name)) {
            lines.push(format!("    {} is :{}:", item.discord_name, item.shortcode.as_deref().unwrap_or_default()));
        }
    }
    if !plan.already.is_empty() {
        lines.push(format!("the same picture as one already in a pack, or coming in, so not kept twice: {}", plan.already.len()));
    }
    for (name, why) in &plan.refused {
        lines.push(format!("left out: {name}: {why}"));
    }
    if let Some(limits) = &plan.limits {
        lines.push(format!(
            "limits raised to {} emoji packs of {} and {} sticker packs of {}",
            limits.emoji_packs, limits.emoji_per_pack, limits.sticker_packs, limits.stickers_per_pack
        ));
    }
    if plan.packs.is_empty() {
        lines.push("no new pictures to bring in".into());
    }
    lines.join("\n")
}

/// Reads `folder`'s `manifest.json`.
pub fn manifest(folder: &Path) -> anyhow::Result<Manifest> {
    let path = folder.join("manifest.json");
    let text = std::fs::read_to_string(&path).with_context(|| format!("{}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("{}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_names_become_shortcodes_and_pack_names() {
        assert_eq!(shortcode_of("PepeLaugh"), "pepelaugh");
        assert_eq!(shortcode_of("a"), "a_");
        assert_eq!(shortcode_of("blob-wave!"), "blob_wave_");
        assert_eq!(shortcode_of(&"x".repeat(40)).len(), 32);
        assert_eq!(pack_name_of("Frosty's Den!"), "frosty-s-den");
        assert_eq!(pack_name_of("❄"), "server");
        let mut taken = HashSet::new();
        assert_eq!(free_shortcode("pepelaugh", &mut taken), "pepelaugh");
        assert_eq!(free_shortcode("pepelaugh", &mut taken), "pepelaugh_2");
        let long = "y".repeat(32);
        assert_eq!(free_shortcode(&long, &mut taken), long);
        assert_eq!(free_shortcode(&long, &mut taken), format!("{}_2", "y".repeat(30)));
        assert_eq!(emoji_of("star, ⭐"), "⭐");
        assert_eq!(emoji_of("wave"), "🙂");
    }

    /// A PNG's signature and size, which is all `inspect` reads; `seed` makes it a
    /// picture of its own.
    fn png(side: u32, seed: u8, padding: usize) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend(side.to_be_bytes());
        bytes.extend(side.to_be_bytes());
        bytes.extend([8, 6, 0, 0, 0, seed]);
        bytes.extend(b"IDAT");
        bytes.extend(std::iter::repeat_n(seed, padding));
        bytes
    }

    #[test]
    fn a_servers_emoji_and_stickers_become_packs_once() {
        let folder = std::env::temp_dir().join(format!("wyvrn-expressions-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let pictures = [("1.png", png(128, 1, 0)), ("2.png", png(128, 2, 0)), ("3.png", png(128, 1, 0)), ("4.png", png(256, 4, 200_000)), ("5.png", png(320, 5, 0))];
        for (file, bytes) in &pictures {
            std::fs::write(folder.join(file), bytes).unwrap();
        }
        let fetched = |id: &str, name: &str, file: &str, tags: &str| Fetched { id: id.into(), name: name.into(), file: file.into(), animated: false, tags: tags.into() };
        let manifest = Manifest {
            guild: Guild { id: "42".into(), name: "Frosty's Den".into() },
            emoji: vec![fetched("1", "PepeLaugh", "1.png", ""), fetched("2", "pepelaugh", "2.png", ""), fetched("3", "Again", "3.png", ""), fetched("4", "Huge", "4.png", "")],
            stickers: vec![fetched("5", "Wave", "5.png", "wave, 👋")],
        };
        let store = Store::in_memory().unwrap();
        let owner = "did:web:example.com:c:den";
        let limits = Limits { emoji_per_pack: 2, ..Limits::default() };

        let planned = plan(&manifest, &folder, &store, owner, &limits, None).unwrap();
        let packs: Vec<(&str, usize)> = planned.packs.iter().map(|pack| (pack.name.as_str(), pack.items.len())).collect();
        // Three still emoji (one picture is had twice) fit one pack once the limit is raised to hold them.
        assert_eq!(packs, [("frosty-s-den-emoji", 3), ("frosty-s-den-stickers", 1)]);
        let codes: Vec<_> = planned.packs[0].items.iter().map(|item| item.shortcode.clone().unwrap()).collect();
        assert_eq!(codes, ["pepelaugh", "pepelaugh_2", "huge"]);
        assert!(planned.packs[0].items[2].oversize, "over 128 KB, and kept as it is");
        assert_eq!(planned.packs[1].items[0].emoji, "👋");
        assert_eq!(planned.limits.as_ref().map(|limits| limits.emoji_per_pack), Some(3));

        let became = write(&planned, &store, owner, 1).unwrap();
        assert_eq!(became["3"]["shortcode"], "pepelaugh", "the same picture as PepeLaugh's: given what it became");
        assert_eq!(became["3"]["same_as_another"], true);
        assert_eq!(became["5"]["pack"], "frosty-s-den-stickers");
        let made = store.packs(Some(owner)).unwrap();
        assert_eq!(made.len(), 2);
        assert_eq!(store.stickers(&pack_scope("frosty-s-den-emoji")).unwrap().len(), 3);

        // Again: nothing new, every id still mapped.
        let again = plan(&manifest, &folder, &store, owner, &limits, None).unwrap();
        assert!(again.packs.is_empty());
        assert_eq!(again.already.len(), 5);
        assert_eq!(write(&again, &store, owner, 2).unwrap().as_object().unwrap().len(), 5);
        std::fs::remove_dir_all(&folder).ok();
    }
}
