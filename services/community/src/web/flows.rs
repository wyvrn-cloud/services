//! The web view's workflows (community/1.0's administration workflows):
//!
//! - `web`, for members holding `manage_community`: the look (colours for light
//!   and dark, corner radius, font, a CSS override) and the identity provider's
//!   websites (added with a name and exact redirect addresses, the secret shown
//!   once; taken away), and whether websites may register themselves or be known
//!   by their address.
//! - `signed-in-sites`, for any member: the websites they let sign them in
//!   through the community, and taking any of them back.
//!
//! [`Theme`] is the look, and what `/theme.css` is made from.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::store::{hashed, secret, Client};
use crate::community::{line, Community};
use crate::questions::{failed, Refusal};
use crate::store::{Flow, Store};

pub const WEB_FLOW: &str = "web";
pub const SITES_FLOW: &str = "signed-in-sites";
/// The most a CSS override may be.
pub const MAX_CSS: usize = 32 * 1024;
const MAX_NAME: usize = 80;
const MAX_REDIRECTS: usize = 10;

/// The fonts an admin may choose: each `(id, label, CSS font-family)`.
pub const FONTS: [(&str, &str, &str); 4] = [
    ("inter", "Inter (the default)", "'Inter', system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif"),
    ("serif", "Source Serif", "'Source Serif 4', Georgia, 'Times New Roman', serif"),
    ("mono", "JetBrains Mono", "'JetBrains Mono', ui-monospace, SFMono-Regular, Menlo, monospace"),
    ("system", "This device's own", "system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif"),
];

/// A community's look on the web.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub accent_light: String,
    pub background_light: String,
    pub surface_light: String,
    pub text_light: String,
    pub accent_dark: String,
    pub background_dark: String,
    pub surface_dark: String,
    pub text_dark: String,
    /// Corner radius, in pixels, 0 to 24.
    pub radius: u8,
    pub font: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent_light: "#4a63e7".into(),
            background_light: "#ffffff".into(),
            surface_light: "#f4f4f6".into(),
            text_light: "#1a1a1e".into(),
            accent_dark: "#8c9bff".into(),
            background_dark: "#16161a".into(),
            surface_dark: "#1e1e24".into(),
            text_dark: "#ececee".into(),
            radius: 12,
            font: "inter".into(),
        }
    }
}

/// `#rrggbb`, lowercase, if `text` is a colour.
fn colour(text: &str) -> Option<String> {
    let text = text.trim().to_ascii_lowercase();
    let hex = text.strip_prefix('#').unwrap_or(&text);
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("#{hex}"))
}

/// What reads on a colour: near-black on a light one, white on a dark one.
fn on(colour: &str) -> &'static str {
    let channel = |at: usize| u8::from_str_radix(&colour[at..at + 2], 16).map(|v| v as f64 / 255.0).unwrap_or(0.0);
    let linear = |c: f64| if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    let luminance = 0.2126 * linear(channel(1)) + 0.7152 * linear(channel(3)) + 0.0722 * linear(channel(5));
    // Whichever contrasts more (WCAG's ratio), against white and near-black.
    let on_white = 1.05 / (luminance + 0.05);
    let on_dark = (luminance + 0.05) / 0.056;
    if on_dark > on_white { "#111114" } else { "#ffffff" }
}

impl Theme {
    pub fn load(store: &Store) -> Self {
        store.setting(super::THEME).ok().flatten().and_then(|text| serde_json::from_str(&text).ok()).map(Self::checked).unwrap_or_default()
    }

    /// Itself, with anything that isn't what it should be as the default has it.
    fn checked(self) -> Self {
        let default = Self::default();
        let pick = |given: &str, fallback: &str| colour(given).unwrap_or_else(|| fallback.to_string());
        Self {
            accent_light: pick(&self.accent_light, &default.accent_light),
            background_light: pick(&self.background_light, &default.background_light),
            surface_light: pick(&self.surface_light, &default.surface_light),
            text_light: pick(&self.text_light, &default.text_light),
            accent_dark: pick(&self.accent_dark, &default.accent_dark),
            background_dark: pick(&self.background_dark, &default.background_dark),
            surface_dark: pick(&self.surface_dark, &default.surface_dark),
            text_dark: pick(&self.text_dark, &default.text_dark),
            radius: self.radius.min(24),
            font: if FONTS.iter().any(|(id, _, _)| *id == self.font) { self.font } else { default.font },
        }
    }

