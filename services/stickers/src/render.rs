//! A template's display hints, filled in for one instance: what `status` returns
//! as `ui`.
//!
//! The hints are Workflow UI 1.0 items (`{ "type": "text", "text": … }`), written
//! once in the template with places left for an instance's values:
//!
//! - `{{ path }}` in any string. A string that is nothing but one place takes
//!   the value itself, whatever its type; inside other text it becomes text.
//! - `showWhen`: the item is there only if the expression holds.
//! - `bind` with `row_template`: one copy of the template for each element of
//!   the array at `bind`, which the copy reaches as `item`.
//!
//! An expression here is a dotted path (`artifacts.pack.title`), or one negated
//! (`!artifacts.full`): the part of JMESPath every renderer of these hints
//! agrees on, so a coordinator that renders the template itself draws the same.

use serde_json::{Map, Value};

fn lookup<'a>(scope: &'a Value, path: &str) -> &'a Value {
    path.trim().split('.').fold(scope, |at, key| &at[key])
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Whether `expression` (a path, or `!path`) holds in `scope`.
pub fn holds(expression: &str, scope: &Value) -> bool {
    match expression.trim().strip_prefix('!') {
        Some(path) => !truthy(lookup(scope, path)),
        None => truthy(lookup(scope, expression)),
    }
}

fn as_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn fill(text: &str, scope: &Value) -> Value {
    let trimmed = text.trim();
    if let Some(path) = trimmed.strip_prefix("{{").and_then(|rest| rest.strip_suffix("}}")).filter(|path| !path.contains("{{")) {
        return lookup(scope, path).clone();
    }
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("}}") {
            Some(end) => {
                out.push_str(&as_text(lookup(scope, &rest[start + 2..start + 2 + end])));
                rest = &rest[start + 2 + end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    Value::String(out)
}

/// `hints` with every place filled from `scope` (`{context, artifacts, participants}`).
/// `None` for an item its `showWhen` leaves out.
pub fn render(hints: &Value, scope: &Value) -> Option<Value> {
    match hints {
        Value::String(text) => Some(fill(text, scope)),
        Value::Array(items) => Some(Value::Array(items.iter().filter_map(|item| render(item, scope)).collect())),
        Value::Object(fields) => {
            if let Some(expression) = fields.get("showWhen").and_then(Value::as_str) {
                if !holds(expression, scope) {
                    return None;
                }
            }
            let mut out = Map::new();
            for (key, value) in fields {
                match key.as_str() {
                    "showWhen" | "bind" | "row_template" => {}
                    _ => {
                        out.insert(key.clone(), render(value, scope).unwrap_or(Value::Null));
                    }
                }
            }
            if let (Some(bound), Some(template)) = (fields.get("bind").and_then(Value::as_str), fields.get("row_template")) {
                let rows = lookup(scope, bound).as_array().cloned().unwrap_or_default();
                let children = rows
                    .iter()
                    .filter_map(|item| {
                        let mut inner = scope.clone();
                        inner["item"] = item.clone();
                        render(template, &inner)
                    })
                    .collect();
                out.insert("children".to_string(), Value::Array(children));
            }
            Some(Value::Object(out))
        }
        other => Some(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn places_are_filled_and_keep_their_type_when_alone() {
        let scope = json!({ "context": { "title": "Cats" }, "artifacts": { "count": 3, "tags": ["a", "b"] } });
        let hints = json!([
            { "type": "title", "text": "{{ context.title }}" },
            { "type": "text", "text": "{{ artifacts.count }} of 120 in {{context.title}}." },
            { "type": "list", "items": "{{ artifacts.tags }}", "gap": 4 },
            { "type": "text", "text": "{{ context.missing }}!" },
        ]);
        assert_eq!(
            render(&hints, &scope).unwrap(),
            json!([
                { "type": "title", "text": "Cats" },
                { "type": "text", "text": "3 of 120 in Cats." },
                { "type": "list", "items": ["a", "b"], "gap": 4 },
                { "type": "text", "text": "!" },
            ])
        );
    }

    #[test]
    fn an_item_is_shown_only_when_its_condition_holds() {
        let hints = json!([
            { "type": "text", "text": "some", "showWhen": "artifacts.count" },
            { "type": "text", "text": "none", "showWhen": "!artifacts.count" },
            { "type": "form", "showWhen": "!artifacts.full", "children": [{ "type": "text-input", "showWhen": "artifacts.named" }] },
        ]);
        let shown = |scope: Value| render(&hints, &scope).unwrap();
        assert_eq!(shown(json!({ "artifacts": { "count": 0 } })), json!([{ "type": "text", "text": "none" }, { "type": "form", "children": [] }]));
        assert_eq!(shown(json!({ "artifacts": { "count": 2, "full": true } })), json!([{ "type": "text", "text": "some" }]));
    }

    #[test]
    fn a_bound_item_has_a_copy_of_its_template_for_each_row() {
        let scope = json!({ "artifacts": { "stickers": [{ "id": "a", "emoji": "😺" }, { "id": "b", "emoji": "🐟" }] } });
        let hints = json!({
            "type": "list",
            "bind": "artifacts.stickers",
            "row_template": { "type": "submit-button", "label": "Remove {{ item.emoji }}", "event": "remove", "input": { "sticker": "{{ item.id }}" } },
        });
        assert_eq!(
            render(&hints, &scope).unwrap(),
            json!({ "type": "list", "children": [
                { "type": "submit-button", "label": "Remove 😺", "event": "remove", "input": { "sticker": "a" } },
                { "type": "submit-button", "label": "Remove 🐟", "event": "remove", "input": { "sticker": "b" } },
            ]})
        );
        // Nothing to bind to is no rows, not a failure.
        assert_eq!(render(&hints, &json!({})).unwrap()["children"], json!([]));
    }
}
