//! Forums as message boards (community/1.0's "Forum channels", asked
//! 2026-11-18): a forum's sections, a thread's standing (sticky, locked, its
//! section), how much each member has posted in the forums, and each
//! section's counts; and recounting what is kept of them (asked the same day).

use serde_json::{json, Value};

use crate::community::{line, refuse, reply_to, Community, Outcome};
use crate::store::Member;

pub const SET_THREAD: &str = "https://wyvrn.app/community/1.0/set-thread";
pub const FORUM_STATS_REQUEST: &str = "https://wyvrn.app/community/1.0/forum-stats-request";
pub const FORUM_STATS: &str = "https://wyvrn.app/community/1.0/forum-stats";
pub const REBUILD_COUNTS: &str = "https://wyvrn.app/community/1.0/rebuild-counts";
pub const COUNTS_REBUILT: &str = "https://wyvrn.app/community/1.0/counts-rebuilt";

/// How many sections a forum may have, and how deep they may sit inside each other.
pub const MOST_SECTIONS: usize = 50;
pub const DEEPEST_SECTION: usize = 3;

/// A forum's sections as an admin gives them, checked; a new one gets an id.
/// A section may sit inside another (`parent`, the other's id), up to
/// [`DEEPEST_SECTION`] levels; a parent not in the list, or a loop, is refused.
pub fn read_sections(given: &Value) -> Result<Value, &'static str> {
    let given = given.as_array().ok_or("Sections are a list.")?;
    if given.len() > MOST_SECTIONS {
        return Err("A forum has at most fifty sections.");
    }
    let mut out = Vec::new();
    for section in given {
        let name = line(&section["name"], 40).ok_or("Each section needs a name of up to 40 characters.")?;
        let id = section["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 32).map(str::to_string).unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()[..8].to_string());
        let mut entry = json!({ "id": id, "name": name });
        if let Some(description) = line(&section["description"], 200) {
            entry["description"] = json!(description);
        }
        match &section["parent"] {
            Value::Null => {}
            Value::String(parent) if !parent.is_empty() => entry["parent"] = json!(parent),
            _ => return Err("A section's parent is another section's id."),
        }
        out.push(entry);
    }
    let ids: Vec<&str> = out.iter().filter_map(|s| s["id"].as_str()).collect();
    if (1..ids.len()).any(|i| ids[..i].contains(&ids[i])) {
        return Err("Two sections have the same id.");
    }
    for section in &out {
        let mut depth = 1;
        let mut at = section;
        while let Some(parent) = at["parent"].as_str() {
            at = out.iter().find(|s| s["id"] == parent).ok_or("A section's parent is not one of the forum's sections.")?;
            depth += 1;
            if depth > DEEPEST_SECTION {
                return Err("Sections go at most three deep (or a section is inside itself).");
            }
        }
    }
    Ok(Value::Array(out))
}

/// The names of a section and those it is inside, outermost first ("Help › Android").
pub fn section_path(sections: Option<&Value>, id: &str) -> Option<String> {
    let list = sections?.as_array()?;
    let mut names = Vec::new();
    let mut at = Some(id.to_string());
    while let Some(here) = at.filter(|_| names.len() < DEEPEST_SECTION) {
        let section = list.iter().find(|s| s["id"] == here.as_str())?;
        names.push(section["name"].as_str().unwrap_or_default().to_string());
        at = section["parent"].as_str().map(str::to_string);
    }
    names.reverse();
    Some(names.join(" › "))
}

/// Each section's id and the ids of every section inside it, at any depth (its own first).
fn with_inside(sections: &[Value]) -> Vec<(String, Vec<String>)> {
    let parent_of = |id: &str| sections.iter().find(|s| s["id"] == id).and_then(|s| s["parent"].as_str()).map(str::to_string);
    let ids: Vec<String> = sections.iter().filter_map(|s| s["id"].as_str().map(str::to_string)).collect();
    ids.iter()
        .map(|id| {
            let inside = ids
                .iter()
                .filter(|other| {
                    let mut at = Some((*other).clone());
                    // (Bounded: what is stored was checked to be at most three deep.)
                    for _ in 0..=DEEPEST_SECTION {
                        match at {
                            Some(ref x) if x == id => return true,
                            Some(ref x) => at = parent_of(x),
                            None => return false,
                        }
                    }
                    false
                })
                .cloned()
                .collect();
            (id.clone(), inside)
        })
        .collect()
}

