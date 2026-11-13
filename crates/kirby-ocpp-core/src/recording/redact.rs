//! Redaction of everything that must never reach a recording file.

use serde_json::Value;

use crate::ocpp::frame::{REDACTED, redact_payload};

/// Longest string that is searched for URL credentials.
const MAX_SCANNED: usize = 4096;

/// Hides `AuthorizationKey` values (the `value` beside a `"key": "AuthorizationKey"`, anywhere)
/// and the password of every `scheme://user:password@host` URL found in a string value.
pub fn redact_value(value: &mut Value) {
    redact_payload(value);
    redact_urls(value);
}

fn redact_urls(value: &mut Value) {
    match value {
        Value::String(text) => {
            if let Some(clean) = redact_text(text) {
                *text = clean;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_urls),
        Value::Object(fields) => fields.values_mut().for_each(redact_urls),
        _ => {}
    }
}

/// `text` with URL passwords hidden, or `None` when it holds none.
fn redact_text(text: &str) -> Option<String> {
    if text.len() > MAX_SCANNED || !text.contains("://") || !text.contains('@') {
        return None;
    }
    let mut changed = false;
    let mut out = String::with_capacity(text.len());
    for (index, token) in text.split(' ').enumerate() {
        if index > 0 {
            out.push(' ');
        }
        match redact_token(token) {
            Some(clean) => {
                changed = true;
                out.push_str(&clean);
            }
            None => out.push_str(token),
        }
    }
    changed.then_some(out)
}

/// Replaces the password of a URL-looking token by [`REDACTED`], keeping the rest of the text
/// byte for byte (the URL is not re-serialized).
fn redact_token(token: &str) -> Option<String> {
    let (scheme, rest) = token.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
    {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let at = authority.rfind('@')?;
    let (user, _password) = authority[..at].split_once(':')?;
    Some(format!(
        "{scheme}://{user}:{REDACTED}@{}{}",
        &authority[at + 1..],
        &rest[end..]
    ))
}

/// Whether a payload still holds a redaction marker: a value that cannot be sent as a
/// credential. Used by the replay planner.
pub(crate) fn holds_redacted(value: &Value) -> bool {
    match value {
        Value::String(text) => {
            text == REDACTED || text.contains(&format!(":{REDACTED}@")) && text.contains("://")
        }
        Value::Array(items) => items.iter().any(holds_redacted),
        Value::Object(fields) => fields.values().any(holds_redacted),
        _ => false,
    }
}
