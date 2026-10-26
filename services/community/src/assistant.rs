//! An AI assistant answering in a community's channels: what `wyvrn-claude` is
//! (services/claude), a community server whose every channel is a room with a
//! Claude session in it. Each post in a room is answered, in its thread where it
//! is in one, from that room's (or thread's) recent posts, through Anthropic's
//! Messages API with the operator's key. The answer is a post of the server's own
//! DID, under the assistant's name (community/1.0's `assistant`).
//!
//! Who may ask is who the community lets in (its admission, invites and
//! members), and how much: so many requests a member a day, and so many tokens a
//! 30-day period for the whole service, counted in the store's settings.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::store::Post;

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantConfig {
    /// What its posts are signed as.
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// The Messages API (`https://api.anthropic.com/v1/messages`; a stand-in in tests).
    #[serde(default = "default_api_url")]
    pub api_url: String,
    /// The operator's API key. Usually from the environment (`ANTHROPIC_API_KEY`).
    #[serde(default)]
    pub api_key: String,
    /// What it is told before every room's conversation, besides where it is.
    #[serde(default)]
    pub system: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// How many of a room's latest posts it is given.
    #[serde(default = "default_context_posts")]
    pub context_posts: usize,
    /// Requests one member may make a day; 0 for no limit.
    #[serde(default = "default_daily_requests")]
    pub daily_requests_per_member: u32,
    /// Tokens (in and out) the service may use in 30 days; 0 for no limit.
    #[serde(default)]
    pub period_tokens: u64,
}

fn default_name() -> String {
    "Claude".into()
}
fn default_model() -> String {
    "claude-sonnet-5-5".into()
}
fn default_api_url() -> String {
    "https://api.anthropic.com/v1/messages".into()
}
fn default_max_tokens() -> u32 {
    2048
}
fn default_context_posts() -> usize {
    40
}
fn default_daily_requests() -> u32 {
    50
}

impl AssistantConfig {
    /// Every setting as it is when none is given.
    pub fn from_defaults() -> Self {
        serde_json::from_value(json!({})).expect("every field has a default")
    }
}

/// How long the counted period of tokens is.
pub const PERIOD_SECS: i64 = 30 * 24 * 60 * 60;

pub struct Assistant {
    pub config: AssistantConfig,
    http: reqwest::Client,
}

/// One turn of a conversation as the Messages API takes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub assistant: bool,
    pub text: String,
}

impl Assistant {
    pub fn new(config: AssistantConfig) -> Self {
        Self { config, http: reqwest::Client::new() }
    }

    /// What `community` says of it (community/1.0's `assistant`).
    pub fn listing(&self) -> Value {
        json!({ "name": self.config.name, "provider": "Anthropic", "model": self.config.model })
    }

    /// Why a member's request isn't asked, if it isn't: their day's requests, or
    /// the service's period of tokens, used up. `used` is theirs today, `spent`
    /// the service's this period.
    pub fn refusal(&self, used: u32, spent: u64) -> Option<&'static str> {
        if self.config.daily_requests_per_member > 0 && used >= self.config.daily_requests_per_member {
            return Some("You’ve asked as much as this service allows in a day. Ask again tomorrow.");
        }
        if self.config.period_tokens > 0 && spent >= self.config.period_tokens {
            return Some("This service has used all it may for now. An admin can raise its limit.");
        }
        None
    }

    /// Asks the model, in room `room`, the conversation `turns`. Answers what it
    /// said and how many tokens that took.
    pub async fn ask(&self, room: &str, turns: &[Turn]) -> anyhow::Result<(String, u64)> {
        if self.config.api_key.is_empty() {
            anyhow::bail!("no API key is set (ANTHROPIC_API_KEY)");
        }
        let mut system = format!(
            "You are {}, an assistant in a chat room called \"{room}\". Several people may talk in it: each of their lines starts with their name. Answer as a chat message, in Markdown.",
            self.config.name
        );
        if !self.config.system.trim().is_empty() {
            system = format!("{system}\n\n{}", self.config.system.trim());
        }
        let messages: Vec<Value> = turns.iter().map(|turn| json!({ "role": if turn.assistant { "assistant" } else { "user" }, "content": turn.text })).collect();
        let response = self
            .http
            .post(&self.config.api_url)
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&json!({ "model": self.config.model, "max_tokens": self.config.max_tokens, "system": system, "messages": messages }))
            .send()
            .await?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            anyhow::bail!("the model answered {status}: {}", body["error"]["message"].as_str().unwrap_or("no reason given"));
        }
        let text = body["content"]
            .as_array()
            .map(|parts| parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join(""))
            .unwrap_or_default();
        let tokens = body["usage"]["input_tokens"].as_u64().unwrap_or(0) + body["usage"]["output_tokens"].as_u64().unwrap_or(0);
        if text.trim().is_empty() {
            anyhow::bail!("the model said nothing");
        }
        Ok((text, tokens))
    }
}

