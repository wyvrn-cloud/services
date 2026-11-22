//! The communities the host keeps, opened when spoken to and closed when idle.
//!
//! Each is wyvrn-community's own server, opened from settings the host makes for
//! it ([`Host::settings`]): its own DID (the `did:web` at `<public_url>c/<name>/`),
//! keys, stores and router, and nothing of one reaching another. A request under
//! `/c/<name>/` opens it if it isn't open, and is handed to its router with the
//! prefix taken off, live connections (WebSockets) included.
//!
//! Thousands can be kept because few are open at once. One is closed once nobody
//! has spoken to it for a while (`idle_seconds`) and nobody holds a live connection
//! or is in a voice room, and the least recently used are closed past
//! `max_loaded`. What it still has to do unasked -- try a member again who couldn't
//! be reached, tell everyone of a poll that ended -- is noted as when to open it
//! next (`wake_at`), and [`Host::housekeeping`] opens it then.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::task::JoinHandle;
use tower::ServiceExt;
use wyvrn_community::config::{CommunityConfig, Config as CommunityServerConfig, FilesConfig, PathConfig, ServerConfig, StickersConfig};
use wyvrn_community::serve::{self, Opened, Opening, Stores};
use wyvrn_community::server::{self, AppState};
use wyvrn_service::db::Location;
use wyvrn_service::hosted_address::Address;
use wyvrn_service::now;

use crate::config::Config;
use crate::registry::{Record, Registry, State, Storage};

/// How long an unloaded community that owes someone a delivery waits to be
/// opened again and try, the first time.
const RETRY_AFTER: i64 = 60;
/// The longest it waits: each time it is closed still owing, the wait doubles, so
/// a member whose mediator stays down doesn't wake it every minute for good.
const RETRY_AT_MOST: i64 = 6 * 60 * 60;

/// The wait before the next try, after one of `waited` that still left something
/// owed (`None`: the first).
fn next_retry(waited: Option<i64>) -> i64 {
    waited.map_or(RETRY_AFTER, |waited| (waited * 2).min(RETRY_AT_MOST))
}

/// One community, open.
pub struct Tenant {
    pub name: String,
    pub state: Arc<AppState>,
    router: axum::Router,
    /// Its deliveries, pruning, voice sweep, polls and public address: stopped
    /// when it is closed.
    tasks: Vec<JoinHandle<()>>,
    last_used: AtomicI64,
}

impl Tenant {
    /// Answers `request` as the community's own server would, at its own root.
    pub async fn serve(&self, request: Request<Body>) -> Response {
        let answer = self.router.clone().oneshot(request).await;
        self.touch();
        answer.into_response()
    }

    fn touch(&self) {
        self.last_used.store(now(), Ordering::Relaxed);
    }

    /// Whether it is in use beyond requests: a live connection, or a voice room.
    fn busy(&self) -> bool {
        !self.state.live.lock().unwrap_or_else(|p| p.into_inner()).is_empty() || self.state.rooms.occupied()
    }

    /// When it has to be opened next to do what it owes, if anything, and the
    /// wait for deliveries that came into it: `retry` seconds from now if it owes
    /// any (`None` if it owes none).
    fn next_wake(&self, now: i64, retry: i64) -> (Option<i64>, Option<i64>) {
        let owed = server::deliveries_owed(&self.state).unwrap_or(false);
        let poll = self.state.community.store.next_poll_end().ok().flatten().map(|end| end.max(now));
        // What a deleted post said is forgotten when due even while nobody speaks to
        // the community: it is woken to prune, then sleeps again (pruning doesn't keep
        // it awake: nothing it does on its own counts as use).
        let forget = self.state.community.store.next_deleted_text_due(self.state.community.deleted_text_kept_for()).ok().flatten().map(|due| due.max(now));
        let delivery = owed.then_some(now + retry);
        (delivery.into_iter().chain(poll).chain(forget).min(), owed.then_some(retry))
    }
}

/// How often each open community's store is measured, in seconds.
const MEASURE_EVERY: i64 = 300;

pub struct Host {
    pub config: Config,
    pub registry: Registry,
    /// Exports made for owners moving out, waiting to be fetched (export.rs).
    pub exports: crate::export::Exports,
    /// Itself, for what each community is given that answers back to it.
    me: Weak<Host>,
    open: Mutex<HashMap<String, Arc<Tenant>>>,
    /// One community opened at a time: two requests for one that isn't open open it once.
    opening: tokio::sync::Mutex<()>,
    /// For each community closed while still owing someone a delivery, the wait
    /// it was last given (`next_retry`). Forgotten once it owes nothing.
    retrying: Mutex<HashMap<String, i64>>,
}

