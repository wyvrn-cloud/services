//! Slack's and GitHub's webhook bodies, made into the Discord payload a webhook
//! takes (webhooks.rs), as Discord's own `/slack` and `/github` endpoints do: so a
//! service that only knows how to post to Slack, or GitHub's own webhooks, can
//! post into a channel given `.../slack` or `.../github` after the webhook's URL.
//!
//! Only the shape changes here. What comes out is checked, kept and posted exactly
//! as a Discord payload is (the limits, mentions, AutoMod, the rate).

use serde_json::{json, Map, Value};

/// GitHub's colours for what an event says, as Discord shows them.
const GREEN: u64 = 0x2E_A0_43;
const RED: u64 = 0xCB_24_31;
const GREY: u64 = 0x6E_76_81;
const BLUE: u64 = 0x1F_6F_EB;

/// How many commits a push lists, as Discord's.
const MOST_COMMITS: usize = 5;

fn str_of(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.trim().is_empty())
}

/// Slack's `mrkdwn` as Markdown: `<url|label>` and `<url>` links, `*bold*`,
/// `~struck~` (`_italic_` and code are the same in both). `<@U…>`, `<#C…>` and
/// `<!here>` name things in a Slack workspace, and are left out.
pub fn mrkdwn(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('>') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let inside = &after[..close];
        rest = &after[close + 1..];
        if inside.starts_with('@') || inside.starts_with('#') || inside.starts_with('!') {
            // A workspace's user, channel or alert: its label where it has one.
            if let Some((_, label)) = inside.split_once('|') {
                out.push_str(label);
            }
            continue;
        }
        match inside.split_once('|') {
            Some((url, label)) => out.push_str(&format!("[{label}]({url})")),
            None => out.push_str(inside),
        }
    }
    out.push_str(rest);
    // Slack's single marks to Markdown's doubled ones, a pair at a time.
    let doubled = |text: &str, mark: char| -> String {
        let pieces: Vec<&str> = text.split(mark).collect();
        if pieces.len() < 3 {
            return text.to_string();
        }
        let mut out = String::new();
        let pairs = (pieces.len() - 1) / 2 * 2;
        for (i, piece) in pieces.iter().enumerate() {
            if i > 0 {
                if i <= pairs {
                    out.push(mark);
                    out.push(mark);
                } else {
                    out.push(mark);
                }
            }
            out.push_str(piece);
        }
        out
    };
    doubled(&doubled(&out, '*'), '~')
}

/// A Slack colour (`good`, `warning`, `danger` or `#rrggbb`) as a number.
fn slack_colour(value: &Value) -> Option<u64> {
    match value.as_str()? {
        "good" => Some(GREEN),
        "warning" => Some(0xDA_A0_38),
        "danger" => Some(RED),
        hex => u64::from_str_radix(hex.trim_start_matches('#'), 16).ok().filter(|c| *c <= 0xFF_FFFF),
    }
}