    fn block(accent: &str, background: &str, surface: &str, text: &str) -> String {
        format!(
            "--accent: {accent}; --on-accent: {}; --bg: {background}; --surface: {surface}; --text: {text}; --muted: color-mix(in srgb, {text} 62%, {background}); --border: color-mix(in srgb, {text} 13%, {background});",
            on(accent)
        )
    }

    /// The CSS variables, for light and for dark (by the device's choice, or the
    /// page's switch).
    pub fn variables(&self) -> String {
        let font = FONTS.iter().find(|(id, _, _)| *id == self.font).map_or(FONTS[0].2, |(_, _, family)| family);
        let light = Self::block(&self.accent_light, &self.background_light, &self.surface_light, &self.text_light);
        let dark = Self::block(&self.accent_dark, &self.background_dark, &self.surface_dark, &self.text_dark);
        format!(
            "/* The community's theme */\n:root {{ {light} --radius: {}px; --font: {font}; color-scheme: light; }}\n@media (prefers-color-scheme: dark) {{ :root:not([data-theme=\"light\"]) {{ {dark} color-scheme: dark; }} }}\n:root[data-theme=\"dark\"] {{ {dark} color-scheme: dark; }}\n",
            self.radius
        )
    }
}

/// A CSS override, if it may be served: not too long, and reaching nowhere (no
/// `@import`, and `url()` only for a `data:` picture). The page's content
/// security policy would stop it loading anything anyway; this says so to the
/// admin, rather than leaving it silently broken.
pub fn css_allowed(css: &str) -> Result<(), &'static str> {
    if css.len() > MAX_CSS {
        return Err("The CSS is longer than 32 KB.");
    }
    let lower = css.to_ascii_lowercase();
    if lower.contains("@import") {
        return Err("The CSS can't @import anything: it can't load from elsewhere.");
    }
    if lower.contains("expression(") || lower.contains("javascript:") || lower.contains("behavior:") || lower.contains("-moz-binding") {
        return Err("The CSS can't run anything.");
    }
    let mut rest = lower.as_str();
    while let Some(at) = rest.find("url(") {
        let inside = rest[at + 4..].trim_start().trim_start_matches(['"', '\'']);
        if !inside.starts_with("data:image/") || inside.starts_with("data:image/svg") {
            return Err("The CSS can use url() only for a data: picture (PNG, JPEG, GIF or WebP): it can't load from elsewhere.");
        }
        rest = &rest[at + 4..];
    }
    Ok(())
}

fn button(label: &str, event: &str, input: Value) -> Value {
    json!({ "type": "submit-button", "label": label, "event": event, "input": input })
}

fn refused(comment: &str) -> Refusal {
    Refusal::new("guard_failed", comment)
}

fn ticked(value: &Value) -> bool {
    match value {
        Value::Bool(ticked) => *ticked,
        Value::String(text) => matches!(text.as_str(), "true" | "on" | "yes" | "1"),
        _ => false,
    }
}

