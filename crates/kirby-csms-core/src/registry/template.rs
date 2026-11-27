//! Minimal valid request payloads, derived from the request schemas: what a form for a raw
//! payload starts with, so a person edits a valid message instead of an empty one.

use kirby_ocpp_core::ocpp::{PayloadKind, Protocol, SchemaRegistry};
use serde_json::{Map, Value, json};

/// How deep the generator follows nested objects (a recursive schema stops here).
const DEPTH: usize = 12;

/// The smallest payload that satisfies the request schema of `action` in `protocol`
/// (a subprotocol token such as `ocpp2.0.1`): required members only, the first value of an
/// enumeration, the lowest allowed number. `None` for an unknown action or protocol, and when
/// no valid payload could be derived.
pub fn payload_template(protocol: &str, action: &str) -> Option<Value> {
    let registry = SchemaRegistry::for_protocol(Protocol::from_token(protocol)?).ok()?;
    let source = registry.schema_source(action, PayloadKind::Request)?;
    let root: Value = serde_json::from_str(source).ok()?;
    let template = build(&root, &root, 0);
    registry
        .validate_request(action, &template)
        .is_ok()
        .then_some(template)
}

/// The schema node `node` with a local `$ref` followed.
fn resolve<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
    node.get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix("#/definitions/"))
        .and_then(|key| root.get("definitions")?.get(key))
        .unwrap_or(node)
}

fn build(root: &Value, node: &Value, depth: usize) -> Value {
    let node = resolve(root, node);
    if let Some(first) = node
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|list| list.first())
    {
        return first.clone();
    }
    if let Some(first) = ["oneOf", "anyOf", "allOf"]
        .iter()
        .find_map(|key| node.get(*key)?.as_array()?.first())
        && node.get("type").is_none()
    {
        return build(root, first, depth + 1);
    }
    match node.get("type").and_then(Value::as_str) {
        Some("object") => object(root, node, depth),
        Some("array") => array(root, node, depth),
        Some("string") => string(node),
        Some("integer") => Value::from(lowest_integer(node)),
        Some("number") => Value::from(lowest_number(node)),
        Some("boolean") => Value::Bool(false),
        _ => Value::Null,
    }
}

fn object(root: &Value, node: &Value, depth: usize) -> Value {
    let mut out = Map::new();
    if depth >= DEPTH {
        return Value::Object(out);
    }
    let required = node
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    for name in required {
        if let Some(schema) = node.get("properties").and_then(|p| p.get(name)) {
            out.insert(name.to_owned(), build(root, schema, depth + 1));
        }
    }
    Value::Object(out)
}

fn array(root: &Value, node: &Value, depth: usize) -> Value {
    let wanted = node.get("minItems").and_then(Value::as_u64).unwrap_or(0);
    let Some(items) = node.get("items").filter(|_| depth < DEPTH) else {
        return json!([]);
    };
    let count = usize::try_from(wanted).unwrap_or(1).min(8);
    Value::Array((0..count).map(|_| build(root, items, depth + 1)).collect())
}

fn string(node: &Value) -> Value {
    match node.get("format").and_then(Value::as_str) {
        Some("date-time") => return json!("2030-01-01T00:00:00Z"),
        Some("uri") => return json!("https://example.com"),
        _ => {}
    }
    let min = node
        .get("minLength")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0);
    Value::String("x".repeat(min.clamp(1, 64)))
}

/// The lowest whole number from 0 the schema allows.
fn lowest_integer(node: &Value) -> i64 {
    let minimum = node.get("minimum").and_then(Value::as_i64);
    let above = node
        .get("exclusiveMinimum")
        .and_then(Value::as_i64)
        .map(|v| v + 1);
    minimum.into_iter().chain(above).fold(0, i64::max)
}

/// The lowest number from 0 the schema allows.
fn lowest_number(node: &Value) -> f64 {
    let minimum = node.get("minimum").and_then(Value::as_f64);
    let above = node
        .get("exclusiveMinimum")
        .and_then(Value::as_f64)
        .map(|v| v + 1.0);
    minimum.into_iter().chain(above).fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry;

    /// Every action a central system can send has a template that satisfies its schema: the
    /// forms and the raw call start from it.
    #[test]
    fn every_csms_action_has_a_valid_template() {
        for protocol in registry::protocols() {
            let table = registry::protocol_actions(protocol).expect("table");
            for spec in table.csms_actions {
                assert!(
                    payload_template(protocol, spec.name).is_some(),
                    "{protocol} {}: no valid template",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn templates_hold_the_required_members_only() {
        let reset = payload_template("ocpp1.6", "Reset").expect("template");
        assert_eq!(reset, json!({"type": "Hard"}));
        let start = payload_template("ocpp2.0.1", "RequestStartTransaction").expect("template");
        assert!(start.get("remoteStartId").is_some());
        assert!(
            start.get("evseId").is_none(),
            "optional members are left out"
        );
        assert!(payload_template("ocpp9.9", "Reset").is_none());
        assert!(payload_template("ocpp1.6", "NoSuchAction").is_none());
    }

    #[test]
    fn times_and_urls_use_their_formats() {
        let update = payload_template("ocpp1.6", "UpdateFirmware").expect("template");
        assert_eq!(update["location"], "https://example.com");
        assert_eq!(update["retrieveDate"], "2030-01-01T00:00:00Z");
    }
}
