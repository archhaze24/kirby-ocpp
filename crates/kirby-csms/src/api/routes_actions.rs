//! The action tables: what the server can send in a protocol and what each action takes.

use axum::http::{StatusCode, Uri};
use axum::response::Response;
use kirby_csms_core::registry::{self, ActionInput, ActionSpec, FieldSpec, FieldType};
use serde_json::{Value, json};

use super::error::{error, reply};
use super::json::query_param;

/// `GET /v1/actions?protocol=`: the actions the server can send to a station of the
/// protocol, in menu order, each with its input fields.
pub(super) async fn actions(uri: Uri) -> Response {
    let bad = |message: &str| error(StatusCode::BAD_REQUEST, "bad_request", message);
    let protocol = match query_param(&uri, "protocol") {
        Ok(Some(protocol)) => protocol,
        Ok(None) => return bad("the query parameter `protocol` is required"),
        Err(message) => return bad(&message),
    };
    match action_list(&protocol) {
        Some(list) => reply(StatusCode::OK, &list),
        None => bad(&format!(
            "no action table for protocol {}; supported: {}",
            kirby_ocpp_tui::text::sanitize(&protocol),
            registry::protocols().collect::<Vec<_>>().join(", ")
        )),
    }
}

/// The answer for `protocol`; `None` when it has no table.
fn action_list(protocol: &str) -> Option<Value> {
    let table = registry::protocol_actions(protocol)?;
    let actions: Vec<Value> = table
        .csms_actions
        .iter()
        .map(|spec| action_json(table.protocol, spec))
        .collect();
    Some(json!({"protocol": table.protocol, "actions": actions}))
}

fn action_json(protocol: &str, spec: &ActionSpec) -> Value {
    let input = registry::takes_input(protocol, spec);
    let template = if input == ActionInput::Raw {
        registry::payload_template(protocol, spec.name)
    } else {
        None
    };
    json!({
        "name": spec.name,
        "title": spec.title,
        "summary": spec.summary,
        "group": spec.group.as_str(),
        "input": match input {
            ActionInput::Form => "form",
            ActionInput::None => "none",
            _ => "raw",
        },
        "fields": spec.fields.iter().map(field_json).collect::<Vec<_>>(),
        "template": template,
    })
}

fn field_json(field: &FieldSpec) -> Value {
    json!({
        "name": field.name,
        "label": field.label,
        "type": field_type(field),
        "choices": field.choices,
        "default": field.default,
        "required": field.required,
        "hint": field.hint,
    })
}

/// The lowercase name of the field type; a choice without listed values is `transaction`
/// (the client fills it with the running transactions of the station).
fn field_type(field: &FieldSpec) -> String {
    if field.ty == FieldType::Choice && field.choices.is_empty() {
        "transaction".to_owned()
    } else {
        format!("{:?}", field.ty).to_ascii_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(list: &'a Value, name: &str) -> &'a Value {
        list["actions"]
            .as_array()
            .and_then(|actions| actions.iter().find(|action| action["name"] == name))
            .unwrap_or_else(|| panic!("no action {name}"))
    }

    #[test]
    fn the_three_tables_keep_registry_order() {
        for protocol in registry::protocols() {
            let list = action_list(protocol).expect("table");
            assert_eq!(list["protocol"], protocol);
            let names: Vec<&str> = list["actions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|action| action["name"].as_str().unwrap())
                .collect();
            let table: Vec<&str> = registry::protocol_actions(protocol)
                .unwrap()
                .csms_actions
                .iter()
                .map(|spec| spec.name)
                .collect();
            assert_eq!(names, table, "{protocol}");
        }
        assert!(action_list("ocpp9.9").is_none());
    }

    #[test]
    fn input_kinds_follow_the_payload_rules() {
        let list = action_list("ocpp2.1").unwrap();
        let raw = find(&list, "SetDERControl");
        assert_eq!(raw["input"], "raw");
        assert_eq!(raw["fields"], json!([]));
        assert!(raw["template"].is_object(), "{raw}");
        let none = find(&list, "GetPeriodicEventStream");
        assert_eq!(none["input"], "none");
        assert_eq!(none["template"], Value::Null);
        let form = find(&list, "UsePriorityCharging");
        assert_eq!(form["input"], "form");
        assert_eq!(form["template"], Value::Null);
        let types: Vec<&str> = form["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| field["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, ["transaction", "boolean"]);
    }

    #[test]
    fn field_values_are_never_absent() {
        let list = action_list("ocpp1.6").unwrap();
        let reset = find(&list, "Reset");
        assert_eq!(
            reset["fields"][0],
            json!({
                "name": "type",
                "label": "type",
                "type": "choice",
                "choices": ["Hard", "Soft"],
                "default": "Soft",
                "required": true,
                "hint": "",
            })
        );
    }
}
