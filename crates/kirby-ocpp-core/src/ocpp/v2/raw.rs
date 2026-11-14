//! Untyped 2.0.1 actions: a runtime action name with a JSON object payload.
//!
//! [`RawAction`] carries the 44 actions without a typed payload, and any name an explicit
//! negative test wants to send. It does not implement
//! [`Action`](crate::ocpp::action::Action), whose name is a compile-time constant. Unknown
//! names are representable on purpose; validation reports them as
//! [`ViolationCause::UnknownAction`](crate::ocpp::schema::ViolationCause::UnknownAction).

use serde_json::{Map, Value};
use thiserror::Error;

use super::registry::{self, ActionInfo};
use crate::ocpp::Protocol;
use crate::ocpp::frame::ErrorCode;
use crate::ocpp::schema::{SchemaError, SchemaRegistry, SchemaViolation};

/// A raw payload that is not a JSON object.
#[derive(Debug, Clone, Error)]
#[error("the payload of {action} must be a JSON object, not {found}")]
pub struct NotAnObject {
    /// The action the payload was meant for.
    pub action: String,
    /// The JSON type that was given: `null`, `boolean`, `number`, `string` or `array`.
    pub found: &'static str,
}

/// Why [`RawAction::validate_request`] or [`RawAction::validate_response`] failed.
#[derive(Debug, Clone, Error)]
pub enum RawValidationError {
    /// The payload breaks the 2.0.1 schema.
    #[error(transparent)]
    Violation(#[from] SchemaViolation),
    /// The 2.0.1 schemas are not available.
    #[error(transparent)]
    Schemas(#[from] SchemaError),
}

/// An action name and a JSON object payload, both decided at runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct RawAction {
    name: String,
    payload: Map<String, Value>,
}

impl RawAction {
    /// Wraps `payload` for action `name`. The name is not checked against the catalog.
    ///
    /// # Errors
    /// [`NotAnObject`] if `payload` is not a JSON object.
    pub fn new(name: impl Into<String>, payload: Value) -> Result<Self, NotAnObject> {
        let name = name.into();
        match payload {
            Value::Object(payload) => Ok(Self { name, payload }),
            other => Err(NotAnObject {
                action: name,
                found: json_type(&other),
            }),
        }
    }

    /// The action name as it goes on the wire.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The payload object.
    pub fn payload(&self) -> &Map<String, Value> {
        &self.payload
    }

    /// The catalog entry of the action, or `None` for a name 2.0.1 does not define.
    pub fn info(&self) -> Option<&'static ActionInfo> {
        registry::lookup(&self.name)
    }

    /// `true` if 2.0.1 defines the action.
    pub fn is_known(&self) -> bool {
        registry::is_known(&self.name)
    }

    /// The `(action, payload)` pair the raw call path takes (`Command::SendRaw`'s `action`
    /// and `payload`; it adds the `validate` flag).
    pub fn into_parts(self) -> (String, Value) {
        (self.name, Value::Object(self.payload))
    }

    /// Validates the payload as the request of the action against the shared 2.0.1
    /// schemas.
    ///
    /// # Errors
    /// [`RawValidationError::Violation`] with the issues, including
    /// [`ViolationCause::UnknownAction`](crate::ocpp::schema::ViolationCause::UnknownAction)
    /// for an unknown name; [`RawValidationError::Schemas`] if the schemas cannot be loaded.
    pub fn validate_request(&self) -> Result<(), RawValidationError> {
        let registry = SchemaRegistry::for_protocol(Protocol::V201)?;
        self.validate_request_with(&registry).map_err(Into::into)
    }

    /// Validates the payload as the response to a CALL of the action against the shared
    /// 2.0.1 schemas. Errors as in [`Self::validate_request`].
    pub fn validate_response(&self) -> Result<(), RawValidationError> {
        let registry = SchemaRegistry::for_protocol(Protocol::V201)?;
        self.validate_response_with(&registry).map_err(Into::into)
    }

    /// Like [`Self::validate_request`] against a registry the caller already holds. The
    /// registry should be the 2.0.1 one.
    pub fn validate_request_with(&self, registry: &SchemaRegistry) -> Result<(), SchemaViolation> {
        registry.validate_request(&self.name, &Value::Object(self.payload.clone()))
    }

