//! JSON-schema validation of OCPP payloads.
//!
//! The bundled OCPP 1.6J schemas (draft-04, 28 actions x request/response) are
//! the source of truth for payload validity. Validation applies to **both
//! directions**: requests we send, requests we receive, responses we receive and
//! the replies we send ourselves (see `docs/rewrite/core-analysis.md`).
//!
//! The registry API and the violation -> CALLERROR mapping are final. Compiling
//! and running the schemas (`jsonschema` with Draft 4, `include_str!`-embedded,
//! compiled once, `id`/`$schema` stripped as the TS validator does) is the
//! job of the ocpp stage.

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

/// A payload that does not conform to the schema of its action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaViolation {
    pub action: String,
    pub kind: PayloadKind,
    pub cause: ViolationCause,
    /// Human-readable error messages (with instance paths), for logs and CALLERROR descriptions.
    pub errors: Vec<String>,
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

/// The compiled set of OCPP 1.6J schemas. Build once, share (`Arc`) freely.
#[derive(Debug)]
pub struct SchemaRegistry {
    _compiled: (),
}

/// A bundled schema could not be compiled (a build-time bug, not a runtime condition).
#[derive(Debug, Error)]
#[error("cannot compile the {kind} schema of {action}: {message}")]
pub struct SchemaLoadError {
    pub action: String,
    pub kind: PayloadKind,
    pub message: String,
}

#[allow(
    unused_variables,
    clippy::unused_self,
    reason = "signatures are final; bodies arrive with the ocpp stage"
)]
impl SchemaRegistry {
    /// Compiles all bundled schemas.
    pub fn new() -> Result<Self, SchemaLoadError> {
        Ok(Self { _compiled: () })
    }

    /// `true` if `action` is one of the 28 OCPP 1.6J actions.
    pub fn knows_action(&self, action: &str) -> bool {
        unimplemented!("ocpp stage: schema lookup")
    }

    /// Validates the payload of a CALL. Unknown actions yield [`ViolationCause::UnknownAction`].
    pub fn validate_request(&self, action: &str, payload: &Value) -> Result<(), SchemaViolation> {
        unimplemented!("ocpp stage: draft-04 validation")
    }

    /// Validates the payload of a CALLRESULT for a CALL of `action`.
    pub fn validate_response(&self, action: &str, payload: &Value) -> Result<(), SchemaViolation> {
        unimplemented!("ocpp stage: draft-04 validation")
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
}
