//! Scenario variables: a name -> JSON value map, `${name}` substitution and capture by
//! JSON pointer.
//!
//! UI- and station-free, so other crates (the `kirby-csms` script runner) reuse it.
//!
//! * A string that is exactly `${name}` is replaced by the variable's JSON value with its
//!   type (a number stays a number) by [`substitute`].
//! * `${name}` inside longer text is replaced by the value's text: strings without quotes,
//!   numbers and booleans as written; objects, arrays and `null` are an error.
//! * `$${` is a literal `${`.
//! * An unknown variable or a malformed `${` is an error that names the variable.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Variables every run defines itself; user variables must not use these names.
pub const BUILTIN_NAMES: [&str; 2] = ["station_id", "index"];

/// Is `name` a valid variable name (`[A-Za-z_][A-Za-z0-9_]*`)?
#[must_use]
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Is `name` one of the [`BUILTIN_NAMES`]?
#[must_use]
pub fn is_builtin(name: &str) -> bool {
    BUILTIN_NAMES.contains(&name)
}

/// Checks that `value` may be declared as a variable in a `[vars]` table or on a command
/// line: a string, a number or a boolean.
///
/// # Errors
/// A description when it is `null`, an array or an object.
pub fn check_scalar(value: &Value) -> Result<(), String> {
    match value {
        Value::String(_) | Value::Number(_) | Value::Bool(_) => Ok(()),
        _ => Err("must be a string, a number or a boolean".to_owned()),
    }
}

/// Checks a JSON pointer: empty (the whole value) or starting with `/`.
///
/// # Errors
/// A description of the problem.
pub fn check_pointer(pointer: &str) -> Result<(), String> {
    if pointer.is_empty() || pointer.starts_with('/') {
        Ok(())
    } else {
        Err(format!(
            "`{pointer}` is not a JSON pointer (it must be empty or start with `/`)"
        ))
    }
}

/// Parses `NAME=VALUE` (a `--var` argument). The value is a boolean or a number when it
/// reads as one (`true`, `42`, `1.5`), else a string.
///
/// # Errors
/// A bad name or a missing `=`.
pub fn parse_assignment(text: &str) -> Result<(String, Value), String> {
    let (name, raw) = text
        .split_once('=')
        .ok_or_else(|| format!("`{text}` is not NAME=VALUE"))?;
    if !is_valid_name(name) {
        return Err(format!(
            "`{name}` is not a valid variable name (use letters, digits and `_`, not starting with a digit)"
        ));
    }
    let value = match serde_json::from_str::<Value>(raw) {
        Ok(v @ (Value::Bool(_) | Value::Number(_))) if raw.trim() == raw => v,
        _ => Value::String(raw.to_owned()),
    };
    Ok((name.to_owned(), value))
}

/// A map of variables.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Vars(BTreeMap<String, Value>);

impl Vars {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a variable (any JSON value; a capture may store objects).
    ///
    /// # Errors
    /// The name is not valid.
    pub fn insert(&mut self, name: &str, value: Value) -> Result<(), String> {
        if !is_valid_name(name) {
            return Err(format!("`{name}` is not a valid variable name"));
        }
        self.0.insert(name.to_owned(), value);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }
}

/// Replaces `${name}` in every string of `value` (recursively through arrays and objects;
/// object keys are left alone). A string that is exactly `${name}` takes the variable's
/// value with its JSON type.
///
/// # Errors
/// An unknown variable, a malformed `${`, or a non-scalar value inside longer text.
pub fn substitute(value: &Value, vars: &Vars) -> Result<Value, String> {
    let mut budget = MAX_SUBSTITUTED_BYTES;
    substitute_in(value, vars, &mut budget)
}

/// Longest result of one [`substitute`] call (approximate serialized bytes): a variable that
/// holds a large captured payload must not be multiplied without bound.
pub const MAX_SUBSTITUTED_BYTES: usize = 1024 * 1024;

/// How many characters of a foreign value a failure reason shows.
const CLIP_CHARS: usize = 200;

/// `text` cut to about 200 characters with a `...` marker; for values a peer controls that
/// end up in failure reasons.
#[must_use]
pub fn clip_text(text: &str) -> String {
    match text.char_indices().nth(CLIP_CHARS) {
        Some((at, _)) => format!("{}... ({} bytes)", &text[..at], text.len()),
        None => text.to_owned(),
    }
}

/// The JSON text of `value`, clipped like [`clip_text`].
#[must_use]
pub fn clip_json(value: &Value) -> String {
    clip_text(&value.to_string())
}

