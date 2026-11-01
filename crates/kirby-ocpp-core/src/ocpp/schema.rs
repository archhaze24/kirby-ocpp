//! JSON-schema validation of OCPP payloads.
//!
//! The bundled OCPP 1.6J schemas (draft-04, 28 actions x request/response) are
//! the source of truth for payload validity. Validation applies to **both
//! directions**: requests we send, requests we receive, responses we receive and
//! the replies we send ourselves.
//!
//! The schemas live in `crates/kirby-ocpp-core/schemas/` and are embedded with
//! `include_str!` (no runtime file lookup). [`SchemaRegistry::new`] compiles each
//! of them once with the `jsonschema` crate forced to Draft 4. Before compiling,
//! the root `$schema` and `id` keys are removed (as the TS validator does): the
//! official files carry `"id": "urn:OCPP:1.6:..."`, which Draft 4 would treat as
//! the base URI of the document. Only the **root** keys are stripped, so a
//! property that is literally named `id` (`ClearChargingProfile`) is untouched.
//! `format` is validated (`date-time`, `uri`), unknown keywords such as
//! `javaType` are ignored.

use std::collections::HashMap;

use jsonschema::Validator;
use serde_json::Value;
use thiserror::Error;

use crate::ocpp::frame::ErrorCode;

/// Which payload of an action a schema describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PayloadKind {
    /// The CALL payload.
    Request,
    /// The CALLRESULT payload.
    Response,
}

impl std::fmt::Display for PayloadKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Request => "request",
            Self::Response => "response",
        })
    }
}

/// The JSON-schema keyword that a validation error originated from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SchemaKeyword {
    Required,
    Type,
    AdditionalProperties,
    Enum,
    MaxLength,
    MinLength,
    Minimum,
    Maximum,
    MultipleOf,
    Format,
    /// Any other keyword, by name.
    Other(String),
}

fn keyword_name(keyword: &SchemaKeyword) -> &str {
    match keyword {
        SchemaKeyword::Required => "required",
        SchemaKeyword::Type => "type",
        SchemaKeyword::AdditionalProperties => "additionalProperties",
        SchemaKeyword::Enum => "enum",
        SchemaKeyword::MaxLength => "maxLength",
        SchemaKeyword::MinLength => "minLength",
        SchemaKeyword::Minimum => "minimum",
        SchemaKeyword::Maximum => "maximum",
        SchemaKeyword::MultipleOf => "multipleOf",
        SchemaKeyword::Format => "format",
        SchemaKeyword::Other(name) => name,
    }
}

impl SchemaKeyword {
    /// Maps a draft-04 keyword name (as reported by the validator) to a variant.
    pub fn from_name(name: &str) -> Self {
        match name {
            "required" => Self::Required,
            "type" => Self::Type,
            "additionalProperties" => Self::AdditionalProperties,
            "enum" => Self::Enum,
            "maxLength" => Self::MaxLength,
            "minLength" => Self::MinLength,
            "minimum" => Self::Minimum,
            "maximum" => Self::Maximum,
            "multipleOf" => Self::MultipleOf,
            "format" => Self::Format,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// Why a payload was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViolationCause {
    /// There is no schema for this action name (not an OCPP 1.6J action).
    UnknownAction,
    /// Schema errors, as the keywords that failed (one entry per error).
    Keywords(Vec<SchemaKeyword>),
}

/// One schema error: where in the payload, which keyword, what the validator said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaIssue {
    /// JSON pointer to the offending value (`""` is the payload root).
    pub instance_path: String,
    pub keyword: SchemaKeyword,
    pub message: String,
}

impl std::fmt::Display for SchemaIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let path = if self.instance_path.is_empty() {
            "/"
        } else {
            &self.instance_path
        };
        write!(f, "{path}: {}", self.message)
    }
}

