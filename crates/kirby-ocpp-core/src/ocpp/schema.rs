//! JSON-schema validation of OCPP payloads.
//!
//! The bundled schemas are the source of truth for payload validity: OCPP 1.6J (28 core
//! actions + 11 Security Whitepaper actions, x request/response; the core ones are draft-04,
//! the security ones draft-06 but only use keywords draft-04 shares) and OCPP 2.0.1 (64
//! actions, all draft-06). Validation applies to **both directions**: requests we send,
//! requests we receive, responses we receive and the replies we send ourselves.
//!
//! The schemas live in `crates/kirby-ocpp-core/schemas/` (2.0.1 in `schemas/ocpp201/`) and
//! are embedded with `include_str!` (no runtime file lookup). Each registry compiles its
//! files once with the `jsonschema` crate: 1.6 forced to Draft 4, 2.0.1 to Draft 6. Before
//! compiling, the root `$schema`, `id` and `$id` keys are removed (as the TS validator
//! does): the official 1.6 files carry `"id": "urn:OCPP:1.6:..."`, which Draft 4 would treat
//! as the base URI of the document. Only the **root** keys are stripped, so a property that
//! is literally named `id` (`ClearChargingProfile`) and nested `id` keys are untouched.
//! `format` is validated (`date-time`, `uri`), unknown keywords such as `javaType` are
//! ignored. [`SchemaRegistry::schema_source`] returns the embedded bytes unmodified.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use jsonschema::Validator;
use serde_json::Value;
use thiserror::Error;

use crate::ocpp::frame::ErrorCode;
use crate::ocpp::protocol::Protocol;
use crate::ocpp::schema_v201::EMBEDDED_V201;

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
    /// There is no schema for this action name (not an action of the registry's version).
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
    /// The version whose schema rejected the payload.
    pub protocol: Protocol,
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
            "{} {} violates {} schema ({}): {}",
            self.action,
            self.kind,
            version_label(self.protocol),
            self.error_code_for(self.protocol),
            self.errors.join("; ")
        )
    }
}

impl std::error::Error for SchemaViolation {}

/// How diagnostics name a version: `OCPP 1.6J`, `OCPP 2.0.1`, `OCPP 2.1`.
fn version_label(protocol: Protocol) -> String {
    match protocol {
        Protocol::V16 => "OCPP 1.6J".to_owned(),
        other => format!("OCPP {}", other.ocpp_version()),
    }
}

impl SchemaViolation {
    fn unknown_action(protocol: Protocol, action: &str, kind: PayloadKind) -> Self {
        Self {
            protocol,
            action: action.to_owned(),
            kind,
            cause: ViolationCause::UnknownAction,
            errors: vec![format!(
                "`{action}` is not an {} action",
                version_label(protocol)
            )],
            issues: Vec::new(),
        }
    }

