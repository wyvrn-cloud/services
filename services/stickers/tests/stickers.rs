//! The service run for real: its HTTP server, makers that talk to it over
//! DIDComm as a workflow coordinator would, and someone fetching what it serves.

use std::sync::Arc;

use base64::Engine;
use didcomm_agent::{Agent, Identity};
use didcomm_crypto_askar::{AskarCryptoService, AskarSigningKey};
use serde_json::{json, Value};
use wyvrn_stickers::{
    processor::Processor,
    server::{self, AppState},
    store::Store,
};

const P: &str = "https://didcomm.org/workflow/1.0/";

struct Running {
    did: String,
    url: String,
    state: Arc<AppState>,
}

async fn start_service(extra: &str) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let config: wyvrn_stickers::config::Config =
        toml::from_str(&format!("[server]\nbind = \"unused\"\npublic_url = \"{url}\"\n[identity]\npath = \"unused\"\n[store]\npath = \"unused\"\n[service]\n{extra}\n")).unwrap();
    let (agent, did_document) = server::build_agent(&config, Identity::generate().unwrap()).unwrap();
    let did = agent.did();
    let processor = Processor::new(Store::in_memory().unwrap(), config.service.clone(), &url);
    let state = Arc::new(AppState { agent, voice: Default::default(), processor, did_document, resolver: didcomm_quickstart::default_resolver(), admin: Default::default() });
    let app = server::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Running { did, url, state }
}

/// A PNG that says it is `width` by `height`, and is otherwise `salt`: enough for
/// the service to know what it is, and to tell one from another.
fn png(width: u32, height: u32, salt: u8) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend(b"\x00\x00\x00\x01IDAT");
    bytes.push(salt);
    bytes
}

fn data_url(bytes: &[u8]) -> String {
    format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Someone with an app, making packs: a workflow coordinator.
struct Maker {
    agent: Agent,
    service: String,
    keys: std::cell::Cell<u32>,
}

impl Maker {
    fn new(service: &Running) -> Self {
        Self { agent: Agent::new(Identity::generate().unwrap()).unwrap(), service: service.did.clone(), keys: std::cell::Cell::new(0) }
    }

    async fn ask(&self, message_type: &str, body: Value) -> Value {
        let message = json!({ "type": format!("{P}{message_type}"), "body": body, "return_route": "all" });
        // (`send`, not `request`: a refusal is an answer here, not an error.)
        self.agent.send(&self.service, &message).await.expect("sent").expect("an answer").message
    }

    async fn start(&self, template: &str, instance: &str) -> Value {
        self.ask("start", json!({ "template_id": template, "instance_id": instance })).await
    }

    /// Presses a button: each press has a key of its own.
    async fn advance(&self, instance: &str, event: &str, input: Value) -> Value {
        self.keys.set(self.keys.get() + 1);
        self.ask("advance", json!({ "instance_id": instance, "event": event, "idempotency_key": format!("press-{}", self.keys.get()), "input": input })).await
    }

    /// Makes and publishes a sticker pack of `count` stickers under `name`.
    async fn publish_pack(&self, instance: &str, name: &str, count: u8) {
        assert_eq!(kind(&self.start("new-sticker-pack", instance).await), "status");
        self.advance(instance, "set_title", json!({ "title": "Cats" })).await;
        for n in 0..count {
            let added = self.advance(instance, "add_sticker", json!({ "image": data_url(&png(512, 512, n)), "emoji": "😺" })).await;
            assert_eq!(kind(&added), "status", "{added}");
        }
        self.advance(instance, "publish", json!({})).await;
        self.advance(instance, "skip_icon", json!({})).await;
        let done = self.advance(instance, "set_name", json!({ "name": name })).await;
        assert_eq!(kind(&done), "complete", "{done}");
    }
}

fn kind(message: &Value) -> &str {
    message["type"].as_str().unwrap_or_default().rsplit('/').next().unwrap_or_default()
}

fn refused_with<'a>(message: &'a Value, code: &str) -> &'a str {
    assert_eq!(kind(message), "problem-report", "{message}");
    assert_eq!(message["body"]["code"], code, "{message}");
    message["body"]["comment"].as_str().unwrap()
}

