//! The community's signing secrets kept sealed (WYV-32), and opened at start-up
//! (WYV-111). A process of its own: the seal key is the process's.

use wyvrn_community::{community::Community, store::EventHook, store::Store};

fn hook(id: &str, secret: &str) -> EventHook {
    EventHook {
        id: id.into(),
        url: "https://example.com/events".into(),
        secret: secret.into(),
        events: vec!["post.created".into()],
        channel: None,
        created_by: "did:peer:2.maker".into(),
        created_at: 1,
        failures: 0,
        off_at: None,
    }
}

fn community(store: Store, did: &str) -> Community {
    let mut community = Community::new(store, Default::default(), None).unwrap();
    community.own = Some(did.into());
    community
}

#[test]
fn signing_secrets_are_sealed_to_the_community_and_plain_ones_are_adopted_only_when_the_operator_says() {
    wyvrn_service::identity::seal_with("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=").unwrap();
    let den = community(Store::in_memory().unwrap(), "did:web:c.example:den");

    // A new receiver's secret is kept sealed, and opens as itself.
    let kept = den.seal_hook_secret("h1", "whsec_abc").unwrap();
    assert!(!kept.contains("whsec_abc") && kept.contains("wyvrn-sealed-identity/1"));
    den.store.save_event_hook(&hook("h1", &kept)).unwrap();
    assert_eq!(den.hook_secret(&den.store.event_hook("h1").unwrap().unwrap()).unwrap(), "whsec_abc");
    // Copied into another community (a shared database's row moved), it doesn't open.
    let other = community(Store::in_memory().unwrap(), "did:web:c.example:other");
    assert!(other.hook_secret(&hook("h1", &kept)).is_err());
    // A follower's the same.
    let follower_kept = den.seal_follower_secret("f1", "whsec_f").unwrap();
    assert!(!follower_kept.contains("whsec_f"));

    // Kept plain from before the seal key: not taken, until the operator adopts them,
    // and then all of them at start-up, not only what that start happens to use (WYV-111).
    den.store.save_event_hook(&hook("h2", "whsec_old")).unwrap();
    den.store.set_setting("web.secret", "an old plain secret").unwrap();
    den.store.add_oidc_key("k1", "{\"kty\":\"EC\"}", 1).unwrap();
    assert!(den.hook_secret(&den.store.event_hook("h2").unwrap().unwrap()).is_err());
    assert!(wyvrn_community::web::settle_secret(&den.store, &den.sealing()).is_err());
    assert!(wyvrn_community::web::oidc::settle_keys(&den.store, &den.sealing()).is_err());
    std::env::set_var("WYVRN_SEAL_ADOPT_PLAIN", "true");
    den.settle_secrets().unwrap();
    wyvrn_community::web::settle_secret(&den.store, &den.sealing()).unwrap();
    wyvrn_community::web::oidc::settle_keys(&den.store, &den.sealing()).unwrap();
    std::env::remove_var("WYVRN_SEAL_ADOPT_PLAIN");
    let adopted = den.store.event_hook("h2").unwrap().unwrap();
    assert!(!adopted.secret.contains("whsec_old"));
    assert_eq!(den.hook_secret(&adopted).unwrap(), "whsec_old");
    assert!(!den.store.setting("web.secret").unwrap().unwrap().contains("an old plain secret"));
    assert!(!den.store.oidc_keys().unwrap()[0].1.contains("\"kty\""));
    wyvrn_community::web::settle_secret(&den.store, &den.sealing()).unwrap();
    wyvrn_community::web::oidc::settle_keys(&den.store, &den.sealing()).unwrap();

    // Sealed under the community's DID, as before WYV-124: still opens, and is
    // sealed again under its keys, so a move to another DID keeps it.
    let identity = didcomm_agent::Identity::generate().unwrap();
    let mut keyed = community(Store::in_memory().unwrap(), "did:web:c.example:keyed");
    let by_did = keyed.seal_hook_secret("h4", "whsec_moved").unwrap();
    keyed.store.save_event_hook(&hook("h4", &by_did)).unwrap();
    keyed.store.set_setting("web.secret", &wyvrn_service::identity::seal_text("the web secret", "did:web:c.example:keyed#web-secret").unwrap()).unwrap();
    keyed.store.add_oidc_key("k2", &wyvrn_service::identity::seal_text("{\"kty\":\"OKP\"}", "did:web:c.example:keyed#oidc-k2").unwrap(), 1).unwrap();
    keyed.sealed_to = Some(wyvrn_community::sealing::bound_to(&identity));
    keyed.settle_secrets().unwrap();
    wyvrn_community::web::settle_secret(&keyed.store, &keyed.sealing()).unwrap();
    wyvrn_community::web::oidc::settle_keys(&keyed.store, &keyed.sealing()).unwrap();
    assert_ne!(keyed.store.event_hook("h4").unwrap().unwrap().secret, by_did);
    // Moved (its did:web is another now, its keys the same): everything opens.
    keyed.own = Some("did:web:elsewhere.example".into());
    assert_eq!(keyed.hook_secret(&keyed.store.event_hook("h4").unwrap().unwrap()).unwrap(), "whsec_moved");
    assert_eq!(wyvrn_community::web::secret(&keyed.store, &keyed.sealing()).unwrap(), "the web secret");
    wyvrn_community::web::oidc::settle_keys(&keyed.store, &keyed.sealing()).unwrap();
    // Another community, with keys of its own, still opens none of it.
    let mut stranger = community(Store::in_memory().unwrap(), "did:web:elsewhere.example");
    stranger.sealed_to = Some(wyvrn_community::sealing::bound_to(&didcomm_agent::Identity::generate().unwrap()));
    assert!(stranger.hook_secret(&keyed.store.event_hook("h4").unwrap().unwrap()).is_err());
    // Taken to another server (community-host's export), they go unsealed, as its
    // keys do: that server hasn't this one's seal key.
    wyvrn_community::sealing::unseal_all(&keyed.store, &keyed.sealing()).unwrap();
    assert_eq!(keyed.store.event_hook("h4").unwrap().unwrap().secret, "whsec_moved");
    assert_eq!(keyed.store.setting("web.secret").unwrap().unwrap(), "the web secret");
    assert_eq!(keyed.store.oidc_keys().unwrap()[0].1, "{\"kty\":\"OKP\"}");

    // On MySQL, a store from before (secrets in VARCHAR(128)) is widened to keep them.
    let Some(url) = std::env::var("WYVRN_TEST_DATABASE_URL").ok().filter(|url| !url.is_empty()) else { return };
    let server = url.trim_end_matches('/');
    let database = format!("wyvrn_test_sealed_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    {
        let mut admin = mysql::Conn::new(mysql::Opts::from_url(server).unwrap()).unwrap();
        mysql::prelude::Queryable::query_drop(&mut admin, format!("CREATE DATABASE {database}")).unwrap();
        mysql::prelude::Queryable::query_drop(
            &mut admin,
            format!("CREATE TABLE {database}.event_hooks (id VARCHAR(64) NOT NULL PRIMARY KEY, url TEXT NOT NULL, secret VARCHAR(128) NOT NULL, events TEXT NOT NULL, channel VARCHAR(255), created_by VARCHAR(3000) NOT NULL, created_at BIGINT NOT NULL, failures BIGINT NOT NULL DEFAULT 0, off_at BIGINT)"),
        )
        .unwrap();
    }
    let kept = community(Store::open(&wyvrn_service::db::Location::MySql(format!("{server}/{database}"))).unwrap(), "did:web:c.example:den");
    let sealed = kept.seal_hook_secret("h3", &format!("whsec_{}", "x".repeat(43))).unwrap();
    assert!(sealed.len() > 128);
    kept.store.save_event_hook(&hook("h3", &sealed)).unwrap();
    assert_eq!(kept.hook_secret(&kept.store.event_hook("h3").unwrap().unwrap()).unwrap(), format!("whsec_{}", "x".repeat(43)));
    // And opened again (the widening said again at each start) changes nothing.
    let again = Store::open(&wyvrn_service::db::Location::MySql(format!("{server}/{database}"))).unwrap();
    assert!(again.event_hook("h3").unwrap().is_some());
}
