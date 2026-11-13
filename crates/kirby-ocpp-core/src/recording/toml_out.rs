//! Writing scenario TOML from JSON values (the `toml` dependency is parse-only here).

use std::fmt::Write as _;

use serde_json::Value;

use crate::text::is_unsafe_char;

/// A TOML basic string; control and invisible characters are written as escapes so the file
/// is safe to print.
pub(super) fn string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if is_unsafe_char(c) => {
                let code = u32::from(c);
                if code > 0xFFFF {
                    let _ = write!(out, "\\U{code:08X}");
                } else {
                    let _ = write!(out, "\\u{code:04X}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A JSON value as a TOML value (objects become inline tables).
///
/// # Errors
/// `null` and numbers TOML cannot hold.
pub(super) fn value(json: &Value) -> Result<String, String> {
    Ok(match json {
        Value::Null => return Err("a null value cannot be written in TOML".to_owned()),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                int.to_string()
            } else if number.is_u64() {
                return Err(
                    "an integer above 9223372036854775807 cannot be written in TOML".to_owned(),
                );
            } else {
                let float = number.as_f64().filter(|f| f.is_finite());
                match float {
                    Some(f) if f.fract() == 0.0 => format!("{f:.1}"),
                    Some(f) => f.to_string(),
                    None => return Err("a non-finite number cannot be written in TOML".to_owned()),
                }
            }
        }
        Value::String(text) => string(text),
        Value::Array(items) => {
            let parts: Result<Vec<_>, _> = items.iter().map(value).collect();
            format!("[{}]", parts?.join(", "))
        }
        Value::Object(fields) => {
            if fields.is_empty() {
                return Ok("{}".to_owned());
            }
            let mut parts = Vec::with_capacity(fields.len());
            for (key, item) in fields {
                parts.push(format!("{} = {}", string(key), value(item)?));
            }
            format!("{{ {} }}", parts.join(", "))
        }
    })
}

/// One `[[steps]]` block from a JSON object: `action` first, then the other keys.
///
/// # Errors
/// A value TOML cannot hold.
pub(super) fn step(fields: &serde_json::Map<String, Value>) -> Result<String, String> {
    let mut out = String::from("[[steps]]\n");
    if let Some(action) = fields.get("action") {
        let _ = writeln!(out, "action = {}", value(action)?);
    }
    for (key, item) in fields {
        if key != "action" {
            let _ = writeln!(out, "{key} = {}", value(item)?);
        }
    }
    Ok(out)
}

/// Doubles every `${` in the strings of `json`: recorded text is data, never a placeholder.
pub(super) fn escape_placeholders(json: &mut Value) {
    match json {
        Value::String(text) => {
            if text.contains("${") {
                *text = text.replace("${", "$${");
            }
        }
        Value::Array(items) => items.iter_mut().for_each(escape_placeholders),
        Value::Object(fields) => fields.values_mut().for_each(escape_placeholders),
        _ => {}
    }
}