/// A payload that does not conform to the schema of its action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaViolation {
    pub action: String,
    pub kind: PayloadKind,
    pub cause: ViolationCause,
    /// Human-readable error messages (with instance paths), for logs and CALLERROR descriptions.
    pub errors: Vec<String>,
    /// The same errors, structured. Sorted by instance path, then keyword, then
    /// message, so the order does not depend on the validator's traversal.
    pub issues: Vec<SchemaIssue>,
}

impl std::fmt::Display for SchemaViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} violates OCPP 1.6J schema ({}): {}",
            self.action,
            self.kind,
            self.error_code(),
            self.errors.join("; ")
        )
    }
}

impl std::error::Error for SchemaViolation {}

impl SchemaViolation {
    fn unknown_action(action: &str, kind: PayloadKind) -> Self {
        Self {
            action: action.to_owned(),
            kind,
            cause: ViolationCause::UnknownAction,
            errors: vec![format!("`{action}` is not an OCPP 1.6J action")],
            issues: Vec::new(),
        }
    }

    fn from_issues(action: &str, kind: PayloadKind, mut issues: Vec<SchemaIssue>) -> Self {
        issues.sort_by(|a, b| {
            (&a.instance_path, keyword_name(&a.keyword), &a.message).cmp(&(
                &b.instance_path,
                keyword_name(&b.keyword),
                &b.message,
            ))
        });
        Self {
            action: action.to_owned(),
            kind,
            cause: ViolationCause::Keywords(issues.iter().map(|i| i.keyword.clone()).collect()),
            errors: issues.iter().map(ToString::to_string).collect(),
            issues,
        }
    }

    /// The CALLERROR code to answer with / record, per `core-analysis.md`:
    ///
    /// * unknown action -> `NotImplemented`;
    /// * otherwise by priority: any `required` -> `OccurenceConstraintViolation`
    ///   (the 1.6 spelling); else any `type` -> `TypeConstraintViolation`; else any of
    ///   `additionalProperties`/`enum`/`maxLength`/`minLength`/`minimum`/`maximum`/
    ///   `multipleOf`/`format` -> `PropertyConstraintViolation`; else `FormationViolation`.
    pub fn error_code(&self) -> ErrorCode {
        let ViolationCause::Keywords(keywords) = &self.cause else {
            return ErrorCode::NotImplemented;
        };
        if keywords.contains(&SchemaKeyword::Required) {
            ErrorCode::OccurenceConstraintViolation
        } else if keywords.contains(&SchemaKeyword::Type) {
            ErrorCode::TypeConstraintViolation
        } else if keywords.iter().any(|k| {
            matches!(
                k,
                SchemaKeyword::AdditionalProperties
                    | SchemaKeyword::Enum
                    | SchemaKeyword::MaxLength
                    | SchemaKeyword::MinLength
                    | SchemaKeyword::Minimum
                    | SchemaKeyword::Maximum
                    | SchemaKeyword::MultipleOf
                    | SchemaKeyword::Format
            )
        }) {
            ErrorCode::PropertyConstraintViolation
        } else {
            ErrorCode::FormationViolation
        }
    }
}

/// Expands to the table of embedded schema files: `(action, request json, response json)`.
macro_rules! embedded_schemas {
    ($($action:literal),+ $(,)?) => {
        &[$((
            $action,
            include_str!(concat!("../../schemas/", $action, ".json")),
            include_str!(concat!("../../schemas/", $action, "Response.json")),
        )),+]
    };
}

/// Every OCPP 1.6J action with its embedded request and response schema source.
const EMBEDDED: &[(&str, &str, &str)] = embedded_schemas![
    "Authorize",
    "BootNotification",
    "CancelReservation",
    "ChangeAvailability",
    "ChangeConfiguration",
    "ClearCache",
    "ClearChargingProfile",
    "DataTransfer",
    "DiagnosticsStatusNotification",
    "FirmwareStatusNotification",
    "GetCompositeSchedule",
    "GetConfiguration",
    "GetDiagnostics",
    "GetLocalListVersion",
    "Heartbeat",
    "MeterValues",
    "RemoteStartTransaction",
    "RemoteStopTransaction",
    "ReserveNow",
    "Reset",
    "SendLocalList",
    "SetChargingProfile",
    "StartTransaction",
    "StatusNotification",
    "StopTransaction",
    "TriggerMessage",
    "UnlockConnector",
    "UpdateFirmware",
];

