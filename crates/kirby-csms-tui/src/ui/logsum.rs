//! One-line summaries of log frames: the scalar fields of the payload, then one value for each
//! nested object (an id token shows as `idToken=TAG1`) and the size of each array.

use kirby_ocpp_tui::text::{sanitize, truncate};
use serde_json::{Map, Value};

/// Most fields one summary lists.
const FIELDS: usize = 6;
/// Longest value of one field, in cells.
const VALUE_MAX: usize = 32;

/// Keys tried, after the key itself, to name a nested object by one value.
const NESTED_KEYS: [&str; 3] = ["id", "name", "reasonCode"];

/// The summary of a CALL or CALLRESULT with an object payload. `None` for any other frame
/// (an error, a text that is not a frame); the caller keeps its own summary then.
pub(super) fn summarize(frame: &Value) -> Option<String> {
    let parts = frame.as_array()?;
    let payload = match parts.first()?.as_u64()? {
        2 => parts.get(3)?,
        3 => parts.get(2)?,
        _ => return None,
    };
    Some(fields(payload.as_object()?))
}

fn fields(map: &Map<String, Value>) -> String {
    let mut out: Vec<String> = Vec::new();
    // `status` first: it is what one looks for in an answer.
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_by_key(|k| k.as_str() != "status");
    for key in &keys {
        if let Some(text) = map.get(*key).and_then(scalar) {
            out.push(pair(key, &text));
        }
    }
    for key in &keys {
        match map.get(*key) {
            Some(Value::Object(inner)) => {
                if let Some(text) = nested(key, inner) {
                    out.push(pair(key, &text));
                }
            }
            Some(Value::Array(items)) if !items.is_empty() => {
                out.push(sanitize(&format!("{key}[{}]", items.len())));
            }
            _ => {}
        }
    }
    out.truncate(FIELDS);
    out.join(" ")
}

fn pair(key: &str, value: &str) -> String {
    sanitize(&format!("{key}={}", truncate(&sanitize(value), VALUE_MAX)))
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

/// The one value that names a nested object: the member with the key of the object itself
/// (`idToken.idToken`), else `id`, `name` or `reasonCode`.
fn nested(key: &str, inner: &Map<String, Value>) -> Option<String> {
    std::iter::once(key)
        .chain(NESTED_KEYS)
        .find_map(|k| inner.get(k).and_then(scalar))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn call(payload: &Value) -> Option<String> {
        summarize(&json!([2, "m1", "X", payload]))
    }

    #[test]
    fn calls_carry_their_key_fields() {
        assert_eq!(
            call(&json!({"remoteStartId": 1, "idToken": {"idToken": "TAG1", "type": "Central"}}))
                .as_deref(),
            Some("remoteStartId=1 idToken=TAG1")
        );
        assert_eq!(
            call(&json!({"idTag": "TAG1"})).as_deref(),
            Some("idTag=TAG1")
        );
        assert_eq!(call(&json!({"type": "Soft"})).as_deref(), Some("type=Soft"));
        assert_eq!(
            call(&json!({"requestedMessage": "StatusNotification"})).as_deref(),
            Some("requestedMessage=StatusNotification")
        );
    }

    #[test]
    fn an_answer_lists_its_status_first_and_counts_arrays() {
        let answer = json!([3, "m1", {"zeta": 1, "status": "Accepted", "list": [1, 2, 3]}]);
        assert_eq!(
            summarize(&answer).as_deref(),
            Some("status=Accepted zeta=1 list[3]")
        );
    }

    #[test]
    fn a_summary_is_sanitized_and_length_capped() {
        let long = "x".repeat(200);
        let text = call(&json!({"a": "\u{1b}[2J\u{202e}evil", "b": long})).expect("summary");
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{202e}'),
            "{text:?}"
        );
        assert!(width_of(&text) < 120, "{text:?}");
        let many: Map<String, Value> = (0..20).map(|i| (format!("k{i:02}"), json!(i))).collect();
        let text = call(&Value::Object(many)).expect("summary");
        assert_eq!(text.split(' ').count(), FIELDS);
    }

    #[test]
    fn frames_without_an_object_payload_have_no_summary() {
        assert_eq!(summarize(&json!([4, "m1", "InternalError", "", {}])), None);
        assert_eq!(call(&json!("text")), None);
        assert_eq!(summarize(&json!("not a frame")), None);
    }

    fn width_of(s: &str) -> usize {
        kirby_ocpp_tui::text::width(s)
    }
}