impl Community {
    pub fn handle_board_message(&self, member: &Member, message: &Value, _now: i64) -> anyhow::Result<Option<Outcome>> {
        let body = &message["body"];
        let did = member.did.as_str();
        match message["type"].as_str().unwrap_or_default() {
            SET_THREAD => {
                let Some(thread) = body["thread"].as_str().map(|id| self.store.thread(id)).transpose()?.flatten() else {
                    return refuse(message, "e.p.msg.unknown-thread", "There is no such thread.").map(Some);
                };
                let Some(channel) = self.store.channel(&thread.channel)?.filter(|c| c.forum.is_some()) else {
                    return refuse(message, "e.p.msg.unknown-thread", "There is no such thread.").map(Some);
                };
                if !self.sees_id(did, &channel.id)? {
                    return refuse(message, "e.p.msg.unknown-thread", "There is no such thread.").map(Some);
                }
                if !self.may_in(did, &channel, "manage_messages")? {
                    return refuse(message, "e.p.trust.not-authorized", "A thread's standing is set by whoever manages posts in the forum.").map(Some);
                }
                let sticky = body["sticky"].as_bool().unwrap_or(thread.sticky);
                let locked = body["locked"].as_bool().unwrap_or(thread.locked);
                let section = match body.get("section") {
                    Some(Value::Null) => None,
                    Some(Value::String(id)) => match channel.sections.as_ref().and_then(|s| s.as_array()).is_some_and(|s| s.iter().any(|x| x["id"] == id.as_str())) {
                        true => Some(id.clone()),
                        false => return refuse(message, "e.p.msg.unknown-section", "There is no such section in this forum.").map(Some),
                    },
                    _ => thread.section.clone(),
                };
                self.store.set_thread_flags(&thread.id, sticky, locked, section.as_deref())?;
                self.store.touch_thread(&thread.id)?;
                self.audit(crate::gates::now(), did, "thread.set", Some(&thread.id), Some(json!({ "sticky": sticky, "locked": locked, "section": section })));
                self.changed_answer(message, &thread.id).map(Some)
            }
            FORUM_STATS_REQUEST => {
                let Some(channel) = body["channel"].as_str().map(|id| self.store.channel(id)).transpose()?.flatten().filter(|c| c.forum.is_some()) else {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such forum.").map(Some);
                };
                if !self.sees_id(did, &channel.id)? {
                    return refuse(message, "e.p.msg.unknown-channel", "There is no such forum.").map(Some);
                }
                let mut stats = Vec::new();
                for (author, posts) in self.store.forum_post_counts(&channel.id)? {
                    let since = self.store.member(&author)?.map(|m| m.since);
                    let mut entry = json!({ "member": author, "posts": posts });
                    if let Some(since) = since {
                        entry["since"] = json!(since);
                    }
                    stats.push(entry);
                }
                // Each section's own counts, then each with those of the sections inside it.
                let listed: Vec<Value> = channel.sections.as_ref().and_then(Value::as_array).cloned().unwrap_or_default();
                let ids: Vec<String> = listed.iter().filter_map(|s| s["id"].as_str().map(str::to_string)).collect();
                let tallies = self.store.section_tallies(&channel.id, &ids)?;
                let own = |id: &str| tallies.iter().find(|t| t.section.as_deref() == Some(id));
                let entry = |section: Option<&str>, parts: Vec<&crate::store::SectionTally>| {
                    let mut entry = json!({ "section": section, "threads": parts.iter().map(|t| t.threads).sum::<i64>(), "posts": parts.iter().map(|t| t.posts).sum::<i64>() });
                    if let Some((at, thread)) = parts.iter().filter_map(|t| Some((t.last_at?, t.last_thread.clone()?))).max_by_key(|(at, _)| *at) {
                        entry["last_at"] = json!(at);
                        entry["last_thread"] = json!(thread);
                    }
                    entry
                };
                let mut sections = vec![entry(None, tallies.iter().filter(|t| t.section.is_none()).collect())];
                for (id, inside) in with_inside(&listed) {
                    sections.push(entry(Some(&id), inside.iter().filter_map(|i| own(i)).collect()));
                }
                Ok(Some(Outcome::reply(reply_to(message, FORUM_STATS, json!({ "channel": channel.id, "stats": stats, "sections": sections })))))
            }
            REBUILD_COUNTS => {
                if !self.powers(did)?.has("manage_community") {
                    return refuse(message, "e.p.trust.not-authorized", "Recounting takes Manage the community.").map(Some);
                }
                let (threads, members, sections) = self.store.rebuild_counts()?;
                self.audit(crate::gates::now(), did, "counts.rebuilt", None, Some(json!({ "threads": threads, "members": members, "sections": sections })));
                Ok(Some(Outcome::reply(reply_to(message, COUNTS_REBUILT, json!({ "threads": threads, "members": members, "sections": sections })))))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_are_checked_and_new_ones_get_an_id() {
        let sections = read_sections(&json!([{ "name": "Help", "description": "Ask here" }, { "id": "news", "name": "News" }])).unwrap();
        assert_eq!(sections[0]["name"], "Help");
        assert_eq!(sections[0]["id"].as_str().unwrap().len(), 8);
        assert_eq!(sections[1]["id"], "news");
        assert!(read_sections(&json!([{ "name": "" }])).is_err());
        assert!(read_sections(&json!(vec![json!({ "name": "x" }); 51])).is_err());
    }

    #[test]
    fn sections_sit_inside_others_up_to_three_deep_and_never_in_themselves() {
        let ok = read_sections(&json!([{ "id": "a", "name": "A" }, { "id": "b", "name": "B", "parent": "a" }, { "id": "c", "name": "C", "parent": "b" }])).unwrap();
        assert_eq!(ok[2]["parent"], "b");
        assert!(read_sections(&json!([{ "id": "a", "name": "A" }, { "id": "b", "name": "B", "parent": "a" }, { "id": "c", "name": "C", "parent": "b" }, { "id": "d", "name": "D", "parent": "c" }])).is_err());
        assert!(read_sections(&json!([{ "id": "a", "name": "A", "parent": "b" }, { "id": "b", "name": "B", "parent": "a" }])).is_err());
        assert!(read_sections(&json!([{ "id": "a", "name": "A", "parent": "a" }])).is_err());
        assert!(read_sections(&json!([{ "id": "a", "name": "A", "parent": "gone" }])).is_err());
        assert!(read_sections(&json!([{ "id": "a", "name": "A" }, { "id": "a", "name": "Again" }])).is_err());
        assert_eq!(section_path(Some(&ok), "c").as_deref(), Some("A › B › C"));
        assert_eq!(section_path(Some(&ok), "gone"), None);
        let inside = with_inside(ok.as_array().unwrap());
        assert_eq!(inside, vec![("a".into(), vec!["a".into(), "b".into(), "c".into()]), ("b".into(), vec!["b".into(), "c".into()]), ("c".into(), vec!["c".into()])]);
    }
}