impl Host {
    pub fn new(config: Config, registry: Registry) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            config,
            registry,
            exports: Default::default(),
            me: me.clone(),
            open: Mutex::default(),
            opening: tokio::sync::Mutex::new(()),
            retrying: Mutex::default(),
        })
    }

    fn open_map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Tenant>>> {
        self.open.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The community called `name`, if it is open.
    pub fn loaded(&self, name: &str) -> Option<Arc<Tenant>> {
        self.open_map().get(name).cloned()
    }

    /// How many are open.
    pub fn loaded_count(&self) -> usize {
        self.open_map().len()
    }

    /// A community's folder: its SQLite files and keys, and its members' files.
    pub fn folder(&self, name: &str) -> PathBuf {
        self.config.storage.dir.join(name)
    }

    /// Where `record`'s stores are, kept as `storage`.
    pub fn stores(&self, record: &Record, storage: Storage) -> anyhow::Result<Stores> {
        let folder = self.folder(&record.name);
        let shared = |database: String, prefix: String| -> anyhow::Result<Location> {
            if self.config.storage.mysql.is_empty() {
                anyhow::bail!("{} is kept in MySQL, and no MySQL server is set (storage.mysql, COMMUNITY_HOST_MYSQL_URL)", record.name);
            }
            Ok(Location::Shared { server: self.config.storage.mysql.clone(), database, prefix })
        };
        Ok(match storage {
            Storage::Sqlite => Stores {
                community: Location::Sqlite(folder.join("community.sqlite")),
                stickers: Some(Location::Sqlite(folder.join("stickers.sqlite"))),
                files: Some(Location::Sqlite(folder.join("files.sqlite"))),
            },
            // Its own database; its packs and its files' records in tables apart. No
            // store's prefix starts another's, so each store's tables can be listed.
            Storage::MySqlDatabase => {
                let database = format!("{}{}", self.config.storage.database_prefix, record.id);
                Stores {
                    community: shared(database.clone(), "m_".into())?,
                    stickers: Some(shared(database.clone(), "s_".into())?),
                    files: Some(shared(database, "f_".into())?),
                }
            }
            Storage::MySqlShared => {
                let database = self.config.storage.shared_database.clone();
                let prefix = format!("c{}_", record.id);
                Stores {
                    community: shared(database.clone(), format!("{prefix}m_"))?,
                    stickers: Some(shared(database.clone(), format!("{prefix}s_"))?),
                    files: Some(shared(database, format!("{prefix}f_"))?),
                }
            }
        })
    }

    /// The community server's settings for `record`: what wyvrn-community would
    /// read from its configuration file, made here.
    pub fn settings(&self, record: &Record) -> CommunityServerConfig {
        let folder = self.folder(&record.name);
        let limits = record.limits.clone().unwrap_or_else(|| self.config.limits.clone());
        let mut settings = CommunityServerConfig {
            server: ServerConfig { bind: String::new(), public_url: format!("{}c/{}/", self.config.server.public_url, record.name), did_method: self.config.server.did_method },
            identity: PathConfig { path: folder.join("identity.json") },
            store: PathConfig { path: folder.join("community.sqlite") },
            community: CommunityConfig {
                name: record.title.clone(),
                // Its owner is its first admin, and so its owner (community/1.0's endnote 3).
                admins: vec![record.owner.clone()],
                owners: self.owners_of(record),
                max_members: limits.members,
                max_channels: limits.channels,
                max_roles: limits.roles,
                max_community_posts_per_minute: limits.posts_per_minute,
                max_store_bytes: limits.store_bytes,
                max_sticker_packs: limits.sticker_packs,
                max_stickers_per_pack: limits.stickers_per_pack,
                max_emoji_packs: limits.emoji_packs,
                max_emoji_per_pack: limits.emoji_per_pack,
                disabled_channel_kinds: limits.disabled_kinds(&self.config.limits),
                deleted_text_days: limits.deleted_text_days,
                ..CommunityConfig::default()
            },
            mediator: None,
            profile: Default::default(),
            voice: self.config.voice.clone(),
            stickers: Some(StickersConfig { path: folder.join("stickers.sqlite") }),
            files: Some(FilesConfig { dir: folder.join("files"), max_file_bytes: limits.file_bytes, quota_bytes: limits.files_quota, total_bytes: limits.files_total }),
            assistant: None,
            // Each community's web view is at its own address, under the host's, or
            // at its own domain once proved (domains.rs).
            web: wyvrn_community::config::WebConfig {
                public_url: self.registry.domain_of(&record.name).ok().flatten().filter(|d| d.verified_at.is_some()).map(|d| format!("https://{}/", d.domain)),
                ..self.config.web.clone()
            },
        };
        settings.community.owners_are_admins();
        settings
    }

    /// `record`'s owners: the one it was made for, and those the configuration
    /// names beside them (`[owners]`). Just the one, with none named: then it can
    /// be handed on from the app, as a community with one owner can.
    pub fn owners_of(&self, record: &Record) -> Vec<String> {
        let named = self.config.owners.get(&record.name).cloned().unwrap_or_default();
        if named.is_empty() {
            return Vec::new();
        }
        let mut owners = vec![record.owner.clone()];
        owners.extend(named.into_iter().filter(|did| *did != record.owner));
        owners
    }

    /// The public address `name` holds, where the host has a DID host to hold them at.
    pub fn address(&self, name: &str) -> anyhow::Result<Option<Address>> {
        let did_host = self.config.host.did_host.trim();
        if did_host.is_empty() {
            return Ok(None);
        }
        Ok(Some(Address::parse(&format!("did:web:{did_host}:community:{name}"))?))
    }

    /// The community called `name`, opened if it is served and isn't open yet.
    pub async fn tenant(&self, name: &str) -> anyhow::Result<Option<Arc<Tenant>>> {
        if let Some(tenant) = self.loaded(name) {
            tenant.touch();
            return Ok(Some(tenant));
        }
        let _opening = self.opening.lock().await;
        if let Some(tenant) = self.loaded(name) {
            return Ok(Some(tenant));
        }
        let Some(record) = self.registry.get(name)? else { return Ok(None) };
        if !record.state.served() {
            return Ok(None);
        }
        let tenant = Arc::new(self.load(&record).await?);
        self.open_map().insert(name.to_string(), tenant.clone());
        Ok(Some(tenant))
    }

    async fn load(&self, record: &Record) -> anyhow::Result<Tenant> {
        let config = self.settings(record);
        let stores = self.stores(record, record.storage)?;
        let owner_pages: Arc<dyn wyvrn_service::admin::Pages> = Arc::new(crate::domains::Both(
            Arc::new(crate::export::MoveOut { host: self.me.clone(), community: record.name.clone() }),
            Arc::new(crate::domains::OwnDomain { host: self.me.clone(), community: record.name.clone() }),
        ));
        let opening = Opening { profile_env: None, address: self.address(&record.name)?, voice_tokens: format!("{}.", record.name), assistant: None, owner_pages: Some(owner_pages) };
        let Opened { state, holding } = serve::open(&config, &stores, opening).await?;
        // What the host says of it stands over what it says of itself.
        state.community.set_moving(record.state == State::Moving)?;
        let mut tasks = vec![
            tokio::spawn(server::run_deliveries(state.clone())),
            tokio::spawn(server::run_pruning(state.clone())),
            tokio::spawn(server::run_voice_sweep(state.clone())),
            tokio::spawn(server::run_polls(state.clone())),
            tokio::spawn(wyvrn_community::outgoing::run_outgoing(state.clone())),
        ];
        // Moving out, the address is the owner's server's to write next.
        if let Some(holding) = holding {
            tasks.push(tokio::spawn(if record.state == State::Moving { Box::pin(holding.keep_speaking()) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> } else { Box::pin(holding.run()) }));
        }
        tracing::info!(community = %record.name, did = %state.agent.did(), storage = record.storage.as_str(), "opened a community");
        Ok(Tenant { name: record.name.clone(), router: server::router(state.clone()), state, tasks, last_used: AtomicI64::new(now()) })
    }

    /// Gives the community called `name` new limits (`before`: what it had, `None`
    /// for the host's), taken up as it is opened again. Voice turned off with people
    /// talking ends their rooms first, and tells them why, rather than leave them
    /// to be cut off without a word.
    pub fn change_limits(&self, name: &str, before: Option<&crate::config::Limits>, limits: &crate::config::Limits) -> anyhow::Result<()> {
        self.registry.set_limits(name, Some(limits))?;
        let was_off = before.unwrap_or(&self.config.limits).disabled_kinds(&self.config.limits);
        let now_off: Vec<String> = limits.disabled_kinds(&self.config.limits).into_iter().filter(|kind| (kind == "voice" || kind == "stage") && !was_off.contains(kind)).collect();
        if let Some(tenant) = self.loaded(name).filter(|_| !now_off.is_empty()) {
            wyvrn_community::server::end_voice_of_kinds(&tenant.state, &now_off);
        }
        self.unload(name);
        Ok(())
    }

    /// Closes the community called `name`, if it is open, noting when it has to be
    /// opened again.
    pub fn unload(&self, name: &str) {
        let Some(tenant) = self.open_map().remove(name) else { return };
        for task in &tenant.tasks {
            task.abort();
        }
        let mut retrying = self.retrying.lock().unwrap_or_else(|p| p.into_inner());
        let (wake, waiting) = tenant.next_wake(now(), next_retry(retrying.get(name).copied()));
        match waiting {
            Some(wait) => retrying.insert(name.to_string(), wait),
            None => retrying.remove(name),
        };
        drop(retrying);
        if let Err(error) = self.registry.set_wake(name, wake) {
            tracing::warn!(community = %name, %error, "couldn't note when to open a community again");
        }
        tracing::info!(community = %name, wake_at = ?wake, "closed a community");
    }

    /// Hands `request` to the community called `name` (its path already without
    /// `/c/<name>`), opening it first if need be.
    pub async fn forward(&self, name: &str, request: Request<Body>) -> Response {
        match self.tenant(name).await {
            Ok(Some(tenant)) => tenant.serve(request).await,
            Ok(None) => StatusCode::NOT_FOUND.into_response(),
            Err(error) => {
                tracing::error!(community = %name, error = %format!("{error:#}"), "couldn't open a community");
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
        }
    }

    /// Opens what is due, and closes what is idle or past the most kept open.
    pub async fn housekeeping(&self, now: i64) {
        match self.registry.due(now) {
            Ok(due) => {
                for record in due {
                    match self.tenant(&record.name).await {
                        Ok(Some(tenant)) => {
                            // A poll that ended while it was closed is told of now.
                            server::polls_once(&tenant.state);
                            let _ = self.registry.set_wake(&record.name, None);
                        }
                        Ok(None) => {}
                        Err(error) => tracing::warn!(community = %record.name, error = %format!("{error:#}"), "couldn't open a community that was due"),
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "couldn't read which communities are due"),
        }
        let idle_seconds = self.config.loading.idle_seconds;
        let mut open: Vec<Arc<Tenant>> = self.open_map().values().cloned().collect();
        open.sort_by_key(|tenant| tenant.last_used.load(Ordering::Relaxed));
        let mut count = open.len();
        for tenant in open {
            if tenant.busy() {
                continue;
            }
            let idle = now - tenant.last_used.load(Ordering::Relaxed) >= idle_seconds;
            if idle || count > self.config.loading.max_loaded {
                self.unload(&tenant.name);
                count -= 1;
            }
        }
    }

    /// What each open community's store takes up, measured for its limit
    /// (`store_bytes`): every few minutes, and as it opens.
    pub fn measure(&self) {
        for tenant in self.open_map().values() {
            let community = &tenant.state.community;
            match community.store.bytes_used() {
                Ok(bytes) => community.used_bytes.store(bytes, Ordering::Relaxed),
                Err(error) => tracing::warn!(community = %tenant.name, %error, "couldn't measure a community's store"),
            }
        }
    }

    /// Housekeeping every 15 seconds, for as long as the host runs; stores
    /// measured every five minutes.
    pub async fn run_housekeeping(self: Arc<Self>) {
        let mut measured = 0;
        let mut backed_up = 0;
        loop {
            if now() - measured >= MEASURE_EVERY {
                measured = now();
                let host = self.clone();
                let _ = tokio::task::spawn_blocking(move || host.measure()).await;
            }
            // Backups due, looked for every ten minutes, made one at a time off to the side.
            if now() - backed_up >= 600 {
                backed_up = now();
                let host = self.clone();
                tokio::task::spawn_blocking(move || host.back_up_due(now()));
            }
            self.housekeeping(now()).await;
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        }
    }

    /// Moves the community called `name` to another kind of storage: closed, every
    /// table copied to where `to` keeps it (its keys too), and opened from there
    /// at the next request. Requests that come meanwhile wait. What it was kept in
    /// is left as it was, for an admin to remove; SQLite files it left before, met
    /// again, are renamed aside (`.before-<time>`). Blocks: from async code, inside
    /// `block_in_place`.
    pub fn move_storage(&self, name: &str, to: Storage) -> anyhow::Result<String> {
        let record = self.registry.get(name)?.ok_or_else(|| anyhow::anyhow!("there is no community called {name}"))?;
        anyhow::ensure!(record.storage != to, "{name} is kept in {} already", to.as_str());
        let _opening = self.opening.blocking_lock();
        self.unload(name);
        // (Whatever was under way when it closed has a moment to finish.)
        std::thread::sleep(std::time::Duration::from_millis(500));
        let (from, target) = (self.stores(&record, record.storage)?, self.stores(&record, to)?);
        let folder = self.folder(name);
        std::fs::create_dir_all(&folder)?;
        // Coming back to SQLite: the files it left are set aside, not written over.
        if to == Storage::Sqlite {
            let aside = wyvrn_service::now();
            for file in ["community.sqlite", "stickers.sqlite", "files.sqlite"] {
                for part in ["", "-wal", "-shm"] {
                    let path = folder.join(format!("{file}{part}"));
                    if path.exists() {
                        std::fs::rename(&path, folder.join(format!("{file}{part}.before-{aside}")))?;
                    }
                }
            }
        }
        let keys = folder.join("identity.json");
        let identity = wyvrn_service::identity::load(&from.community, &keys, "community")?.ok_or_else(|| anyhow::anyhow!("{name} has no keys where it is kept"))?;
        wyvrn_service::identity::keep(&target.community, &keys, "community", &identity)?;
        // Its tables made there, as each store makes its own.
        wyvrn_community::store::Store::open(&target.community)?;
        wyvrn_service::profile::Kept::open(&target.community, "community")?;
        wyvrn_service::hosted_address::Kept::open(&target.community, "community")?;
        // Each store is copied in a transaction of its own: one that fails leaves
        // nothing, but those copied before it stay. They are emptied again, so that
        // trying again doesn't find them full and refuse.
        let mut done: Vec<(Location, &str, Vec<String>)> = Vec::new();
        let copied = (|| -> anyhow::Result<wyvrn_service::db::Copied> {
            let mut copied = wyvrn_service::db::copy_store(&from.community, &target.community, "community", &["wyvrn_identity"])?;
            done.push((target.community.clone(), "community", copied.iter().map(|(table, _)| table.clone()).collect()));
            if let (Some(from), Some(target)) = (&from.stickers, &target.stickers) {
                wyvrn_stickers::store::Store::open(target)?;
                let these = wyvrn_service::db::copy_store(from, target, "stickers", &[])?;
                done.push((target.clone(), "stickers", these.iter().map(|(table, _)| table.clone()).collect()));
                copied.extend(these);
            }
            if let (Some(from), Some(target)) = (&from.files, &target.files) {
                wyvrn_files::store::Store::open(target, &folder.join("files"))?;
                copied.extend(wyvrn_service::db::copy_store(from, target, "files", &[])?);
            }
            Ok(copied)
        })();
        let copied = match copied {
            Ok(copied) => copied,
            Err(error) => {
                for (target, service, tables) in done.iter().rev() {
                    if let Err(undoing) = wyvrn_service::db::empty_tables(target, service, tables) {
                        tracing::warn!(community = %name, %service, error = %undoing, "couldn't empty a store a failed move had copied into");
                    }
                }
                return Err(error.context(format!("moving {name} to {} failed; it is still kept in {}", to.as_str(), record.storage.as_str())));
            }
        };
        self.registry.set_storage(name, to)?;
        let rows: usize = copied.iter().map(|(_, rows)| rows).sum();
        tracing::info!(community = %name, from = record.storage.as_str(), to = to.as_str(), tables = copied.len(), rows, "moved a community's storage");
        Ok(format!("{name} is kept in {} now: {} tables, {rows} rows copied. What it was kept in is still there.", to.as_str(), copied.len()))
    }

    /// Sets how `name` stands, and acts on it: one no longer served is closed, one
    /// moving is read-only from now on and its members told.
    pub fn set_state(&self, name: &str, state: State, sealed_until: Option<i64>) -> anyhow::Result<bool> {
        if !self.registry.set_state(name, state, sealed_until)? {
            return Ok(false);
        }
        if !state.served() {
            self.unload(name);
        } else if let Some(tenant) = self.loaded(name) {
            server::set_moving(&tenant.state, state == State::Moving)?;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_community_still_owing_waits_longer_each_time_up_to_six_hours() {
        let mut waits = vec![next_retry(None)];
        while waits.len() < 12 {
            waits.push(next_retry(waits.last().copied()));
        }
        assert_eq!(&waits[..5], [60, 120, 240, 480, 960]);
        assert_eq!(*waits.last().unwrap(), RETRY_AT_MOST);
        assert!(waits.windows(2).all(|pair| pair[1] >= pair[0]));
    }
}
