//! community/1.0's member list for a community of thousands ("A community of
//! thousands"): `members-request`'s `brief`, pages, `query`, `ids`, `bots`,
//! `address`, `did` and `ids_only`, `member-request`, and `"answer": "member"`.
//! Asked of the community as the server asks it, and answered as the asker is
//! sent it (member ids for DIDs).

use serde_json::{json, Value};
use wyvrn_community::{
    community::Community,
    linked_summary::Kept,
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
        store
            .set_member_linked(&did, &Kept { summaries: Some(json!([{ "platform": "bluesky", "homepage": "https://bsky.app", "account": format!("did:plc:{i}"), "handle": format!("person{i}.example"), "checkedAt": 1 }])), statement: None }, 1)
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
    // The list carries what the server found of linked accounts, never the
    // credentials: those are one member's whole entry's (member-request).
    assert!(one["profile"].get("linkedAccounts").is_none(), "{one}");
    assert_eq!(one["profile"]["linkedAccountSummaries"][0]["handle"], "person3.example");
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
    // The same, with the credentials themselves beside their summaries.
    let mut whole_one = one["body"]["member"].clone();
    assert_eq!(whole_one["profile"]["linkedAccounts"], json!([{ "credentialSubject": { "account": "person4" } }]));
    whole_one["profile"].as_object_mut().unwrap().remove("linkedAccounts");
    assert_eq!(whole_one, listed);
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
    let big = json!([{ "handle": "x".repeat(60 * 1024) }]);
    for i in 0..30 {
        community.store.set_member_linked(&did(i), &Kept { summaries: Some(big.clone()), statement: None }, 3).unwrap();
    }
    let page = ask(&community, &did(1), "members-request", json!({ "limit": 200 }));
    let listed = page["body"]["members"].as_array().unwrap().len();
    assert!(listed < 25, "{listed}");
    assert!(page.to_string().len() < wyvrn_community::roster::PAGE_BYTES + 64 * 1024, "{}", page.to_string().len());
    assert!(page["body"]["next"].is_string());
}

/// A profile sent to the community, as server.rs passes it on once it has seen
/// the public address is the sender's.
fn send_profile(community: &Community, sender: &str, profile: Value, attachments: Value) {
    let message = json!({ "id": uuid::Uuid::new_v4().to_string(), "type": wyvrn_community::community::PROFILE, "body": { "profile": profile }, "attachments": attachments });
    community.handle(sender, &message, 20).unwrap();
}

fn webp(fill: u8) -> Vec<u8> {
    let mut bytes = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
    bytes.extend(std::iter::repeat_n(fill, 1500));
    bytes
}

/// A credential referring to the picture `bytes`, as wyvrn-verify issues them.
fn referring(n: usize, bytes: &[u8]) -> Value {
    use wyvrn_hosted_did::linked::{digest_name, digest_sri, sha384};
    let url = format!("https://verify.example/pictures/{}.webp", digest_name(&sha384(bytes)));
    json!({
        "id": format!("urn:uuid:{n}"),
        "type": ["VerifiableCredential", "LinkedAccountCredential"],
        "issuer": "did:web:verify.example",
        "credentialSubject": { "id": "did:webvh:Qm:id.example:alice", "account": { "type": "OnlineAccount", "accountServiceHomepage": "https://bsky.app", "accountName": format!("did:plc:{n}"), "wyvrn": { "handle": format!("a{n}.example"), "image": url } } },
        "relatedResource": [{ "id": url, "mediaType": "image/webp", "digestSRI": digest_sri(bytes) }],
    })
}