/// A room's posts, oldest first, as turns: each person's line under their name,
/// the assistant's own (`own`, the server's DID) as its turns, consecutive lines
/// of people in one turn. It begins with someone's, as the API needs.
pub fn turns_of(posts: &[Post], own: &str) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    for post in posts.iter().filter(|post| !post.deleted && !post.content.trim().is_empty()) {
        let assistant = post.author_did == own;
        let line = if assistant {
            post.content.clone()
        } else {
            format!("{}: {}", post.author_name.as_deref().unwrap_or("Someone"), post.content)
        };
        // (The API takes someone's turn first: the assistant's before anyone's is left out.)
        if turns.is_empty() && assistant {
            continue;
        }
        match turns.last_mut() {
            Some(last) if last.assistant == assistant => {
                last.text.push_str("\n\n");
                last.text.push_str(&line);
            }
            _ => turns.push(Turn { assistant, text: line }),
        }
    }
    turns
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(author: &str, name: &str, content: &str) -> Post {
        Post {
            serial: 0,
            channel: "c".into(),
            seq: 0,
            id: content.into(),
            content: content.into(),
            posted_at: 0,
            author_did: author.into(),
            author_name: Some(name.into()),
            question: None,
            answer: None,
            answered_by: None,
            rev: 0,
            reply_to: None,
            reply_author: None,
            reply_snippet: None,
            thread: None,
            mentions: vec![],
            edited_at: None,
            deleted: false,
            removed_by: None,
            pinned: false,
            reactions: vec![],
            grew: None,
            voice_invite: None,
        }
    }

    #[test]
    fn a_room_becomes_turns_people_by_name_the_assistant_as_itself() {
        let posts = [
            post("did:me", "Claude", "an answer before anyone asked"),
            post("did:a", "Alice", "what is a wyvern?"),
            post("did:b", "Bob", "and a dragon?"),
            post("did:me", "Claude", "A wyvern has two legs."),
            post("did:a", "Alice", "thanks"),
        ];
        assert_eq!(
            turns_of(&posts, "did:me"),
            vec![
                Turn { assistant: false, text: "Alice: what is a wyvern?\n\nBob: and a dragon?".into() },
                Turn { assistant: true, text: "A wyvern has two legs.".into() },
                Turn { assistant: false, text: "Alice: thanks".into() },
            ]
        );
    }

    #[test]
    fn a_member_over_their_day_or_the_service_over_its_period_is_told_so() {
        let assistant = Assistant::new(AssistantConfig {
            name: "Claude".into(),
            model: "m".into(),
            api_url: String::new(),
            api_key: String::new(),
            system: String::new(),
            max_tokens: 1,
            context_posts: 1,
            daily_requests_per_member: 2,
            period_tokens: 100,
        });
        assert_eq!(assistant.refusal(1, 99), None);
        assert!(assistant.refusal(2, 0).is_some());
        assert!(assistant.refusal(0, 100).is_some());
    }
}
