//! Answering a question a post asks, as a workflow (didcomm.org's workflow/1.0).
//!
//! A post may carry a question: one to four of them, each with a few answers to
//! choose from. A member's app answers by starting the `answer-question`
//! workflow with the post's id as its context; the server is the Processor. Each
//! question is a step, drawn from the display hints `status` returns (Workflow UI
//! 1.0 items), so an app that can show any workflow can show these. The first
//! member to finish has answered it, and the answer is posted to the channel in
//! their name; anyone else part-way through is told it has been.

use serde_json::{json, Value};

use crate::community::Community;
use crate::store::{Answering, Member, Post};

pub const PIURI: &str = "https://didcomm.org/workflow/1.0";
const P: &str = "https://didcomm.org/workflow/1.0/";
pub const TEMPLATE: &str = "answer-question";
pub const VERSION: &str = "1.0";
const MAX_OTHER: usize = 2_000;

/// A refusal: the protocol's `problem-report`, with one of its codes.
#[derive(Debug)]
pub struct Refusal {
    pub code: &'static str,
    pub comment: String,
    pub args: Value,
}

impl Refusal {
    pub fn new(code: &'static str, comment: impl Into<String>) -> Self {
        Self { code, comment: comment.into(), args: json!({}) }
    }
}

pub fn failed(error: impl std::fmt::Display) -> Refusal {
    tracing::error!(%error, "answering a question failed");
    Refusal::new("action_failed", "Something went wrong here. Try again.")
}

/// An answer: a message type and its body.
pub type Answer = Result<(String, Value), Refusal>;

/// What `handle` came to, beyond the answer: a post to send to everyone.
#[derive(Debug, Default)]
pub struct Handled {
    pub answer: Option<Answer>,
    pub posted: bool,
    /// Roles or a channel's permissions changed: every joined member is sent
    /// `community` again.
    pub community_changed: bool,
}

/// The template, as `fetch-template` hands it over: two states, and one event
/// that either asks the next question or finishes.
pub fn template() -> Value {
    json!({
        "id": TEMPLATE,
        "name": "Answer a question",
        "version": VERSION,
        "instance_policy": { "mode": "multi_per_connection", "multiplicity_key": "context.question" },
        "states": [
            { "name": "asking", "type": "start" },
            { "name": "answered", "type": "final" }
        ],
        "transitions": {
            "answer": { "from": "asking", "to": "asking", "action": "record_answer" },
            "finish": { "from": "asking", "to": "answered", "action": "post_answer" }
        },
        "display_hints": { "profiles": { "receiver": { "states": { "asking": [], "answered": [] } } } }
    })
}

/// Whether a form field that is a tick box was ticked, however the app says so.
fn ticked(value: &Value) -> bool {
    match value {
        Value::Bool(ticked) => *ticked,
        Value::String(text) => matches!(text.as_str(), "true" | "on" | "yes" | "1"),
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        _ => false,
    }
}

/// What is drawn for the question `at`: its text, and a way to pick each answer
/// it offers, or to say something else.
fn asking_ui(question: &Value, at: usize, of: usize) -> Value {
    let mut items = Vec::new();
    let header = question["header"].as_str().unwrap_or("Question");
    items.push(json!({ "type": "title", "text": if of > 1 { format!("{header} ({} of {of})", at + 1) } else { header.to_string() } }));
    items.push(json!({ "type": "text", "text": question["text"] }));
    let empty = Vec::new();
    let options = question["options"].as_array().unwrap_or(&empty);
    if question["multiple"] == true {
        let mut fields = Vec::new();
        for (n, option) in options.iter().enumerate() {
            fields.push(json!({ "type": "check-box", "form-id": format!("option-{n}"), "label": option["label"] }));
            if let Some(description) = option["description"].as_str() {
                fields.push(json!({ "type": "text", "text": description }));
            }
        }
        fields.push(json!({ "type": "text-input", "form-id": "other", "label": "Something else", "placeholder": "Optional" }));
        fields.push(json!({ "type": "submit-button", "label": "Send", "event": "answer" }));
        items.push(json!({ "type": "form", "children": fields }));
    } else {
        for option in options {
            items.push(json!({ "type": "submit-button", "label": option["label"], "event": "answer", "input": { "choice": option["label"] } }));
            if let Some(description) = option["description"].as_str() {
                items.push(json!({ "type": "text", "text": description }));
            }
        }
        items.push(json!({ "type": "divider" }));
        items.push(json!({
            "type": "form",
            "children": [
                { "type": "text-input", "form-id": "other", "label": "Something else", "placeholder": "Your own answer", "required": true },
                { "type": "submit-button", "label": "Send this instead", "event": "answer" }
            ]
        }));
    }
    Value::Array(items)
}