impl Community {
    /// The `web` workflow's form, or its "add a website" step.
    pub fn web_flow_ui(&self, flow: &Flow) -> anyhow::Result<(Value, Vec<&'static str>)> {
        let mut items = vec![json!({ "type": "title", "text": "On the web" })];
        if let Some(note) = flow.data["note"].as_str() {
            items.push(json!({ "type": "text", "text": note }));
        }
        if flow.state == "add" {
            items.push(json!({ "type": "text", "text": "A website that may sign members in with this community. It is shown to them as verified, since you added it." }));
            items.push(json!({ "type": "form", "children": [
                { "type": "text-input", "form-id": "name", "label": "Its name, as members are shown it", "value": "" },
                { "type": "text-area", "form-id": "redirect_uris", "label": "Where it is sent back to: exact addresses, one a line (https://, or http://localhost for testing)", "value": "" },
                { "type": "check-box", "form-id": "public", "label": "It can't keep a secret (it runs in the browser): PKCE alone", "value": false },
                button("Add", "create", json!({})),
            ] }));
            items.push(button("Back", "back", json!({})));
            return Ok((Value::Array(items), vec!["create", "back"]));
        }
        let theme = Theme::load(&self.store);
        let css = self.store.setting(super::CSS)?.unwrap_or_default();
        let url = self.web_url.clone().unwrap_or_default();
        items.push(json!({ "type": "text", "text": format!("The community's page is at {url}. Whether it is on, and who sees who is in voice, are in the community's settings; this is how it looks, and the websites members sign in to with it.") }));
        let colour_input = |id: &str, label: &str, value: &str| json!({ "type": "text-input", "form-id": id, "label": label, "value": value });
        let fonts: Vec<Value> = FONTS.iter().map(|(id, label, _)| json!({ "value": id, "label": label })).collect();
        items.push(json!({ "type": "form", "children": [
            { "type": "title", "text": "Light" },
            colour_input("accent_light", "Accent (#rrggbb)", &theme.accent_light),
            colour_input("background_light", "Background", &theme.background_light),
            colour_input("surface_light", "Panels and cards", &theme.surface_light),
            colour_input("text_light", "Text", &theme.text_light),
            { "type": "title", "text": "Dark" },
            colour_input("accent_dark", "Accent", &theme.accent_dark),
            colour_input("background_dark", "Background", &theme.background_dark),
            colour_input("surface_dark", "Panels and cards", &theme.surface_dark),
            colour_input("text_dark", "Text", &theme.text_dark),
            { "type": "title", "text": "Shape and type" },
            { "type": "text-input", "form-id": "radius", "label": "Corner radius, 0 to 24 pixels", "value": theme.radius.to_string() },
            { "type": "drop-down", "form-id": "font", "label": "Font", "options": fonts, "value": theme.font },
            { "type": "text-area", "form-id": "css", "label": "Your own CSS, after the theme (up to 32 KB; it can't load anything from elsewhere, and isn't used on the sign-in and consent pages)", "value": css },
            { "type": "title", "text": "Other websites signing members in" },
            { "type": "check-box", "form-id": "dynamic", "label": "Websites may register themselves (shown to members as unverified)", "value": self.store.setting(super::DYNAMIC)?.as_deref() == Some("1") },
            { "type": "check-box", "form-id": "indieauth", "label": "Websites may be known by their address, IndieAuth (shown as unverified)", "value": self.store.setting(super::INDIEAUTH)?.as_deref() == Some("1") },
            button("Save", "save", json!({})),
        ] }));
        items.push(json!({ "type": "title", "text": "Websites" }));
        let clients = self.store.clients()?;
        if clients.is_empty() {
            items.push(json!({ "type": "text", "text": "None yet." }));
        }
        for client in &clients {
            let kind = match client.kind.as_str() {
                "registered" => "added by an admin",
                "dynamic" => "registered itself",
                _ => "known by its address",
            };
            items.push(json!({ "type": "text", "text": format!("{} ({kind}): {}. Its id: {}", client.name, client.redirect_uris.join(", "), client.id) }));
            items.push(button(&format!("Take {} away", client.name), "remove", json!({ "client": client.id })));
        }
        items.push(button("Add a website", "add", json!({})));
        items.push(button("Done", "done", json!({})));
        Ok((Value::Array(items), vec!["save", "add", "remove", "done"]))
    }

