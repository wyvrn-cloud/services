//! Discord's roles and member overrides kept, a claim giving them, and a bot's own
//! role, on a store as each database keeps it: SQLite always, and MySQL where
//! WYVRN_TEST_DATABASE_URL names a server (`mysql://user:password@host:port`).
use serde_json::json;
use wyvrn_community::discord_roles::{self, MemberOverride};
use wyvrn_community::roles::EVERYONE_ROLE;
use wyvrn_community::store::{Role, RoleDef, Store};

fn role(id: &str, name: &str, position: i64) -> RoleDef {
    RoleDef { id: id.into(), name: name.into(), color: None, icon: None, position, permissions: Vec::new(), mentionable: false, hoist: false, self_assignable: false }
}

fn exercise(store: &Store) {
    // A DID as long as a did:peer:4 in full.
    let ann = format!("did:peer:4zQm{}", "a".repeat(1400));
    let bot = format!("did:peer:4zQm{}", "b".repeat(1400));
    store.save_role(&role(EVERYONE_ROLE, "everyone", 0)).unwrap();
    store.save_role(&role("art", "Artists", 1)).unwrap();
    store.add_channel("fan", "fanart", "", "did:example:owner", 1).unwrap();
    store.set_member(&ann, Role::Member, 1).unwrap();
    let members = vec![json!({ "user": { "id": "111", "username": "ann" }, "roles": ["71"] }), json!({ "user": { "id": "900", "username": "dice", "bot": true }, "roles": ["72"] })];
    let roles = vec![json!({ "id": "71", "name": "Artists" }), json!({ "id": "72", "name": "Dice", "managed": true, "tags": { "bot_id": "900" } })];
    let channels = vec![json!({ "id": "21", "guild_id": "1", "type": 0, "name": "fanart", "permission_overwrites": [{ "id": "111", "type": 1, "allow": "0", "deny": "2048" }] })];
    for _ in 0..2 {
        let report = discord_roles::import(store, &members, &roles, &channels, false, 2).unwrap();
        assert_eq!((report.accounts, report.overrides_kept), (2, 1));
    }
    let author = "did:wyvrn:tmp:discord:111";
    assert_eq!(store.member_overrides("fan").unwrap().len(), 1);
    let given = store.give_discord_account("111", &ann, None, None, 3).unwrap().unwrap();
    assert_eq!((given.roles.clone(), given.overrides), (vec!["art".to_string()], 1));
    assert!(store.member_overrides_of(author).unwrap().is_empty());
    assert_eq!(store.member_overrides("fan").unwrap(), vec![MemberOverride { channel: "fan".into(), did: ann.clone(), allow: vec![], deny: vec!["post".into()] }]);
    // A bot, approved, has a role of its own; moving DID, it keeps it; gone, it goes.
    store.set_member(&bot, Role::Member, 1).unwrap();
    store.set_bot(&bot, Some(&json!([])), 1).unwrap();
    store.set_bot_approved(&bot, Some(&ann), 1).unwrap();
    assert!(store.settle_bot_role(&bot).unwrap());
    let own = store.bot_role_of(&bot).unwrap().unwrap();
    let moved = format!("{bot}c");
    store.rotate(&bot, &moved, 4).unwrap();
    assert_eq!(store.bot_role_of(&moved).unwrap(), Some(own.clone()));
    store.remove_member(&moved).unwrap();
    assert!(store.role(&own).unwrap().is_none());
    store.rotate(&ann, &format!("{ann}d"), 5).unwrap();
    assert_eq!(store.member_overrides("fan").unwrap()[0].did, format!("{ann}d"));
}

#[test]
fn discords_roles_are_kept_and_given_on_sqlite() {
    exercise(&Store::in_memory().unwrap());
}

#[test]
fn discords_roles_are_kept_and_given_on_mysql() {
    let Some(url) = std::env::var("WYVRN_TEST_DATABASE_URL").ok().filter(|url| !url.is_empty()) else { return };
    let server = url.trim_end_matches('/');
    let database = format!("wyvrn_test_discord_roles_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    {
        let mut admin = mysql::Conn::new(mysql::Opts::from_url(server).unwrap()).unwrap();
        mysql::prelude::Queryable::query_drop(&mut admin, format!("CREATE DATABASE {database}")).unwrap();
    }
    let store = Store::open(&wyvrn_service::db::Location::MySql(format!("{server}/{database}"))).unwrap();
    exercise(&store);
    // Opened again, the schema said again changes nothing.
    let again = Store::open(&wyvrn_service::db::Location::MySql(format!("{server}/{database}"))).unwrap();
    assert_eq!(again.discord_account("111").unwrap().unwrap().roles, vec!["art".to_string()]);
}