    /// Like [`Self::validate_response`] against a registry the caller already holds.
    pub fn validate_response_with(&self, registry: &SchemaRegistry) -> Result<(), SchemaViolation> {
        registry.validate_response(&self.name, &Value::Object(self.payload.clone()))
    }

    /// The CALLERROR code for an incoming CALL of this action that has no handler. See
    /// [`registry::unhandled_error_code`]: `NotImplemented` for an unknown name,
    /// `NotSupported` for every defined action, typed or not.
    pub fn unhandled_error_code(&self) -> ErrorCode {
        registry::unhandled_error_code(&self.name)
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ocpp::schema::ViolationCause;

    fn raw(name: &str, payload: Value) -> RawAction {
        RawAction::new(name, payload).unwrap()
    }

    #[test]
    fn rejects_non_object_payloads() {
        for (payload, found) in [
            (json!(null), "null"),
            (json!(true), "boolean"),
            (json!(1), "number"),
            (json!("x"), "string"),
            (json!([]), "array"),
        ] {
            let error = RawAction::new("GetLog", payload).unwrap_err();
            assert_eq!(error.action, "GetLog");
            assert_eq!(error.found, found);
        }
    }

    #[test]
    fn valid_minimal_payload_validates() {
        raw("GetLocalListVersion", json!({}))
            .validate_request()
            .unwrap();
        raw("GetLocalListVersion", json!({"versionNumber": 3}))
            .validate_response()
            .unwrap();
        raw(
            "GetLog",
            json!({
                "logType": "DiagnosticsLog",
                "requestId": 1,
                "log": {"remoteLocation": "https://example.com/up"}
            }),
        )
        .validate_request()
        .unwrap();
    }

    #[test]
    fn invalid_payload_reports_the_path() {
        let error = raw(
            "GetLog",
            json!({"logType": "Nope", "requestId": 1, "log": {"remoteLocation": "x"}}),
        )
        .validate_request()
        .unwrap_err();
        let RawValidationError::Violation(violation) = error else {
            panic!("expected a violation");
        };
        assert_eq!(violation.protocol, Protocol::V201);
        assert!(
            violation
                .issues
                .iter()
                .any(|i| i.instance_path == "/logType"),
            "{:?}",
            violation.issues
        );
        assert_eq!(
            violation.error_code_for(Protocol::V201).as_str(),
            "PropertyConstraintViolation"
        );

        let missing = raw("GetLog", json!({"requestId": 1}))
            .validate_request()
            .unwrap_err();
        let RawValidationError::Violation(missing) = missing else {
            panic!("expected a violation");
        };
        assert_eq!(
            missing.error_code_for(Protocol::V201).as_str(),
            "OccurrenceConstraintViolation"
        );

        let response = raw("GetLocalListVersion", json!({}))
            .validate_response()
            .unwrap_err();
        assert!(matches!(response, RawValidationError::Violation(_)));
    }

    #[test]
    fn unknown_name_is_representable_and_fails_validation() {
        let action = raw("StartTransaction", json!({"idTag": "x"}));
        assert!(!action.is_known());
        assert!(action.info().is_none());
        let RawValidationError::Violation(violation) = action.validate_request().unwrap_err()
        else {
            panic!("expected a violation");
        };
        assert_eq!(violation.cause, ViolationCause::UnknownAction);
        assert_eq!(action.unhandled_error_code(), ErrorCode::NotImplemented);
    }

    #[test]
    fn known_actions_are_not_supported_when_unhandled() {
        let untyped = raw("GetLog", json!({}));
        assert!(untyped.is_known());
        assert_eq!(untyped.unhandled_error_code(), ErrorCode::NotSupported);
        let typed = raw("Reset", json!({"type": "Immediate"}));
        assert_eq!(typed.info().map(|i| i.typed), Some(true));
        assert_eq!(typed.unhandled_error_code(), ErrorCode::NotSupported);
    }

    #[test]
    fn into_parts_feeds_the_raw_call_path() {
        let (name, payload) = raw("GetLog", json!({"requestId": 1})).into_parts();
        assert_eq!(name, "GetLog");
        assert_eq!(payload, json!({"requestId": 1}));
    }
}
