//! Redaction of everything that must never reach a recording file.

use serde_json::Value;

use crate::ocpp::frame::{REDACTED, redact_payload};

/// Hides `AuthorizationKey` values (the `value` beside a `"key": "AuthorizationKey"`, anywhere)
/// and the password of every `scheme://user:password@host` URL found in a string value.
pub fn redact_value(value: &mut Value) {
    redact_payload(value);
    redact_urls(value);
}

fn redact_urls(value: &mut Value) {
    match value {
        Value::String(text) => {
            if json_with_authorization_key(text) {
                *text = match serde_json::from_str::<Value>(text) {
                    Ok(mut inner) => {
                        redact_value(&mut inner);
                        inner.to_string()
                    }
                    Err(_) => REDACTED.to_owned(),
                };
            } else if let Some(clean) = redact_text(text) {
                *text = clean;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_urls),
        Value::Object(fields) => fields.values_mut().for_each(redact_urls),
        _ => {}
    }
}

/// Whether a string value is JSON text (an object or array) that mentions `AuthorizationKey`.
fn json_with_authorization_key(text: &str) -> bool {
    let trimmed = text.trim_start();
    (trimmed.starts_with('{') || trimmed.starts_with('['))
        && text
            .to_ascii_lowercase()
            .contains(&"AuthorizationKey".to_ascii_lowercase())
}

/// The end of the scheme when the whole text is one URL (`scheme://...` without whitespace).
fn single_url_scheme_len(text: &str) -> Option<usize> {
    let at = text.find("://")?;
    let scheme = &text[..at];
    (!scheme.is_empty()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
        && !text.contains(char::is_whitespace))
    .then_some(at)
}

/// `text` with URL passwords hidden, or `None` when it holds none.
///
/// Every `://` that follows a scheme character starts a URL whose authority ends at the first
/// `/ ? #`, whitespace, quote or bracket. When the whole string is one URL, the authority ends
/// at the first `/` only, so a password may hold any of those characters. The rest of the text
/// is kept byte for byte (nothing is re-serialized). One pass, linear in the length of the
/// string.
fn redact_text(text: &str) -> Option<String> {
    if !text.contains("://") || !text.contains('@') {
        return None;
    }
    let single = single_url_scheme_len(text);
    let mut out = String::new();
    let mut copied = 0;
    for (at, _) in text.match_indices("://") {
        let start = at + 3;
        if start < copied
            || !text[..at]
                .bytes()
                .next_back()
                .is_some_and(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
        {
            continue;
        }
        let rest = &text[start..];
        // An authority contains no slash, so scans for successive `://` cannot overlap
        // beyond the next delimiter. Long credentials must be searched in full too.
        let end = if single == Some(at) {
            rest.find('/')
        } else {
            rest.find(ends_authority)
        }
        .unwrap_or(rest.len());
        let authority = &rest[..end];
        let Some(userinfo_end) = authority.rfind('@') else {
            continue;
        };
        let Some(colon) = authority[..userinfo_end].find(':') else {
            continue;
        };
        let password = colon + 1..userinfo_end;
        if password.is_empty() || &authority[password.clone()] == REDACTED {
            continue;
        }
        out.push_str(&text[copied..start + password.start]);
        out.push_str(REDACTED);
        copied = start + password.end;
    }
    if copied == 0 {
        return None;
    }
    out.push_str(&text[copied..]);
    Some(out)
}

/// Characters that end the authority of a URL inside free text.
fn ends_authority(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '/' | '?' | '#' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
        )
}

/// Whether a payload still holds a redaction marker: a value that cannot be sent as a
/// credential. Used by the replay planners.
#[must_use]
pub fn holds_redacted(value: &Value) -> bool {
    match value {
        Value::String(text) => {
            text == REDACTED
                || text.contains(&format!(":{REDACTED}@")) && text.contains("://")
                || text.contains(REDACTED)
                    && json_with_authorization_key(text)
                    && serde_json::from_str::<Value>(text)
                        .map_or(true, |inner| holds_redacted(&inner))
        }
        Value::Array(items) => items.iter().any(holds_redacted),
        Value::Object(fields) => fields.values().any(holds_redacted),
        _ => false,
    }
}