    fn from_issues(
        protocol: Protocol,
        action: &str,
        kind: PayloadKind,
        mut issues: Vec<SchemaIssue>,
    ) -> Self {
        issues.sort_by(|a, b| {
            (&a.instance_path, keyword_name(&a.keyword), &a.message).cmp(&(
                &b.instance_path,
                keyword_name(&b.keyword),
                &b.message,
            ))
        });
        Self {
            protocol,
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
        self.error_code_for(Protocol::V16)
    }

    /// Like [`SchemaViolation::error_code`] with the spellings of `protocol`: 2.x answers
    /// `OccurrenceConstraintViolation` and `FormatViolation` where 1.6 says
    /// `OccurenceConstraintViolation` (sic) and `FormationViolation`. The other codes do not
    /// change. For 2.x an array that breaks `minItems`/`maxItems` counts as an occurrence
    /// violation too (the 2.x schemas express cardinality that way); 1.6 never did.
    pub fn error_code_for(&self, protocol: Protocol) -> ErrorCode {
        let ViolationCause::Keywords(keywords) = &self.cause else {
            return ErrorCode::NotImplemented;
        };
        let cardinality = protocol != Protocol::V16
            && keywords.iter().any(
                |k| matches!(k, SchemaKeyword::Other(name) if name == "minItems" || name == "maxItems"),
            );
        if cardinality || keywords.contains(&SchemaKeyword::Required) {
            ErrorCode::occurrence_constraint_violation(protocol)
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
            ErrorCode::formation_violation(protocol)
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
const EMBEDDED: SchemaTable = embedded_schemas![
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
    // Security Whitepaper edition 3 (see schemas/README.md).
    "CertificateSigned",
    "DeleteCertificate",
    "ExtendedTriggerMessage",
    "GetInstalledCertificateIds",
    "GetLog",
    "InstallCertificate",
    "LogStatusNotification",
    "SecurityEventNotification",
    "SignCertificate",
    "SignedFirmwareStatusNotification",
    "SignedUpdateFirmware",
];

#[derive(Debug)]
struct Entry {
    request: Validator,
    response: Validator,
    request_source: &'static str,
    response_source: &'static str,
}

/// The embedded schema table of one OCPP version: `(action, request json, response json)`.
pub(crate) type SchemaTable = &'static [(&'static str, &'static str, &'static str)];

/// The compiled set of the schemas of one OCPP version. Build once, share (`Arc`) freely;
/// [`SchemaRegistry::for_protocol`] hands out the shared instance.
#[derive(Debug)]
pub struct SchemaRegistry {
    protocol: Protocol,
    table: SchemaTable,
    actions: HashMap<&'static str, Entry>,
}

/// A bundled schema could not be compiled (a build-time bug, not a runtime condition).
#[derive(Debug, Clone, Error)]
#[error("cannot compile the {kind} schema of {action}: {message}")]
pub struct SchemaLoadError {
    pub action: String,
    pub kind: PayloadKind,
    pub message: String,
}

/// Why [`SchemaRegistry::for_protocol`] returned no registry.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum SchemaError {
    /// This build embeds no schemas for the version.
    #[error("this build has no OCPP schemas for {}", .0.token())]
    Unavailable(Protocol),
    /// A bundled schema failed to compile.
    #[error(transparent)]
    Load(#[from] SchemaLoadError),
}

/// Compiles one schema file in the dialect of `protocol`: Draft 4 for 1.6, Draft 6 for 2.x.
fn compile(
    protocol: Protocol,
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
        root.remove("$id");
    }
    match protocol {
        Protocol::V16 => jsonschema::draft4::options(),
        _ => jsonschema::draft6::options(),
    }
    .should_validate_formats(true)
    .build(&schema)
    .map_err(|e| fail(e.to_string()))
}

impl SchemaRegistry {
    /// Compiles all bundled OCPP 1.6J schemas. Prefer [`SchemaRegistry::for_protocol`], which
    /// shares one compiled set per process.
    pub fn new() -> Result<Self, SchemaLoadError> {
        Self::build(Protocol::V16, EMBEDDED)
    }

    /// The shared registry of `protocol`, compiled on first use.
    ///
    /// # Errors
    /// [`SchemaError::Unavailable`] for a version this build embeds no schemas for (2.1
    /// today); [`SchemaError::Load`] if a bundled schema does not compile.
    pub fn for_protocol(protocol: Protocol) -> Result<Arc<Self>, SchemaError> {
        static V16: OnceLock<Result<Arc<SchemaRegistry>, SchemaLoadError>> = OnceLock::new();
        static V201: OnceLock<Result<Arc<SchemaRegistry>, SchemaLoadError>> = OnceLock::new();
        match protocol {
            Protocol::V16 => V16
                .get_or_init(|| Self::new().map(Arc::new))
                .clone()
                .map_err(SchemaError::Load),
            Protocol::V201 => V201
                .get_or_init(|| Self::build(Protocol::V201, EMBEDDED_V201).map(Arc::new))
                .clone()
                .map_err(SchemaError::Load),
            other => Err(SchemaError::Unavailable(other)),
        }
    }

    /// The version these schemas belong to.
    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    pub(crate) fn build(protocol: Protocol, table: SchemaTable) -> Result<Self, SchemaLoadError> {
        let mut actions = HashMap::with_capacity(table.len());
        for &(action, request_source, response_source) in table {
            actions.insert(
                action,
                Entry {
                    request: compile(protocol, action, PayloadKind::Request, request_source)?,
                    response: compile(protocol, action, PayloadKind::Response, response_source)?,
                    request_source,
                    response_source,
                },
            );
        }
        Ok(Self {
            protocol,
            table,
            actions,
        })
    }

    /// `true` if `action` has a schema in this registry (1.6: 28 core + 11 security
    /// extensions; 2.0.1: 64).
    pub fn knows_action(&self, action: &str) -> bool {
        self.actions.contains_key(action)
    }

