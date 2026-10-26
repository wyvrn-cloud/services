//! The join gates (community/1.0's "Join gates"): questions on joining, a wait
//! before posting, and a credential. What is set is kept as one setting, `gates`,
//! as JSON: `{ questions: [{ id, text, required, options? }], post_wait? }`.

use serde_json::{json, Value};

use crate::community::Community;

/// The time now, in seconds since 1970.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl Community {
    /// The gates as last set: `{}` with none.
    pub fn gates(&self) -> anyhow::Result<Value> {
        Ok(self.store.setting("gates")?.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_else(|| json!({})))
    }

    /// How many minutes a new member waits to post, if they do.
    pub fn post_wait(&self) -> anyhow::Result<Option<i64>> {
        Ok(self.gates()?["post_wait"].as_i64().filter(|minutes| *minutes > 0))
    }
}

pub const JOIN_QUESTIONS: &str = "https://wyvrn.app/community/1.0/join-questions";
/// As many questions as may be asked on joining, and as long as an answer may be.
pub const MAX_QUESTIONS: usize = 5;
pub const MAX_ANSWER: usize = 500;

impl Community {
    /// The questions on joining, as `join-questions` lists them.
    pub fn join_questions(&self) -> anyhow::Result<Vec<Value>> {
        Ok(self.gates()?["questions"].as_array().cloned().unwrap_or_default())
    }

    /// Where there are questions on joining: `join-questions` for a `join` without
    /// answers, a refusal for answers that don't do, or `None` once they do (and
    /// are kept).
    pub fn questions_before_join(&self, sender: &str, message: &Value, now: i64) -> anyhow::Result<Option<crate::community::Outcome>> {
        let questions = self.join_questions()?;
        if questions.is_empty() {
            return Ok(None);
        }
        let given = &message["body"]["answers"];
        if !given.is_object() {
            let name = self.changed().name.unwrap_or_else(|| self.config.name.clone());
            let asked = crate::community::reply_to(message, JOIN_QUESTIONS, json!({ "name": name, "questions": questions }));
            return Ok(Some(crate::community::Outcome::reply(asked)));
        }
        let mut answers = Vec::new();
        for question in &questions {
            let id = question["id"].as_str().unwrap_or_default();
            let answer: String = given[id].as_str().unwrap_or_default().trim().chars().take(MAX_ANSWER).collect();
            let offered = question["options"].as_array();
            let fine = match offered {
                Some(options) => answer.is_empty() || options.iter().any(|option| option.as_str() == Some(answer.as_str())),
                None => true,
            };
            if !fine || (answer.is_empty() && question["required"] == true) {
                return crate::community::refuse(message, "e.p.msg.invalid-answers", "Answer every question that needs one, from the answers offered.").map(Some);
            }
            if !answer.is_empty() {
                answers.push(json!({ "question": question["text"], "answer": answer }));
            }
        }
        self.store.set_join_answers(sender, &Value::Array(answers), now)?;
        Ok(None)
    }
}
