//! Forums as message boards (community/1.0's "Forum channels", asked
//! 2026-11-18): a forum's sections, a thread's standing (sticky, locked, its
//! section), and how much each member has posted in the forums.

use serde_json::{json, Value};

use crate::community::{line, refuse, reply_to, Community, Outcome};
use crate::store::Member;

pub const SET_THREAD: &str = "https://wyvrn.app/community/1.0/set-thread";
pub const FORUM_STATS_REQUEST: &str = "https://wyvrn.app/community/1.0/forum-stats-request";
pub const FORUM_STATS: &str = "https://wyvrn.app/community/1.0/forum-stats";

/// A forum's sections as an admin gives them, checked; a new one gets an id.
pub fn read_sections(given: &Value) -> Result<Value, &'static str> {
    let given = given.as_array().ok_or("Sections are a list.")?;
    if given.len() > 20 {
        return Err("A forum has at most twenty sections.");
    }
    let mut out = Vec::new();
    for section in given {
        let name = line(&section["name"], 40).ok_or("Each section needs a name of up to 40 characters.")?;
        let id = section["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 32).map(str::to_string).unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()[..8].to_string());
        let mut entry = json!({ "id": id, "name": name });
        if let Some(description) = line(&section["description"], 200) {
            entry["description"] = json!(description);
        }
        out.push(entry);
    }
    Ok(Value::Array(out))
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
                let forums: Vec<String> = self.store.channels()?.into_iter().filter(|c| c.forum.is_some()).map(|c| c.id).collect();
                let mut stats = Vec::new();
                for (author, posts) in self.store.forum_post_counts(&forums, &channel.id)? {
                    let since = self.store.member(&author)?.map(|m| m.since);
                    let mut entry = json!({ "member": author, "posts": posts });
                    if let Some(since) = since {
                        entry["since"] = json!(since);
                    }
                    stats.push(entry);
                }
                Ok(Some(Outcome::reply(reply_to(message, FORUM_STATS, json!({ "channel": channel.id, "stats": stats })))))
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
        assert!(read_sections(&json!(vec![json!({ "name": "x" }); 21])).is_err());
    }
}