/// One question's answer out of what a form sent, or what is wrong with it.
fn read_answer(question: &Value, input: &Value) -> Result<Value, Refusal> {
    let empty = Vec::new();
    let options = question["options"].as_array().unwrap_or(&empty);
    let label_of = |wanted: &str| options.iter().filter_map(|o| o["label"].as_str()).find(|label| *label == wanted).map(str::to_string);
    let other: String = input["other"].as_str().unwrap_or_default().trim().chars().take(MAX_OTHER).collect();
    let chosen: Vec<String> = if question["multiple"] == true {
        options.iter().enumerate().filter(|(n, _)| ticked(&input[format!("option-{n}")])).filter_map(|(_, o)| o["label"].as_str().map(str::to_string)).collect()
    } else {
        match input["choice"].as_str() {
            Some(choice) => vec![label_of(choice).ok_or_else(|| Refusal::new("action_failed", "That isn't one of the answers."))?],
            None => Vec::new(),
        }
    };
    if chosen.is_empty() && other.is_empty() {
        return Err(Refusal::new("action_failed", "Choose an answer, or write one."));
    }
    let mut answer = json!({ "chosen": chosen });
    if !other.is_empty() {
        answer["other"] = json!(other);
    }
    Ok(answer)
}

impl Community {
    fn questions_of(post: &Post) -> Vec<Value> {
        post.question.as_ref().and_then(|question| question["questions"].as_array().cloned()).unwrap_or_default()
    }

    fn name_of(&self, did: &str) -> String {
        self.store.member(did).ok().flatten().and_then(|member| member.name).unwrap_or_else(|| "someone else".to_string())
    }

    fn answering_status(&self, answering: &Answering) -> Result<Value, Refusal> {
        let post = self.store.post_by_id(&answering.post).map_err(failed)?.ok_or_else(|| Refusal::new("instance_not_found", "That question is gone."))?;
        let questions = Self::questions_of(&post);
        let done = answering.status == "completed";
        // Someone else got there first: said, whatever this one had got to.
        let beaten = post.answered_by.as_deref().filter(|by| !(done && *by == answering.owner));
        let (state, ui, allowed) = if let Some(by) = beaten {
            ("answered", json!([{ "type": "information", "text": format!("This was answered by {}.", self.name_of(by)) }]), Vec::new())
        } else if done {
            ("answered", json!([{ "type": "pass", "text": "Your answer was sent." }]), Vec::new())
        } else if answering.status == "canceled" {
            ("asking", json!([{ "type": "information", "text": "Left unanswered." }]), Vec::new())
        } else {
            match questions.get(answering.at) {
                Some(question) => ("asking", asking_ui(question, answering.at, questions.len()), vec!["answer"]),
                None => ("asking", json!([]), Vec::new()),
            }
        };
        Ok(json!({
            "instance_id": answering.id,
            "state": state,
            "status": if beaten.is_some() && !done { "completed" } else { answering.status.as_str() },
            "allowed_events": allowed,
            "action_menu": {},
            "artifacts": { "question": post.id, "channel": post.channel, "at": answering.at, "of": questions.len() },
            "ui": ui,
            "ui_profile": "receiver",
        }))
    }

    fn mine(&self, member: &Member, body: &Value) -> Result<Answering, Refusal> {
        // Someone else's is, to anyone asking, not there.
        match self.store.answering(body["instance_id"].as_str().unwrap_or_default()) {
            Ok(Some(answering)) if answering.owner == member.did => Ok(answering),
            Ok(_) => Err(Refusal::new("instance_not_found", "There is no such question being answered here.")),
            Err(error) => Err(failed(error)),
        }
    }

    fn start_answering(&self, member: &Member, message: &Value, now: i64) -> Answer {
        let body = &message["body"];
        if body["template_id"].as_str() != Some(TEMPLATE) || body["template_version"].as_str().is_some_and(|version| version != VERSION) {
            return Err(Refusal::new("template_not_found", "This server has no such workflow."));
        }
        let asked = body["context"]["question"].as_str().unwrap_or_default();
        let post = self.store.post_by_id(asked).map_err(failed)?.filter(|post| post.question.is_some()).ok_or_else(|| Refusal::new("guard_failed", "There is no such question."))?;
        let wanted = body["instance_id"].as_str().or(message["thid"].as_str()).or(message["id"].as_str()).unwrap_or_default();
        // One at a time for each member and question: starting again finds it.
        if let Some(existing) = self.store.answering_of(&member.did, &post.id).map_err(failed)? {
            if existing.id == wanted {
                return self.answering_status(&existing).map(|status| (format!("{P}status"), status));
            }
            let mut refusal = Refusal::new("multiplicity_violation", "You are answering this already.");
            refusal.args = json!({ "instance_id": existing.id });
            return Err(refusal);
        }
        if wanted.is_empty() || wanted.len() > 64 || self.store.answering(wanted).map_err(failed)?.is_some() {
            return Err(Refusal::new("multiplicity_violation", "That instance id has been used."));
        }
        let answering = Answering { id: wanted.to_string(), owner: member.did.clone(), post: post.id.clone(), at: 0, answers: Vec::new(), status: "active".to_string(), updated: now };
        self.store.save_answering(&answering).map_err(failed)?;
        self.answering_status(&answering).map(|status| (format!("{P}status"), status))
    }

