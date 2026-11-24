//! community/1.0's member list for a community of thousands ("A community of
//! thousands"): `members-request`'s `brief`, pages, `query`, `ids`, `bots`,
//! `address`, `did` and `ids_only`, `member-request`, and `"answer": "member"`.
//! Asked of the community as the server asks it, and answered as the asker is
//! sent it (member ids for DIDs).

use serde_json::{json, Value};
use wyvrn_community::{
    community::Community,
    member_ids::MemberProfile,
    store::{Role, Store},
};

const C: &str = "https://wyvrn.app/community/1.0/";

fn community() -> Community {
    let config: wyvrn_community::config::Config = toml::from_str(
        "[server]\nbind = \"unused\"\npublic_url = \"https://community.example/\"\n[identity]\npath = \"unused\"\n[store]\npath = \"unused\"\n[community]\nname = \"Pages\"\n",
    )
    .unwrap();
    let mut community = Community::new(Store::in_memory().unwrap(), config.community.clone(), Some(wyvrn_service::profile::Kept::scratch().unwrap())).unwrap();
    community.public_url = "https://community.example/".into();
    community
}

fn did(n: usize) -> String {
    format!("did:example:member{n:05}")
}

/// `n` members, the first an admin: every third online, every tenth a
/// moderator, each with a name, picture and a linked account.
fn filled(n: usize) -> Community {
    let community = community();
    for i in 0..n {
        let did = did(i);
        let store = &community.store;
        store.set_member(&did, if i == 0 { Role::Admin } else { Role::Member }, 1).unwrap();
        store.set_joined(&did, true).unwrap();
        store.set_name(&did, Some(&format!("Person {i:05}"))).unwrap();
        store.set_member_picture(&did, Some(("image/png", &format!("cGljdHVyZQ{i}"))), 1).unwrap();
        store
            .set_member_profile(
                &did,
                &MemberProfile { description: Some("About me".into()), linked: Some(json!([{ "credentialSubject": { "account": format!("person{i}") } }])), at: 1, ..Default::default() },
            )
            .unwrap();
        if i % 10 == 5 {
            store.give_role(&did, "moderator").unwrap();
        }
        if i % 3 == 0 {
            store.set_presence(&did, true, "online").unwrap();
            community.live.write().unwrap().insert(did.clone(), 1);
        }
    }
    community
}

/// What `asker` is sent in answer to `kind` with `body`.
fn ask(community: &Community, asker: &str, kind: &str, body: Value) -> Value {
    let message = json!({ "id": uuid::Uuid::new_v4().to_string(), "type": format!("{C}{kind}"), "body": body });
    let reply = community.handle(asker, &message, 10).unwrap().reply.expect("an answer");
    community.masked_for(asker, &reply).unwrap()
}

fn kind(message: &Value) -> &str {
    message["type"].as_str().unwrap_or_default().rsplit('/').next().unwrap_or_default()
}

fn ids(page: &Value) -> Vec<String> {
    page["body"]["members"].as_array().unwrap().iter().map(|entry| entry["member"].as_str().unwrap().to_string()).collect()
}