#[derive(Debug)]
struct Entry {
    request: Validator,
    response: Validator,
    request_source: &'static str,
    response_source: &'static str,
}

/// The compiled set of OCPP 1.6J schemas. Build once, share (`Arc`) freely.
#[derive(Debug)]
pub struct SchemaRegistry {
    actions: HashMap<&'static str, Entry>,
}

/// A bundled schema could not be compiled (a build-time bug, not a runtime condition).
#[derive(Debug, Error)]
#[error("cannot compile the {kind} schema of {action}: {message}")]
pub struct SchemaLoadError {
    pub action: String,
    pub kind: PayloadKind,
    pub message: String,
}

fn compile(
    action: &'static str,
    kind: PayloadKind,
    source: &str,
) -> Result<Validator, SchemaLoadError> {
    let fail = |message: String| SchemaLoadError {
        action: action.to_owned(),
        kind,
        message,
    };
    let mut schema: Value = serde_json::from_str(source).map_err(|e| fail(e.to_string()))?;
    if let Some(root) = schema.as_object_mut() {
        root.remove("$schema");
        root.remove("id");
    }
    jsonschema::draft4::options()
        .should_validate_formats(true)
        .build(&schema)
        .map_err(|e| fail(e.to_string()))
}

impl SchemaRegistry {
    /// Compiles all bundled schemas.
    pub fn new() -> Result<Self, SchemaLoadError> {
        let mut actions = HashMap::with_capacity(EMBEDDED.len());
        for &(action, request_source, response_source) in EMBEDDED {
            actions.insert(
                action,
                Entry {
                    request: compile(action, PayloadKind::Request, request_source)?,
                    response: compile(action, PayloadKind::Response, response_source)?,
                    request_source,
                    response_source,
                },
            );
        }
        Ok(Self { actions })
    }

    /// `true` if `action` is one of the 28 OCPP 1.6J actions.
    pub fn knows_action(&self, action: &str) -> bool {
        self.actions.contains_key(action)
    }

