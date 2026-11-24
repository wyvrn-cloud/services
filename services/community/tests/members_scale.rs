//! The member list of a community of 3,000 (community/1.0's "A community of
//! thousands"): what each way of asking for it costs the server, in time and
//! in statements to its database, and how big the answer is. A test binary of its
//! own, with one test, so that the process-wide statement count is its alone.
//! Run with `--nocapture` to see the table.

use std::time::Instant;

use serde_json::{json, Value};
use wyvrn_community::{
    community::Community,
    member_ids::MemberProfile,
    store::{Role, Store},
};
use wyvrn_service::db::statements_said;

const C: &str = "https://wyvrn.app/community/1.0/";
const MEMBERS: usize = 3000;

fn did(n: usize) -> String {
    format!("did:peer:2.Ez6LSexample{n:05}.Vz6MkexampleexampleexampleexampleexampleexampleZ.SeyJ0IjoiZG0iLCJzIjp7InVyaSI6Imh0dHBzOi8vbWVkaWF0b3IuZXhhbXBsZS8ifX0")
}

fn ask(community: &Community, asker: &str, kind: &str, body: Value) -> (Value, usize, f64, u64) {
    let message = json!({ "id": uuid::Uuid::new_v4().to_string(), "type": format!("{C}{kind}"), "body": body });
    let before = statements_said();
    let started = Instant::now();
    let reply = community.handle(asker, &message, 10).unwrap().reply.expect("an answer");
    let sent = community.masked_for(asker, &reply).unwrap();
    let took = started.elapsed().as_secs_f64() * 1000.0;
    let statements = statements_said() - before;
    let bytes = sent.to_string().len();
    (sent, bytes, took, statements)
}

#[test]
fn the_list_of_three_thousand_costs_what_one_page_does_not_what_everyone_does() {
    let config: wyvrn_community::config::Config =
        toml::from_str("[server]\nbind = \"unused\"\npublic_url = \"https://community.example/\"\n[identity]\npath = \"unused\"\n[store]\npath = \"unused\"\n[community]\nname = \"Scale\"\n").unwrap();
    let mut community = Community::new(Store::in_memory().unwrap(), config.community.clone(), Some(wyvrn_service::profile::Kept::scratch().unwrap())).unwrap();
    community.public_url = "https://community.example/".into();
    // A picture of about 20 KB and a linked account of about 2 KB each, as a real
    // profile carries; a tenth moderators, a third online.
    let picture = "iVBORw0KGgo".repeat(1800);
    let linked = json!([{ "type": ["VerifiableCredential", "LinkedAccountCredential"], "credentialSubject": { "account": "someone", "service": "example" }, "proof": { "proofValue": "z".repeat(2000) } }]);
    for i in 0..MEMBERS {
        let did = did(i);
        let store = &community.store;
        store.set_member(&did, if i == 0 { Role::Admin } else { Role::Member }, 1).unwrap();
        store.set_joined(&did, true).unwrap();
        store.set_name(&did, Some(&format!("Person {i:05}"))).unwrap();
        store.set_member_picture(&did, Some(("image/png", &format!("{picture}{i}"))), 1).unwrap();
        store.set_member_profile(&did, &MemberProfile { description: Some("A few words about me.".into()), linked: Some(linked.clone()), at: 1, ..Default::default() }).unwrap();
        if i % 10 == 5 {
            store.give_role(&did, "moderator").unwrap();
        }
        if i % 3 == 0 {
            store.set_presence(&did, true, "online").unwrap();
            community.live.write().unwrap().insert(did.clone(), 1);
        }
    }
    let channel = ask(&community, &did(0), "create-channel", json!({ "name": "general" })).0["body"]["created"].as_str().unwrap().to_string();
    let member = did(1);
    // Asked once first: what is kept between requests (store.rs' CACHED_TABLES) is
    // kept, as it is on a server that has run a while.
    ask(&community, &member, "members-request", json!({ "brief": true, "limit": 100 }));

    let mut rows = Vec::new();
    let mut measure = |name: &str, (answer, bytes, took, statements): (Value, usize, f64, u64)| {
        assert!(!answer["type"].as_str().unwrap_or_default().contains("problem"), "{name}: {answer}");
        println!("{name:<44} {bytes:>10} bytes {took:>9.1} ms {statements:>6} statements");
        rows.push((name.to_string(), bytes, took, statements));
        answer
    };
    let whole = measure("whole list (an app before pages)", ask(&community, &member, "members-request", json!({})));
    assert_eq!(whole["body"]["members"].as_array().unwrap().len(), MEMBERS);
    let page = measure("brief, first page of 100", ask(&community, &member, "members-request", json!({ "brief": true, "limit": 100 })));
    assert_eq!(page["body"]["members"].as_array().unwrap().len(), 100);
    assert_eq!(page["body"]["counts"]["total"], MEMBERS);
    let mut after = page["body"]["next"].as_str().map(str::to_string);
    let mut pages = 1;
    let mut paged_bytes = page.to_string().len();
    let started = Instant::now();
    while let Some(next) = after {
        let (page, bytes, _, _) = ask(&community, &member, "members-request", json!({ "brief": true, "limit": 100, "after": next }));
        pages += 1;
        paged_bytes += bytes;
        after = page["body"]["next"].as_str().map(str::to_string);
    }
    println!("{:<44} {paged_bytes:>10} bytes {:>9.1} ms ({pages} pages)", "brief, every page", started.elapsed().as_secs_f64() * 1000.0);
    assert_eq!(pages, MEMBERS.div_ceil(100));
    let some: Vec<String> = (0..50).map(|n| community.store.member_id(&did(n * 7), 1).unwrap()).collect();
    let by_id = measure("brief, 50 post authors (ids)", ask(&community, &member, "members-request", json!({ "brief": true, "ids": some })));
    assert_eq!(by_id["body"]["members"].as_array().unwrap().len(), 50);
    measure("brief, @-search (query \"person 012\")", ask(&community, &member, "members-request", json!({ "brief": true, "query": "person 012", "limit": 10 })));
    measure("a channel's members (ids_only)", ask(&community, &member, "members-request", json!({ "channel": channel, "ids_only": true })));
    measure("a channel's members (as before)", ask(&community, &member, "members-request", json!({ "channel": channel })));
    measure("one profile card (member-request)", ask(&community, &member, "member-request", json!({ "member": did(42) })));

    let (_, page_bytes, page_took, page_statements) = rows[1].clone();
    let (_, whole_bytes, whole_took, _) = rows[0].clone();
    assert!(page_bytes < 40 * 1024, "a brief page of 100 is small: {page_bytes}");
    assert!(page_bytes * 20 < whole_bytes, "a page is a sliver of the whole: {page_bytes} of {whole_bytes}");
    assert!(page_statements < 40, "a page is a handful of statements, whatever the community's size: {page_statements}");
    assert!(page_took < whole_took, "a page costs less than everyone: {page_took} ms, {whole_took} ms");
}
