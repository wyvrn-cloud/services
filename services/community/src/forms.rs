//! Form channels (community/1.0's "Form channels", asked 2026-11-18): a channel
//! that is one form members fill in (an application, a survey, a sign-up, a
//! ticket), filled in with survey/1.0, its answers worked through by whoever
//! manages posts in it.
//!
//! The form is kept as the fields an admin laid out; survey/1.0's
//! `survey_schema` (JSON Schema) and `ui_schema` (JSON Forms' vertical layout)
//! are made from them each time it is asked for, and an answer is checked
//! against the fields themselves.

use serde_json::{json, Map, Value};

use crate::community::{line, refuse, reply_to, Community, Outcome};
use crate::store::{Channel, FormAnswer, Member};

pub const FORM_REQUEST: &str = "https://wyvrn.app/community/1.0/form-request";
pub const FORM_ANSWERED: &str = "https://wyvrn.app/community/1.0/form-answered";
pub const FORM_ANSWERS_REQUEST: &str = "https://wyvrn.app/community/1.0/form-answers-request";
pub const MY_ANSWERS_REQUEST: &str = "https://wyvrn.app/community/1.0/my-answers-request";
pub const FORM_ANSWERS: &str = "https://wyvrn.app/community/1.0/form-answers";
pub const REVIEW_ANSWER: &str = "https://wyvrn.app/community/1.0/review-answer";
pub const SURVEY_REQUEST: &str = "https://didcomm.org/survey/1.0/request";
pub const SURVEY_RESPONSE: &str = "https://didcomm.org/survey/1.0/response";

const TYPES: &[&str] = &["application", "survey", "signup", "ticket"];
const KINDS: &[&str] = &["text", "paragraph", "choice", "choices", "number", "yes_no", "date"];
const MAX_FIELDS: usize = 25;
const MAX_OPTIONS: usize = 25;
const MAX_NOTE: usize = 2000;
const MAX_NOTES: usize = 200;
/// How long a text answer may be: a line, and a paragraph.
const MAX_LINE: usize = 200;
const MAX_PARAGRAPH: usize = 4000;

