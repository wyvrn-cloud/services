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
}

/// The export files `chosen` names, from `folder`.
pub fn chosen(folder: &Path, chosen: &[String]) -> anyhow::Result<Vec<PathBuf>> {
    let all = history::files_in(folder)?;
    let mut files = Vec::new();
    for want in chosen {
        let found: Vec<&PathBuf> = all
            .iter()
            .filter(|path| {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                name.contains(&format!("[{want}]")) || name.contains(want.as_str())
            })
            .collect();
        anyhow::ensure!(!found.is_empty(), "{want}: no file in {} has that channel id or name", folder.display());
        // A channel's id is in its own file's name; a name may also be in its threads'.
        let exact: Vec<&&PathBuf> = found.iter().filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains(&format!("[{want}]")))).collect();
        let found: Vec<&PathBuf> = if exact.is_empty() { found } else { exact.into_iter().copied().collect() };
        let without_threads: Vec<&PathBuf> = found.iter().copied().filter(|p| history::header(p).map(|(_, c)| !c.is_thread()).unwrap_or(true)).collect();
        let found = if found.len() > 1 && without_threads.len() == 1 { without_threads } else { found };
        anyhow::ensure!(found.len() == 1, "{want}: {} files match; give its channel id (the number in [...] in its name)", found.len());
        if !files.contains(found[0]) {
            files.push(found[0].clone());
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
    let files = chosen(&asked.folder, &asked.channels)?;
    let targets = history::match_channels(&store, &files, &asked.map)?;

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
