use std::sync::Arc;

use anyhow::Context;
use wyvrn_community_host::{
    admin::SEALED_FOR,
    config::{Config, Limits},
    registry::{Registry, State, Storage},
    server::{self, AppState},
    tenants::Host,
};
use wyvrn_service::now;

const USAGE: &str = "\
wyvrn-community-host [--config <path>] [serve]                    run the host
wyvrn-community-host [--config <path>] list                       every community
wyvrn-community-host [--config <path>] reseal                     every community's keys (and the host's)
                                                                  read and kept again now: sealed with
                                                                  COMMUNITY_HOST_SEAL_KEY, or unsealed
                                                                  without one. After rotating the key, so
                                                                  the previous one can be dropped.
wyvrn-community-host [--config <path>] create <name> <owner-did> [<storage>] [<title>]
                                                                  make one (storage: sqlite, mysql-db, mysql-shared)
wyvrn-community-host [--config <path>] suspend|resume <name>      stop answering for one, or start again
wyvrn-community-host [--config <path>] delete|restore <name>      remove one (kept 30 days), or bring it back
wyvrn-community-host [--config <path>] limits <name> <members> <channels>
                                                                  what one may hold
wyvrn-community-host [--config <path>] import <name> <owner-did> --community <database>
                     [--stickers <database>] [--files <folder>] [--title <title>] [--storage <storage>]
                                                                  bring in a community run on its own
                                                                  (wyvrn-community), with its keys, its
                                                                  address and everything it keeps. A
                                                                  database is a mysql:// URL or a SQLite file.
                                                                  Stop its own server first.
wyvrn-community-host [--config <path>] import-expressions <name> <folder> [--prefix <name>] [--dry-run]
                                                                  a Discord server's own emoji and stickers,
                                                                  fetched into <folder> by
                                                                  scripts/discord_expressions.py, as the
                                                                  community's packs; its pack limits raised
                                                                  to fit. --dry-run says what it would do.
wyvrn-community-host [--config <path>] discord-survey <folder> [<channel>...] [--out <file>]
                                                                  what a DiscordChatExporter backup (JSON,
                                                                  --media) holds and what importing it would
                                                                  take: messages, attachments, GIF links,
                                                                  disk space. Counts only, as Markdown.