    /// An event of the `web` workflow; whether `community` changed (it doesn't).
    pub fn advance_web_flow(&self, flow: &mut Flow, event: &str, input: &Value, now: i64) -> Result<bool, Refusal> {
        match (flow.state.as_str(), event) {
            ("form", "save") => {
                let given = |id: &str| input[id].as_str().unwrap_or_default().to_string();
                let mut theme = Theme {
                    accent_light: given("accent_light"),
                    background_light: given("background_light"),
                    surface_light: given("surface_light"),
                    text_light: given("text_light"),
                    accent_dark: given("accent_dark"),
                    background_dark: given("background_dark"),
                    surface_dark: given("surface_dark"),
                    text_dark: given("text_dark"),
                    radius: 12,
                    font: given("font"),
                };
                for (id, value) in [("accent_light", &theme.accent_light), ("background_light", &theme.background_light), ("surface_light", &theme.surface_light), ("text_light", &theme.text_light), ("accent_dark", &theme.accent_dark), ("background_dark", &theme.background_dark), ("surface_dark", &theme.surface_dark), ("text_dark", &theme.text_dark)] {
                    if !value.trim().is_empty() && colour(value).is_none() {
                        return Err(refused(&format!("{id}: a colour is written #rrggbb, as #4a63e7.")));
                    }
                }
                theme.radius = match given("radius").trim() {
                    "" => 12,
                    text => text.parse::<u8>().ok().filter(|r| *r <= 24).ok_or_else(|| refused("The corner radius is 0 to 24."))?,
                };
                let theme = theme.checked();
                let css = given("css");
                css_allowed(&css).map_err(refused)?;
                self.store.set_setting(super::THEME, &serde_json::to_string(&theme).unwrap_or_default()).map_err(failed)?;
                self.store.set_setting(super::CSS, &css).map_err(failed)?;
                self.store.set_setting(super::DYNAMIC, if ticked(&input["dynamic"]) { "1" } else { "0" }).map_err(failed)?;
                self.store.set_setting(super::INDIEAUTH, if ticked(&input["indieauth"]) { "1" } else { "0" }).map_err(failed)?;
                self.audit(now, &flow.owner, "community.web", None, Some(json!({ "theme": theme, "css_bytes": css.len(), "dynamic": ticked(&input["dynamic"]), "indieauth": ticked(&input["indieauth"]) })));
                flow.data["note"] = json!("Saved.");
                Ok(false)
            }
            ("form", "add") => {
                flow.state = "add".into();
                Ok(false)
            }
            ("form", "remove") => {
                let id = input["client"].as_str().unwrap_or_default();
                if !self.store.remove_client(id).map_err(failed)? {
                    return Err(refused("There's no such website."));
                }
                self.audit(now, &flow.owner, "web.client.remove", Some(id), None);
                flow.data["note"] = json!("Taken away: it can't sign anyone in now, and what it was given stops working.");
                Ok(false)
            }
            ("form", "done") => {
                flow.status = "completed".into();
                Ok(false)
            }
            ("add", "create") => {
                let name = line(&input["name"], MAX_NAME).ok_or_else(|| refused("A website needs a name."))?;
                let uris: Vec<String> = input["redirect_uris"].as_str().unwrap_or_default().lines().map(str::trim).filter(|u| !u.is_empty()).map(str::to_string).collect();
                if uris.is_empty() || uris.len() > MAX_REDIRECTS {
                    return Err(refused("Give one to ten addresses it is sent back to."));
                }
                if let Some(bad) = uris.iter().find(|u| !super::oidc::redirect_allowed(u)) {
                    return Err(refused(&format!("{bad} isn't an https:// address (or http://localhost), whole and with no #.")));
                }
                let public = ticked(&input["public"]);
                let given_secret = (!public).then(secret);
                let client = Client { id: uuid::Uuid::new_v4().simple().to_string()[..20].to_string(), kind: "registered".into(), name: name.clone(), secret: given_secret.as_deref().map(hashed), redirect_uris: uris, created: now, created_by: Some(flow.owner.clone()) };
                self.store.add_client(&client).map_err(failed)?;
                self.audit(now, &flow.owner, "web.client.add", Some(&client.id), Some(json!({ "name": name })));
                let issuer = self.web_url.clone().unwrap_or_default();
                flow.data["note"] = json!(match given_secret {
                    Some(given) => format!("Added {name}. Its client id is {} and its secret, shown this once, is {given}. The issuer is {}.", client.id, issuer.trim_end_matches('/')),
                    None => format!("Added {name}. Its client id is {} (no secret: it uses PKCE alone). The issuer is {}.", client.id, issuer.trim_end_matches('/')),
                });
                flow.state = "form".into();
                Ok(false)
            }
            ("add", "back") => {
                flow.state = "form".into();
                Ok(false)
            }
            _ => Err(refused("That can't be done here.")),
        }
    }