    fn advance_answering(&self, member: &Member, body: &Value, now: i64) -> Result<(Answer, bool), Refusal> {
        let mut answering = self.mine(member, body)?;
        let post = self.store.post_by_id(&answering.post).map_err(failed)?.ok_or_else(|| Refusal::new("instance_not_found", "That question is gone."))?;
        let questions = Self::questions_of(&post);
        // Finished, cancelled, or answered by someone else: nothing more to take.
        if answering.status != "active" || post.answered_by.is_some() {
            return Ok((self.answering_status(&answering).map(|status| (format!("{P}status"), status)), false));
        }
        if body["event"].as_str() != Some("answer") {
            return Err(Refusal::new("guard_failed", "That can't be done here."));
        }
        let Some(question) = questions.get(answering.at) else {
            return Err(Refusal::new("guard_failed", "There is nothing left to answer."));
        };
        answering.answers.push(read_answer(question, &body["input"])?);
        answering.at += 1;
        answering.updated = now;
        let mut posted = false;
        if answering.at >= questions.len() {
            // The first to finish has answered it; anyone later finds it answered.
            posted = self.answer_question(member, &post, &answering.answers, now).map_err(failed)?.is_some();
            if posted {
                answering.status = "completed".to_string();
            }
        }
        self.store.save_answering(&answering).map_err(failed)?;
        let status = self.answering_status(&answering)?;
        let kind = if answering.status == "completed" { "complete" } else { "status" };
        Ok((Ok((format!("{P}{kind}"), status)), posted))
    }

    /// Acts on one workflow message from `member`: answering a question, or one of
    /// the administration workflows (`crate::admin_flows`). An answer of `None` is
    /// for a message that isn't this protocol's.
    pub fn handle_workflow(&self, member: &Member, message: &Value, now: i64) -> Handled {
        let Some(kind) = message["type"].as_str().and_then(|message_type| message_type.strip_prefix(P)) else {
            return Handled::default();
        };
        let body = &message["body"];
        let instance = body["instance_id"].as_str().unwrap_or_default();
        // An administration workflow under way, if the instance is one.
        let flow = match kind {
            "status" | "advance" | "cancel" => match self.own_flow(&member.did, instance) {
                Ok(flow) => flow,
                Err(refusal) => return Handled { answer: Some(Err(refusal)), ..Handled::default() },
            },
            _ => None,
        };
        if let Some(flow) = flow {
            let answer = match kind {
                "status" => self.flow_status_answer(&flow),
                "cancel" => self.cancel_flow(flow, now),
                _ => match self.advance_flow(flow, body, now) {
                    Ok((answer, community_changed)) => return Handled { answer: Some(answer), posted: false, community_changed },
                    Err(refusal) => Err(refusal),
                },
            };
            return Handled { answer: Some(answer), ..Handled::default() };
        }
        let status_of = |answering: Result<Answering, Refusal>| answering.and_then(|a| self.answering_status(&a)).map(|status| (format!("{P}status"), status));
        let answer = match kind {
            "discover" => {
                let mut workflows = vec![json!({ "template_id": TEMPLATE, "versions": [VERSION], "title": "Answer a question" })];
                match self.flows_for(&member.did) {
                    Ok(flows) => workflows.extend(flows.into_iter().map(|(id, title)| json!({ "template_id": id, "versions": [crate::admin_flows::VERSION], "title": title }))),
                    Err(error) => return Handled { answer: Some(Err(failed(error))), ..Handled::default() },
                }
                Ok((format!("{P}workflows"), json!({ "paging": { "total": workflows.len(), "next_offset": 0 }, "workflows": workflows })))
            }
            "fetch-template" => match body["template_id"].as_str() {
                Some(TEMPLATE) => Ok((format!("{P}template"), json!({ "template": template() }))),
                Some(id) => match crate::admin_flows::template(id) {
                    Some(template) => Ok((format!("{P}template"), json!({ "template": template }))),
                    None => Err(Refusal::new("not_found_remote_template", "This server has no such workflow.")),
                },
                None => Err(Refusal::new("not_found_remote_template", "This server has no such workflow.")),
            },
            "start" if body["template_id"].as_str().is_some_and(crate::admin_flows::is_template) => self.start_flow(member, message, now),
            "start" => self.start_answering(member, message, now),
            "status" => status_of(self.mine(member, body)),
            "advance" => match self.advance_answering(member, body, now) {
                Ok((answer, posted)) => return Handled { answer: Some(answer), posted, community_changed: false },
                Err(refusal) => Err(refusal),
            },
            "cancel" => status_of(self.mine(member, body).and_then(|mut answering| {
                if answering.status == "active" {
                    answering.status = "canceled".to_string();
                    answering.updated = now;
                    self.store.save_answering(&answering).map_err(failed)?;
                }
                Ok(answering)
            })),
            "pause" | "resume" => Err(Refusal::new("guard_failed", "These are done or left; they can't be paused.")),
            "publish-template" => Err(Refusal::new("not_authorized", "This server runs its own workflows, and takes no others.")),
            _ => return Handled::default(),
        };
        Handled { answer: Some(answer), ..Handled::default() }
    }
}