wyvrn-community-host [--config <path>] import-discord <name> <folder> <channel>... [--map <discord-id>=<channel>]
                     [--text-only] [--no-threads] [--expressions <folder>] [--dry-run]
                                                                  a Discord channel's history and its threads
                                                                  (by its id or name; a forum's by name) into the channel
                                                                  here of the same category and name (or as
                                                                  --map says), read among its posts by time.
                                                                  Files sealed; nobody notified; again, only
                                                                  what isn't there yet. --dry-run says what
                                                                  it would do.
wyvrn-community-host [--config <path>] backup <name>              back one up now (backups.key set)
wyvrn-community-host [--config <path>] open-backup <file> <out.tar.gz>
                                                                  open a backup with the backup key: the
                                                                  .tar.gz is what an export is

The config path is --config, else $COMMUNITY_HOST_CONFIG, else config/default.toml.
COMMUNITY_HOST_SEAL_KEY (base64 of 32 bytes; or COMMUNITY_HOST_SEAL_KEY_FILE) seals every
community's keys at rest; keys found unsealed are sealed as they are next read.
To rotate it, give the new one and COMMUNITY_HOST_SEAL_KEY_PREVIOUS (or _PREVIOUS_FILE: the
old ones, separated by commas): keys sealed with an old one open, and are sealed with the new
one as they are next read (or all at once, with reseal). Old ones and no new one: sealing off.
A change made here while the host runs is taken up when the community is next opened.";

fn registry(config: &Config) -> anyhow::Result<Registry> {
    // In the file the configuration names, unless the environment names a database
    // (COMMUNITY_HOST_DATABASE_URL or DATABASE_URL).
    let location = wyvrn_service::db::Location::from_env("COMMUNITY_HOST", &config.store.path)?;
    Registry::open(&location).with_context(|| format!("store {location}"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = match args.iter().position(|arg| arg == "--config") {
        Some(at) if at + 1 < args.len() => {
            let path = args.remove(at + 1);
            args.remove(at);
            path
        }
        Some(_) => anyhow::bail!("--config needs a path\n\n{USAGE}"),
        None => std::env::var("COMMUNITY_HOST_CONFIG").unwrap_or_else(|_| "config/default.toml".to_string()),
    };
    let config = Config::load(&config_path)?;
    // Every community's keys sealed at rest with the host's key, where it is given
    // one (wyvrn_service::identity): a stolen database, folder or backup alone then
    // makes nobody any community. Kept in the environment or a secrets file, never
    // in the store.
    if let Some(key) = std::env::var("COMMUNITY_HOST_SEAL_KEY").ok().filter(|k| !k.trim().is_empty()) {
        wyvrn_service::identity::seal_with(&key).context("COMMUNITY_HOST_SEAL_KEY")?;
    } else if let Some(path) = std::env::var("COMMUNITY_HOST_SEAL_KEY_FILE").ok().filter(|p| !p.trim().is_empty()) {
        let key = std::fs::read_to_string(&path).with_context(|| format!("COMMUNITY_HOST_SEAL_KEY_FILE {path}"))?;
        wyvrn_service::identity::seal_with(&key).context("COMMUNITY_HOST_SEAL_KEY_FILE")?;
    }
    // Keys sealed with before, while the key is being rotated (or sealing turned off).
    if let Some(keys) = std::env::var("COMMUNITY_HOST_SEAL_KEY_PREVIOUS").ok().filter(|k| !k.trim().is_empty()) {
        wyvrn_service::identity::opens_with_previous(&keys).context("COMMUNITY_HOST_SEAL_KEY_PREVIOUS")?;
    } else if let Some(path) = std::env::var("COMMUNITY_HOST_SEAL_KEY_PREVIOUS_FILE").ok().filter(|p| !p.trim().is_empty()) {
        let keys = std::fs::read_to_string(&path).with_context(|| format!("COMMUNITY_HOST_SEAL_KEY_PREVIOUS_FILE {path}"))?;
        wyvrn_service::identity::opens_with_previous(&keys).context("COMMUNITY_HOST_SEAL_KEY_PREVIOUS_FILE")?;
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let state_of = |name: &str, state: State, sealed: Option<i64>| -> anyhow::Result<()> {
        let registry = registry(&config)?;
        anyhow::ensure!(registry.set_state(name, state, sealed)?, "there is no community called {name}");
        println!("{name} is {}", state.as_str());
        Ok(())
    };
    match args.as_slice() {
        [] | ["serve"] => serve(config).await,
        ["open-backup", file, out] => {
            let key = wyvrn_community_host::backups::key(&config.backups)?.context("no backup key: set backups.key or COMMUNITY_HOST_BACKUP_KEY")?;
            let of = wyvrn_community_host::backups::open(&key, std::path::Path::new(file), std::path::Path::new(out))?;
            if let Some(name) = of {
                println!("a backup of {name}");
            }
            println!("opened: {out}");
            Ok(())
        }
        ["backup", name] => {
            let host = Host::new(config.clone(), registry(&config)?);
            let made = tokio::task::block_in_place(|| host.back_up(name, now()))?;
            println!("{}", made.display());
            Ok(())
        }
        ["reseal"] => {
            // Reading keys keeps them again as this process would: sealed with the
            // key now (or unsealed), whatever they were sealed with before.
            let location = wyvrn_service::db::Location::from_env("COMMUNITY_HOST", &config.store.path)?;
            wyvrn_service::identity::load(&location, &config.identity.path, "community-host").context("the host's own keys")?;
            let host = Host::new(config.clone(), registry(&config)?);
            let (mut done, mut failed) = (0, 0);
            for record in registry(&config)?.all()? {
                let read = host.stores(&record, record.storage).and_then(|stores| wyvrn_service::identity::load(&stores.community, &host.settings(&record).identity.path, "community"));
                match read {
                    Ok(Some(_)) => done += 1,
                    Ok(None) => println!("{}: no keys yet", record.name),
                    Err(error) => {
                        failed += 1;
                        println!("{}: {error:#}", record.name);
                    }
                }
            }
            println!("{done} communities' keys kept again, and the host's{}", if failed > 0 { format!("; {failed} couldn't be") } else { String::new() });
            anyhow::ensure!(failed == 0, "some keys couldn't be read: keep the previous key until they can");
            Ok(())
        }
        ["list"] => {
            for record in registry(&config)?.all()? {
                println!("{}\t{}\t{}\t{}\t{}", record.name, record.state.as_str(), record.storage.as_str(), record.owner, record.title);
            }
            Ok(())
        }
        ["create", name, owner, rest @ ..] => {
            anyhow::ensure!(owner.starts_with("did:"), "{owner} isn't a DID");
            let storage = match rest.first() {
                Some(kind) => Storage::parse(kind).with_context(|| format!("{kind}: sqlite, mysql-db or mysql-shared"))?,
                None => Storage::parse(&config.storage.default).context("storage.default: sqlite, mysql-db or mysql-shared")?,
            };
            let title = rest.get(1).copied().unwrap_or(name);
            let record = registry(&config)?.create(name, title, storage, owner, "the command line", now())?;
            println!("{} is made, at {}c/{}/ (opened by the host to take up its address)", record.name, config.server.public_url, record.name);
            Ok(())
        }
        ["suspend", name] => state_of(name, State::Suspended, None),
        ["resume", name] | ["restore", name] => state_of(name, State::Active, None),
        ["delete", name] => state_of(name, State::Deleted, Some(now() + SEALED_FOR)),
        ["limits", name, members, channels] => {
            let registry = registry(&config)?;
            let record = registry.get(name)?.with_context(|| format!("there is no community called {name}"))?;
            let limits = Limits { members: members.parse()?, channels: channels.parse()?, ..record.limits.unwrap_or_else(|| config.limits.clone()) };
            registry.set_limits(name, Some(&limits))?;
            println!("{name} may hold {members} members and {channels} channels");
            Ok(())
        }
        ["discord-survey", folder, rest @ ..] => {
            let mut only = Vec::new();
            let mut out = None;
            let mut args = rest.iter();
            while let Some(arg) = args.next() {
                match *arg {
                    "--out" => out = Some(args.next().context("--out needs a file")?.to_string()),
                    flag if flag.starts_with("--") => anyhow::bail!("{flag}: unrecognized\n\n{USAGE}"),
                    channel => only.push(channel.to_string()),
                }
            }
            let survey = wyvrn_community::discord_history::survey(std::path::Path::new(folder), &only, |file| eprintln!("reading {file}"))?;
            let text = survey.markdown();
            match out {
                Some(out) => {
                    std::fs::write(&out, &text)?;
                    println!("written: {out}");
                }
                None => println!("{text}"),
            }
            Ok(())
        }
        ["import-discord", name, folder, rest @ ..] => {
            let mut asked = wyvrn_community_host::history::Asked {
                folder: std::path::PathBuf::from(folder),
                channels: Vec::new(),
                map: Default::default(),
                media: true,
                expressions: None,
                dry_run: false,
                threads: true,
            };
            let mut args = rest.iter();
            while let Some(arg) = args.next() {
                match *arg {
                    "--dry-run" => asked.dry_run = true,
                    "--text-only" => asked.media = false,
                    "--no-threads" => asked.threads = false,
                    "--expressions" => asked.expressions = Some(args.next().context("--expressions needs a folder")?.into()),
                    "--map" => {
                        let given = args.next().context("--map needs <discord-id>=<channel>")?;
                        let (from, to) = given.split_once('=').context("--map needs <discord-id>=<channel>")?;
                        asked.map.insert(from.to_string(), to.to_string());
                        if !asked.channels.iter().any(|c| c == from) {
                            asked.channels.push(from.to_string());
                        }
                    }
                    flag if flag.starts_with("--") => anyhow::bail!("{flag}: unrecognized\n\n{USAGE}"),
                    channel => asked.channels.push(channel.to_string()),
                }
            }
            anyhow::ensure!(!asked.channels.is_empty(), "name the channels to import\n\n{USAGE}");
            let host = Host::new(config.clone(), registry(&config)?);
            tokio::task::block_in_place(|| wyvrn_community_host::history::run(&host, name, &asked, now()))
        }
        ["import-expressions", name, folder, rest @ ..] => {
            let dry_run = rest.contains(&"--dry-run");
            let prefix = match rest.iter().position(|arg| *arg == "--prefix") {
                Some(at) => Some(rest.get(at + 1).context("--prefix needs a value")?.to_string()),
                None => None,
            };
            for (at, arg) in rest.iter().enumerate() {
                let value_of_flag = at > 0 && rest[at - 1] == "--prefix";
                anyhow::ensure!(value_of_flag || ["--prefix", "--dry-run"].contains(arg), "{arg}: unrecognized\n\n{USAGE}");
            }
            let folder = std::path::Path::new(folder);
            let manifest = wyvrn_community_host::expressions::manifest(folder)?;
            let host = Host::new(config.clone(), registry(&config)?);
            let record = registry(&config)?.get(name)?.with_context(|| format!("there is no community called {name}"))?;
            let stores = host.stores(&record, record.storage)?;
            let settings = host.settings(&record);
            // Its packs are its own DID's, as those made through its workflows are.
            let identity = wyvrn_service::identity::load(&stores.community, &settings.identity.path, "community")?
                .with_context(|| format!("{name} has no keys yet: open it once (speak to it) first"))?;
            let (agent, _) = wyvrn_community::server::build_agent_for(&settings, identity, None)?;
            let owner = agent.did();
            let location = stores.stickers.as_ref().context("this community keeps no stickers")?;
            let store = wyvrn_stickers::store::Store::open(location)?;
            let limits = record.limits.clone().unwrap_or_else(|| config.limits.clone());
            let plan = wyvrn_community_host::expressions::plan(&manifest, folder, &store, &owner, &limits, prefix.as_deref())?;
            println!("{} ({}) into {name}:", manifest.guild.name, manifest.guild.id);
            println!("{}", wyvrn_community_host::expressions::describe(&plan));
            if dry_run {
                return Ok(());
            }
            if let Some(raised) = &plan.limits {
                host.change_limits(name, record.limits.as_ref(), raised)?;
            }
            let became = wyvrn_community_host::expressions::write(&plan, &store, &owner, now())?;
            let out = folder.join("imported.json");
            // Added to what an earlier run wrote, so it covers every run.
            let mut all = std::fs::read_to_string(&out).ok().and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok()).unwrap_or_else(|| serde_json::json!({}));
            if let (Some(all), Some(new)) = (all.as_object_mut(), became.as_object()) {
                all.extend(new.clone());
            }
            std::fs::write(&out, serde_json::to_string_pretty(&all)?)?;
            println!("done; what each Discord id became: {}", out.display());
            println!("members see the new packs the next time their app asks the community for its details");
            Ok(())
        }
        ["import", name, owner, rest @ ..] => {
            anyhow::ensure!(owner.starts_with("did:"), "{owner} isn't a DID");
            let option = |flag: &str| -> anyhow::Result<Option<String>> {
                match rest.iter().position(|arg| *arg == flag) {
                    Some(at) => rest.get(at + 1).map(|value| Some(value.to_string())).with_context(|| format!("{flag} needs a value")),
                    None => Ok(None),
                }
            };
            for (at, arg) in rest.iter().enumerate() {
                let value_of_flag = at > 0 && rest[at - 1].starts_with("--");
                anyhow::ensure!(value_of_flag || ["--community", "--stickers", "--files", "--title", "--storage"].contains(arg), "{arg}: unrecognized\n\n{USAGE}");
            }
            let database = |given: String| -> anyhow::Result<wyvrn_service::db::Location> {
                if given.contains("://") { Ok(wyvrn_service::db::Location::parse(&given)?) } else { Ok(wyvrn_service::db::Location::Sqlite(given.into())) }
            };
            let community = database(option("--community")?.context("--community: where the community keeps what it keeps")?)?;
            let stickers = option("--stickers")?.map(database).transpose()?;
            let files = option("--files")?.map(std::path::PathBuf::from);
            let storage = match option("--storage")? {
                Some(kind) => Storage::parse(&kind).with_context(|| format!("{kind}: sqlite, mysql-db or mysql-shared"))?,
                None => Storage::parse(&config.storage.default).context("storage.default: sqlite, mysql-db or mysql-shared")?,
            };
            let title = option("--title")?.unwrap_or_else(|| name.to_string());
            let host = Host::new(config.clone(), registry(&config)?);
            let source = wyvrn_community_host::import::Source { community, stickers, files, files_records: None };
            println!("{}", tokio::task::block_in_place(|| host.import(name, &title, owner, storage, &source, now()))?);
            Ok(())
        }
        ["help" | "--help" | "-h"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => anyhow::bail!("unrecognized arguments\n\n{USAGE}"),
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    wyvrn_service::logging::init();
    let location = wyvrn_service::db::Location::from_env("COMMUNITY_HOST", &config.store.path)?;
    let identity = wyvrn_service::identity::load_or_generate(&location, &config.identity.path, "community-host")?;
    let (agent, did_document) = server::build_agent(&config, identity)?;
    let registry = registry(&config)?;
    let communities = registry.all()?.len();
    let bind = config.server.bind.clone();
    if config.host.admins.is_empty() {
        tracing::warn!("nobody can make communities from the app: set COMMUNITY_HOST_ADMINS, or use `wyvrn-community-host create`");
    }
    let host = Host::new(config, registry);
    tracing::info!(did = %agent.did(), url = %host.config.server.public_url, communities, "community host ready");
    tokio::spawn(host.clone().run_housekeeping());
    // Owners' servers taking their communities over (export.rs).
    tokio::spawn(host.clone().watch_hand_overs(Arc::from(didcomm_quickstart::default_resolver())));
    let state = Arc::new(AppState { agent, host, did_document, resolver: didcomm_quickstart::default_resolver(), admin: Default::default(), open: wyvrn_community_host::creating::open_pages(), made: Default::default() });
    wyvrn_service::http::serve(&bind, server::router(state)).await
}