/// A Slack incoming-webhook message (`text`, `username`, `icon_url`, and its
/// legacy `attachments`) as Discord's payload. Blocks are read for their text.
pub fn from_slack(slack: &Value) -> Value {
    let mut payload = Map::new();
    let mut content = str_of(&slack["text"]).map(mrkdwn).unwrap_or_default();
    // Blocks: what each says, in order (sections, headers, context).
    for block in slack["blocks"].as_array().into_iter().flatten() {
        let said = str_of(&block["text"]["text"]).map(mrkdwn).or_else(|| {
            let parts: Vec<String> = block["elements"].as_array().into_iter().flatten().filter_map(|e| str_of(&e["text"]).map(mrkdwn)).collect();
            (!parts.is_empty()).then(|| parts.join(" "))
        });
        if let Some(said) = said {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&said);
        }
    }
    if !content.is_empty() {
        payload.insert("content".into(), json!(content));
    }
    if let Some(name) = str_of(&slack["username"]) {
        payload.insert("username".into(), json!(name));
    }
    if let Some(icon) = str_of(&slack["icon_url"]) {
        payload.insert("avatar_url".into(), json!(icon));
    }
    let embeds: Vec<Value> = slack["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| {
            let mut embed = Map::new();
            let description = [str_of(&a["pretext"]), str_of(&a["text"])].into_iter().flatten().map(mrkdwn).collect::<Vec<_>>().join("\n");
            let description = if description.is_empty() { str_of(&a["fallback"]).map(mrkdwn).unwrap_or_default() } else { description };
            if !description.is_empty() {
                embed.insert("description".into(), json!(description));
            }
            if let Some(title) = str_of(&a["title"]) {
                embed.insert("title".into(), json!(mrkdwn(title)));
            }
            if let Some(url) = str_of(&a["title_link"]) {
                embed.insert("url".into(), json!(url));
            }
            if let Some(colour) = slack_colour(&a["color"]) {
                embed.insert("color".into(), json!(colour));
            }
            if let Some(name) = str_of(&a["author_name"]) {
                let mut author = json!({ "name": name });
                if let Some(url) = str_of(&a["author_link"]) {
                    author["url"] = json!(url);
                }
                if let Some(icon) = str_of(&a["author_icon"]) {
                    author["icon_url"] = json!(icon);
                }
                embed.insert("author".into(), author);
            }
            let fields: Vec<Value> = a["fields"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| Some(json!({ "name": str_of(&f["title"]).unwrap_or("\u{200b}"), "value": mrkdwn(str_of(&f["value"])?), "inline": f["short"].as_bool().unwrap_or(false) })))
                .collect();
            if !fields.is_empty() {
                embed.insert("fields".into(), json!(fields));
            }
            if let Some(url) = str_of(&a["image_url"]) {
                embed.insert("image".into(), json!({ "url": url }));
            }
            if let Some(url) = str_of(&a["thumb_url"]) {
                embed.insert("thumbnail".into(), json!({ "url": url }));
            }
            if let Some(text) = str_of(&a["footer"]) {
                let mut footer = json!({ "text": text });
                if let Some(icon) = str_of(&a["footer_icon"]) {
                    footer["icon_url"] = json!(icon);
                }
                embed.insert("footer".into(), footer);
            }
            if let Some(ts) = a["ts"].as_f64().or_else(|| a["ts"].as_str().and_then(|s| s.parse().ok())) {
                if let Some(at) = chrono::DateTime::from_timestamp(ts as i64, 0) {
                    embed.insert("timestamp".into(), json!(at.to_rfc3339()));
                }
            }
            Value::Object(embed)
        })
        .filter(|e| e.as_object().is_some_and(|o| !o.is_empty()))
        .take(10)
        .collect();
    if !embeds.is_empty() {
        payload.insert("embeds".into(), json!(embeds));
    }
    Value::Object(payload)
}

/// The first line of `text`, cut to `most` characters.
fn first_line(text: &str, most: usize) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() > most {
        format!("{}...", line.chars().take(most.saturating_sub(3)).collect::<String>())
    } else {
        line.to_string()
    }
}

/// `text` cut to `most` characters.
fn cut(text: &str, most: usize) -> String {
    if text.chars().count() > most {
        format!("{}...", text.chars().take(most.saturating_sub(3)).collect::<String>())
    } else {
        text.to_string()
    }
}