    /// The action names in table order. 1.6: the 28 core ones alphabetically, then the 11
    /// security ones; 2.0.1: all 64 alphabetically.
    pub fn action_names(&self) -> impl Iterator<Item = &'static str> {
        self.table.iter().map(|&(name, _, _)| name)
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
            return Err(SchemaViolation::unknown_action(self.protocol, action, kind));
        };
        let validator = match kind {
            PayloadKind::Request => &entry.request,
            PayloadKind::Response => &entry.response,
        };
        // The validator's messages quote the offending value; that must never be a secret.
        let secret = kind == PayloadKind::Request && carries_authorization_key(action, payload)
            || super::frame::carries_device_model_secret(payload);
        let issues: Vec<SchemaIssue> = validator
            .iter_errors(payload)
            .map(|error| SchemaIssue {
                instance_path: error.instance_path().to_string(),
                keyword: SchemaKeyword::from_name(error.kind().keyword()),
                message: if secret {
                    format!(
                        "violates `{}` (details hidden: the value is a secret)",
                        error.kind().keyword()
                    )
                } else {
                    error.to_string()
                },
            })
            .collect();
        if issues.is_empty() {
            Ok(())
        } else {
            Err(SchemaViolation::from_issues(
                self.protocol,
                action,
                kind,
                issues,
            ))
        }
    }
}

