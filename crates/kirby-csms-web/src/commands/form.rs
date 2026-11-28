//! Turns the form fields of an action (`GET /v1/actions`) into a request payload, with the same
//! rules as the terminal UI: a dotted name is a key of a nested object, an empty optional field
//! is left out, a boolean is sent as `true` or `false`, and the other types as their JSON type.

use serde_json::{Map, Value};

use crate::api::FieldSpec;

/// The "leave it out" choice of an optional select.
pub const ANY: &str = "";

/// Whether `text` has the shape of an RFC 3339 time (`2026-01-31T12:00:00Z`, fraction and
/// offset allowed).
pub fn is_rfc3339(text: &str) -> bool {
    let bytes = text.as_bytes();
    let digit = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
    let at = |i: usize, c: u8| bytes.get(i) == Some(&c);
    let number =
        |from: usize, len: usize| -> Option<u32> { text.get(from..from + len)?.parse().ok() };
    let date_ok = (0..4).all(digit)
        && at(4, b'-')
        && (5..7).all(digit)
        && at(7, b'-')
        && (8..10).all(digit)
        && matches!(bytes.get(10), Some(b'T' | b't' | b' '))
        && (11..13).all(digit)
        && at(13, b':')
        && (14..16).all(digit)
        && at(16, b':')
        && (17..19).all(digit);
    if !date_ok {
        return false;
    }
    let in_range = number(5, 2).is_some_and(|m| (1..=12).contains(&m))
        && number(8, 2).is_some_and(|d| (1..=31).contains(&d))
        && number(11, 2).is_some_and(|h| h < 24)
        && number(14, 2).is_some_and(|m| m < 60)
        && number(17, 2).is_some_and(|s| s <= 60);
    let mut rest = &text[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return false;
        }
        rest = &fraction[digits..];
    }
    let offset_ok = matches!(rest, "Z" | "z")
        || (rest.len() == 6
            && matches!(rest.as_bytes()[0], b'+' | b'-')
            && rest.as_bytes()[3] == b':'
            && rest[1..3].chars().all(|c| c.is_ascii_digit())
            && rest[4..].chars().all(|c| c.is_ascii_digit()));
    in_range && offset_ok
}

/// Any JSON value. Text that is not JSON and does not start like an object, an array or a
/// string is taken as a plain string, so `hello` and `42` both work for a free `data` field;
/// the station's schema then decides whether the type fits.
pub fn json_value(text: &str) -> Result<Value, String> {
    match serde_json::from_str(text) {
        Ok(value) => Ok(value),
        Err(e) if text.starts_with(['{', '[', '"']) => Err(format!("invalid JSON: {e}")),
        Err(_) => Ok(Value::String(text.to_owned())),
    }
}

/// A JSON object; empty text is `{}`.
pub fn json_object(text: &str) -> Result<Value, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    match serde_json::from_str::<Value>(text) {
        Ok(value @ Value::Object(_)) => Ok(value),
        Ok(_) => Err("the payload must be a JSON object".to_owned()),
        Err(e) => Err(format!("invalid JSON: {e}")),
    }
}

/// The JSON value of one field, `None` when it is left out, or why the text is not valid.
/// `normalize_time` turns a valid time into UTC whole seconds (the browser does it; tests pass
/// the identity).
pub fn field_value(
    spec: &FieldSpec,
    raw: &str,
    normalize_time: &dyn Fn(&str) -> String,
) -> Result<Option<Value>, String> {
    let text = raw.trim();
    if text.is_empty() {
        return if spec.required {
            Err("required".to_owned())
        } else {
            Ok(None)
        };
    }
    let value = match spec.ty.as_str() {
        "boolean" => Value::Bool(text == "true"),
        "integer" => match text.parse::<i32>() {
            Ok(number) if number >= 0 => Value::from(number),
            _ => return Err("a whole number, 0 or more".to_owned()),
        },
        "json" => json_value(text)?,
        "timestamp" => {
            if !is_rfc3339(text) {
                return Err("a time like 2026-01-31T12:00:00Z".to_owned());
            }
            Value::String(normalize_time(text))
        }
        "pem" => {
            if !text.contains("-----BEGIN ") {
                return Err("a PEM block (-----BEGIN ...)".to_owned());
            }
            Value::String(text.to_owned())
        }
        _ => Value::String(text.to_owned()),
    };
    Ok(Some(value))
}