/// A GitHub event (its `X-GitHub-Event` and body) as Discord's payload: an embed
/// saying what happened, by whom, linked to it, as Discord's GitHub webhooks show.
/// `None`: nothing to post (a `ping`, or an event or action that says nothing a
/// channel would want, as Discord also leaves out).
pub fn from_github(event: &str, body: &Value) -> Option<Value> {
    let repo = str_of(&body["repository"]["full_name"]).unwrap_or("a repository");
    let sender = &body["sender"];
    let author = str_of(&sender["login"]).map(|login| {
        let mut author = json!({ "name": login });
        if let Some(url) = str_of(&sender["html_url"]) {
            author["url"] = json!(url);
        }
        if let Some(icon) = str_of(&sender["avatar_url"]) {
            author["icon_url"] = json!(icon);
        }
        author
    });
    let action = body["action"].as_str().unwrap_or_default();
    let embed = |title: String, url: Option<&str>, description: Option<String>, colour: u64| {
        let mut embed = json!({ "title": cut(&title, 256), "color": colour });
        if let Some(url) = url {
            embed["url"] = json!(url);
        }
        if let Some(description) = description.filter(|d| !d.trim().is_empty()) {
            embed["description"] = json!(cut(&description, 500));
        }
        if let Some(author) = &author {
            embed["author"] = author.clone();
        }
        embed
    };
    let made = match event {
        "push" => {
            let branch = body["ref"].as_str().unwrap_or_default().trim_start_matches("refs/heads/").trim_start_matches("refs/tags/");
            let commits = body["commits"].as_array().cloned().unwrap_or_default();
            if commits.is_empty() {
                // A branch made, deleted or forced without new commits: `create` and `delete` say those.
                return None;
            }
            let lines: Vec<String> = commits
                .iter()
                .take(MOST_COMMITS)
                .map(|c| {
                    let id: String = c["id"].as_str().unwrap_or_default().chars().take(7).collect();
                    let url = c["url"].as_str().unwrap_or_default();
                    let who = str_of(&c["author"]["username"]).or_else(|| str_of(&c["author"]["name"])).unwrap_or("someone");
                    format!("[`{id}`]({url}) {} - {who}", first_line(c["message"].as_str().unwrap_or_default(), 50))
                })
                .collect();
            let n = commits.len();
            let title = format!("[{repo}:{branch}] {n} new commit{}", if n == 1 { "" } else { "s" });
            embed(title, str_of(&body["compare"]), Some(lines.join("\n")), BLUE)
        }
        "issues" => {
            let issue = &body["issue"];
            let colour = match action {
                "opened" | "reopened" => GREEN,
                "closed" => RED,
                _ => return None,
            };
            let title = format!("[{repo}] Issue {}: #{} {}", action, issue["number"], issue["title"].as_str().unwrap_or_default());
            let description = (action == "opened").then(|| issue["body"].as_str().unwrap_or_default().to_string());
            embed(title, str_of(&issue["html_url"]), description, colour)
        }
        "pull_request" => {
            let pull = &body["pull_request"];
            let (said, colour) = match action {
                "opened" | "reopened" => (action, GREEN),
                "closed" if pull["merged"] == true => ("merged", 0x82_50_DF),
                "closed" => ("closed", RED),
                _ => return None,
            };
            let title = format!("[{repo}] Pull request {said}: #{} {}", pull["number"], pull["title"].as_str().unwrap_or_default());
            let description = (action == "opened").then(|| pull["body"].as_str().unwrap_or_default().to_string());
            embed(title, str_of(&pull["html_url"]), description, colour)
        }
        "issue_comment" | "pull_request_review_comment" | "commit_comment" => {
            if action != "created" && event != "commit_comment" {
                return None;
            }
            let comment = &body["comment"];
            let on = match event {
                "issue_comment" => {
                    let issue = &body["issue"];
                    let kind = if issue["pull_request"].is_object() { "pull request" } else { "issue" };
                    format!("{kind} #{}: {}", issue["number"], issue["title"].as_str().unwrap_or_default())
                }
                "pull_request_review_comment" => format!("pull request #{}: {}", body["pull_request"]["number"], body["pull_request"]["title"].as_str().unwrap_or_default()),
                _ => format!("commit {}", comment["commit_id"].as_str().unwrap_or_default().chars().take(7).collect::<String>()),
            };
            let title = format!("[{repo}] New comment on {on}");
            embed(title, str_of(&comment["html_url"]), Some(comment["body"].as_str().unwrap_or_default().to_string()), GREY)
        }
        "pull_request_review" => {
            if action != "submitted" {
                return None;
            }
            let review = &body["review"];
            let pull = &body["pull_request"];
            let said = match review["state"].as_str().unwrap_or_default() {
                "approved" => "approved",
                "changes_requested" => "changes requested",
                _ => "reviewed",
            };
            let title = format!("[{repo}] Pull request {said}: #{} {}", pull["number"], pull["title"].as_str().unwrap_or_default());
            embed(title, str_of(&review["html_url"]), review["body"].as_str().map(str::to_string), GREY)
        }
        "release" => {
            if action != "published" {
                return None;
            }
            let release = &body["release"];
            let name = str_of(&release["name"]).or_else(|| str_of(&release["tag_name"])).unwrap_or_default();
            embed(format!("[{repo}] New release published: {name}"), str_of(&release["html_url"]), release["body"].as_str().map(str::to_string), GREEN)
        }
        "create" | "delete" => {
            let kind = body["ref_type"].as_str().unwrap_or("ref");
            let name = body["ref"].as_str().unwrap_or_default();
            let (made, colour) = if event == "create" { ("created", GREEN) } else { ("deleted", RED) };
            embed(format!("[{repo}] New {kind} {made}: {name}"), str_of(&body["repository"]["html_url"]), None, colour)
        }
        "fork" => {
            let fork = str_of(&body["forkee"]["full_name"]).unwrap_or_default();
            embed(format!("[{repo}] Fork created: {fork}"), str_of(&body["forkee"]["html_url"]), None, GREY)
        }
        // A star (GitHub's event for it is still called `watch`).
        "watch" | "star" => {
            if action != "started" && action != "created" {
                return None;
            }
            embed(format!("[{repo}] New star added"), str_of(&body["repository"]["html_url"]), None, GREY)
        }
        _ => return None,
    };
    Some(json!({ "username": "GitHub", "avatar_url": "https://github.githubassets.com/images/modules/logos_page/GitHub-Mark.png", "embeds": [made] }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mrkdwn_becomes_markdown() {
        assert_eq!(mrkdwn("*Deploy* of <https://ci.example/1|build 1> ~failed~"), "**Deploy** of [build 1](https://ci.example/1) ~~failed~~");
        assert_eq!(mrkdwn("see <https://example.com>"), "see https://example.com");
        assert_eq!(mrkdwn("hi <@U123|ada> in <#C1|general> <!here>"), "hi ada in general ");
        assert_eq!(mrkdwn("2 * 3 = 6"), "2 * 3 = 6");
        assert_eq!(mrkdwn("a < b"), "a < b");
    }

    #[test]
    fn a_slack_message_is_discords_payload() {
        let slack = json!({
            "text": "Build <https://ci.example/7|#7> *passed*",
            "username": "CI",
            "icon_url": "https://ci.example/icon.png",
            "attachments": [{ "color": "good", "title": "main", "title_link": "https://ci.example/7", "text": "All 312 tests", "fields": [{ "title": "Time", "value": "3m", "short": true }], "footer": "ci", "ts": 1700000000 }]
        });
        let payload = from_slack(&slack);
        assert_eq!(payload["content"], "Build [#7](https://ci.example/7) **passed**");
        assert_eq!(payload["username"], "CI");
        assert_eq!(payload["avatar_url"], "https://ci.example/icon.png");
        let embed = &payload["embeds"][0];
        assert_eq!(embed["color"], GREEN);
        assert_eq!(embed["title"], "main");
        assert_eq!(embed["url"], "https://ci.example/7");
        assert_eq!(embed["description"], "All 312 tests");
        assert_eq!(embed["fields"][0], json!({ "name": "Time", "value": "3m", "inline": true }));
        assert_eq!(embed["footer"]["text"], "ci");
        assert!(embed["timestamp"].as_str().unwrap().starts_with("2023-11-14"));
        // Blocks' text, after the text.
        let blocks = from_slack(&json!({ "blocks": [{ "type": "section", "text": { "type": "mrkdwn", "text": "*Hi*" } }, { "type": "context", "elements": [{ "type": "mrkdwn", "text": "a" }, { "type": "mrkdwn", "text": "b" }] }] }));
        assert_eq!(blocks["content"], "**Hi**\na b");
    }

    #[test]
    fn github_events_are_embeds_and_a_ping_is_nothing() {
        assert!(from_github("ping", &json!({ "zen": "Keep it simple." })).is_none());
        let sender = json!({ "login": "ada", "html_url": "https://github.com/ada", "avatar_url": "https://avatars.example/ada" });
        let repository = json!({ "full_name": "wyvrn/chat", "html_url": "https://github.com/wyvrn/chat" });
        let push = from_github(
            "push",
            &json!({ "ref": "refs/heads/main", "compare": "https://github.com/wyvrn/chat/compare/a...b", "repository": repository, "sender": sender,
                "commits": [{ "id": "0123456789abcdef", "url": "https://github.com/wyvrn/chat/commit/0123456", "message": "Fix the thing\n\nAt length.", "author": { "username": "ada" } }] }),
        )
        .unwrap();
        assert_eq!(push["username"], "GitHub");
        let embed = &push["embeds"][0];
        assert_eq!(embed["title"], "[wyvrn/chat:main] 1 new commit");
        assert_eq!(embed["description"], "[`0123456`](https://github.com/wyvrn/chat/commit/0123456) Fix the thing - ada");
        assert_eq!(embed["author"]["name"], "ada");
        // A branch pushed with no commits: left to `create`.
        assert!(from_github("push", &json!({ "ref": "refs/heads/x", "commits": [], "repository": repository })).is_none());
        let merged = from_github("pull_request", &json!({ "action": "closed", "repository": repository, "sender": sender, "pull_request": { "number": 4, "title": "Stages", "merged": true, "html_url": "https://github.com/wyvrn/chat/pull/4" } })).unwrap();
        assert_eq!(merged["embeds"][0]["title"], "[wyvrn/chat] Pull request merged: #4 Stages");
        let opened = from_github("issues", &json!({ "action": "opened", "repository": repository, "issue": { "number": 9, "title": "Crash", "body": "It crashed.", "html_url": "https://github.com/wyvrn/chat/issues/9" } })).unwrap();
        assert_eq!(opened["embeds"][0]["description"], "It crashed.");
        assert_eq!(opened["embeds"][0]["color"], GREEN);
        // An action nobody shows: nothing.
        assert!(from_github("issues", &json!({ "action": "labeled", "repository": repository, "issue": { "number": 9 } })).is_none());
        assert!(from_github("workflow_job", &json!({})).is_none());
        let star = from_github("watch", &json!({ "action": "started", "repository": repository, "sender": sender })).unwrap();
        assert_eq!(star["embeds"][0]["title"], "[wyvrn/chat] New star added");
    }
}
