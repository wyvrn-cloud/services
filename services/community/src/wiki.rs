//! Wiki channels (community/1.0's "Wiki channels", asked 2026-11-18): a tree of
//! pages members edit together. A page is a Yjs document in slate-yjs's shape
//! (`content`, an XmlText of one embedded XmlText per paragraph, each a line of
//! Markdown), kept here with yrs: every change as it came, with who sent it and
//! when (its history, and who wrote what), and the document they make now.
//!
//! A Yjs client id is one member's on a page: the first to send it holds it, so
//! "who wrote what" (each piece of text's client) can't be claimed by another.
//! Changes and cursors (Yjs awareness) go to whoever has the page open, over
//! their live connection only; a page is open for a minute after its last
//! awareness.

use std::collections::HashMap;

use base64::Engine;
use serde_json::{json, Value};
use yrs::types::text::YChange;
use yrs::updates::decoder::Decode;
use yrs::{Any, Doc, GetString, Out, ReadTxn, StateVector, Text, Transact, Update, Xml, XmlTextPrelim};

use crate::community::{line, refuse, reply_to, Community, Outcome};
use crate::store::{Channel, Member, WikiComment, WikiPage};

const P: &str = "https://wyvrn.app/community/1.0/";
/// How long a page stays open for someone after their last awareness.
const OPEN_FOR: i64 = 60;
/// A new version after this long apart, or another member's change.
const VERSION_GAP: i64 = 5 * 60;
const MAX_TITLE: usize = 100;
const MAX_PAGES: usize = 500;
const MAX_UPDATE_BYTES: usize = 512 * 1024;
const MAX_COMMENT: usize = 2000;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// The page a document makes, as text: each paragraph a line.
pub fn text_of(doc: &Doc) -> String {
    let root = doc.get_or_insert_text("content");
    let txn = doc.transact();
    root.diff(&txn, YChange::identity)
        .into_iter()
        .filter_map(|d| match d.insert {
            Out::YXmlText(paragraph) => Some(paragraph.get_string(&txn)),
            Out::YText(paragraph) => Some(paragraph.get_string(&txn)),
            Out::Any(Any::String(text)) => Some(text.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A document from what a page holds (one Yjs update), or an empty one.
fn doc_from(state: Option<&[u8]>) -> anyhow::Result<Doc> {
    let doc = Doc::new();
    if let Some(state) = state {
        let update = Update::decode_v1(state)?;
        doc.transact_mut().apply_update(update)?;
    }
    Ok(doc)
}

fn encoded(doc: &Doc) -> Vec<u8> {
    doc.transact().encode_state_as_update_v1(&StateVector::default())
}

/// The change that makes `doc` say `text` (each line a paragraph), made as Yjs
/// client `client`: a restore.
fn saying(doc: &Doc, text: &str) -> Vec<u8> {
    let root = doc.get_or_insert_text("content");
    let before = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let len = root.len(&txn);
        root.remove_range(&mut txn, 0, len);
        for (n, paragraph) in text.split('\n').enumerate() {
            let embedded = root.insert_embed(&mut txn, n as u32, XmlTextPrelim::new(paragraph));
            embedded.insert_attribute(&mut txn, "type", "paragraph");
        }
    }
    doc.transact().encode_state_as_update_v1(&before)
}

impl Community {
    fn wiki_channel(&self, did: &str, id: Option<&str>) -> anyhow::Result<Option<Channel>> {
        let Some(channel) = id.map(|id| self.store.channel(id)).transpose()?.flatten().filter(|c| c.wiki) else { return Ok(None) };
        Ok(self.sees_id(did, &channel.id)?.then_some(channel))
    }

    /// The page `id` and its channel, if `did` sees it.
    fn wiki_page(&self, did: &str, id: Option<&str>) -> anyhow::Result<Option<(WikiPage, Channel)>> {
        let Some(page) = id.map(|id| self.store.wiki_page(id)).transpose()?.flatten().filter(|p| !p.deleted) else { return Ok(None) };
        Ok(self.wiki_channel(did, Some(&page.channel))?.map(|channel| (page, channel)))
    }

    fn pages_body(&self, channel: &Channel) -> anyhow::Result<Value> {
        let pages: Vec<Value> = self
            .store
            .wiki_pages(&channel.id)?
            .into_iter()
            .filter(|p| !p.deleted)
            .map(|p| {
                let mut page = json!({ "id": p.id, "title": p.title, "position": p.position, "updated_at": p.updated_at });
                if let Some(parent) = &p.parent {
                    page["parent"] = json!(parent);
                }
                page
            })
            .collect();
        Ok(json!({ "channel": channel.id, "pages": pages }))
    }

    /// Who has `page` open now, but `except`.
    fn open_on(&self, page: &str, except: &str, now: i64) -> Vec<String> {
        let mut open = self.wiki_open.lock().unwrap_or_else(|p| p.into_inner());
        let Some(here) = open.get_mut(page) else { return Vec::new() };
        here.retain(|_, at| *at > now - OPEN_FOR);
        here.keys().filter(|d| d.as_str() != except).cloned().collect()
    }

    fn comments_body(&self, page: &str, now: i64) -> anyhow::Result<Value> {
        let comments: Vec<Value> = self
            .store
            .wiki_comments(page)?
            .into_iter()
            .map(|c| {
                let mut comment = json!({ "id": c.id, "text": c.text, "by": self.store.member_id(&c.did, now).unwrap_or_default(), "at": c.at });
                for (key, value) in [("start", &c.start), ("end", &c.end), ("quote", &c.quote), ("reply_to", &c.reply_to)] {
                    if let Some(value) = value {
                        comment[key] = json!(value);
                    }
                }
                if c.resolved {
                    comment["resolved"] = json!(true);
                }
                comment
            })
            .collect();
        Ok(json!({ "page": page, "comments": comments }))
    }

    /// The messages of wiki channels, from a member.
    pub fn handle_wiki_message(&self, member: &Member, message: &Value, now: i64) -> anyhow::Result<Option<Outcome>> {
        let Some(kind) = message["type"].as_str().and_then(|t| t.strip_prefix(P)).filter(|k| k.starts_with("wiki-")) else { return Ok(None) };
        let body = &message["body"];
        let did = member.did.as_str();
        let no_page = || refuse(message, "e.p.msg.unknown-page", "There is no such page.").map(Some);
        let not_allowed = |why: &str| refuse(message, "e.p.trust.not-authorized", why).map(Some);
        match kind {
            "wiki-pages-request" => {
                let Some(channel) = self.wiki_channel(did, body["channel"].as_str())? else { return refuse(message, "e.p.msg.unknown-channel", "There is no such wiki.").map(Some) };
                Ok(Some(Outcome::reply(reply_to(message, &format!("{P}wiki-pages"), self.pages_body(&channel)?))))
            }
            "wiki-create-page" | "wiki-update-page" | "wiki-delete-page" => {
                let (mut page, channel) = if kind == "wiki-create-page" {
                    let Some(channel) = self.wiki_channel(did, body["channel"].as_str())? else { return refuse(message, "e.p.msg.unknown-channel", "There is no such wiki.").map(Some) };
                    if self.store.wiki_pages(&channel.id)?.iter().filter(|p| !p.deleted).count() >= MAX_PAGES {
                        return refuse(message, "e.p.msg.too-many", "A wiki holds 500 pages at most.").map(Some);
                    }
                    let page = WikiPage { id: uuid::Uuid::new_v4().simple().to_string()[..16].to_string(), channel: channel.id.clone(), parent: None, title: String::new(), position: now, created_by: did.into(), created_at: now, updated_at: now, deleted: false };
                    (page, channel)
                } else {
                    match self.wiki_page(did, body["page"].as_str())? {
                        Some(found) => found,
                        None => return no_page(),
                    }
                };
                if !self.may_in(did, &channel, "post")? {
                    return not_allowed("Editing this wiki takes posting in it.");
                }
                if kind == "wiki-delete-page" {
                    page.deleted = true;
                } else {
                    if kind == "wiki-create-page" {
                        page.title = line(&body["title"], MAX_TITLE).unwrap_or_else(|| "Untitled".into());
                    } else if let Some(title) = body.get("title") {
                        match line(title, MAX_TITLE) {
                            Some(title) => page.title = title,
                            None => return refuse(message, "e.p.msg.empty", "A page needs a title.").map(Some),
                        }
                    }
                    if let Some(parent) = body.get("parent") {
                        page.parent = match parent.as_str() {
                            // Under a page of the same wiki, and never under itself or its own.
                            Some(id) => {
                                let mut at = Some(id.to_string());
                                while let Some(up) = at {
                                    if up == page.id {
                                        return refuse(message, "e.p.msg.unknown-page", "A page can't be under itself.").map(Some);
                                    }
                                    at = self.store.wiki_page(&up)?.filter(|p| p.channel == channel.id && !p.deleted).and_then(|p| p.parent);
                                }
                                if self.store.wiki_page(id)?.is_none_or(|p| p.channel != channel.id || p.deleted) {
                                    return no_page();
                                }
                                Some(id.to_string())
                            }
                            None => None,
                        };
                    }
                    if let Some(position) = body["position"].as_i64() {
                        page.position = position;
                    }
                }
                page.updated_at = now;
                self.store.save_wiki_page(&page)?;
                self.audit(now, did, &format!("wiki.{}", kind.trim_start_matches("wiki-").trim_end_matches("-page")), Some(&page.id), Some(json!({ "title": page.title })));
                let pages = self.pages_body(&channel)?;
                let mut outcome = Outcome::reply(reply_to(message, &format!("{P}wiki-pages"), pages.clone()));
                if kind == "wiki-create-page" {
                    if let Some(reply) = outcome.reply.as_mut() {
                        reply["body"]["created"] = json!(page.id);
                    }
                }
                for other in self.store.joined()?.into_iter().filter(|m| m.did != did) {
                    if self.sees_id(&other.did, &channel.id)? {
                        outcome.notices.push((other.did, json!({ "type": format!("{P}wiki-pages"), "id": uuid::Uuid::new_v4().to_string(), "body": pages })));
                    }
                }
                Ok(Some(outcome))
            }
            "wiki-page-request" => {
                let Some((page, _)) = self.wiki_page(did, body["page"].as_str())? else { return no_page() };
                let (state, seq) = self.store.wiki_state(&page.id)?.unwrap_or_default();
                let clients: Vec<Value> = self.store.wiki_clients(&page.id)?.into_iter().map(|(client, who)| json!({ "client": client, "member": self.store.member_id(&who, now).unwrap_or_default() })).collect();
                Ok(Some(Outcome::reply(reply_to(message, &format!("{P}wiki-page"), json!({ "page": page.id, "state": b64().encode(&state), "seq": seq, "clients": clients })))))
            }
            "wiki-update" => {
                let Some((mut page, channel)) = self.wiki_page(did, body["page"].as_str())? else { return no_page() };
                if !self.may_in(did, &channel, "post")? {
                    return not_allowed("Editing this wiki takes posting in it.");
                }
                let bytes = match body["update"].as_str().map(|u| b64().decode(u)) {
                    Some(Ok(bytes)) if bytes.len() <= MAX_UPDATE_BYTES => bytes,
                    _ => return refuse(message, "e.p.msg.invalid-update", "That isn't a Yjs update.").map(Some),
                };
                let Ok(update) = Update::decode_v1(&bytes) else { return refuse(message, "e.p.msg.invalid-update", "That isn't a Yjs update.").map(Some) };
                // Every client whose text it brings is the sender's on this page.
                let clients: Vec<u64> = update.state_vector().iter().map(|(client, _)| client.get()).collect();
                for client in &clients {
                    match self.store.wiki_client(&page.id, *client)? {
                        Some(holder) if holder != did => return refuse(message, "e.p.msg.not-your-client", "That Yjs client id is someone else's on this page.").map(Some),
                        Some(_) => {}
                        None => self.store.add_wiki_client(&page.id, *client, did)?,
                    }
                }
                let (state, seq) = self.store.wiki_state(&page.id)?.unwrap_or_default();
                let doc = doc_from((!state.is_empty()).then_some(state.as_slice()))?;
                if doc.transact_mut().apply_update(update).is_err() {
                    return refuse(message, "e.p.msg.invalid-update", "That update doesn't apply.").map(Some);
                }
                let seq = seq + 1;
                self.store.add_wiki_update(&page.id, seq, &bytes, did, now)?;
                self.store.set_wiki_state(&page.id, &encoded(&doc), seq)?;
                page.updated_at = now;
                self.store.save_wiki_page(&page)?;
                let by = self.store.member_id(did, now)?;
                let mut outcome = Outcome::reply(reply_to(message, &format!("{P}wiki-updated"), json!({ "page": page.id, "seq": seq })));
                for other in self.open_on(&page.id, did, now) {
                    outcome.live_to.push((other, json!({ "type": format!("{P}wiki-update"), "id": uuid::Uuid::new_v4().to_string(), "body": { "page": page.id, "update": body["update"], "seq": seq, "by": by, "clients": clients } })));
                }
                Ok(Some(outcome))
            }
            "wiki-awareness" => {
                let Some((page, _)) = self.wiki_page(did, body["page"].as_str())? else { return no_page() };
                let state = body["state"].as_str().filter(|s| s.len() <= 16 * 1024).unwrap_or_default();
                self.wiki_open.lock().unwrap_or_else(|p| p.into_inner()).entry(page.id.clone()).or_default().insert(did.to_string(), now);
                let by = self.store.member_id(did, now)?;
                let mut outcome = Outcome::default();
                for other in self.open_on(&page.id, did, now) {
                    outcome.live_to.push((other, json!({ "type": format!("{P}wiki-awareness"), "id": uuid::Uuid::new_v4().to_string(), "body": { "page": page.id, "state": state, "by": by } })));
                }
                Ok(Some(outcome))
            }
            "wiki-history-request" => {
                let Some((page, _)) = self.wiki_page(did, body["page"].as_str())? else { return no_page() };
                // One member's changes close together are one version; but a restore
                // is a version of its own, so what it undid stays in the history to go
                // back to, and what is written after it isn't hidden in it either.
                let restores = self.store.wiki_restores(&page.id)?;
                let mut versions: Vec<(i64, i64, String)> = Vec::new();
                let mut after_restore = false;
                for (seq, who, at) in self.store.wiki_update_heads(&page.id)? {
                    let restore = restores.contains(&seq);
                    let joins = !restore && !after_restore;
                    after_restore = restore;
                    match versions.last_mut() {
                        Some((last_seq, last_at, last_who)) if joins && *last_who == who && at - *last_at < VERSION_GAP => {
                            (*last_seq, *last_at) = (seq, at);
                        }
                        _ => versions.push((seq, at, who)),
                    }
                }
                let versions: Vec<Value> = versions.into_iter().rev().map(|(seq, at, who)| json!({ "seq": seq, "at": at, "by": self.store.member_id(&who, now).unwrap_or_default() })).collect();
                Ok(Some(Outcome::reply(reply_to(message, &format!("{P}wiki-history"), json!({ "page": page.id, "versions": versions })))))
            }
            "wiki-version-request" | "wiki-restore" => {
                let Some((mut page, channel)) = self.wiki_page(did, body["page"].as_str())? else { return no_page() };
                let Some(seq) = body["seq"].as_i64().filter(|s| *s >= 0) else { return no_page() };
                let then = Doc::new();
                for update in self.store.wiki_updates_to(&page.id, seq)? {
                    if let Ok(update) = Update::decode_v1(&update) {
                        let _ = then.transact_mut().apply_update(update);
                    }
                }
                let text = text_of(&then);
                if kind == "wiki-version-request" {
                    return Ok(Some(Outcome::reply(reply_to(message, &format!("{P}wiki-version"), json!({ "page": page.id, "seq": seq, "text": text })))));
                }
                if !self.may_in(did, &channel, "manage_messages")? {
                    return not_allowed("Restoring a version takes managing posts in the wiki.");
                }
                // As a change of the restorer's, by a client id of theirs alone.
                let (state, last) = self.store.wiki_state(&page.id)?.unwrap_or_default();
                let doc = doc_from((!state.is_empty()).then_some(state.as_slice()))?;
                let client = loop {
                    let candidate = u64::from(uuid::Uuid::new_v4().as_u128() as u32) & 0x3FFF_FFFF;
                    if self.store.wiki_client(&page.id, candidate)?.is_none() {
                        break candidate;
                    }
                };
                let restoring = Doc::with_client_id(client);
                restoring.transact_mut().apply_update(Update::decode_v1(&encoded(&doc))?)?;
                let change = saying(&restoring, &text);
                self.store.add_wiki_client(&page.id, client, did)?;
                doc.transact_mut().apply_update(Update::decode_v1(&change)?)?;
                let seq_now = last + 1;
                self.store.add_wiki_update(&page.id, seq_now, &change, did, now)?;
                self.store.add_wiki_restore(&page.id, seq_now)?;
                self.store.set_wiki_state(&page.id, &encoded(&doc), seq_now)?;
                page.updated_at = now;
                self.store.save_wiki_page(&page)?;
                self.audit(now, did, "wiki.restore", Some(&page.id), Some(json!({ "seq": seq })));
                let by = self.store.member_id(did, now)?;
                let encoded_change = b64().encode(&change);
                let mut outcome = Outcome::reply(reply_to(message, &format!("{P}wiki-updated"), json!({ "page": page.id, "seq": seq_now, "update": encoded_change })));
                for other in self.open_on(&page.id, did, now) {
                    outcome.live_to.push((other, json!({ "type": format!("{P}wiki-update"), "id": uuid::Uuid::new_v4().to_string(), "body": { "page": page.id, "update": encoded_change, "seq": seq_now, "by": by, "clients": [client] } })));
                }
                Ok(Some(outcome))
            }
            "wiki-comment" | "wiki-resolve" | "wiki-delete-comment" | "wiki-comments-request" => {
                let page_id = match kind {
                    "wiki-comment" | "wiki-comments-request" => body["page"].as_str().map(str::to_string),
                    _ => body["comment"].as_str().map(|c| self.store.wiki_comment(c)).transpose()?.flatten().map(|c| c.page),
                };
                let Some((page, channel)) = self.wiki_page(did, page_id.as_deref())? else { return no_page() };
                match kind {
                    "wiki-comment" => {
                        if !self.may_in(did, &channel, "post")? {
                            return not_allowed("Commenting takes posting in the wiki.");
                        }
                        let text = body["text"].as_str().map(str::trim).filter(|t| !t.is_empty() && t.chars().count() <= MAX_COMMENT);
                        let Some(text) = text else { return refuse(message, "e.p.msg.empty", "A comment says something, in up to 2,000 characters.").map(Some) };
                        let reply_to_id = body["reply_to"].as_str().filter(|r| self.store.wiki_comment(r).ok().flatten().is_some_and(|c| c.page == page.id)).map(str::to_string);
                        let anchor = |v: &Value| v.as_str().filter(|s| s.len() <= 512).map(str::to_string);
                        if reply_to_id.is_none() && (anchor(&body["start"]).is_none() || anchor(&body["end"]).is_none()) {
                            return refuse(message, "e.p.msg.unknown-comment", "A comment is on a stretch of text, or answers another.").map(Some);
                        }
                        let comment = WikiComment {
                            id: uuid::Uuid::new_v4().simple().to_string()[..16].to_string(),
                            page: page.id.clone(),
                            start: anchor(&body["start"]),
                            end: anchor(&body["end"]),
                            quote: body["quote"].as_str().map(|q| q.chars().take(200).collect()),
                            text: text.to_string(),
                            did: did.to_string(),
                            at: now,
                            reply_to: reply_to_id,
                            resolved: false,
                        };
                        self.store.save_wiki_comment(&comment)?;
                    }
                    "wiki-resolve" | "wiki-delete-comment" => {
                        let Some(mut comment) = body["comment"].as_str().map(|c| self.store.wiki_comment(c)).transpose()?.flatten() else {
                            return refuse(message, "e.p.msg.unknown-comment", "There is no such comment.").map(Some);
                        };
                        let moderator = self.may_in(did, &channel, "manage_messages")?;
                        if kind == "wiki-delete-comment" {
                            if comment.did != did && !moderator {
                                return not_allowed("A comment is deleted by whoever wrote it, or a moderator.");
                            }
                            self.store.delete_wiki_comment(&comment.id)?;
                        } else {
                            if !self.may_in(did, &channel, "post")? {
                                return not_allowed("Resolving takes posting in the wiki.");
                            }
                            comment.resolved = body["resolved"] != false;
                            self.store.save_wiki_comment(&comment)?;
                        }
                    }
                    _ => {}
                }
                let comments = self.comments_body(&page.id, now)?;
                let mut outcome = Outcome::reply(reply_to(message, &format!("{P}wiki-comments"), comments.clone()));
                if kind != "wiki-comments-request" {
                    for other in self.open_on(&page.id, did, now) {
                        outcome.live_to.push((other, json!({ "type": format!("{P}wiki-comments"), "id": uuid::Uuid::new_v4().to_string(), "body": comments })));
                    }
                }
                Ok(Some(outcome))
            }
            _ => Ok(None),
        }
    }

    /// A wiki page as it now reads, for the web view.
    pub fn wiki_text(&self, page: &str) -> anyhow::Result<String> {
        let (state, _) = self.store.wiki_state(page)?.unwrap_or_default();
        Ok(text_of(&doc_from((!state.is_empty()).then_some(state.as_slice()))?))
    }
}

/// Pages open, by page, each with who has it open and since when they last said.
pub type OpenPages = std::sync::Mutex<HashMap<String, HashMap<String, i64>>>;

#[cfg(test)]
mod tests {
    use super::*;
    use yrs::Text;

    /// A page as slate-yjs makes one: paragraphs embedded in `content`.
    fn page(lines: &[&str]) -> Doc {
        let doc = Doc::with_client_id(1);
        let root = doc.get_or_insert_text("content");
        {
            let mut txn = doc.transact_mut();
            for (n, line) in lines.iter().enumerate() {
                let p = root.insert_embed(&mut txn, n as u32, XmlTextPrelim::new(*line));
                p.insert_attribute(&mut txn, "type", "paragraph");
            }
        }
        doc
    }

    #[test]
    fn a_page_reads_as_its_paragraphs_and_is_made_to_say_an_old_version() {
        let doc = page(&["# Rules", "Be kind"]);
        assert_eq!(text_of(&doc), "# Rules\nBe kind");
        // Edited elsewhere, and the edit brought here as an update.
        let other = Doc::with_client_id(2);
        other.transact_mut().apply_update(Update::decode_v1(&encoded(&doc)).unwrap()).unwrap();
        let before = other.transact().state_vector();
        {
            let root = other.get_or_insert_text("content");
            let mut txn = other.transact_mut();
            let p = root.insert_embed(&mut txn, 2, XmlTextPrelim::new("No spam"));
            p.insert_attribute(&mut txn, "type", "paragraph");
        }
        let change = other.transact().encode_state_as_update_v1(&before);
        doc.transact_mut().apply_update(Update::decode_v1(&change).unwrap()).unwrap();
        assert_eq!(text_of(&doc), "# Rules\nBe kind\nNo spam");
        // Made to say the first again, by a client of its own.
        let restorer = Doc::with_client_id(3);
        restorer.transact_mut().apply_update(Update::decode_v1(&encoded(&doc)).unwrap()).unwrap();
        let back = saying(&restorer, "# Rules\nBe kind");
        doc.transact_mut().apply_update(Update::decode_v1(&back).unwrap()).unwrap();
        assert_eq!(text_of(&doc), "# Rules\nBe kind");
        assert_eq!(Update::decode_v1(&back).unwrap().state_vector().iter().map(|(c, _)| c.get()).collect::<Vec<_>>(), vec![3]);
    }
}