/// A form as an admin gives it, checked: what is wrong with it, if anything.
pub fn read_form(given: &Value, channel_exists: &dyn Fn(&str) -> bool, role_exists: &dyn Fn(&str) -> bool) -> Result<Value, String> {
    let kind = given["type"].as_str().filter(|t| TYPES.contains(t)).ok_or("A form is an application, a survey, a sign-up or a ticket.")?;
    let title = line(&given["title"], 100).ok_or("A form needs a title.")?;
    let description = given["description"].as_str().map(str::trim).unwrap_or_default();
    if description.chars().count() > 2000 {
        return Err("A form's description is up to 2,000 characters.".into());
    }
    let mut fields = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for (n, field) in given["fields"].as_array().into_iter().flatten().enumerate() {
        let label = line(&field["label"], 200).ok_or("Each question needs its words.")?;
        let kind = field["kind"].as_str().filter(|k| KINDS.contains(k)).ok_or("A question is text, a paragraph, a choice, choices, a number, yes or no, or a date.")?;
        let id = field["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')).map(str::to_string).unwrap_or_else(|| format!("q{}", n + 1));
        if !ids.insert(id.clone()) {
            return Err("Two questions have the same id.".into());
        }
        let mut out = json!({ "id": id, "label": label, "kind": kind });
        if field["required"] == true {
            out["required"] = json!(true);
        }
        if kind == "choice" || kind == "choices" {
            let options: Vec<String> = field["options"].as_array().into_iter().flatten().filter_map(|o| line(o, 100)).take(MAX_OPTIONS).collect();
            if options.len() < 2 {
                return Err(format!("“{label}” offers two choices at least."));
            }
            out["options"] = json!(options);
        }
        fields.push(out);
    }
    if fields.is_empty() || fields.len() > MAX_FIELDS {
        return Err("A form asks one to 25 questions.".into());
    }
    let mut form = json!({ "type": kind, "title": title, "fields": fields });
    if !description.is_empty() {
        form["description"] = json!(description);
    }
    if kind == "survey" && given["anonymous"] == true {
        form["anonymous"] = json!(true);
    }
    if kind == "signup" {
        form["places"] = json!(given["places"].as_i64().filter(|n| (1..=100_000).contains(n)).ok_or("A sign-up has a number of places.")?);
    }
    if let Some(at) = given["closes_at"].as_i64() {
        form["closes_at"] = json!(at);
    }
    if kind == "application" {
        if let Some(role) = given["role"].as_str().filter(|r| !r.is_empty()) {
            if !role_exists(role) {
                return Err("There is no such role.".into());
            }
            form["role"] = json!(role);
        }
    }
    if let Some(channel) = given["post_to"].as_str().filter(|c| !c.is_empty()) {
        if !channel_exists(channel) {
            return Err("Answers are posted in a text channel here.".into());
        }
        form["post_to"] = json!(channel);
    }
    Ok(form)
}

/// survey/1.0's `survey_schema` and `ui_schema` for `form`.
pub fn survey_of(form: &Value) -> (Value, Value) {
    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut elements = Vec::new();
    for field in form["fields"].as_array().into_iter().flatten() {
        let id = field["id"].as_str().unwrap_or_default();
        let schema = match field["kind"].as_str().unwrap_or_default() {
            "paragraph" => json!({ "type": "string", "maxLength": MAX_PARAGRAPH }),
            "choice" => json!({ "type": "string", "enum": field["options"] }),
            "choices" => json!({ "type": "array", "items": { "type": "string", "enum": field["options"] }, "uniqueItems": true }),
            "number" => json!({ "type": "number" }),
            "yes_no" => json!({ "type": "boolean" }),
            "date" => json!({ "type": "string", "format": "date" }),
            _ => json!({ "type": "string", "maxLength": MAX_LINE }),
        };
        let mut schema = schema;
        schema["title"] = field["label"].clone();
        properties.insert(id.to_string(), schema);
        if field["required"] == true {
            required.push(json!(id));
        }
        let mut control = json!({ "type": "Control", "scope": format!("#/properties/{id}"), "label": field["label"] });
        if field["kind"] == "paragraph" {
            control["options"] = json!({ "multi": true });
        }
        elements.push(control);
    }
    (json!({ "type": "object", "properties": properties, "required": required }), json!({ "type": "VerticalLayout", "elements": elements }))
}

/// `data` checked against `form`'s fields: only what they ask, as they ask it.
pub fn read_answer(form: &Value, data: &Value) -> Result<Value, String> {
    let mut out = Map::new();
    for field in form["fields"].as_array().into_iter().flatten() {
        let id = field["id"].as_str().unwrap_or_default();
        let label = field["label"].as_str().unwrap_or(id);
        let given = &data[id];
        let empty = given.is_null() || given.as_str().is_some_and(|s| s.trim().is_empty()) || given.as_array().is_some_and(Vec::is_empty);
        if empty {
            if field["required"] == true {
                return Err(format!("“{label}” needs an answer."));
            }
            continue;
        }
        let options: Vec<&str> = field["options"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let value = match field["kind"].as_str().unwrap_or_default() {
            "paragraph" => given.as_str().map(str::trim).filter(|s| s.chars().count() <= MAX_PARAGRAPH).map(|s| json!(s)),
            "text" => given.as_str().map(str::trim).filter(|s| s.chars().count() <= MAX_LINE && !s.contains('\n')).map(|s| json!(s)),
            "choice" => given.as_str().filter(|s| options.contains(s)).map(|s| json!(s)),
            "choices" => given.as_array().filter(|a| a.iter().all(|c| c.as_str().is_some_and(|c| options.contains(&c)))).map(|a| {
                let mut chosen: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
                chosen.dedup();
                json!(chosen)
            }),
            "number" => given.as_f64().filter(|n| n.is_finite()).map(|n| json!(n)),
            "yes_no" => given.as_bool().map(|b| json!(b)),
            "date" => given.as_str().filter(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()).map(|s| json!(s)),
            _ => None,
        };
        out.insert(id.to_string(), value.ok_or_else(|| format!("“{label}” isn't answered as it asks."))?);
    }
    Ok(Value::Object(out))
}

/// A survey's summary: for each choice and yes/no, how many chose each; for each number, the mean.
pub fn summary(form: &Value, answers: &[FormAnswer]) -> Value {
    let mut out = Map::new();
    for field in form["fields"].as_array().into_iter().flatten() {
        let id = field["id"].as_str().unwrap_or_default();
        let given: Vec<&Value> = answers.iter().map(|a| &a.data[id]).filter(|v| !v.is_null()).collect();
        let value = match field["kind"].as_str().unwrap_or_default() {
            "choice" | "choices" => {
                let mut counts = Map::new();
                for option in field["options"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    let n = given.iter().filter(|v| v.as_str() == Some(option) || v.as_array().is_some_and(|a| a.iter().any(|c| c.as_str() == Some(option)))).count();
                    counts.insert(option.to_string(), json!(n));
                }
                json!({ "counts": counts })
            }
            "yes_no" => json!({ "counts": { "yes": given.iter().filter(|v| v.as_bool() == Some(true)).count(), "no": given.iter().filter(|v| v.as_bool() == Some(false)).count() } }),
            "number" => {
                let numbers: Vec<f64> = given.iter().filter_map(|v| v.as_f64()).collect();
                json!({ "mean": if numbers.is_empty() { Value::Null } else { json!(numbers.iter().sum::<f64>() / numbers.len() as f64) }, "count": numbers.len() })
            }
            _ => json!({ "count": given.len() }),
        };
        out.insert(id.to_string(), value);
    }
    Value::Object(out)
}

impl Community {
    /// A form as an admin gives it, checked against this community's channels and roles.
    /// `editor` may only make a form grant a role ranked below their own, as they
    /// could give it by hand (security review WYV-04).
    pub(crate) fn checked_form(&self, given: &Value, editor: &str) -> Result<Value, String> {
        let channels = self.store.channels().map_err(|e| e.to_string())?;
        let roles = self.store.roles().map_err(|e| e.to_string())?;
        let form = read_form(given, &|id| channels.iter().any(|c| c.id == id && c.voice.is_none() && c.form.is_none() && !c.calendar), &|id| roles.iter().any(|r| r.id == id && r.id != "everyone"))?;
        if let Some(role) = form["role"].as_str().and_then(|id| roles.iter().find(|r| r.id == id)) {
            if !self.powers(editor).map_err(|e| e.to_string())?.above(role.position) {
                return Err("A form can only give a role ranked below yours.".into());
            }
        }
        Ok(form)
    }

    /// An answer as `to` is shown it: who answered left out of an anonymous survey's, for its reviewers.
    fn answer_body(&self, form: &Value, answer: &FormAnswer, to: &str, now: i64) -> anyhow::Result<Value> {
        let mut body = json!({ "id": answer.id, "channel": answer.channel, "data": answer.data, "status": answer.status, "at": answer.at });
        if form["anonymous"] != true || answer.did == to {
            body["member"] = json!(self.store.member_id(&answer.did, now)?);
        }
        let notes: Vec<Value> = answer
            .notes
            .iter()
            .map(|n| json!({ "by": self.store.member_id(n["by"].as_str().unwrap_or_default(), now).unwrap_or_default(), "text": n["text"], "at": n["at"] }))
            .collect();
        if !notes.is_empty() {
            body["notes"] = json!(notes);
        }
        Ok(body)
    }

    /// The form channel `body` names, and its form, if `did` sees it.
    fn form_channel(&self, did: &str, id: Option<&str>) -> anyhow::Result<Option<(Channel, Value)>> {
        let Some(channel) = id.map(|id| self.store.channel(id)).transpose()?.flatten() else { return Ok(None) };
        let Some(form) = channel.form.clone() else { return Ok(None) };
        if !self.sees_id(did, &channel.id)? {
            return Ok(None);
        }
        Ok(Some((channel, form)))
    }

    /// `form-answered` for `did`, unasked.
    fn answered_notice(&self, form: &Value, answer: &FormAnswer, did: &str, now: i64) -> anyhow::Result<(String, Value)> {
        Ok((did.to_string(), json!({ "type": FORM_ANSWERED, "id": uuid::Uuid::new_v4().to_string(), "body": { "channel": answer.channel, "answer": self.answer_body(form, answer, did, now)? } })))
    }

    /// Who reviews answers in `channel`: whoever manages posts there.
    fn reviewers(&self, channel: &Channel) -> anyhow::Result<Vec<String>> {
        let mut out = Vec::new();
        for member in self.store.joined()? {
            if self.may_in(&member.did, channel, "manage_messages")? {
                out.push(member.did);
            }
        }
        Ok(out)
    }

    /// What a form channel adds to `community`'s entry for it, for `did`.
    pub(crate) fn form_entry(&self, channel: &Channel, did: &str) -> anyhow::Result<Option<Value>> {
        let Some(form) = &channel.form else { return Ok(None) };
        let answers = self.store.form_answers(&channel.id)?;
        let mut entry = json!({ "form": form, "answered": answers.iter().filter(|a| a.did == did).count() });
        if form["type"] == "signup" {
            entry["taken"] = json!(answers.iter().filter(|a| a.status != "rejected").count());
        }
        Ok(Some(entry))
    }

    /// The messages of form channels, and survey/1.0's `response`, from a member.
    pub fn handle_form_message(&self, member: &Member, message: &Value, now: i64) -> anyhow::Result<Option<Outcome>> {
        let message_type = message["type"].as_str().unwrap_or_default();
        let body = &message["body"];
        let did = member.did.as_str();
        let unknown = || refuse(message, "e.p.msg.unknown-channel", "There is no such form.").map(Some);
        match message_type {
            FORM_REQUEST => {
                let Some((channel, form)) = self.form_channel(did, body["channel"].as_str())? else { return unknown() };
                let (schema, ui) = survey_of(&form);
                let request = json!({
                    "type": SURVEY_REQUEST,
                    "id": format!("form|{}|{}", channel.id, uuid::Uuid::new_v4().simple()),
                    "thid": message["id"],
                    "body": { "request": { "survey_id": channel.id, "survey_schema": schema, "ui_schema": ui, "i18n_data": { "title": form["title"], "description": form["description"] } } }
                });
                Ok(Some(Outcome::reply(request)))
            }
            SURVEY_RESPONSE => {
                // The request it answers says which form: `form|<channel>|<nonce>`.
                let thid = message["thid"].as_str().unwrap_or_default();
                let Some(channel_id) = thid.strip_prefix("form|").and_then(|rest| rest.rsplit_once('|')).map(|(c, _)| c) else {
                    return Ok(None);
                };
                let Some((channel, form)) = self.form_channel(did, Some(channel_id))? else { return unknown() };
                let response = &body["response"];
                if response["response_type"] != "data" {
                    return Ok(Some(Outcome::default()));
                }
                if form["closes_at"].as_i64().is_some_and(|at| at <= now) || self.channel_off(&channel) {
                    return refuse(message, "e.p.msg.form-closed", "This form no longer takes answers.").map(Some);
                }
                let answers = self.store.form_answers(&channel.id)?;
                let kind = form["type"].as_str().unwrap_or_default();
                if kind != "ticket" && answers.iter().any(|a| a.did == did) {
                    return refuse(message, "e.p.msg.already-answered", "You have answered this form already.").map(Some);
                }
                if kind == "signup" && answers.iter().filter(|a| a.status != "rejected").count() as i64 >= form["places"].as_i64().unwrap_or(0) {
                    return refuse(message, "e.p.msg.form-full", "Every place is taken.").map(Some);
                }
                let data = match read_answer(&form, &response["data"]) {
                    Ok(data) => data,
                    Err(why) => return refuse(message, "e.p.msg.invalid-answer", &why).map(Some),
                };
                let answer = FormAnswer { id: uuid::Uuid::new_v4().simple().to_string()[..16].to_string(), channel: channel.id.clone(), did: did.to_string(), data, status: "open".into(), notes: Vec::new(), at: now };
                self.store.save_form_answer(&answer)?;
                let mut outcome = Outcome::reply(reply_to(message, FORM_ANSWERED, json!({ "channel": channel.id, "answer": self.answer_body(&form, &answer, did, now)? })));
                for reviewer in self.reviewers(&channel)?.into_iter().filter(|r| r != did) {
                    outcome.notices.push(self.answered_notice(&form, &answer, &reviewer, now)?);
                }
                // Posted where its maker said, as the community's.
                if let Some(post_to) = form["post_to"].as_str().and_then(|c| self.store.channel(c).ok().flatten()) {
                    let who = if form["anonymous"] == true { "Someone".to_string() } else { member.name.clone().unwrap_or_else(|| "A member".into()) };
                    let lines: Vec<String> = form["fields"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|f| {
                            let value = &answer.data[f["id"].as_str().unwrap_or_default()];
                            let shown = match value {
                                Value::Null => return None,
                                Value::String(s) => s.clone(),
                                Value::Bool(b) => if *b { "Yes".into() } else { "No".into() },
                                Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
                                other => other.to_string(),
                            };
                            Some(format!("**{}**\n{}", f["label"].as_str().unwrap_or_default(), shown))
                        })
                        .collect();
                    let content = format!("📝 **{}** answered by {who}\n\n{}", form["title"].as_str().unwrap_or("A form"), lines.join("\n\n"));
                    if let Some(own) = self.own.clone() {
                        self.store.admit(&own, crate::store::Role::Member, now)?;
                        if let Some(mut author) = self.store.member(&own)? {
                            author.name = Some(form["title"].as_str().unwrap_or("Form").to_string());
                            let content: String = content.chars().take(self.config.max_post_length).collect();
                            self.store.add_post(&post_to.id, &uuid::Uuid::new_v4().to_string(), &content, &author, None, None, now)?;
                            outcome.posted = true;
                        }
                    }
                }
                outcome.channels_changed = kind == "signup";
                Ok(Some(outcome))
            }
            FORM_ANSWERS_REQUEST | MY_ANSWERS_REQUEST => {
                let Some((channel, form)) = self.form_channel(did, body["channel"].as_str())? else { return unknown() };
                let reviewing = message_type == FORM_ANSWERS_REQUEST;
                if reviewing && !self.may_in(did, &channel, "manage_messages")? {
                    return refuse(message, "e.p.trust.not-authorized", "Answers are read by whoever manages posts in the form's channel.").map(Some);
                }
                let all = self.store.form_answers(&channel.id)?;
                let chosen: Vec<&FormAnswer> = all.iter().filter(|a| (reviewing || a.did == did) && body["status"].as_str().is_none_or(|s| a.status == s)).collect();
                let mut out = json!({ "channel": channel.id, "answers": chosen.iter().map(|a| self.answer_body(&form, a, did, now)).collect::<anyhow::Result<Vec<_>>>()? });
                if reviewing && form["type"] == "survey" {
                    out["summary"] = summary(&form, &all);
                }
                Ok(Some(Outcome::reply(reply_to(message, FORM_ANSWERS, out))))
            }
            REVIEW_ANSWER => {
                let Some(mut answer) = body["answer"].as_str().map(|id| self.store.form_answer(id)).transpose()?.flatten() else {
                    return refuse(message, "e.p.msg.unknown-answer", "There is no such answer.").map(Some);
                };
                let Some((channel, form)) = self.form_channel(did, Some(&answer.channel))? else {
                    return refuse(message, "e.p.msg.unknown-answer", "There is no such answer.").map(Some);
                };
                let reviewer = self.may_in(did, &channel, "manage_messages")?;
                let own_ticket = answer.did == did && form["type"] == "ticket";
                if !reviewer && !own_ticket {
                    return refuse(message, "e.p.trust.not-authorized", "Answers are reviewed by whoever manages posts in the form's channel.").map(Some);
                }
                if let Some(status) = body["status"].as_str() {
                    if !reviewer || !["open", "accepted", "rejected"].contains(&status) {
                        return refuse(message, "e.p.trust.not-authorized", "Only a reviewer accepts or rejects an answer.").map(Some);
                    }
                    if status == "accepted" && answer.status != "accepted" {
                        if let Some(role) = form["role"].as_str() {
                            // As giving the role by hand: to someone else, ranked below the reviewer.
                            if answer.did == did {
                                return refuse(message, "e.p.trust.not-authorized", "Someone else accepts your own application.").map(Some);
                            }
                            let position = self.store.role(role)?.map(|r| r.position).unwrap_or(i64::MAX);
                            if !self.powers(did)?.above(position) {
                                return refuse(message, "e.p.trust.not-authorized", "Accepting this gives a role ranked at or above yours.").map(Some);
                            }
                            if self.store.member(&answer.did)?.is_some() {
                                self.store.give_role(&answer.did, role)?;
                            }
                        }
                    }
                    answer.status = status.to_string();
                }
                if let Some(note) = body["note"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
                    if note.chars().count() > MAX_NOTE || answer.notes.len() >= MAX_NOTES {
                        return refuse(message, "e.p.msg.too-long", "A note is up to 2,000 characters.").map(Some);
                    }
                    answer.notes.push(json!({ "by": did, "text": note, "at": now }));
                }
                self.store.save_form_answer(&answer)?;
                self.audit(now, did, "form.review", Some(&answer.channel), Some(json!({ "answer": answer.id, "status": answer.status })));
                let mut outcome = Outcome::reply(reply_to(message, FORM_ANSWERED, json!({ "channel": channel.id, "answer": self.answer_body(&form, &answer, did, now)? })));
                // The member, and the other reviewers, told.
                let mut told: Vec<String> = self.reviewers(&channel)?;
                told.push(answer.did.clone());
                told.sort();
                told.dedup();
                for who in told.into_iter().filter(|w| w != did) {
                    outcome.notices.push(self.answered_notice(&form, &answer, &who, now)?);
                }
                outcome.channels_changed = body["status"].is_string() && (form["role"].is_string() || form["type"] == "signup");
                Ok(Some(outcome))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Value {
        read_form(
            &json!({ "type": "application", "title": "Join the mods", "fields": [
                { "label": "Why?", "kind": "paragraph", "required": true },
                { "label": "Time zone", "kind": "choice", "options": ["Americas", "Europe", "Asia"] },
                { "label": "Hours a week", "kind": "number" },
                { "label": "Over 18", "kind": "yes_no", "required": true }
            ], "role": "moderator" }),
            &|_| true,
            &|r| r == "moderator",
        )
        .unwrap()
    }

    #[test]
    fn a_form_is_checked_and_becomes_json_forms() {
        let form = form();
        assert_eq!(form["fields"][0]["id"], "q1");
        let (schema, ui) = survey_of(&form);
        assert_eq!(schema["properties"]["q2"]["enum"], json!(["Americas", "Europe", "Asia"]));
        assert_eq!(schema["required"], json!(["q1", "q4"]));
        assert_eq!(ui["elements"][0], json!({ "type": "Control", "scope": "#/properties/q1", "label": "Why?", "options": { "multi": true } }));
        for bad in [json!({ "type": "poll", "title": "x", "fields": [{ "label": "a", "kind": "text" }] }), json!({ "type": "survey", "title": "x", "fields": [] }), json!({ "type": "survey", "title": "x", "fields": [{ "label": "a", "kind": "choice", "options": ["one"] }] }), json!({ "type": "signup", "title": "x", "fields": [{ "label": "a", "kind": "text" }] })] {
            assert!(read_form(&bad, &|_| true, &|_| true).is_err(), "{bad}");
        }
        assert!(read_form(&json!({ "type": "application", "title": "x", "fields": [{ "label": "a", "kind": "text" }], "role": "nope" }), &|_| true, &|_| false).is_err());
    }

    #[test]
    fn an_answer_is_only_what_the_form_asks_as_it_asks_it() {
        let form = form();
        let answer = read_answer(&form, &json!({ "q1": " Because ", "q2": "Europe", "q4": true, "extra": "dropped" })).unwrap();
        assert_eq!(answer, json!({ "q1": "Because", "q2": "Europe", "q4": true }));
        assert!(read_answer(&form, &json!({ "q4": true })).unwrap_err().contains("Why?"));
        assert!(read_answer(&form, &json!({ "q1": "x", "q2": "Mars", "q4": true })).is_err());
        assert!(read_answer(&form, &json!({ "q1": "x", "q3": "lots", "q4": true })).is_err());
    }

    #[test]
    fn a_surveys_summary_counts_choices_and_averages_numbers() {
        let form = form();
        let answer = |data: Value| FormAnswer { id: "a".into(), channel: "c".into(), did: "d".into(), data, status: "open".into(), notes: vec![], at: 0 };
        let got = summary(&form, &[answer(json!({ "q2": "Europe", "q3": 4, "q4": true })), answer(json!({ "q2": "Asia", "q3": 8, "q4": false })), answer(json!({ "q2": "Europe", "q4": true }))]);
        assert_eq!(got["q2"]["counts"], json!({ "Americas": 0, "Europe": 2, "Asia": 1 }));
        assert_eq!(got["q3"]["mean"], 6.0);
        assert_eq!(got["q4"]["counts"], json!({ "yes": 2, "no": 1 }));
    }
}
