//! A community handed to its owner's own server (community/1.0, "Hosted
//! communities, and moving").
//!
//! The owner has a page in their community's own admin workflows ([`MoveOut`],
//! which only they are listed). From it:
//!
//! 1. **Begin.** The community is read-only from then on, and every member is told
//!    from its public address, with the `from_prior` that brings whoever knew it at
//!    its path here to that address.
//! 2. **Make the export.** Everything it keeps, as the single `wyvrn-community`
//!    keeps it in SQLite: its keys, its store (members, posts, roles, invites, its
//!    address's log), its packs, its files, and a configuration to start from. It is
//!    offered once, at a link good for 15 minutes ([`download`]).
//! 3. The owner runs it at their own domain, set to hold the same address. Holding
//!    it writes the address's next log entry, naming where the community is now.
//! 4. **The host sees that** ([`Host::watch_hand_overs`]): once the address no
//!    longer names this copy, it is stopped and kept 30 days (`moved`), and a host
//!    admin can still restore it.
//!
//! Called off before then, it is open again as it was.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use axum::{
    body::Body,
    extract::{Path as UrlPath, State},
    http::{header, Request, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use tower::ServiceExt;
use wyvrn_service::admin::{button, Page, Pages};
use wyvrn_service::db::{copy_store, Location};
use wyvrn_service::now;

use crate::admin::SEALED_FOR;
use crate::registry::State as Standing;
use crate::server::AppState;
use crate::tenants::Host;

pub const MOVE_OUT: &str = "admin-move-out";
/// How long an export's link is good for.
const LINK_FOR: i64 = 15 * 60;

/// An export made and not yet fetched.
#[derive(Debug, Clone)]
pub struct Export {
    pub community: String,
    pub file: PathBuf,
    pub expires: i64,
}

/// The exports waiting to be fetched, by the token in their link.
#[derive(Default)]
pub struct Exports {
    waiting: Mutex<HashMap<String, Export>>,
}

impl Exports {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Export>> {
        self.waiting.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The link token of `community`'s export, if one is waiting and good.
    pub fn of(&self, community: &str, now: i64) -> Option<String> {
        self.lock().iter().find(|(_, export)| export.community == community && export.expires > now).map(|(token, _)| token.clone())
    }

    /// Takes the export at `token`: once, while it is good.
    pub fn take(&self, token: &str, now: i64) -> Option<Export> {
        self.lock().remove(token).filter(|export| export.expires > now)
    }

    /// Forgets `community`'s exports, removing their files.
    pub fn forget(&self, community: &str) {
        self.lock().retain(|_, export| {
            let theirs = export.community == community;
            if theirs {
                let _ = std::fs::remove_file(&export.file);
            }
            !theirs
        });
    }

    fn add(&self, token: String, export: Export) {
        self.lock().insert(token, export);
    }
}

/// The owner's page in one community.
pub struct MoveOut {
    pub host: Weak<Host>,
    pub community: String,
}

impl Pages for MoveOut {
    fn pages(&self) -> Vec<Page> {
        vec![Page { id: MOVE_OUT, title: "Move to your own server", summary: "Take this community to a server you run, keeping its members, posts and address.", events: &["begin", "export", "cancel"] }]
    }

    fn view(&self, _page: &str, _owner: &str, now: i64) -> anyhow::Result<Vec<Value>> {
        let Some(host) = self.host.upgrade() else { return Ok(vec![]) };
        let Some(record) = host.registry.get(&self.community)? else { return Ok(vec![]) };
        let address = host.address(&self.community)?.map(|address| address.did_web);
        let mut items = vec![json!({ "type": "text", "text": "Your community can move to a server you run yourself (wyvrn-community), and stay the same community: its members, channels, posts, roles and address go with it, and members' apps follow it without doing anything." })];
        match record.state {
            Standing::Active => {
                items.push(json!({ "type": "text", "text": "While it moves it is read-only here: people can read it, and nobody can post or change anything, until your server takes over. You can call it off until then." }));
                items.push(button("Begin moving", "begin", json!({})));
            }
            Standing::Moving => {
                items.push(json!({ "type": "information", "text": "It is moving, and read-only here." }));
                match host.exports.of(&self.community, now) {
                    Some(token) => items.push(json!({ "type": "web-link", "label": "Download it (once, within 15 minutes)", "url": format!("{}export/{token}", host.config.server.public_url) })),
                    None => items.push(button("Make the export", "export", json!({}))),
                }
                let mut steps = String::from("Unpack it on your server, set public_url in config/default.toml to where it will be reached over HTTPS, and run wyvrn-community there.");
                if let Some(address) = &address {
                    steps.push_str(&format!(" Set COMMUNITY_PUBLIC_ADDRESS={address}: when your server holds the address, this copy stops by itself."));
                }
                items.push(json!({ "type": "text", "text": steps }));
                items.push(button("Call it off", "cancel", json!({})));
            }
            _ => items.push(json!({ "type": "text", "text": "It isn't open here now." })),
        }
        Ok(items)
    }

    fn act(&self, _page: &str, event: &str, _input: &Value, owner: &str, now: i64) -> Result<Option<String>, String> {
        let host = self.host.upgrade().ok_or("The host is stopping.")?;
        let failed = |error: anyhow::Error| {
            tracing::error!(community = %self.community, error = %format!("{error:#}"), "moving out failed");
            format!("That didn't work: {error}")
        };
        let record = host.registry.get(&self.community).map_err(failed)?.ok_or("This community is gone.")?;
        let said = match (event, record.state) {
            ("begin", Standing::Active) => {
                host.set_state(&self.community, Standing::Moving, None).map_err(failed)?;
                "It is moving: read-only from now on, and everyone has been told. Make the export next."
            }
            ("export", Standing::Moving) => {
                tokio::task::block_in_place(|| host.export(&self.community, now)).map_err(failed)?;
                "The export is ready: download it once, within 15 minutes."
            }
            ("cancel", Standing::Moving) => {
                host.exports.forget(&self.community);
                host.set_state(&self.community, Standing::Active, None).map_err(failed)?;
                "Called off: it is open here again."
            }
            _ => return Err("That can't be done now.".into()),
        };
        tracing::info!(community = %self.community, event, by = %owner, "the owner's move out");
        Ok(Some(said.into()))
    }
}

/// A random token for a link.
fn token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Copies a folder's files into another, folders inside included.
fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    if !from.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if entry.file_name() != "files.sqlite" && !entry.file_name().to_string_lossy().starts_with("files.sqlite-") {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

impl Host {
    /// Makes `name`'s export, as `wyvrn-community` keeps a community in SQLite, and
    /// offers it at a link good for 15 minutes. Blocks.
    pub fn export(&self, name: &str, now: i64) -> anyhow::Result<String> {
        let record = self.registry.get(name)?.ok_or_else(|| anyhow::anyhow!("there is no community called {name}"))?;
        anyhow::ensure!(record.state == Standing::Moving, "{name} isn't moving");
        let from = self.stores(&record, record.storage)?;
        let exports = self.config.storage.dir.join(".exports");
        let token = token();
        let staging = exports.join(&token);
        let data = staging.join("data");
        std::fs::create_dir_all(data.join("files"))?;

        // Its keys, as the single server keeps them beside a SQLite store.
        let keys = self.folder(name).join("identity.json");
        let identity = wyvrn_service::identity::load(&from.community, &keys, "community")?.ok_or_else(|| anyhow::anyhow!("{name} has no keys"))?;
        identity.save(data.join("identity.json"))?;
        // Everything it keeps, in the files the single server reads.
        let community = Location::Sqlite(data.join("community.sqlite"));
        let store = wyvrn_community::store::Store::open(&community)?;
        wyvrn_service::profile::Kept::open(&community, "community")?;
        wyvrn_service::hosted_address::Kept::open(&community, "community")?;
        copy_store(&from.community, &community, "community", &["wyvrn_identity"])?;
        // Not moving any more, where it arrives.
        store.set_setting(wyvrn_community::community::MOVING_SETTING, "0")?;
        drop(store);
        if let Some(stickers) = &from.stickers {
            let to = Location::Sqlite(data.join("stickers.sqlite"));
            wyvrn_stickers::store::Store::open(&to)?;
            copy_store(stickers, &to, "stickers", &[])?;
        }
        if let Some(files) = &from.files {
            // Where the single server looks for its files' records: beside the files.
            let to = Location::Sqlite(data.join("files").join("files.sqlite"));
            wyvrn_files::store::Store::open(&to, &data.join("files"))?;
            copy_store(files, &to, "files", &[])?;
            copy_dir(&self.folder(name).join("files"), &data.join("files"))?;
        }
        std::fs::create_dir_all(staging.join("config"))?;
        std::fs::write(staging.join("config").join("default.toml"), self.single_config(&record))?;
        std::fs::write(staging.join("README.txt"), self.export_readme(name)?)?;

        // One file to fetch.
        let file = exports.join(format!("{token}.tar.gz"));
        let packed = std::fs::File::create(&file)?;
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(packed, flate2::Compression::default()));
        archive.append_dir_all(name, &staging)?;
        archive.into_inner()?.finish()?.flush()?;
        std::fs::remove_dir_all(&staging)?;

        self.exports.forget(name);
        self.exports.add(token.clone(), Export { community: name.to_string(), file, expires: now + LINK_FOR });
        tracing::info!(community = %name, "made a community's export");
        Ok(token)
    }

    /// The single server's configuration for `record`, as it stands here.
    fn single_config(&self, record: &crate::registry::Record) -> String {
        let voice = match &self.config.voice {
            Some(_) => "\n# Voice channels need a moq-relay of your own, whose [auth] url is <public_url>voice/auth:\n# [voice]\n# relay_url = \"https://relay.example:4443\"\n",
            None => "",
        };
        format!(
            "# {title}, moved from {host}. Set public_url, then run wyvrn-community.\n\n[server]\nbind = \"0.0.0.0:8080\"\npublic_url = \"https://community.example/\"\ndid_method = \"web\"\n\n[identity]\npath = \"data/identity.json\"\n\n[store]\npath = \"data/community.sqlite\"\n\n[community]\nname = {name}\n\n[stickers]\npath = \"data/stickers.sqlite\"\n\n[files]\ndir = \"data/files\"\n{voice}",
            title = record.title,
            host = self.config.server.public_url,
            name = toml::Value::String(record.title.clone()),
        )
    }

    fn export_readme(&self, name: &str) -> anyhow::Result<String> {
        let address = self.address(name)?.map(|address| address.did_web);
        let mut text = format!(
            "This is the community {name}, as it was kept at {host}c/{name}/: its keys (data/identity.json, which are the community: keep them safe), everything it keeps, and a configuration.\n\nTo run it at your own domain:\n\n1. Put this folder on your server, and have HTTPS in front of it.\n2. In config/default.toml, set public_url to where it is reached.\n3. Run wyvrn-community in this folder (or its Docker image, with this folder as /app).\n",
            host = self.config.server.public_url
        );
        if let Some(address) = address {
            text.push_str(&format!(
                "4. Set COMMUNITY_PUBLIC_ADDRESS={address}. Holding it tells everyone where the community is now; members' apps follow by themselves, and the copy at {host} stops.\n",
                host = self.config.server.public_url
            ));
        }
        Ok(text)
    }

    /// Every community moving out whose address now names somewhere else is
    /// stopped and kept for 30 days: its owner's server has taken over.
    pub async fn watch_hand_overs(self: Arc<Self>, resolver: Arc<dyn didcomm_core::resolver::DIDResolver>) {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let moving: Vec<_> = self.registry.all().unwrap_or_default().into_iter().filter(|record| record.state == Standing::Moving).collect();
            for record in moving {
                if let Ok(true) = self.handed_over(&record.name, resolver.as_ref()).await {
                    tracing::info!(community = %record.name, "its own server holds its address now: stopped, and kept for 30 days");
                    self.exports.forget(&record.name);
                    if let Err(error) = self.set_state(&record.name, Standing::Moved, Some(now() + SEALED_FOR)) {
                        tracing::warn!(community = %record.name, %error, "couldn't stop a community that was handed over");
                    }
                }
            }
        }
    }

    /// Whether `name`'s address names a server other than this host now.
    pub async fn handed_over(&self, name: &str, resolver: &dyn didcomm_core::resolver::DIDResolver) -> anyhow::Result<bool> {
        let Some(address) = self.address(name)? else { return Ok(false) };
        let document = resolver.resolve(&address.did_web).await.map_err(|error| anyhow::anyhow!("{error}"))?;
        let here = format!("{}c/{name}/", self.config.server.public_url);
        let endpoints: Vec<String> = document["service"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|service| service["type"] == "DIDCommMessaging")
            .filter_map(|service| service["serviceEndpoint"]["uri"].as_str().or(service["serviceEndpoint"].as_str()).map(str::to_string))
            .collect();
        Ok(!endpoints.is_empty() && endpoints.iter().all(|uri| !uri.starts_with(&here) && !uri.starts_with(&here.replacen("https://", "wss://", 1))))
    }
}

/// `GET /export/<token>`: an export, once, while its link is good.
pub async fn download(State(state): State<Arc<AppState>>, UrlPath(token): UrlPath<String>) -> Response {
    let Some(export) = state.host.exports.take(&token, now()) else { return StatusCode::NOT_FOUND.into_response() };
    let request = match Request::get("/").body(Body::empty()) {
        Ok(request) => request,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let mut response = match tower_http::services::ServeFile::new(&export.file).oneshot(request).await {
        Ok(response) => response.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if let Ok(disposition) = format!("attachment; filename=\"{}.tar.gz\"", export.community).parse() {
        response.headers_mut().insert(header::CONTENT_DISPOSITION, disposition);
    }
    // Gone once it has had time to be sent.
    let file = export.file.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
        let _ = std::fs::remove_file(file);
    });
    response
}
