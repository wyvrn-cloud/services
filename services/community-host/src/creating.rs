//! Making a community from the app (asked 2026-11-17): `community-create`, a
//! workflow at the host's DID for anyone, as far as the host's admins allow --
//! nobody (`closed`, where a host starts), whoever brings a code they hand out
//! (`codes`), or anyone (`anyone`), chosen on the admin page. The one who makes it
//! is its owner. Each person may have made so many here (`per_owner`), one at a
//! time a while apart (`every_seconds`). What is made is handed back as
//! `artifacts`, `{ community, name, url }`, `community` the DID the app adds: its
//! public address where the host has a DID host, else its own.
//!
//! A page as the admin pages are (wyvrn_service::admin): one screen, its form, and
//! what was last done said at its top.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};
use wyvrn_service::admin::{button, Page, Pages};

use crate::registry::Storage;
use crate::server::AppState;

pub const CREATE: &str = "community-create";
/// What its template id starts with (the open pages' prefix).
pub const PREFIX: &str = "community-";
const MODE: &str = "creating.mode";

/// Who may make a community here now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Closed,
    Codes,
    Anyone,
}

impl Mode {
    pub fn parse(said: &str) -> Option<Self> {
        match said {
            "closed" => Some(Self::Closed),
            "codes" => Some(Self::Codes),
            "anyone" => Some(Self::Anyone),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Codes => "codes",
            Self::Anyone => "anyone",
        }
    }
}

/// The open pages' workflows, kept as admin pages are.
pub fn open_pages() -> wyvrn_service::admin::Admin {
    wyvrn_service::admin::Admin::with_prefix(PREFIX)
}

/// What each person last made here, for their page's `artifacts`.
#[derive(Default)]
pub struct Made(Mutex<HashMap<String, Value>>);

impl AppState {
    /// The mode the admins chose, else the config's.
    pub fn creating_mode(&self) -> Mode {
        let host = &self.host;
        host.registry.setting(MODE).ok().flatten().and_then(|said| Mode::parse(&said)).or_else(|| Mode::parse(&host.config.creating.mode)).unwrap_or(Mode::Closed)
    }

    pub fn set_creating_mode(&self, mode: Mode) -> anyhow::Result<()> {
        self.host.registry.set_setting(MODE, mode.as_str())
    }

    /// The DID a community called `name` is known by: its public address, else its
    /// own (opened to learn it).
    fn did_of(&self, name: &str) -> anyhow::Result<String> {
        if let Some(address) = self.host.address(name)? {
            return Ok(address.did_web.clone());
        }
        let tenant = tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(self.host.tenant(name)))?;
        tenant.map(|tenant| tenant.state.agent.did()).ok_or_else(|| anyhow::anyhow!("{name} was made and then couldn't be opened"))
    }
}

/// The open pages, as wyvrn_service::admin draws them, for whoever asks.
pub struct ForAnyone<'a>(pub &'a AppState);

impl Pages for ForAnyone<'_> {
    fn pages(&self) -> Vec<Page> {
        if self.0.creating_mode() == Mode::Closed {
            return Vec::new();
        }
        vec![Page { id: CREATE, title: "Make a community", summary: "A community of your own, kept by this host.", events: &["create"] }]
    }

    fn view(&self, _page: &str, _owner: &str, _now: i64) -> anyhow::Result<Vec<Value>> {
        let state = self.0;
        let mode = state.creating_mode();
        if mode == Mode::Closed {
            return Ok(vec![json!({ "type": "text", "text": "Only this host's admins make communities here now." })]);
        }
        let creating = &state.host.config.creating;
        let mut fields = vec![
            json!({ "type": "text", "text": format!("It is yours: you are its owner, and can hand it on. Its name is its address, and can't be changed. You can have made {} here.", creating.per_owner) }),
            json!({ "type": "text-input", "form-id": "title", "label": "What it is called", "placeholder": "The Workshop", "required": true }),
            json!({ "type": "text-input", "form-id": "name", "label": "Its address (3 to 32 lowercase letters, digits and -)", "placeholder": "the-workshop", "required": true }),
        ];
        if mode == Mode::Codes {
            fields.push(json!({ "type": "text-input", "form-id": "code", "label": "The code you were given", "required": true }));
        }
        fields.push(button("Make it", "create", json!({})));
        Ok(vec![json!({ "type": "form", "children": fields })])
    }

    fn act(&self, _page: &str, event: &str, input: &Value, owner: &str, now: i64) -> Result<Option<String>, String> {
        let state = self.0;
        if event != "create" {
            return Err("That can't be done here.".into());
        }
        let mode = state.creating_mode();
        if mode == Mode::Closed {
            return Err("Only this host's admins make communities here now.".into());
        }
        let registry = &state.host.registry;
        let creating = &state.host.config.creating;
        let failed = |error: anyhow::Error| {
            tracing::error!(error = %format!("{error:#}"), "making a community from the app failed");
            "Something went wrong here; try again.".to_string()
        };
        if registry.made_by(owner).map_err(failed)? >= creating.per_owner {
            return Err(format!("You have made {} communities here, as many as one person may.", creating.per_owner));
        }
        if registry.last_made_by(owner).map_err(failed)?.is_some_and(|at| now - at < creating.every_seconds) {
            return Err("You made one a moment ago: wait a little before making another.".into());
        }
        let name = input["name"].as_str().map(str::trim).unwrap_or_default().to_lowercase();
        let title: String = input["title"].as_str().map(str::trim).unwrap_or_default().chars().take(100).collect();
        if title.is_empty() {
            return Err("It needs a name to be called by.".into());
        }
        if !crate::registry::valid_name(&name) {
            return Err("Its address: 3 to 32 lowercase letters, digits and -, starting with a letter.".into());
        }
        let code = input["code"].as_str().map(str::trim).unwrap_or_default();
        if mode == Mode::Codes && !registry.use_code(code).map_err(failed)? {
            return Err("That code isn't one, or has been used.".into());
        }
        let storage = Storage::parse(&state.host.config.storage.default).unwrap_or(Storage::Sqlite);
        let record = match registry.create(&name, &title, storage, owner, owner, now) {
            Ok(record) => record,
            Err(error) => {
                if mode == Mode::Codes {
                    let _ = registry.unuse_code(code);
                }
                return Err(error.to_string());
            }
        };
        let did = state.did_of(&record.name).map_err(failed)?;
        let url = format!("{}c/{}/", state.host.config.server.public_url, record.name);
        tracing::info!(community = %record.name, owner = %owner, mode = mode.as_str(), "made a community from the app");
        state.made.0.lock().unwrap_or_else(|p| p.into_inner()).insert(owner.to_string(), json!({ "community": did, "name": record.name, "url": url }));
        Ok(Some(format!("{} is made. It opens in your app now.", record.title)))
    }

    fn artifacts(&self, _page: &str, owner: &str) -> Value {
        self.0.made.0.lock().unwrap_or_else(|p| p.into_inner()).get(owner).cloned().unwrap_or_else(|| json!({}))
    }
}