    /// The `signed-in-sites` workflow's list.
    pub fn sites_flow_ui(&self, flow: &Flow) -> anyhow::Result<(Value, Vec<&'static str>)> {
        let mut items = vec![json!({ "type": "title", "text": "Websites you sign in to with this community" })];
        if let Some(note) = flow.data["note"].as_str() {
            items.push(json!({ "type": "text", "text": note }));
        }
        let mkey = self.store.member_key(&flow.owner)?;
        let consents = self.store.consents_of(&mkey)?;
        if consents.is_empty() {
            items.push(json!({ "type": "text", "text": "None." }));
        }
        for (client_id, scopes, _) in consents {
            let client = self.store.client(&client_id)?;
            let name = client.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| client_id.clone());
            let verified = client.as_ref().is_some_and(Client::verified);
            let told: Vec<&str> = scopes.split_whitespace().filter_map(|s| match s {
                "openid" => Some("that you're a member"),
                "did" => Some("your DID"),
                "profile" => Some("your name and picture"),
                "roles" => Some("your roles"),
                _ => None,
            }).collect();
            items.push(json!({ "type": "text", "text": format!("{name}{}: told {}.", if verified { "" } else { " (unverified)" }, told.join(", ")) }));
            items.push(button(&format!("Take back {name}"), "revoke", json!({ "client": client_id })));
        }
        items.push(button("Done", "done", json!({})));
        Ok((Value::Array(items), vec!["revoke", "done"]))
    }

    pub fn advance_sites_flow(&self, flow: &mut Flow, event: &str, input: &Value) -> Result<bool, Refusal> {
        match event {
            "revoke" => {
                let mkey = self.store.member_key(&flow.owner).map_err(failed)?;
                let client = input["client"].as_str().unwrap_or_default();
                if !self.store.revoke_consent(&mkey, client).map_err(failed)? {
                    return Err(refused("You hadn't let that website sign you in."));
                }
                flow.data["note"] = json!("Taken back: it can't read anything of yours now, and you'll be asked again if you sign in there.");
                Ok(false)
            }
            "done" => {
                flow.status = "completed".into();
                Ok(false)
            }
            _ => Err(refused("That can't be done here.")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_keeps_only_colours() {
        let theme = Theme { accent_light: "red; } body { display: none".into(), radius: 99, font: "comic".into(), ..Theme::default() }.checked();
        assert_eq!(theme.accent_light, Theme::default().accent_light);
        assert_eq!(theme.radius, 24);
        assert_eq!(theme.font, "inter");
        let css = theme.variables();
        assert!(css.contains("--accent: #4a63e7"));
        assert!(!css.contains("display: none"));
    }

    #[test]
    fn text_on_an_accent_reads() {
        assert_eq!(on("#ffffff"), "#111114");
        assert_eq!(on("#4a63e7"), "#ffffff");
        assert_eq!(on("#ffd166"), "#111114");
        assert_eq!(on("#8c9bff"), "#111114", "the default dark accent");
    }

    #[test]
    fn css_reaches_nowhere() {
        assert!(css_allowed(".post { border: 1px solid red; }").is_ok());
        assert!(css_allowed(".a { background: url(data:image/png;base64,AAAA) }").is_ok());
        assert!(css_allowed("@import url(https://evil.example/x.css);").is_err());
        assert!(css_allowed("input[value^=a] { background: url(https://evil.example/a) }").is_err());
        assert!(css_allowed(".a { background: URL( 'data:image/svg+xml,<svg/>' ) }").is_err());
        assert!(css_allowed(&"a".repeat(MAX_CSS + 1)).is_err());
    }
}