/// Sets `value` at the dotted `path` of `object`, creating the objects on the way.
pub fn insert_path(object: &mut Map<String, Value>, path: &str, value: Value) {
    match path.split_once('.') {
        None => {
            object.insert(path.to_owned(), value);
        }
        Some((head, rest)) => {
            let child = object
                .entry(head.to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(inner) = child {
                insert_path(inner, rest, value);
            }
        }
    }
}

/// The payload of a form: one entry per field value, nested by dotted names.
pub fn build_payload(
    entries: &[(FieldSpec, String)],
    normalize_time: &dyn Fn(&str) -> String,
) -> Result<Value, String> {
    let mut root = Map::new();
    for (spec, raw) in entries {
        if let Some(value) =
            field_value(spec, raw, normalize_time).map_err(|e| format!("{}: {e}", spec.label))?
        {
            insert_path(&mut root, &spec.name, value);
        }
    }
    Ok(Value::Object(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(name: &str, ty: &str, required: bool) -> FieldSpec {
        FieldSpec {
            name: name.to_owned(),
            label: name.to_owned(),
            ty: ty.to_owned(),
            required,
            ..FieldSpec::default()
        }
    }

    fn same(text: &str) -> String {
        text.to_owned()
    }

    #[test]
    fn times_follow_rfc3339() {
        for good in [
            "2026-01-31T12:00:00Z",
            "2026-01-31T12:00:00.123Z",
            "2026-01-31T12:00:00+02:00",
            "2026-01-31t12:00:00z",
        ] {
            assert!(is_rfc3339(good), "{good}");
        }
        for bad in [
            "",
            "2026-01-31",
            "2026-13-31T12:00:00Z",
            "2026-01-31T25:00:00Z",
            "2026-01-31T12:00:00",
            "2026-01-31T12:00:00.Z",
            "2026-01-31T12:00:00+0200",
        ] {
            assert!(!is_rfc3339(bad), "{bad}");
        }
    }

    #[test]
    fn dotted_names_nest_and_empty_optionals_are_left_out() {
        let entries = vec![
            (
                spec("log.remoteLocation", "text", true),
                "https://x".to_owned(),
            ),
            (
                spec("log.oldestTimestamp", "timestamp", false),
                String::new(),
            ),
            (spec("retries", "integer", false), "3".to_owned()),
            (spec("flag", "boolean", true), "true".to_owned()),
        ];
        let payload = build_payload(&entries, &same).unwrap();
        assert_eq!(
            payload,
            json!({"log": {"remoteLocation": "https://x"}, "retries": 3, "flag": true})
        );
    }

    #[test]
    fn required_and_typed_fields_are_checked() {
        assert_eq!(
            field_value(&spec("a", "text", true), "  ", &same),
            Err("required".to_owned())
        );
        assert!(field_value(&spec("a", "integer", false), "-1", &same).is_err());
        assert!(field_value(&spec("a", "integer", false), "x", &same).is_err());
        assert!(field_value(&spec("a", "json", false), "{oops", &same).is_err());
        assert_eq!(
            field_value(&spec("a", "json", false), "hello", &same),
            Ok(Some(json!("hello")))
        );
        assert!(field_value(&spec("a", "pem", false), "nope", &same).is_err());
    }

    #[test]
    fn raw_payload_is_an_object_and_empty_is_empty_object() {
        assert_eq!(json_object("").unwrap(), json!({}));
        assert_eq!(json_object("{\"a\":1}").unwrap(), json!({"a": 1}));
        assert!(json_object("[1]").is_err());
        assert!(json_object("{").is_err());
    }
}