/// `true` for a `ChangeConfiguration` payload that sets `AuthorizationKey` (matched
/// case-insensitively, like the configuration registry).
fn carries_authorization_key(action: &str, payload: &Value) -> bool {
    action == "ChangeConfiguration"
        && payload
            .get("key")
            .and_then(Value::as_str)
            .is_some_and(|key| key.eq_ignore_ascii_case("AuthorizationKey"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn violation(keywords: &[&str]) -> SchemaViolation {
        SchemaViolation {
            protocol: Protocol::V16,
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
            protocol: Protocol::V16,
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
    fn registry_knows_exactly_the_39_actions() {
        let registry = registry();
        assert_eq!(registry.action_names().count(), 39);
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
    fn violations_of_an_authorization_key_never_echo_the_value() {
        let registry = SchemaRegistry::new().unwrap();
        let secret = "S3cr3t-".repeat(100);
        for key in ["AuthorizationKey", "authorizationkey"] {
            let violation = registry
                .validate_request(
                    "ChangeConfiguration",
                    &json!({"key": key, "value": secret.clone()}),
                )
                .unwrap_err();
            assert_ne!(violation.errors.len(), 0);
            let shown = format!("{violation} {violation:?}");
            assert!(!shown.contains("S3cr3t"), "{shown}");
        }
        // Other keys keep the normal messages.
        let other = registry
            .validate_request(
                "ChangeConfiguration",
                &json!({"key": "HeartbeatInterval", "value": "v".repeat(501)}),
            )
            .unwrap_err();
        assert!(other.errors.join(" ").contains("vvvv"));
    }

    #[test]
    fn valid_payloads_pass() {
        let registry = registry();
        registry.validate_request("Heartbeat", &json!({})).unwrap();
        registry
            .validate_request(
                "BootNotification",
                &json!({"chargePointVendor": "kirby-ocpp", "chargePointModel": "TUI-16"}),
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

    #[test]
    fn for_protocol_shares_the_1_6_set() {
        let first = SchemaRegistry::for_protocol(Protocol::V16).unwrap();
        let second = SchemaRegistry::for_protocol(Protocol::V16).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.protocol(), Protocol::V16);
        assert!(first.knows_action("Heartbeat"));
        assert_eq!(first.action_names().count(), 39);
        assert_eq!(SchemaRegistry::new().unwrap().protocol(), Protocol::V16);
    }

    #[test]
    fn for_protocol_refuses_versions_without_schemas() {
        let protocol = Protocol::V21;
        let error = SchemaRegistry::for_protocol(protocol).unwrap_err();
        assert!(
            matches!(error, SchemaError::Unavailable(p) if p == protocol),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            format!("this build has no OCPP schemas for {}", protocol.token())
        );
    }

    // ---- OCPP 2.0.1 ----

    fn v201() -> Arc<SchemaRegistry> {
        SchemaRegistry::for_protocol(Protocol::V201).expect("2.0.1 schemas compile")
    }

    fn v16() -> Arc<SchemaRegistry> {
        SchemaRegistry::for_protocol(Protocol::V16).unwrap()
    }

    fn schema_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/ocpp201")
    }

    #[test]
    fn v201_embeds_64_unique_pairs_matching_the_files() {
        let registry = v201();
        assert_eq!(registry.protocol(), Protocol::V201);
        let names: Vec<_> = registry.action_names().collect();
        assert_eq!(names.len(), 64);
        let unique: std::collections::HashSet<_> = names.iter().copied().collect();
        assert_eq!(unique.len(), 64);
        let mut files: Vec<String> = std::fs::read_dir(schema_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|name| name != "README.md")
            .collect();
        files.sort();
        assert_eq!(files.len(), 128);
        let mut expected: Vec<String> = names
            .iter()
            .flat_map(|n| [format!("{n}Request.json"), format!("{n}Response.json")])
            .collect();
        expected.sort();
        assert_eq!(files, expected);
        for name in names {
            assert!(registry.knows_action(name), "{name}");
            for (kind, suffix) in [
                (PayloadKind::Request, "Request"),
                (PayloadKind::Response, "Response"),
            ] {
                let on_disk =
                    std::fs::read_to_string(schema_dir().join(format!("{name}{suffix}.json")))
                        .unwrap();
                assert_eq!(registry.schema_source(name, kind), Some(on_disk.as_str()));
            }
        }
    }

    #[test]
    fn v201_registry_is_shared_and_isolated_from_1_6() {
        let first = SchemaRegistry::for_protocol(Protocol::V201).unwrap();
        assert!(Arc::ptr_eq(&first, &v201()));
        assert!(!Arc::ptr_eq(&first, &v16()));
        assert_eq!(v16().action_names().count(), 39);
        // Names that exist in one version only.
        assert!(v16().knows_action("RemoteStartTransaction"));
        assert!(!v201().knows_action("RemoteStartTransaction"));
        assert!(v201().knows_action("RequestStartTransaction"));
        assert!(!v16().knows_action("RequestStartTransaction"));
        let unknown = v201()
            .validate_request("RemoteStartTransaction", &json!({}))
            .unwrap_err();
        assert_eq!(unknown.cause, ViolationCause::UnknownAction);
        assert_eq!(unknown.protocol, Protocol::V201);
        assert_eq!(
            unknown.errors,
            ["`RemoteStartTransaction` is not an OCPP 2.0.1 action"]
        );
    }

    #[test]
    fn a_shared_action_name_validates_its_own_shape() {
        let boot16 = json!({"chargePointVendor": "v", "chargePointModel": "m"});
        let boot201 = json!({
            "reason": "PowerUp",
            "chargingStation": {"model": "m", "vendorName": "v"}
        });
        v16().validate_request("BootNotification", &boot16).unwrap();
        v201()
            .validate_request("BootNotification", &boot201)
            .unwrap();
        let wrong201 = v201()
            .validate_request("BootNotification", &boot16)
            .unwrap_err();
        assert_eq!(
            wrong201.error_code_for(Protocol::V201).as_str(),
            "OccurrenceConstraintViolation"
        );
        assert!(
            wrong201
                .to_string()
                .starts_with("BootNotification request violates OCPP 2.0.1 schema (Occurrence")
        );
        assert!(
            v16()
                .validate_request("BootNotification", &boot201)
                .is_err()
        );
        v201()
            .validate_response(
                "BootNotification",
                &json!({"currentTime": "2026-05-06T07:08:09Z", "interval": 300, "status": "Accepted"}),
            )
            .unwrap();
    }

    #[test]
    fn v201_validates_real_actions() {
        let registry = v201();
        registry.validate_request("Heartbeat", &json!({})).unwrap();
        registry
            .validate_request(
                "GetVariables",
                &json!({"getVariableData": [
                    {"component": {"name": "SecurityCtrlr"}, "variable": {"name": "OrganizationName"}}
                ]}),
            )
            .unwrap();
        // minItems: 1 on the list.
        let empty = registry
            .validate_request("GetVariables", &json!({"getVariableData": []}))
            .unwrap_err();
        assert_eq!(
            empty.issues[0].keyword,
            SchemaKeyword::Other("minItems".into())
        );
        assert_eq!(
            empty.error_code_for(Protocol::V201).as_str(),
            "OccurrenceConstraintViolation"
        );
        // Cardinality is not an occurrence violation in 1.6 (unchanged mapping).
        assert_eq!(
            empty.error_code_for(Protocol::V16),
            ErrorCode::FormationViolation
        );
        let event = json!({
            "eventType": "Started",
            "timestamp": "2026-05-06T07:08:09Z",
            "triggerReason": "Authorized",
            "seqNo": 0,
            "transactionInfo": {"transactionId": "tx-1"}
        });
        registry
            .validate_request("TransactionEvent", &event)
            .unwrap();
        let mut bad = event.clone();
        bad["seqNo"] = json!("0");
        bad["timestamp"] = json!("yesterday");
        let violation = registry
            .validate_request("TransactionEvent", &bad)
            .unwrap_err();
        assert_eq!(
            violation.error_code_for(Protocol::V201),
            ErrorCode::TypeConstraintViolation
        );
        assert!(
            violation
                .issues
                .iter()
                .any(|i| i.keyword == SchemaKeyword::Format)
        );
        let mut missing = event;
        missing.as_object_mut().unwrap().remove("transactionInfo");
        assert_eq!(
            registry
                .validate_request("TransactionEvent", &missing)
                .unwrap_err()
                .error_code_for(Protocol::V201)
                .as_str(),
            "OccurrenceConstraintViolation"
        );
    }

    #[test]
    fn the_compiler_selects_the_dialect_per_protocol() {
        // `const` and numeric `exclusiveMinimum` exist from draft 6 on; `propertyNames` too.
        const SCHEMA: &str = r#"{
            "type": "object",
            "properties": {
                "kind": {"const": "a"},
                "n": {"type": "integer", "exclusiveMinimum": 0}
            },
            "propertyNames": {"maxLength": 4}
        }"#;
        let draft6 = compile(Protocol::V201, "Synthetic", PayloadKind::Request, SCHEMA).unwrap();
        assert!(draft6.is_valid(&json!({"kind": "a", "n": 1})));
        assert!(!draft6.is_valid(&json!({"kind": "b"})));
        assert!(!draft6.is_valid(&json!({"n": 0})));
        assert!(!draft6.is_valid(&json!({"toolong": 1})));
        // Draft 4 reads `exclusiveMinimum: 0` as a malformed boolean flag or ignores the
        // draft-6 keywords; either way it must not behave like draft 6.
        let draft4 = compile(Protocol::V16, "Synthetic", PayloadKind::Request, SCHEMA);
        match draft4 {
            Ok(validator) => {
                assert!(validator.is_valid(&json!({"kind": "b"})));
                assert!(validator.is_valid(&json!({"toolong": 1})));
            }
            Err(error) => assert!(error.message.contains("boolean"), "{error}"),
        }
    }

    #[test]
    fn v201_keeps_nested_ids_and_original_bytes() {
        // Only root metadata is stripped for compiling; the source stays untouched.
        let source = v201()
            .schema_source("BootNotification", PayloadKind::Request)
            .unwrap();
        assert!(source.contains("\"$id\": \"urn:OCPP:Cp:2:2020:3:BootNotificationRequest\""));
        assert!(source.contains("draft-06"));
    }

    #[test]
    fn a_registry_can_be_built_from_a_custom_table() {
        const TABLE: SchemaTable = &[(
            "Ping",
            r#"{"type":"object","required":["n"],"properties":{"n":{"type":"integer"}}}"#,
            r#"{"type":"object"}"#,
        )];
        let registry = SchemaRegistry::build(Protocol::V201, TABLE).unwrap();
        assert_eq!(registry.protocol(), Protocol::V201);
        assert_eq!(registry.action_names().collect::<Vec<_>>(), ["Ping"]);
        assert!(!registry.knows_action("Heartbeat"));
        registry.validate_request("Ping", &json!({"n": 1})).unwrap();
        assert!(registry.validate_request("Ping", &json!({})).is_err());
    }

    #[test]
    fn error_codes_follow_the_protocol_spelling() {
        let wire = |keywords: &[&str], protocol| {
            violation(keywords)
                .error_code_for(protocol)
                .as_str()
                .to_owned()
        };
        for (keywords, v16, v2) in [
            (
                &["required"][..],
                "OccurenceConstraintViolation",
                "OccurrenceConstraintViolation",
            ),
            (
                &["type"][..],
                "TypeConstraintViolation",
                "TypeConstraintViolation",
            ),
            (
                &["enum"][..],
                "PropertyConstraintViolation",
                "PropertyConstraintViolation",
            ),
            (&["pattern"][..], "FormationViolation", "FormatViolation"),
        ] {
            assert_eq!(wire(keywords, Protocol::V16), v16);
            assert_eq!(violation(keywords).error_code().as_str(), v16);
            assert_eq!(wire(keywords, Protocol::V201), v2);
            assert_eq!(wire(keywords, Protocol::V21), v2);
        }
        let unknown = SchemaViolation::unknown_action(Protocol::V16, "Nope", PayloadKind::Request);
        for protocol in Protocol::ALL {
            assert_eq!(unknown.error_code_for(protocol), ErrorCode::NotImplemented);
        }
    }
}