#[test]
fn a_brief_list_comes_a_page_at_a_time_in_the_order_it_is_drawn_and_each_member_once() {
    let community = filled(60);
    let asker = did(1);
    let mut seen = Vec::new();
    let mut groups = Vec::new();
    let mut after: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut body = json!({ "brief": true, "limit": 7 });
        if let Some(after) = &after {
            body["after"] = json!(after);
        }
        let page = ask(&community, &asker, "members-request", body);
        assert_eq!(kind(&page), "members");
        assert_eq!(page["body"]["counts"]["total"], 60);
        assert_eq!(page["body"]["counts"]["online"], 20);
        // The admin (0) under Admin, online moderators (15, 45: every tenth from 5
        // that is a third) under Moderator.
        assert_eq!(page["body"]["counts"]["roles"], json!({ "admin": 1, "moderator": 2 }));
        assert!(page["body"].get("waiting").is_none());
        for entry in page["body"]["members"].as_array().unwrap() {
            assert!(entry.get("did").is_none(), "a member not told DIDs isn't: {entry}");
            assert!(entry["profile"].get("linkedAccounts").is_none() && entry["profile"].get("description").is_none(), "brief: {entry}");
            assert!(entry["profile"]["picture"].as_str().unwrap().starts_with("https://community.example/m/"));
            let roles = entry["roles"].as_array().unwrap();
            let moderator = roles.contains(&json!("moderator")) || roles.contains(&json!("admin"));
            groups.push(match (entry.get("presence").is_some(), moderator) {
                (true, true) => 0,
                (true, false) => 1,
                (false, _) => 2,
            });
        }
        seen.extend(ids(&page));
        pages += 1;
        match page["body"]["next"].as_str() {
            Some(next) => after = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(pages, 9);
    assert_eq!(seen.len(), 60);
    assert_eq!(seen.iter().collect::<std::collections::HashSet<_>>().len(), 60, "nobody twice");
    assert!(groups.windows(2).all(|pair| pair[0] <= pair[1]), "online moderators, online, offline: {groups:?}");
}

#[test]
fn a_page_after_someone_came_online_starts_where_the_last_ended() {
    let community = filled(30);
    let asker = did(1);
    let first = ask(&community, &asker, "members-request", json!({ "brief": true, "limit": 5 }));
    // Someone listed later (offline) comes online: they move up, ahead of the page.
    let late = did(29);
    community.store.set_presence(&late, true, "online").unwrap();
    community.live.write().unwrap().insert(late.clone(), 1);
    let second = ask(&community, &asker, "members-request", json!({ "brief": true, "limit": 5, "after": first["body"]["next"] }));
    let (first, second) = (ids(&first), ids(&second));
    assert!(first.iter().all(|id| !second.contains(id)), "nobody listed twice");
}

#[test]
fn without_any_of_it_the_list_is_whole_as_before() {
    let community = filled(12);
    let admin = did(0);
    let whole = ask(&community, &admin, "members-request", json!({}));
    let members = whole["body"]["members"].as_array().unwrap();
    assert_eq!(members.len(), 12);
    assert!(whole["body"].get("counts").is_none() && whole["body"].get("next").is_none());
    assert!(whole["body"]["waiting"].is_array(), "the whole list has who is waiting, for an admin");
    let one = &members[3];
    assert!(one["did"].as_str().unwrap().starts_with("did:example:"), "an admin is told DIDs");
    assert_eq!(one["profile"]["description"], "About me");
    assert!(one["profile"]["linkedAccounts"].is_array());
    assert_eq!(one["joined"], true);
    // A brief entry is the whole one, with less in it.
    let brief = ask(&community, &admin, "members-request", json!({ "brief": true }));
    assert!(brief["body"].get("counts").is_none(), "brief alone is no page");
    for entry in brief["body"]["members"].as_array().unwrap() {
        let whole = members.iter().find(|m| m["member"] == entry["member"]).unwrap();
        for (key, value) in entry.as_object().unwrap() {
            if key == "profile" || key == "here" {
                for (inner, said) in value.as_object().unwrap() {
                    assert_eq!(&whole[key][inner], said, "{key}.{inner}");
                }
            } else {
                assert_eq!(&whole[key], value, "{key}");
            }
        }
    }
    // As a page: who is waiting comes with the first.
    let page = ask(&community, &admin, "members-request", json!({ "brief": true, "limit": 5 }));
    assert!(page["body"]["waiting"].is_array());
    let next = ask(&community, &admin, "members-request", json!({ "brief": true, "limit": 5, "after": page["body"]["next"] }));
    assert!(next["body"].get("waiting").is_none());
}

#[test]
fn a_query_finds_those_whose_name_or_a_word_of_it_starts_so() {
    let community = filled(40);
    community.store.set_name(&did(7), Some("Zoë Example")).unwrap();
    let asker = did(1);
    let found = ask(&community, &asker, "members-request", json!({ "brief": true, "query": "ZO" }));
    assert_eq!(found["body"]["members"].as_array().unwrap().len(), 1);
    assert_eq!(found["body"]["members"][0]["name"], "Zoë Example");
    let found = ask(&community, &asker, "members-request", json!({ "brief": true, "query": "exam" }));
    assert_eq!(found["body"]["members"].as_array().unwrap().len(), 1, "a word of it");
    let many = ask(&community, &asker, "members-request", json!({ "brief": true, "query": "person" }));
    assert_eq!(many["body"]["counts"]["total"], 39);
    assert_eq!(many["body"]["members"].as_array().unwrap().len(), 20, "a page of 20 with no limit");
    assert!(many["body"]["next"].is_string());
}

#[test]
fn ids_bots_and_an_address_narrow_it_and_a_did_only_for_one_told_dids() {
    let community = filled(20);
    let asker = did(1);
    let admin = did(0);
    let id = |n: usize| community.store.member_id(&did(n), 1).unwrap();
    let wanted = json!([id(4), id(9), "mnobody", did(12)]);
    let found = ask(&community, &asker, "members-request", json!({ "brief": true, "ids": wanted }));
    let mut got = ids(&found);
    got.sort();
    let mut expected = vec![id(4), id(9)];
    expected.sort();
    assert_eq!(got, expected, "a DID tells a member not told DIDs nothing");
    let found = ask(&community, &admin, "members-request", json!({ "brief": true, "ids": wanted }));
    assert_eq!(ids(&found).len(), 3, "an admin may name members by DID");

    let address = "did:webvh:QmExample:people.example";
    community.store.set_member_profile(&did(6), &MemberProfile { public_address: Some(address.into()), at: 2, ..Default::default() }).unwrap();
    let found = ask(&community, &asker, "members-request", json!({ "brief": true, "address": address }));
    assert_eq!(ids(&found), vec![id(6)]);
    assert!(ids(&ask(&community, &asker, "members-request", json!({ "brief": true, "address": "did:webvh:nobody" }))).is_empty());

    assert!(ids(&ask(&community, &asker, "members-request", json!({ "brief": true, "did": did(6) }))).is_empty(), "as if nobody were at it");
    assert_eq!(ids(&ask(&community, &admin, "members-request", json!({ "brief": true, "did": did(6) }))), vec![id(6)]);

    assert!(ids(&ask(&community, &asker, "members-request", json!({ "brief": true, "bots": true }))).is_empty());
}

#[test]
fn ids_only_says_who_sees_a_channel_by_member_id_alone() {
    let community = filled(15);
    let admin = did(0);
    let made = ask(&community, &admin, "create-channel", json!({ "name": "general" }));
    let channel = made["body"]["created"].as_str().unwrap().to_string();
    let answer = ask(&community, &did(2), "members-request", json!({ "channel": channel, "ids_only": true }));
    assert_eq!(answer["body"]["channel"], channel);
    assert!(answer["body"].get("members").is_none());
    let listed = answer["body"]["ids"].as_array().unwrap();
    assert_eq!(listed.len(), 15);
    assert!(listed.iter().all(|id| id.as_str().unwrap().starts_with('m')));
}

#[test]
fn member_request_gives_one_whole_entry_as_the_list_does() {
    let community = filled(10);
    let asker = did(2);
    let whole = ask(&community, &asker, "members-request", json!({}));
    let id = community.store.member_id(&did(4), 1).unwrap();
    let listed = whole["body"]["members"].as_array().unwrap().iter().find(|entry| entry["member"] == id.as_str()).unwrap().clone();
    // (The server is handed DIDs: server.rs turns member ids into them first.)
    let one = ask(&community, &asker, "member-request", json!({ "member": did(4) }));
    assert_eq!(kind(&one), "member");
    assert_eq!(one["body"]["member"], listed);
    let none = ask(&community, &asker, "member-request", json!({ "member": "did:example:nobody" }));
    assert_eq!(none["body"]["code"], "e.p.msg.invalid-member");
}

#[test]
fn a_change_may_be_answered_with_the_member_it_changed() {
    let community = filled(8);
    let me = did(3);
    let old = ask(&community, &me, "set-member-profile", json!({ "nickname": "Trey" }));
    assert_eq!(kind(&old), "members", "as an app written before pages reads it");
    let new = ask(&community, &me, "set-member-profile", json!({ "nickname": "Tres", "answer": "member" }));
    assert_eq!(kind(&new), "member");
    assert_eq!(new["body"]["member"]["name"], "Tres");
    let admin = did(0);
    let taken_out = ask(&community, &admin, "set-member", json!({ "did": did(5), "role": null, "answer": "member" }));
    assert_eq!(kind(&taken_out), "member");
    assert_eq!(taken_out["body"]["member"]["gone"], true);
    assert!(taken_out["body"]["member"]["member"].as_str().unwrap().starts_with('m'));
}

#[test]
fn a_page_of_whole_entries_ends_before_it_is_too_big() {
    let community = filled(30);
    let big = json!([{ "proof": "x".repeat(60 * 1024) }]);
    for i in 0..30 {
        community.store.set_member_profile(&did(i), &MemberProfile { linked: Some(big.clone()), at: 3, ..Default::default() }).unwrap();
    }
    let page = ask(&community, &did(1), "members-request", json!({ "limit": 200 }));
    let listed = page["body"]["members"].as_array().unwrap().len();
    assert!(listed < 25, "{listed}");
    assert!(page.to_string().len() < wyvrn_community::roster::PAGE_BYTES + 64 * 1024, "{}", page.to_string().len());
    assert!(page["body"]["next"].is_string());
}