/// A cheap upper-ish estimate of the serialized size of `value`, without allocating.
fn approx_size(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(_) => 5,
        Value::Number(_) => 24,
        Value::String(s) => s.len() + 2,
        Value::Array(items) => 2 + items.iter().map(|v| approx_size(v) + 1).sum::<usize>(),
        Value::Object(map) => {
            2 + map
                .iter()
                .map(|(k, v)| k.len() + 4 + approx_size(v))
                .sum::<usize>()
        }
    }
}

fn spend(budget: &mut usize, bytes: usize) -> Result<(), String> {
    *budget = budget.checked_sub(bytes).ok_or_else(|| {
        format!("the substituted value is larger than {MAX_SUBSTITUTED_BYTES} bytes")
    })?;
    Ok(())
}

fn substitute_in(value: &Value, vars: &Vars, budget: &mut usize) -> Result<Value, String> {
    Ok(match value {
        Value::String(text) => {
            if let Some(name) = exact_reference(text) {
                let found = lookup(vars, name)?;
                spend(budget, approx_size(found))?;
                return Ok(found.clone());
            }
            let out = substitute_str(text, vars)?;
            spend(budget, out.len() + 2)?;
            Value::String(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| substitute_in(item, vars, budget))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| Ok((key.clone(), substitute_in(item, vars, budget)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

/// `name` when `text` is exactly `${name}` with a valid name.
fn exact_reference(text: &str) -> Option<&str> {
    let inner = text.strip_prefix("${")?.strip_suffix('}')?;
    is_valid_name(inner).then_some(inner)
}

fn lookup<'a>(vars: &'a Vars, name: &str) -> Result<&'a Value, String> {
    vars.get(name)
        .ok_or_else(|| format!("unknown variable `{name}`"))
}

/// Replaces `${name}` in `text` with the variable's text (always a string result; see the
/// module docs for the rules).
///
/// # Errors
/// An unknown variable, a malformed `${`, or an object/array/null value.
pub fn substitute_str(text: &str, vars: &Vars) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("$${") {
            out.push_str("${");
            rest = after;
        } else if let Some(after) = tail.strip_prefix("${") {
            let end = after
                .find('}')
                .ok_or_else(|| format!("unterminated `${{` in `{text}`"))?;
            let name = &after[..end];
            if !is_valid_name(name) {
                return Err(format!("invalid variable name `{name}` in `${{{name}}}`"));
            }
            match lookup(vars, name)? {
                Value::String(s) => out.push_str(s),
                v @ (Value::Number(_) | Value::Bool(_)) => out.push_str(&v.to_string()),
                _ => {
                    return Err(format!(
                        "variable `{name}` is not text, a number or a boolean; use it as the whole value (`\"${{{name}}}\"`)"
                    ));
                }
            }
            rest = &after[end + 1..];
        } else {
            out.push('$');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Reads the value at the JSON pointer `pointer` (RFC 6901; empty = the whole value).
///
/// # Errors
/// A bad pointer or nothing at that place.
pub fn capture(value: &Value, pointer: &str) -> Result<Value, String> {
    check_pointer(pointer)?;
    value
        .pointer(pointer)
        .cloned()
        .ok_or_else(|| format!("nothing at `{pointer}` in {}", clip_json(value)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn vars() -> Vars {
        let mut v = Vars::new();
        v.insert("n", json!(5)).unwrap();
        v.insert("s", json!("abc")).unwrap();
        v.insert("b", json!(true)).unwrap();
        v.insert("o", json!({"a": 1})).unwrap();
        v
    }

    #[test]
    fn substitution_output_is_capped() {
        let mut v = Vars::new();
        v.insert("big", json!("x".repeat(300_000))).unwrap();
        let ok = json!(["${big}", "${big}"]);
        assert!(substitute(&ok, &v).is_ok());
        let many = Value::Array(vec![json!("${big}"); 5]);
        let e = substitute(&many, &v).unwrap_err();
        assert!(e.contains("larger than"), "{e}");
        let text = json!({"k": "${big}${big}${big}${big}"});
        assert!(substitute(&text, &v).is_err());
    }

    #[test]
    fn peer_values_are_clipped_in_messages() {
        let long = json!({"k": "y".repeat(5000)});
        let e = capture(&long, "/nope").unwrap_err();
        assert!(e.len() < 400, "{}", e.len());
        assert_eq!(clip_text("short"), "short");
        assert!(clip_text(&"é".repeat(500)).contains("..."));
    }

    #[test]
    fn names() {
        for ok in ["a", "_a", "A9_", "station_id"] {
            assert!(is_valid_name(ok), "{ok}");
        }
        for bad in ["", "9a", "a-b", "a b", "é"] {
            assert!(!is_valid_name(bad), "{bad}");
        }
        assert!(is_builtin("index") && !is_builtin("n"));
    }

    #[test]
    fn exact_reference_keeps_the_type() {
        let v = vars();
        assert_eq!(substitute(&json!("${n}"), &v).unwrap(), json!(5));
        assert_eq!(substitute(&json!("${b}"), &v).unwrap(), json!(true));
        assert_eq!(substitute(&json!("${o}"), &v).unwrap(), json!({"a": 1}));
        assert_eq!(substitute(&json!("${s}"), &v).unwrap(), json!("abc"));
    }

    #[test]
    fn embedded_references_become_text() {
        let v = vars();
        assert_eq!(
            substitute_str("x-${s}-${n}-${b}!", &v).unwrap(),
            "x-abc-5-true!"
        );
        assert_eq!(
            substitute(&json!(" ${n}"), &v).unwrap(),
            json!(" 5"),
            "not an exact reference"
        );
    }

    #[test]
    fn recursion_through_arrays_and_objects() {
        let v = vars();
        let got = substitute(&json!({"k${n}": ["${n}", {"x": "a${s}"}, 7, null]}), &v).unwrap();
        assert_eq!(got, json!({"k${n}": [5, {"x": "aabc"}, 7, null]}));
    }

    #[test]
    fn escapes() {
        let v = vars();
        assert_eq!(substitute_str("$${n} ${n}", &v).unwrap(), "${n} 5");
        assert_eq!(substitute(&json!("$${n}"), &v).unwrap(), json!("${n}"));
        assert_eq!(substitute_str("cost $5 $$ $", &v).unwrap(), "cost $5 $$ $");
    }

    #[test]
    fn errors_name_the_variable() {
        let v = vars();
        let e = substitute_str("${nope}", &v).unwrap_err();
        assert!(e.contains("nope"), "{e}");
        let e = substitute(&json!("${nope}"), &v).unwrap_err();
        assert!(e.contains("nope"), "{e}");
        assert!(
            substitute_str("a ${n", &v)
                .unwrap_err()
                .contains("unterminated")
        );
        assert!(substitute_str("${}", &v).is_err());
        assert!(substitute_str("${a b}", &v).unwrap_err().contains("a b"));
        let e = substitute_str("x ${o}", &v).unwrap_err();
        assert!(e.contains('o') && e.contains("not text"), "{e}");
    }

    #[test]
    fn capture_by_pointer() {
        let doc = json!({"a": {"b": [10, 20]}, "k~/": 1});
        assert_eq!(capture(&doc, "/a/b/1").unwrap(), json!(20));
        assert_eq!(capture(&doc, "/a").unwrap(), json!({"b": [10, 20]}));
        assert_eq!(capture(&doc, "").unwrap(), doc);
        assert_eq!(capture(&doc, "/k~0~1").unwrap(), json!(1));
        assert!(capture(&doc, "/a/c").unwrap_err().contains("/a/c"));
        assert!(capture(&doc, "a").unwrap_err().contains("JSON pointer"));
    }

    #[test]
    fn assignments() {
        assert_eq!(parse_assignment("a=1").unwrap(), ("a".into(), json!(1)));
        assert_eq!(parse_assignment("a=1.5").unwrap().1, json!(1.5));
        assert_eq!(parse_assignment("a=true").unwrap().1, json!(true));
        assert_eq!(parse_assignment("a=007").unwrap().1, json!("007"));
        assert_eq!(parse_assignment("a=").unwrap().1, json!(""));
        assert_eq!(parse_assignment("a=x=y").unwrap().1, json!("x=y"));
        assert_eq!(parse_assignment("a=\"q\"").unwrap().1, json!("\"q\""));
        assert!(parse_assignment("a").is_err());
        assert!(parse_assignment("1a=2").is_err());
        assert!(parse_assignment("=2").is_err());
    }

    #[test]
    fn scalars() {
        assert!(check_scalar(&json!("x")).is_ok());
        assert!(check_scalar(&json!(1)).is_ok());
        assert!(check_scalar(&json!(false)).is_ok());
        assert!(check_scalar(&json!(null)).is_err());
        assert!(check_scalar(&json!([1])).is_err());
        assert!(check_scalar(&json!({})).is_err());
    }
}