    /// The 28 action names, in alphabetical order.
    pub fn action_names(&self) -> impl Iterator<Item = &'static str> {
        EMBEDDED.iter().map(|&(name, _, _)| name)
    }

    /// The embedded (unmodified) schema source for one payload of `action`.
    pub fn schema_source(&self, action: &str, kind: PayloadKind) -> Option<&'static str> {
        let entry = self.actions.get(action)?;
        Some(match kind {
            PayloadKind::Request => entry.request_source,
            PayloadKind::Response => entry.response_source,
        })
    }

    /// Validates the payload of a CALL. Unknown actions yield [`ViolationCause::UnknownAction`].
    pub fn validate_request(&self, action: &str, payload: &Value) -> Result<(), SchemaViolation> {
        self.validate(action, PayloadKind::Request, payload)
    }

    /// Validates the payload of a CALLRESULT for a CALL of `action`.
    pub fn validate_response(&self, action: &str, payload: &Value) -> Result<(), SchemaViolation> {
        self.validate(action, PayloadKind::Response, payload)
    }

    fn validate(
        &self,
        action: &str,
        kind: PayloadKind,
        payload: &Value,
    ) -> Result<(), SchemaViolation> {
        let Some(entry) = self.actions.get(action) else {
            return Err(SchemaViolation::unknown_action(action, kind));
        };
        let validator = match kind {
            PayloadKind::Request => &entry.request,
            PayloadKind::Response => &entry.response,
        };
        let issues: Vec<SchemaIssue> = validator
            .iter_errors(payload)
            .map(|error| SchemaIssue {
                instance_path: error.instance_path().to_string(),
                keyword: SchemaKeyword::from_name(error.kind().keyword()),
                message: error.to_string(),
            })
            .collect();
        if issues.is_empty() {
            Ok(())
        } else {
            Err(SchemaViolation::from_issues(action, kind, issues))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn violation(keywords: &[&str]) -> SchemaViolation {
        SchemaViolation {
            action: "Authorize".into(),
            kind: PayloadKind::Request,
            cause: ViolationCause::Keywords(
                keywords
                    .iter()
                    .map(|k| SchemaKeyword::from_name(k))
                    .collect(),
            ),
            errors: vec!["boom".into()],
            issues: vec![],
        }
    }

    #[test]
    fn required_beats_everything() {
        assert_eq!(
            violation(&["enum", "type", "required"]).error_code(),
            ErrorCode::OccurenceConstraintViolation
        );
    }

    #[test]
    fn type_beats_property_constraints() {
        assert_eq!(
            violation(&["maxLength", "type"]).error_code(),
            ErrorCode::TypeConstraintViolation
        );
    }

    #[test]
    fn property_constraint_keywords() {
        for keyword in [
            "additionalProperties",
            "enum",
            "maxLength",
            "minLength",
            "minimum",
            "maximum",
            "multipleOf",
            "format",
        ] {
            assert_eq!(
                violation(&[keyword]).error_code(),
                ErrorCode::PropertyConstraintViolation,
                "{keyword}"
            );
        }
    }

    #[test]
    fn anything_else_is_a_formation_violation() {
        assert_eq!(
            violation(&["pattern"]).error_code(),
            ErrorCode::FormationViolation
        );
        assert_eq!(violation(&[]).error_code(), ErrorCode::FormationViolation);
    }

    #[test]
    fn unknown_action_is_not_implemented() {
        let v = SchemaViolation {
            action: "Frobnicate".into(),
            kind: PayloadKind::Request,
            cause: ViolationCause::UnknownAction,
            errors: vec![],
            issues: vec![],
        };
        assert_eq!(v.error_code(), ErrorCode::NotImplemented);
    }

    #[test]
    fn display_names_the_action_code_and_errors() {
        let text = violation(&["required"]).to_string();
        assert_eq!(
            text,
            "Authorize request violates OCPP 1.6J schema (OccurenceConstraintViolation): boom"
        );
    }

    // ---- the compiled registry (ports tests/ocpp-validation.test.mjs) ----

    use serde_json::json;

    fn registry() -> SchemaRegistry {
        SchemaRegistry::new().expect("bundled schemas compile")
    }

    fn request_code(action: &str, payload: &Value) -> ErrorCode {
        registry()
            .validate_request(action, payload)
            .expect_err("payload must be rejected")
            .error_code()
    }

    #[test]
    fn registry_knows_exactly_the_28_actions() {
        let registry = registry();
        assert_eq!(registry.action_names().count(), 28);
        for name in registry.action_names() {
            assert!(registry.knows_action(name), "{name}");
            for kind in [PayloadKind::Request, PayloadKind::Response] {
                assert!(
                    registry.schema_source(name, kind).is_some(),
                    "{name} {kind}"
                );
            }
        }
        assert!(!registry.knows_action("Frobnicate"));
        assert!(!registry.knows_action("authorize"));
        assert!(!registry.knows_action("AuthorizeResponse"));
        assert!(!registry.knows_action(""));
    }

    #[test]
    fn valid_payloads_pass() {
        let registry = registry();
        registry.validate_request("Heartbeat", &json!({})).unwrap();
        registry
            .validate_request(
                "BootNotification",
                &json!({"chargePointVendor": "Kirby", "chargePointModel": "TUI-16"}),
            )
            .unwrap();
        registry
            .validate_response(
                "BootNotification",
                &json!({"status": "Accepted", "currentTime": "2026-05-06T07:08:09Z", "interval": 300}),
            )
            .unwrap();
    }

    #[test]
    fn missing_required_field_is_an_occurence_violation() {
        let violation = registry()
            .validate_request("RemoteStartTransaction", &json!({}))
            .unwrap_err();
        assert_eq!(
            violation.error_code(),
            ErrorCode::OccurenceConstraintViolation
        );
        assert_eq!(violation.issues.len(), 1);
        assert_eq!(violation.issues[0].keyword, SchemaKeyword::Required);
        assert_eq!(violation.issues[0].instance_path, "");
        assert!(violation.issues[0].message.contains("idTag"));
        assert!(violation.errors[0].starts_with("/: "));
    }

    #[test]
    fn missing_field_in_a_response_is_an_occurence_violation() {
        let violation = registry()
            .validate_response("BootNotification", &json!({"status": "Accepted"}))
            .unwrap_err();
        assert_eq!(violation.kind, PayloadKind::Response);
        assert_eq!(
            violation.error_code(),
            ErrorCode::OccurenceConstraintViolation
        );
    }

    #[test]
    fn wrong_type_is_a_type_violation() {
        assert_eq!(
            request_code("Authorize", &json!({"idTag": 5})),
            ErrorCode::TypeConstraintViolation
        );
        assert_eq!(
            request_code("UnlockConnector", &json!({"connectorId": "1"})),
            ErrorCode::TypeConstraintViolation
        );
        assert_eq!(
            request_code("Authorize", &json!([])),
            ErrorCode::TypeConstraintViolation
        );
    }

    #[test]
    fn bad_enum_is_a_property_violation() {
        let violation = registry()
            .validate_request("Reset", &json!({"type": "Warm"}))
            .unwrap_err();
        assert_eq!(
            violation.error_code(),
            ErrorCode::PropertyConstraintViolation
        );
        assert_eq!(violation.issues[0].keyword, SchemaKeyword::Enum);
        assert_eq!(violation.issues[0].instance_path, "/type");
    }

    #[test]
    fn additional_property_is_a_property_violation() {
        let violation = registry()
            .validate_request("Authorize", &json!({"idTag": "A", "extra": true}))
            .unwrap_err();
        assert_eq!(
            violation.error_code(),
            ErrorCode::PropertyConstraintViolation
        );
        assert_eq!(
            violation.issues[0].keyword,
            SchemaKeyword::AdditionalProperties
        );
        assert_eq!(
            request_code("Heartbeat", &json!({"x": 1})),
            ErrorCode::PropertyConstraintViolation
        );
    }

    #[test]
    fn too_long_string_is_a_property_violation() {
        let violation = registry()
            .validate_request("Authorize", &json!({"idTag": "x".repeat(21)}))
            .unwrap_err();
        assert_eq!(
            violation.error_code(),
            ErrorCode::PropertyConstraintViolation
        );
        assert_eq!(violation.issues[0].keyword, SchemaKeyword::MaxLength);
        assert_eq!(violation.issues[0].instance_path, "/idTag");
        registry()
            .validate_request("Authorize", &json!({"idTag": "x".repeat(20)}))
            .unwrap();
    }

    #[test]
    fn bad_date_time_is_a_property_violation() {
        let violation = registry()
            .validate_response("Heartbeat", &json!({"currentTime": "yesterday"}))
            .unwrap_err();
        assert_eq!(
            violation.error_code(),
            ErrorCode::PropertyConstraintViolation
        );
        assert_eq!(violation.issues[0].keyword, SchemaKeyword::Format);
        registry()
            .validate_response(
                "Heartbeat",
                &json!({"currentTime": "2026-05-06T09:08:09.123+02:00"}),
            )
            .unwrap();
    }

    #[test]
    fn uri_format_is_checked() {
        let payload =
            |location: &str| json!({"location": location, "retrieveDate": "2026-05-06T07:08:09Z"});
        registry()
            .validate_request("UpdateFirmware", &payload("ftp://host/fw.bin"))
            .unwrap();
        assert_eq!(
            request_code("UpdateFirmware", &payload("not a uri")),
            ErrorCode::PropertyConstraintViolation
        );
    }

    #[test]
    fn unknown_action_is_not_implemented_for_both_directions() {
        let registry = registry();
        let request = registry
            .validate_request("Frobnicate", &json!({}))
            .unwrap_err();
        assert_eq!(request.cause, ViolationCause::UnknownAction);
        assert_eq!(request.error_code(), ErrorCode::NotImplemented);
        assert_eq!(request.issues.len(), 0);
        let response = registry
            .validate_response("Frobnicate", &json!({}))
            .unwrap_err();
        assert_eq!(response.kind, PayloadKind::Response);
        assert_eq!(response.error_code(), ErrorCode::NotImplemented);
    }

    #[test]
    fn a_property_literally_named_id_survives_metadata_stripping() {
        let registry = registry();
        registry
            .validate_request("ClearChargingProfile", &json!({"id": 424_242}))
            .unwrap();
        let violation = registry
            .validate_request("ClearChargingProfile", &json!({"id": "x"}))
            .unwrap_err();
        assert_eq!(violation.error_code(), ErrorCode::TypeConstraintViolation);
        assert_eq!(violation.issues[0].instance_path, "/id");
    }

    #[test]
    fn several_violations_pick_the_code_by_priority_not_by_order() {
        // required (idTag) + type (connectorId) + additionalProperties.
        let violation = registry()
            .validate_request(
                "StartTransaction",
                &json!({"connectorId": "1", "meterStart": 0, "timestamp": "2026-01-01T00:00:00Z", "zzz": 1}),
            )
            .unwrap_err();
        let keywords: Vec<_> = violation.issues.iter().map(|i| i.keyword.clone()).collect();
        assert!(keywords.contains(&SchemaKeyword::Required));
        assert!(keywords.contains(&SchemaKeyword::Type));
        assert!(keywords.contains(&SchemaKeyword::AdditionalProperties));
        assert_eq!(
            violation.error_code(),
            ErrorCode::OccurenceConstraintViolation
        );

        // type + enum, no required.
        let violation = registry()
            .validate_request(
                "ChangeAvailability",
                &json!({"connectorId": "1", "type": "Maybe"}),
            )
            .unwrap_err();
        assert_eq!(violation.error_code(), ErrorCode::TypeConstraintViolation);
    }

    #[test]
    fn issues_are_sorted_deterministically() {
        let payload =
            json!({"connectorId": "1", "meterStart": "0", "timestamp": "no", "zzz": 1, "aaa": 2});
        let first = registry()
            .validate_request("StartTransaction", &payload)
            .unwrap_err();
        let second = registry()
            .validate_request("StartTransaction", &payload)
            .unwrap_err();
        assert_eq!(first, second);
        let mut sorted = first.issues.clone();
        sorted.sort_by(|a, b| a.instance_path.cmp(&b.instance_path));
        let paths: Vec<_> = first.issues.iter().map(|i| &i.instance_path).collect();
        let sorted_paths: Vec<_> = sorted.iter().map(|i| &i.instance_path).collect();
        assert_eq!(paths, sorted_paths);
    }

    #[test]
    fn charging_limits_with_decimals_are_multiples_of_a_tenth() {
        let registry = registry();
        for limit in [0.0, 6.0, 7.2, 16.0, 11.5, 0.3, 32.7, 22_000.0, 3.1] {
            let payload = json!({
                "connectorId": 1,
                "csChargingProfiles": {
                    "chargingProfileId": 1,
                    "stackLevel": 0,
                    "chargingProfilePurpose": "TxDefaultProfile",
                    "chargingProfileKind": "Absolute",
                    "chargingSchedule": {
                        "chargingRateUnit": "A",
                        "chargingSchedulePeriod": [{"startPeriod": 0, "limit": limit}],
                        "minChargingRate": limit
                    }
                }
            });
            registry
                .validate_request("SetChargingProfile", &payload)
                .unwrap_or_else(|e| panic!("limit {limit}: {e}"));
        }
    }
}