/// Every `type` in a rendered screen, in order: what a coordinator would draw.
fn drawn(ui: &Value) -> Vec<String> {
    fn walk(item: &Value, out: &mut Vec<String>) {
        match item {
            Value::Array(items) => items.iter().for_each(|item| walk(item, out)),
            Value::Object(fields) => {
                if let Some(kind) = fields.get("type").and_then(Value::as_str) {
                    out.push(kind.to_string());
                }
                if let Some(children) = fields.get("children") {
                    walk(children, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(ui, &mut out);
    out
}

fn events(status: &Value) -> Vec<&str> {
    status["body"]["allowed_events"].as_array().unwrap().iter().filter_map(Value::as_str).collect()
}

async fn fetch(service: &Running, path: &str) -> reqwest::Response {
    reqwest::get(format!("{}{path}", service.url)).await.unwrap()
}

#[tokio::test]
async fn the_workflows_on_offer_can_be_found_and_fetched() {
    let service = start_service("").await;
    let alice = Maker::new(&service);

    let listed = alice.ask("discover", json!({ "include_hash": true })).await;
    assert_eq!(kind(&listed), "workflows");
    let workflows = listed["body"]["workflows"].as_array().unwrap();
    assert_eq!(workflows.iter().map(|w| w["template_id"].as_str().unwrap()).collect::<Vec<_>>(), ["new-sticker-pack", "new-emoji-pack", "edit-pack"]);
    assert_eq!(workflows[0]["versions"], json!(["1.0.0"]));
    assert_eq!(listed["body"]["paging"]["total"], 3);
    let narrowed = alice.ask("discover", json!({ "filters": { "text": "emoji" } })).await;
    assert_eq!(narrowed["body"]["workflows"].as_array().unwrap().len(), 1);

    let fetched = alice.ask("fetch-template", json!({ "template_id": "new-sticker-pack", "prefer_hash": workflows[0]["hash"] })).await;
    assert_eq!(kind(&fetched), "template");
    let template = &fetched["body"]["template"];
    assert_eq!(template["id"], "new-sticker-pack");
    assert_eq!(template["transitions"]["set_title"], json!({ "from": "title", "to": "stickers", "guard": null, "action": "set_title" }));
    assert_eq!(template["display_hints"]["ui_version"], "1.0");

    refused_with(&alice.ask("fetch-template", json!({ "template_id": "new-sticker-pack", "prefer_hash": "0000" })).await, "not_found_remote_template");
    refused_with(&alice.ask("fetch-template", json!({ "template_id": "make-coffee" })).await, "not_found_remote_template");
    refused_with(&alice.start("make-coffee", "i-0").await, "template_not_found");
    // The workflows here are the service's own.
    refused_with(&alice.ask("publish-template", json!({ "template": { "id": "x", "version": "1.0.0", "states": {}, "transitions": {} } })).await, "not_authorized");
}

#[tokio::test]
async fn a_maker_names_a_pack_adds_stickers_publishes_it_and_anyone_can_fetch_it() {
    let service = start_service("").await;
    let alice = Maker::new(&service);

    // "A new pack": it asks for a title, and nothing else can be done yet.
    let started = alice.start("new-sticker-pack", "i-1").await;
    assert_eq!(kind(&started), "status", "{started}");
    assert_eq!((started["body"]["instance_id"].as_str(), started["body"]["state"].as_str(), started["body"]["status"].as_str()), (Some("i-1"), Some("title"), Some("active")));
    assert_eq!(events(&started), ["set_title"]);
    assert_eq!(drawn(&started["body"]["ui"]), ["title", "text", "form", "text-input", "submit-button"]);
    refused_with(&alice.advance("i-1", "publish", json!({})).await, "guard_failed");
    assert!(refused_with(&alice.advance("i-1", "set_title", json!({ "title": "  " })).await, "action_failed").contains("1 to 64"));

    let named = alice.advance("i-1", "set_title", json!({ "title": "Cats being cats" })).await;
    assert_eq!(named["body"]["state"], "stickers");
    assert_eq!(named["body"]["ui"][0], json!({ "type": "title", "text": "Cats being cats" }));
    // Nothing in it yet: it can be added to, and not published.
    assert_eq!(events(&named), ["add_sticker", "remove_sticker"]);
    assert!(!drawn(&named["body"]["ui"]).contains(&"list".to_string()));
    assert!(refused_with(&alice.advance("i-1", "publish", json!({})).await, "guard_failed").contains("at least one"));

    // A sticker is a picture and the emoji it stands for. Each is checked.
    let wave = png(512, 512, 1);
    for (input, why) in [
        (json!({ "image": data_url(&wave), "emoji": "cat" }), "emoji"),
        (json!({ "image": data_url(&wave) }), "emoji"),
        (json!({ "image": data_url(&png(600, 512, 1)), "emoji": "😺" }), "600 by 512"),
        (json!({ "image": data_url(b"<svg/>"), "emoji": "😺" }), "PNG, GIF or WebP"),
        (json!({ "image": "https://example.com/cat.png", "emoji": "😺" }), "No picture"),
        (json!({ "emoji": "😺" }), "No picture"),
    ] {
        let comment = refused_with(&alice.advance("i-1", "add_sticker", input.clone()).await, "action_failed").to_string();
        assert!(comment.contains(why), "{input}: {comment}");
    }
    let added = alice.advance("i-1", "add_sticker", json!({ "image": data_url(&wave), "emoji": "😺" })).await;
    assert_eq!(kind(&added), "status", "{added}");
    assert_eq!(added["body"]["artifacts"]["count"], 1);
    assert_eq!(events(&added), ["add_sticker", "remove_sticker", "publish"]);
    // The same picture twice is one sticker.
    assert!(refused_with(&alice.advance("i-1", "add_sticker", json!({ "image": data_url(&wave), "emoji": "🐟" })).await, "action_failed").contains("already"));
    let second = alice.advance("i-1", "add_sticker", json!({ "image": data_url(&png(256, 512, 2)), "emoji": "🐟🎣" })).await;
    let listed = second["body"]["artifacts"]["stickers"].as_array().unwrap();
    assert_eq!(listed.iter().map(|s| s["emoji"].as_str().unwrap()).collect::<Vec<_>>(), ["😺", "🐟🎣"]);

    // What was added is shown back, from where only this workflow's id leads.
    let preview = listed[0]["url"].as_str().unwrap();
    assert!(preview.starts_with(&format!("{}drafts/i-1/", service.url)), "{preview}");
    let shown = reqwest::get(preview).await.unwrap();
    assert_eq!(shown.headers()["content-type"], "image/png");
    assert_eq!(shown.bytes().await.unwrap().to_vec(), wave);
    // Each has a row of its own, with a way to take it back out.
    let rows = second["body"]["ui"].as_array().unwrap().iter().find(|item| item["type"] == "list").unwrap()["children"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["children"][2], json!({ "type": "submit-button", "label": "Remove", "event": "remove_sticker", "input": { "sticker": listed[1]["id"] }, "col": 4 }));
    let fewer = alice.advance("i-1", "remove_sticker", json!({ "sticker": listed[1]["id"] })).await;
    assert_eq!(fewer["body"]["artifacts"]["count"], 1);
    alice.advance("i-1", "add_sticker", json!({ "image": data_url(&png(256, 512, 2)), "emoji": "🐟" })).await;

    // Publish, choose which stands for the pack, then the short name its address has.
    let icon = alice.advance("i-1", "publish", json!({})).await;
    assert_eq!((icon["body"]["state"].as_str(), events(&icon)), (Some("icon"), vec!["set_icon", "skip_icon"]));
    refused_with(&alice.advance("i-1", "set_icon", json!({ "sticker": "nope" })).await, "action_failed");
    let naming = alice.advance("i-1", "set_icon", json!({ "sticker": listed[1]["id"] })).await;
    assert_eq!(naming["body"]["state"], "name");
    assert!(naming["body"]["ui"].to_string().contains(&format!("{}<name>", service.url)));
    for bad in ["Ab", "-cats", "cats--two", "cats!", "admin"] {
        refused_with(&alice.advance("i-1", "set_name", json!({ "name": bad })).await, "action_failed");
    }
    assert_eq!(fetch(&service, "cats/pack.json").await.status(), 404);
    let done = alice.advance("i-1", "set_name", json!({ "name": "Cats" })).await;
    // Reaching a final state is told with `complete`, as the protocol has it.
    assert_eq!((kind(&done), done["body"]["instance_id"].as_str()), ("complete", Some("i-1")));
    let after = alice.ask("status", json!({ "instance_id": "i-1" })).await;
    assert_eq!((after["body"]["state"].as_str(), after["body"]["status"].as_str()), (Some("published"), Some("completed")));
    assert!(events(&after).is_empty());
    let link = format!("{}cats", service.url);
    assert_eq!(after["body"]["ui"][2], json!({ "type": "web-link", "label": link, "url": link }));
    // What it gathered is the pack's now, not a draft's.
    assert_eq!(reqwest::get(preview).await.unwrap().status(), 404);

    // Anyone can fetch it: the pack, then each picture, which is what its hash says.
    let served = fetch(&service, "cats/pack.json").await;
    assert_eq!(served.headers()["content-type"], "application/json");
    assert_eq!(served.headers()["access-control-allow-origin"], "*");
    let tag = served.headers()["etag"].to_str().unwrap().to_string();
    let pack: Value = served.json().await.unwrap();
    assert_eq!(
        (pack["type"].as_str(), pack["url"].as_str(), pack["name"].as_str(), pack["title"].as_str(), pack["kind"].as_str(), pack["reuse"].as_bool(), pack["version"].as_i64()),
        (Some("https://wyvrn.app/stickers/1.0/pack"), Some(link.as_str()), Some("cats"), Some("Cats being cats"), Some("sticker"), Some(true), Some(1))
    );
    assert_eq!(pack["icon"], listed[1]["id"]);
    let stickers = pack["stickers"].as_array().unwrap();
    assert_eq!(stickers.len(), 2);
    let first = &stickers[0];
    assert_eq!((first["media_type"].as_str(), first["width"].as_u64(), first["height"].as_u64(), first["animated"].as_bool(), first["emoji"].as_str()), (Some("image/png"), Some(512), Some(512), Some(false), Some("😺")));
    assert_eq!(first["bytes"].as_u64(), Some(wave.len() as u64));
    assert!(first.get("shortcode").is_none());
    let file = fetch(&service, &format!("cats/{}", first["file"].as_str().unwrap())).await;
    assert_eq!(file.headers()["content-type"], "image/png");
    assert!(file.headers()["cache-control"].to_str().unwrap().contains("immutable"));
    assert_eq!(file.headers()["x-content-type-options"], "nosniff");
    let bytes = file.bytes().await.unwrap();
    assert_eq!(bytes.to_vec(), wave);
    use sha2::Digest;
    let hash: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(first["sha256"], hash.as_str());
    assert!(first["file"].as_str().unwrap().starts_with(&hash[..32]));
    // The address alone is the pack too, and an unchanged one costs nothing to ask for again.
    assert_eq!(fetch(&service, "cats").await.json::<Value>().await.unwrap(), pack);
    let again = reqwest::Client::new().get(format!("{}cats/pack.json", service.url)).header("if-none-match", &tag).send().await.unwrap();
    assert_eq!(again.status(), 304);
    assert_eq!(fetch(&service, "cats/nothing.png").await.status(), 404);
    assert_eq!(fetch(&service, "dogs/pack.json").await.status(), 404);
}

#[tokio::test]
async fn a_workflow_is_its_makers_alone_and_one_of_each_at_a_time() {
    let service = start_service("").await;
    let (alice, mallory) = (Maker::new(&service), Maker::new(&service));
    alice.start("new-sticker-pack", "i-1").await;
    alice.advance("i-1", "set_title", json!({ "title": "Cats" })).await;

    // To anyone else, it isn't there.
    refused_with(&mallory.ask("status", json!({ "instance_id": "i-1" })).await, "instance_not_found");
    refused_with(&mallory.advance("i-1", "add_sticker", json!({ "image": data_url(&png(10, 10, 1)), "emoji": "😈" })).await, "instance_not_found");
    refused_with(&mallory.ask("cancel", json!({ "instance_id": "i-1" })).await, "instance_not_found");
    refused_with(&mallory.start("new-sticker-pack", "i-1").await, "multiplicity_violation");

    // Starting the same one again finds it where it was; starting another says which is under way.
    assert_eq!(alice.start("new-sticker-pack", "i-1").await["body"]["state"], "stickers");
    let second = alice.start("new-sticker-pack", "i-2").await;
    refused_with(&second, "multiplicity_violation");
    assert_eq!(second["body"]["args"]["instance_id"], "i-1");
    // A different workflow is a different matter.
    assert_eq!(kind(&alice.start("new-emoji-pack", "i-3").await), "status");

    // The same press arriving twice is done once.
    let press = json!({ "instance_id": "i-1", "event": "add_sticker", "idempotency_key": "tap-1", "input": { "image": data_url(&png(10, 10, 1)), "emoji": "😺" } });
    assert_eq!(alice.ask("advance", press.clone()).await["body"]["artifacts"]["count"], 1);
    let repeated = alice.ask("advance", press).await;
    assert_eq!((kind(&repeated), repeated["body"]["artifacts"]["count"].as_i64()), ("status", Some(1)));

    // Paused, it waits; cancelled, it is over and what it gathered is gone.
    assert_eq!(alice.ask("pause", json!({ "instance_id": "i-1" })).await["body"]["status"], "paused");
    refused_with(&alice.advance("i-1", "publish", json!({})).await, "guard_failed");
    assert_eq!(alice.ask("resume", json!({ "instance_id": "i-1" })).await["body"]["status"], "active");
    let preview = alice.ask("status", json!({ "instance_id": "i-1" })).await["body"]["artifacts"]["stickers"][0]["url"].as_str().unwrap().to_string();
    assert_eq!(reqwest::get(&preview).await.unwrap().status(), 200);
    let cancelled = alice.ask("cancel", json!({ "instance_id": "i-1" })).await;
    assert_eq!((cancelled["body"]["status"].as_str(), events(&cancelled).len()), (Some("canceled"), 0));
    assert_eq!(reqwest::get(&preview).await.unwrap().status(), 404);
    refused_with(&alice.advance("i-1", "publish", json!({})).await, "guard_failed");
    // ...and another can be started.
    assert_eq!(kind(&alice.start("new-sticker-pack", "i-2").await), "status");

    // One left alone long enough is forgotten.
    let now = wyvrn_service::now();
    assert_eq!(service.state.processor.store.prune(now - 60).unwrap(), 0);
    assert!(service.state.processor.store.prune(now + 60).unwrap() >= 2);
    refused_with(&alice.ask("status", json!({ "instance_id": "i-2" })).await, "instance_not_found");
}

#[tokio::test]
async fn a_pack_has_a_name_nobody_else_has_and_as_many_stickers_as_allowed() {
    let service = start_service("max_per_pack = 2\nmax_packs_per_maker = 1").await;
    let (alice, bob) = (Maker::new(&service), Maker::new(&service));
    alice.publish_pack("a-1", "cats", 2).await;

    // Bob's is full at two, and can't have Alice's name.
    bob.start("new-sticker-pack", "b-1").await;
    bob.advance("b-1", "set_title", json!({ "title": "Also cats" })).await;
    bob.advance("b-1", "add_sticker", json!({ "image": data_url(&png(10, 10, 1)), "emoji": "😺" })).await;
    let full = bob.advance("b-1", "add_sticker", json!({ "image": data_url(&png(10, 10, 2)), "emoji": "😺" })).await;
    assert_eq!(full["body"]["artifacts"]["full"], true);
    assert!(!drawn(&full["body"]["ui"]).contains(&"photo".to_string()));
    assert!(refused_with(&bob.advance("b-1", "add_sticker", json!({ "image": data_url(&png(10, 10, 3)), "emoji": "😺" })).await, "action_failed").contains("2 at most"));
    bob.advance("b-1", "publish", json!({})).await;
    bob.advance("b-1", "skip_icon", json!({})).await;
    assert!(refused_with(&bob.advance("b-1", "set_name", json!({ "name": "cats" })).await, "action_failed").contains("taken"));
    // He is still at that step, with everything he gathered, and tries another.
    assert_eq!(kind(&bob.advance("b-1", "set_name", json!({ "name": "more-cats", "private": true })).await), "complete");
    let pack: Value = fetch(&service, "more-cats/pack.json").await.json().await.unwrap();
    assert_eq!((pack["reuse"].as_bool(), pack["stickers"].as_array().unwrap().len()), (Some(false), 2));
    // Kept to himself, it names its maker in a way only someone with his DID can check.
    let link = pack["url"].as_str().unwrap();
    assert_eq!(pack["maker"].as_str(), Some(wyvrn_stickers::processor::maker_check(link, &bob.agent.did()).as_str()));
    assert_ne!(pack["maker"].as_str(), Some(wyvrn_stickers::processor::maker_check(link, &alice.agent.did()).as_str()));
    assert!(fetch(&service, "cats/pack.json").await.json::<Value>().await.unwrap().get("maker").is_none());
    // With no icon chosen, the pack names none: the first sticker stands for it.
    assert!(pack.get("icon").is_none());

    // One pack is all this service lets a maker have.
    alice.start("new-sticker-pack", "a-2").await;
    alice.advance("a-2", "set_title", json!({ "title": "Dogs" })).await;
    alice.advance("a-2", "add_sticker", json!({ "image": data_url(&png(10, 10, 9)), "emoji": "🐶" })).await;
    alice.advance("a-2", "publish", json!({})).await;
    alice.advance("a-2", "skip_icon", json!({})).await;
    assert!(refused_with(&alice.advance("a-2", "set_name", json!({ "name": "dogs" })).await, "action_failed").contains("as many as one maker may"));
}

#[tokio::test]
async fn custom_emoji_are_small_and_each_known_by_a_shortcode() {
    let service = start_service("").await;
    let alice = Maker::new(&service);
    let started = alice.start("new-emoji-pack", "e-1").await;
    assert_eq!(started["body"]["state"], "title");
    let named = alice.advance("e-1", "set_title", json!({ "title": "Party" })).await;
    assert_eq!(events(&named), ["add_emoji", "remove_sticker"]);
    assert!(named["body"]["ui"].to_string().contains("128 by 128"));
    assert!(named["body"]["ui"].to_string().contains("\"form-id\":\"shortcode\""));

    for (input, why) in [
        (json!({ "image": data_url(&png(64, 64, 1)), "shortcode": "Party Cat" }), "shortcode"),
        (json!({ "image": data_url(&png(64, 64, 1)) }), "shortcode"),
        (json!({ "image": data_url(&png(512, 512, 1)), "shortcode": "party_cat" }), "128 by 128"),
    ] {
        let comment = refused_with(&alice.advance("e-1", "add_emoji", input.clone()).await, "action_failed").to_string();
        assert!(comment.contains(why), "{input}: {comment}");
    }
    // A sticker pack's event is not this workflow's.
    refused_with(&alice.advance("e-1", "add_sticker", json!({ "image": data_url(&png(64, 64, 1)), "emoji": "🎉" })).await, "guard_failed");
    let added = alice.advance("e-1", "add_emoji", json!({ "image": data_url(&png(64, 64, 1)), "shortcode": "party_cat" })).await;
    assert_eq!(added["body"]["artifacts"]["stickers"][0]["label"], ":party_cat:");
    // A shortcode is one picture's, within a pack.
    assert!(refused_with(&alice.advance("e-1", "add_emoji", json!({ "image": data_url(&png(64, 64, 2)), "shortcode": "party_cat" })).await, "action_failed").contains(":party_cat:"));
    alice.advance("e-1", "add_emoji", json!({ "image": data_url(&png(64, 64, 2)), "shortcode": "blob_wave", "emoji": "👋" })).await;
    alice.advance("e-1", "publish", json!({})).await;
    alice.advance("e-1", "skip_icon", json!({})).await;
    assert_eq!(kind(&alice.advance("e-1", "set_name", json!({ "name": "party" })).await), "complete");

    let pack: Value = fetch(&service, "party/pack.json").await.json().await.unwrap();
    assert_eq!(pack["kind"], "emoji");
    let listed = pack["stickers"].as_array().unwrap();
    assert_eq!(listed.iter().map(|s| s["shortcode"].as_str().unwrap()).collect::<Vec<_>>(), ["party_cat", "blob_wave"]);
    // The emoji a custom one stands for is there only if it was given.
    assert!(listed[0].get("emoji").is_none());
    assert_eq!(listed[1]["emoji"], "👋");
}

#[tokio::test]
async fn a_maker_changes_and_deletes_their_own_packs_and_nobody_elses() {
    let service = start_service("").await;
    let (alice, mallory) = (Maker::new(&service), Maker::new(&service));
    alice.publish_pack("a-1", "cats", 2).await;
    let before: Value = fetch(&service, "cats/pack.json").await.json().await.unwrap();

    // Someone with no packs is shown that, and can't choose somebody else's.
    let none = mallory.start("edit-pack", "m-1").await;
    assert_eq!(none["body"]["state"], "choosing");
    assert!(drawn(&none["body"]["ui"]).contains(&"information".to_string()));
    assert!(refused_with(&mallory.advance("m-1", "choose", json!({ "pack": "cats" })).await, "action_failed").contains("isn't one of your packs"));
    // Naming it at the start gets no further.
    mallory.ask("cancel", json!({ "instance_id": "m-1" })).await;
    let sneaky = mallory.ask("start", json!({ "template_id": "edit-pack", "instance_id": "m-2", "context": { "pack": "cats" } })).await;
    assert_eq!(sneaky["body"]["state"], "choosing");
    assert_eq!(sneaky["body"]["artifacts"]["packs"], json!([]));

    // Alice is shown hers, and chooses one.
    let choosing = alice.start("edit-pack", "a-2").await;
    let packs = choosing["body"]["artifacts"]["packs"].as_array().unwrap();
    assert_eq!((packs.len(), packs[0]["name"].as_str(), packs[0]["summary"].as_str()), (1, Some("cats"), Some("cats · 2 stickers")));
    assert_eq!(packs[0]["icon"], format!("{}cats/{}", service.url, before["stickers"][0]["file"].as_str().unwrap()));
    let tile = choosing["body"]["ui"].as_array().unwrap().iter().find(|item| item["type"] == "list").unwrap()["children"][0].clone();
    assert_eq!((tile["type"].as_str(), tile["event"].as_str(), tile["input"]["pack"].as_str()), (Some("tile"), Some("choose"), Some("cats")));
    let editing = alice.advance("a-2", "choose", json!({ "pack": "cats" })).await;
    assert_eq!(editing["body"]["state"], "editing");
    // It is a sticker pack: stickers can be added to it, and custom emoji can't.
    assert!(events(&editing).contains(&"add_sticker") && !events(&editing).contains(&"add_emoji"));
    assert_eq!(editing["body"]["artifacts"]["stickers"][0]["url"], format!("{}cats/{}", service.url, before["stickers"][0]["file"].as_str().unwrap()));

    // Each change is the pack's at once, and counted.
    alice.advance("a-2", "add_sticker", json!({ "image": data_url(&png(300, 300, 7)), "emoji": "🐈" })).await;
    alice.advance("a-2", "set_title", json!({ "title": "Cats, mostly" })).await;
    alice.advance("a-2", "set_private", json!({ "private": true })).await;
    let third = fetch(&service, "cats/pack.json").await.json::<Value>().await.unwrap()["stickers"][2]["id"].as_str().unwrap().to_string();
    alice.advance("a-2", "set_icon", json!({ "sticker": third })).await;
    let changed: Value = fetch(&service, "cats/pack.json").await.json().await.unwrap();
    assert_eq!((changed["title"].as_str(), changed["reuse"].as_bool(), changed["stickers"].as_array().unwrap().len(), changed["icon"].as_str()), (Some("Cats, mostly"), Some(false), 3, Some(third.as_str())));
    assert_eq!(changed["version"], 5);
    // Removing the one that stood for the pack leaves it with none.
    alice.advance("a-2", "remove_sticker", json!({ "sticker": third })).await;
    let fewer: Value = fetch(&service, "cats/pack.json").await.json().await.unwrap();
    assert_eq!((fewer["stickers"].as_array().unwrap().len(), fewer.get("icon")), (2, None));
    assert_eq!(fetch(&service, &format!("cats/{third}.png")).await.status(), 404);

    // Deleting takes its name, typed, and is for everyone and for good.
    assert!(refused_with(&alice.advance("a-2", "delete_pack", json!({ "name": "dogs" })).await, "action_failed").contains("cats"));
    let file = format!("cats/{}", before["stickers"][0]["file"].as_str().unwrap());
    assert_eq!(fetch(&service, &file).await.status(), 200);
    assert_eq!(kind(&alice.advance("a-2", "delete_pack", json!({ "name": "cats" })).await), "complete");
    assert_eq!(fetch(&service, "cats/pack.json").await.status(), 410);
    assert_eq!(fetch(&service, &file).await.status(), 410);
    // Nobody can take the name and pass for what it was.
    mallory.start("new-sticker-pack", "m-3").await;
    mallory.advance("m-3", "set_title", json!({ "title": "Cats" })).await;
    mallory.advance("m-3", "add_sticker", json!({ "image": data_url(&png(10, 10, 1)), "emoji": "😈" })).await;
    mallory.advance("m-3", "publish", json!({})).await;
    mallory.advance("m-3", "skip_icon", json!({})).await;
    assert!(refused_with(&mallory.advance("m-3", "set_name", json!({ "name": "cats" })).await, "action_failed").contains("taken"));
    // And it is no longer among Alice's.
    assert_eq!(alice.start("edit-pack", "a-3").await["body"]["artifacts"]["packs"], json!([]));
}

#[tokio::test]
async fn a_message_nobody_vouches_for_is_turned_away() {
    let service = start_service("").await;
    let alice = Maker::new(&service);
    // Sealed to the service, and from nobody: there is no maker for a pack to be the pack of.
    let start = json!({ "id": "m-1", "type": format!("{P}start"), "body": { "template_id": "new-sticker-pack", "instance_id": "x" } });
    let anonymous = alice.agent.messaging().pack(&start, &service.did, None).await.unwrap().message;
    let response = reqwest::Client::new().post(&service.url).body(anonymous).send().await.unwrap();
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn a_pack_taken_down_is_gone_and_its_name_stays_taken() {
    let service = start_service("").await;
    let alice = Maker::new(&service);
    alice.publish_pack("a-1", "cats", 1).await;
    assert!(service.state.processor.store.remove_pack("cats", wyvrn_service::now()).unwrap());
    assert!(!service.state.processor.store.remove_pack("cats", wyvrn_service::now()).unwrap());
    assert_eq!(fetch(&service, "cats").await.status(), 410);
    assert_eq!(alice.start("edit-pack", "a-2").await["body"]["artifacts"]["packs"], json!([]));
}

/// A `from_prior` saying `prior` is now `new`, signed with `signer`'s key as `prior`'s.
async fn from_prior(signer: &Identity, prior: &str, new: &str) -> String {
    let key = AskarSigningKey::new(format!("{prior}#key-1"), signer.verification_key().clone());
    didcomm_core::rotation::build_from_prior(&AskarCryptoService, prior, new, &key, wyvrn_service::now()).await.unwrap()
}

#[tokio::test]
async fn a_maker_whose_did_changes_keeps_their_packs_and_what_they_were_doing() {
    let service = start_service("").await;
    let identity = Identity::generate().unwrap();
    let signer = Identity::from_json(&identity.to_json().unwrap()).unwrap();
    let before = Maker { agent: Agent::new(identity).unwrap(), service: service.did.clone(), keys: std::cell::Cell::new(0) };
    before.publish_pack("a-1", "cats", 1).await;
    before.start("new-sticker-pack", "a-2").await;
    before.advance("a-2", "set_title", json!({ "title": "Dogs" })).await;

    // A new device: the same person, writing from a new DID.
    let after = Maker::new(&service);
    // Someone else saying they are what Alice became signs with a key that isn't hers.
    let mallory = Maker::new(&service);
    let forged = from_prior(mallory.agent.identity(), &before.agent.did(), &mallory.agent.did()).await;
    let claimed = json!({ "type": format!("{P}status"), "from_prior": forged, "body": { "instance_id": "a-2" }, "return_route": "all" });
    refused_with(&mallory.agent.send(&service.did, &claimed).await.unwrap().unwrap().message, "instance_not_found");

    // Before saying so, the new DID is a stranger.
    refused_with(&after.ask("status", json!({ "instance_id": "a-2" })).await, "instance_not_found");
    let notice = from_prior(&signer, &before.agent.did(), &after.agent.did()).await;
    let told = json!({ "type": format!("{P}status"), "from_prior": notice, "body": { "instance_id": "a-2" }, "return_route": "all" });
    let status = after.agent.send(&service.did, &told).await.unwrap().unwrap().message;
    assert_eq!((kind(&status), status["body"]["state"].as_str()), ("status", Some("stickers")));

    // Their pack is theirs to change, and the DID left behind is nobody's.
    let mine = after.start("edit-pack", "a-3").await;
    assert_eq!(mine["body"]["artifacts"]["packs"][0]["name"], "cats");
    refused_with(&before.ask("status", json!({ "instance_id": "a-2" })).await, "instance_not_found");
    assert_eq!(before.start("edit-pack", "b-1").await["body"]["artifacts"]["packs"], json!([]));
}

#[tokio::test]
async fn an_admin_takes_a_pack_down_and_sets_names_aside() {
    let admin_agent = Agent::new(Identity::generate().unwrap()).unwrap();
    let service = start_service(&format!("admins = [\"{}\"]", admin_agent.did())).await;
    let admin = Maker { agent: admin_agent, service: service.did.clone(), keys: std::cell::Cell::new(0) };
    let alice = Maker::new(&service);
    let titles = |answer: &Value| answer["body"]["workflows"].as_array().unwrap().iter().map(|w| w["template_id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    // Only an admin is shown the admin pages, beside the service's own workflows.
    assert!(!titles(&alice.ask("discover", json!({})).await).iter().any(|id| id.starts_with("admin-")));
    let listed = titles(&admin.ask("discover", json!({})).await);
    assert!(listed.contains(&"new-sticker-pack".to_string()) && listed.contains(&"admin-stickers-packs".to_string()));
    assert_eq!(alice.start("admin-stickers-packs", "x").await["body"]["code"], "not_authorized");

    alice.publish_pack("p1", "cats", 1).await;
    let page = admin.start("admin-stickers-packs", "a1").await;
    assert!(page["body"]["ui"][1]["children"][0]["text"].as_str().unwrap().starts_with("Cats (cats)"));
    let down = admin.advance("a1", "take-down", json!({ "pack": "cats" })).await;
    assert_eq!(down["body"]["ui"][1]["text"], "Took cats down. Its name stays taken.");
    assert!(service.state.processor.store.pack("cats").unwrap().unwrap().removed);

    // A name set aside by an admin can't be taken.
    admin.start("admin-stickers-names", "a2").await;
    admin.advance("a2", "save", json!({ "names": "dogs\nadmin" })).await;
    assert_eq!(service.state.processor.reserved_names(), ["admin", "dogs"]);
    assert_eq!(kind(&alice.start("new-sticker-pack", "p2").await), "status");
    alice.advance("p2", "set_title", json!({ "title": "Dogs" })).await;
    alice.advance("p2", "add_sticker", json!({ "image": data_url(&png(512, 512, 9)), "emoji": "🐶" })).await;
    alice.advance("p2", "publish", json!({})).await;
    alice.advance("p2", "skip_icon", json!({})).await;
    let refused = alice.advance("p2", "set_name", json!({ "name": "dogs" })).await;
    assert!(refused["body"]["comment"].as_str().unwrap().contains("set aside"), "{refused}");
    admin.advance("a2", "reset", json!({})).await;
    assert!(service.state.processor.reserved_names().contains(&"healthz".to_string()));
}