#[test]
fn a_profiles_linked_accounts_come_with_their_pictures_twelve_at_most() {
    use wyvrn_hosted_did::linked::{digest_name, sha384};
    let community = filled(3);
    let alice = did(1);
    let address = "did:webvh:Qm:id.example:alice";
    let attach = |bytes: &[u8]| json!({ "id": digest_name(&sha384(bytes)), "media_type": "image/webp", "data": { "base64": base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes) } });
    let twelve: Vec<Value> = (0..12).map(|n| referring(n, &webp(n as u8))).collect();
    // One picture is sent as it is; another isn't what its credential names.
    send_profile(&community, &alice, json!({ "publicAddress": address, "linkedAccounts": twelve }), json!([attach(&webp(0)), { "id": "x", "data": { "base64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, webp(99)) } }]));
    let kept = community.store.member_profile(&alice).unwrap().unwrap();
    assert_eq!(kept.linked.unwrap().as_array().unwrap().len(), 12);
    assert_eq!(community.store.member_linked_picture_names(&alice).unwrap(), vec![digest_name(&sha384(&webp(0)))]);
    // Served under its digest, as members' pictures are.
    let member = community.store.member_id(&alice, 1).unwrap();
    let (media_type, data) = community.picture_at(&member, "linked", &digest_name(&sha384(&webp(0)))).unwrap().unwrap();
    assert_eq!(media_type, "image/webp");
    assert_eq!(base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data).unwrap(), webp(0));
    assert_eq!(community.picture_at(&member, "linked", &digest_name(&sha384(&webp(99)))).unwrap(), None);
    // A thirteenth is one more than a member shows: the profile's accounts are
    // left as they were.
    let thirteen: Vec<Value> = (0..13).map(|n| referring(n, &webp(n as u8))).collect();
    send_profile(&community, &alice, json!({ "publicAddress": address, "linkedAccounts": thirteen }), json!([]));
    assert_eq!(community.store.member_profile(&alice).unwrap().unwrap().linked.unwrap().as_array().unwrap().len(), 12);
    // None shown: no pictures either.
    send_profile(&community, &alice, json!({ "publicAddress": address, "linkedAccounts": [] }), json!([]));
    assert!(community.store.member_linked_picture_names(&alice).unwrap().is_empty());
}

#[test]
fn a_members_statement_is_kept_for_their_address_and_member_id_and_shown_beside_the_summaries() {
    let mut community = filled(3);
    community.own = Some("did:web:community.example".into());
    let alice = did(1);
    let address = "did:webvh:Qm:id.example:alice";
    let member = community.store.member_id(&alice, 1).unwrap();
    let statement = |holder: &str, domain: &str, challenge: &str| {
        json!({ "@context": ["https://www.w3.org/ns/credentials/v2"], "type": ["VerifiablePresentation"], "holder": holder, "proof": { "type": "DataIntegrityProof", "cryptosuite": "eddsa-jcs-2022", "proofPurpose": "authentication", "challenge": challenge, "domain": domain, "proofValue": "z1" } })
    };
    // For another member, another community or another address: not kept.
    for wrong in [statement(address, "did:web:community.example", "m-other"), statement(address, "did:web:elsewhere.example", &member), statement("did:webvh:Qm:id.example:bob", "did:web:community.example", &member)] {
        send_profile(&community, &alice, json!({ "publicAddress": address, "memberStatement": wrong }), json!([]));
        assert_eq!(community.store.member_linked(&alice).unwrap().statement, None);
    }
    let good = statement(address, "did:web:community.example", &member);
    send_profile(&community, &alice, json!({ "publicAddress": address, "memberStatement": good }), json!([]));
    assert_eq!(community.store.member_linked(&alice).unwrap().statement, Some(good.clone()));
    // In their entry, as signed (its holder no member id), beside the summaries;
    // in a brief one, neither.
    let entry = ask(&community, &did(2), "member-request", json!({ "member": alice }))["body"]["member"].clone();
    assert_eq!(entry["profile"]["memberStatement"], good);
    assert_eq!(entry["member"], member.as_str());
    let brief = ask(&community, &did(2), "members-request", json!({ "brief": true, "ids": [member] }));
    let brief = &brief["body"]["members"][0]["profile"];
    assert!(brief.get("memberStatement").is_none() && brief.get("linkedAccountSummaries").is_none(), "{brief}");
    // A profile saying nothing of it keeps it; a new address ends it.
    send_profile(&community, &alice, json!({ "publicAddress": address }), json!([]));
    assert_eq!(community.store.member_linked(&alice).unwrap().statement, Some(good));
    send_profile(&community, &alice, json!({ "publicAddress": "did:webvh:Qm:id.example:alice2" }), json!([]));
    assert_eq!(community.store.member_linked(&alice).unwrap().statement, None);
}
