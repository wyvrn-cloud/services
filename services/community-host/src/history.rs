//! A Discord server's history brought into a hosted community: the host's side of
//! [`wyvrn_community::discord_history`]: which community, its stores and files,
//! and its file limits raised to hold what comes in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use wyvrn_community::discord_history::{self as history, Context as Setting, Target};

use crate::tenants::Host;

/// What `import-discord` was asked.
pub struct Asked {
    pub folder: PathBuf,
    /// Files chosen: a Discord channel id, or part of a file's name.
    pub channels: Vec<String>,
    /// Discord channel id -> channel here (id or name).
    pub map: HashMap<String, String>,
    pub media: bool,
    /// `import-expressions`' folder, whose `imported.json` says what the server's
    /// own emoji and stickers became.
    pub expressions: Option<PathBuf>,
    pub dry_run: bool,
    /// A channel's threads come with it.
    pub threads: bool,
}

/// The export files `chosen` names, from `folder`: each a Discord channel's id, or
/// its name (a forum's too, which has no file of its own: its posts' files). A
/// channel brings its threads, unless `threads` is false.
pub fn chosen(folder: &Path, chosen: &[String], threads: bool) -> anyhow::Result<Vec<PathBuf>> {
    let all: Vec<(PathBuf, history::ExportChannel)> = history::files_in(folder)?.into_iter().filter_map(|path| history::header(&path).ok().map(|(_, c)| (path, c))).collect();
    let key = |name: &str| wyvrn_community::discord_history::name_key(name);
    let mut files: Vec<PathBuf> = Vec::new();
    for want in chosen {
        let channels: Vec<&(PathBuf, history::ExportChannel)> = all
            .iter()
            .filter(|(_, c)| !c.is_thread() && if want.chars().all(|ch| ch.is_ascii_digit()) { c.id == *want } else { key(&c.name) == key(want) })
            .collect();
        anyhow::ensure!(channels.len() <= 1, "{want}: {} channels have that name; give its id (the number in [...] in its file's name)", channels.len());
        let parent_id = channels.first().map(|(_, c)| c.id.clone());
        let picked: Vec<&PathBuf> = all
            .iter()
            .filter(|(_, c)| {
                let is_it = parent_id.as_deref() == Some(c.id.as_str()) || (c.is_thread() && c.id == *want);
                let its_thread = threads
                    && c.is_thread()
                    && match &parent_id {
                        Some(parent) => c.category_id.as_deref() == Some(parent.as_str()),
                        // A forum, by name: its posts are threads of a channel with no file.
                        None => !want.chars().all(|ch| ch.is_ascii_digit()) && c.category.as_deref().is_some_and(|n| key(n) == key(want)),
                    };
                is_it || its_thread
            })
            .map(|(path, _)| path)
            .collect();
        anyhow::ensure!(!picked.is_empty(), "{want}: no file in {} is that channel, thread or forum", folder.display());
        for path in picked {
            if !files.contains(path) {
                files.push(path.clone());
            }
        }
    }
    Ok(files)
}

fn shown(target: &Target) -> String {
    let from = match &target.source.category {
        Some(category) if !category.is_empty() => format!("{} / #{}", category, target.source.name),
        _ => format!("#{}", target.source.name),
    };
    match &target.channel {
        Ok(channel) => format!("{from} ({}) -> #{} [{}], {}", target.source.id, channel.name, channel.id, target.how),
        Err(why) => format!("{from} ({}) -> not imported: {why}", target.source.id),
    }
}

/// Imports what `asked` says into the community `name`, or says what it would.
pub fn run(host: &Host, name: &str, asked: &Asked, now: i64) -> anyhow::Result<()> {
    let record = host.registry.get(name)?.with_context(|| format!("there is no community called {name}"))?;
    let stores = host.stores(&record, record.storage)?;
    let settings = host.settings(&record);
    let store = wyvrn_community::store::Store::open(&stores.community)?;
    let files = chosen(&asked.folder, &asked.channels, asked.threads)?;
    let targets = history::match_channels(&store, &files, &history::files_in(&asked.folder)?, &asked.map)?;

    let public_url = &settings.server.public_url;
    let expressions = match &asked.expressions {
        Some(folder) => {
            let path = folder.join("imported.json");
            let text = std::fs::read_to_string(&path).with_context(|| format!("{}: run import-expressions first", path.display()))?;
            history::expressions_from(&serde_json::from_str(&text)?, &format!("{public_url}s/"))
        }
        None => HashMap::new(),
    };
    println!("Into {name}{}:", if asked.media { "" } else { " (text only)" });
    let mut planned_bytes = 0u64;
    let mut ready = Vec::new();
    for target in &targets {
        println!("  {}", shown(target));
        let Ok(channel) = &target.channel else { continue };
        let export = history::read(&target.file)?;
        let planned = history::plan_one(&store, &asked.folder, &export, channel, asked.media)?;
        println!(
            "    {} posts to bring in, {} already here, {} system messages left out; {} files ({}){}",
            planned.posts,
            planned.already,
            planned.left_out,
            planned.files,
            history::bytes(planned.file_bytes),
            if planned.linked > 0 { format!(", {} attachments only a link", planned.linked) } else { String::new() }
        );
        if planned.interleaved > 0 {
            println!("    {} of them are from after the channel's first post here: they are read among its posts by time", planned.interleaved);
        }
        planned_bytes += planned.file_bytes;
        ready.push((channel.clone(), export));
    }
    if asked.dry_run || ready.is_empty() {
        if ready.is_empty() {
            println!("nothing to import");
        }
        return Ok(());
    }

    // Room for what comes in: the community's total raised to hold it. Each file
    // is kept whatever its size (the largest a member may send stays as it was).
    let files_store = match (&settings.files, &stores.files) {
        (Some(config), Some(location)) if asked.media => Some(wyvrn_community::files::Files::open(config, location, public_url)?),
        _ => None,
    };
    if let Some(files) = &files_store {
        let limits = record.limits.clone().unwrap_or_else(|| host.config.limits.clone());
        let used = files.host.store.used_total()?;
        if limits.files_total > 0 && used + planned_bytes > limits.files_total {
            let mut raised = limits.clone();
            raised.files_total = (used + planned_bytes).div_ceil(1 << 30).saturating_add(1) << 30;
            host.change_limits(name, record.limits.as_ref(), &raised)?;
            println!("its files may now take {} in all (was {})", history::bytes(raised.files_total), history::bytes(limits.files_total));
        }
    }
    let channel_names = history::channel_names(&history::files_in(&asked.folder)?);
    for (channel, export) in &ready {
        let setting = Setting {
            folder: &asked.folder,
            files: files_store.as_ref(),
            channel_names: channel_names.clone(),
            roles: history::roles_by_name(&store, export)?,
            expressions: expressions.clone(),
            now,
        };
        let written = history::write(&store, export, channel, &setting, |done, total| {
            if total > 0 && (done == total || done % 5000 == 0) {
                println!("    #{}: {done}/{total}", channel.name);
            }
        })?;
        println!(
            "  #{}: {} posts brought in ({} files, {}; {} reactions), {} already here",
            channel.name,
            written.posts,
            written.files,
            history::bytes(written.file_bytes),
            written.reactions,
            written.already
        );
        if written.reactions_left_out > 0 {
            println!("    {} reactions with another server's emoji left out", written.reactions_left_out);
        }
        for failed in &written.files_failed {
            println!("    not brought over: {failed}");
        }
    }
    println!("members read it the next time they open those channels and scroll back; nobody is notified");
    Ok(())
}
