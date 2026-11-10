//! OCPP-J wire frames: `CALL`, `CALLRESULT`, `CALLERROR`.
//!
//! ```text
//! [2, "<id>", "<Action>", {payload}]                              CALL
//! [3, "<id>", {payload}]                                          CALLRESULT
//! [4, "<id>", "<ErrorCode>", "<description>", {details}]          CALLERROR
//! ```
//!
//! Parsing is deliberately strict (it mirrors `isValidCall` & co. in the TS
//! client): the message type must be the *integer* 2, 3 or 4 (`2.0` and `"2"` are
//! rejected), the array length must match exactly, and payloads and error details
//! must be JSON objects. Failures are reported as [`FrameError`], which keeps
//! whatever message id could still be read so that the caller can answer a broken
//! CALL with a `FormationViolation` CALLERROR ([`FrameError::reply`]).

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use thiserror::Error;

/// OCPP-J message id (a string of at most 36 characters per the spec; not enforced on receive).
pub type MessageId = String;

/// `MessageTypeId` of a CALL.
pub const MESSAGE_TYPE_CALL: u8 = 2;
/// `MessageTypeId` of a CALLRESULT.
pub const MESSAGE_TYPE_CALL_RESULT: u8 = 3;
/// `MessageTypeId` of a CALLERROR.
pub const MESSAGE_TYPE_CALL_ERROR: u8 = 4;

/// OCPP 1.6 `CALLERROR` error codes.
///
/// `OccurenceConstraintViolation` is spelled exactly as in the 1.6 specification
/// (one `r` in "Occurence"); that is what goes on the wire. Codes that are not in
/// the 1.6 list (for example the correctly spelled `OccurrenceConstraintViolation`
/// that some CSMS send, or 2.0.1 codes) are kept verbatim in [`ErrorCode::Other`]
/// so a received CALLERROR survives a re-serialization byte for byte.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorCode {
    NotImplemented,
    NotSupported,
    InternalError,
    ProtocolError,
    SecurityError,
    FormationViolation,
    PropertyConstraintViolation,
    /// Wire string `OccurenceConstraintViolation` (sic).
    OccurenceConstraintViolation,
    TypeConstraintViolation,
    GenericError,
    /// A code that is not part of OCPP 1.6, preserved as received.
    Other(String),
}

impl ErrorCode {
    /// The ten codes defined by OCPP 1.6.
    pub const STANDARD: [Self; 10] = [
        Self::NotImplemented,
        Self::NotSupported,
        Self::InternalError,
        Self::ProtocolError,
        Self::SecurityError,
        Self::FormationViolation,
        Self::PropertyConstraintViolation,
        Self::OccurenceConstraintViolation,
        Self::TypeConstraintViolation,
        Self::GenericError,
    ];

    /// The wire string.
    pub fn as_str(&self) -> &str {
        match self {
            Self::NotImplemented => "NotImplemented",
            Self::NotSupported => "NotSupported",
            Self::InternalError => "InternalError",
            Self::ProtocolError => "ProtocolError",
            Self::SecurityError => "SecurityError",
            Self::FormationViolation => "FormationViolation",
            Self::PropertyConstraintViolation => "PropertyConstraintViolation",
            Self::OccurenceConstraintViolation => "OccurenceConstraintViolation",
            Self::TypeConstraintViolation => "TypeConstraintViolation",
            Self::GenericError => "GenericError",
            Self::Other(code) => code,
        }
    }

    /// Parses a wire string; never fails (unknown codes become [`ErrorCode::Other`]).
    pub fn from_wire(code: &str) -> Self {
        Self::STANDARD
            .iter()
            .find(|known| known.as_str() == code)
            .cloned()
            .unwrap_or_else(|| Self::Other(code.to_owned()))
    }

    /// `true` for the ten codes defined by OCPP 1.6.
    pub fn is_standard(&self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&String::deserialize(deserializer)?))
    }
}

/// A decoded OCPP-J frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Call {
        id: MessageId,
        action: String,
        payload: Value,
    },
    CallResult {
        id: MessageId,
        payload: Value,
    },
    CallError {
        id: MessageId,
        code: ErrorCode,
        description: String,
        details: Value,
    },
}

impl Frame {
    /// Builds a CALL.
    pub fn call(id: impl Into<MessageId>, action: impl Into<String>, payload: Value) -> Self {
        Self::Call {
            id: id.into(),
            action: action.into(),
            payload,
        }
    }

    /// Builds a CALLRESULT.
    pub fn call_result(id: impl Into<MessageId>, payload: Value) -> Self {
        Self::CallResult {
            id: id.into(),
            payload,
        }
    }

    /// Builds a CALLERROR with empty (`{}`) details.
    pub fn call_error(
        id: impl Into<MessageId>,
        code: ErrorCode,
        description: impl Into<String>,
    ) -> Self {
        Self::CallError {
            id: id.into(),
            code,
            description: description.into(),
            details: Value::Object(Map::new()),
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Call { id, .. } | Self::CallResult { id, .. } | Self::CallError { id, .. } => id,
        }
    }

    /// `2`, `3` or `4`.
    pub fn message_type(&self) -> u8 {
        match self {
            Self::Call { .. } => MESSAGE_TYPE_CALL,
            Self::CallResult { .. } => MESSAGE_TYPE_CALL_RESULT,
            Self::CallError { .. } => MESSAGE_TYPE_CALL_ERROR,
        }
    }

    /// The action name, for CALLs only.
    pub fn action(&self) -> Option<&str> {
        match self {
            Self::Call { action, .. } => Some(action),
            _ => None,
        }
    }

    /// Parses a text frame.
    pub fn parse(text: &str) -> Result<Self, FrameError> {
        let value: Value = serde_json::from_str(text).map_err(|e| FrameError {
            kind: FrameErrorKind::InvalidJson(e),
            message_type: None,
            message_id: None,
        })?;
        Self::try_from(value)
    }

    /// The JSON array form of this frame.
    pub fn to_value(&self) -> Value {
        match self {
            Self::Call {
                id,
                action,
                payload,
            } => Value::Array(vec![
                Value::from(MESSAGE_TYPE_CALL),
                Value::from(id.as_str()),
                Value::from(action.as_str()),
                payload.clone(),
            ]),
            Self::CallResult { id, payload } => Value::Array(vec![
                Value::from(MESSAGE_TYPE_CALL_RESULT),
                Value::from(id.as_str()),
                payload.clone(),
            ]),
            Self::CallError {
                id,
                code,
                description,
                details,
            } => Value::Array(vec![
                Value::from(MESSAGE_TYPE_CALL_ERROR),
                Value::from(id.as_str()),
                Value::from(code.as_str()),
                Value::from(description.as_str()),
                details.clone(),
            ]),
        }
    }

    /// Compact JSON text of this frame, ready for a WebSocket text message.
    pub fn to_json_string(&self) -> String {
        // A `Value` always serializes.
        self.to_value().to_string()
    }
}

impl TryFrom<Value> for Frame {
    type Error = FrameError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let Value::Array(mut items) = value else {
            return Err(FrameError::new(FrameErrorKind::NotAnArray));
        };
        if items.is_empty() {
            return Err(FrameError::new(FrameErrorKind::EmptyArray));
        }

        let message_type = match items[0].as_u64() {
            Some(2) => MESSAGE_TYPE_CALL,
            Some(3) => MESSAGE_TYPE_CALL_RESULT,
            Some(4) => MESSAGE_TYPE_CALL_ERROR,
            _ => {
                // Not one of 2/3/4: report what was there. The id is still
                // readable for logging, but there is nothing to reply to.
                let id = items.get(1).and_then(Value::as_str).map(str::to_owned);
                return Err(FrameError {
                    kind: FrameErrorKind::UnknownMessageType(items[0].clone()),
                    message_type: None,
                    message_id: id,
                });
            }
        };

        let message_id = items.get(1).and_then(Value::as_str).map(str::to_owned);
        let fail = |kind| FrameError {
            kind,
            message_type: Some(message_type),
            message_id: message_id.clone(),
        };

        let expected = match message_type {
            MESSAGE_TYPE_CALL => 4,
            MESSAGE_TYPE_CALL_RESULT => 3,
            _ => 5,
        };
        if items.len() != expected {
            return Err(fail(FrameErrorKind::WrongLength {
                expected,
                actual: items.len(),
            }));
        }

        // Take fields from the back so no clones are needed.
        let take_string =
            |items: &mut Vec<Value>, index: usize, field: &'static str| match std::mem::take(
                &mut items[index],
            ) {
                Value::String(s) => Ok(s),
                _ => Err(fail(FrameErrorKind::InvalidField {
                    field,
                    expected: "a string",
                })),
            };
        let take_object =
            |items: &mut Vec<Value>, index: usize, field: &'static str| match std::mem::take(
                &mut items[index],
            ) {
                object @ Value::Object(_) => Ok(object),
                _ => Err(fail(FrameErrorKind::InvalidField {
                    field,
                    expected: "a JSON object",
                })),
            };

        let id = take_string(&mut items, 1, "messageId")?;
        Ok(match message_type {
            MESSAGE_TYPE_CALL => {
                let action = take_string(&mut items, 2, "action")?;
                let payload = take_object(&mut items, 3, "payload")?;
                Self::Call {
                    id,
                    action,
                    payload,
                }
            }
            MESSAGE_TYPE_CALL_RESULT => {
                let payload = take_object(&mut items, 2, "payload")?;
                Self::CallResult { id, payload }
            }
            _ => {
                let code = ErrorCode::from_wire(&take_string(&mut items, 2, "errorCode")?);
                let description = take_string(&mut items, 3, "errorDescription")?;
                let details = take_object(&mut items, 4, "errorDetails")?;
                Self::CallError {
                    id,
                    code,
                    description,
                    details,
                }
            }
        })
    }
}

impl Serialize for Frame {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Frame {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

/// Why a frame could not be decoded.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FrameErrorKind {
    #[error("not valid JSON: {0}")]
    InvalidJson(serde_json::Error),
    #[error("expected a JSON array")]
    NotAnArray,
    #[error("empty array")]
    EmptyArray,
    #[error("unsupported message type {0}")]
    UnknownMessageType(Value),
    #[error("wrong number of elements: expected {expected}, got {actual}")]
    WrongLength { expected: usize, actual: usize },
    #[error("{field} must be {expected}")]
    InvalidField {
        field: &'static str,
        expected: &'static str,
    },
}

/// A frame that failed to decode, with whatever context could still be read.
#[derive(Debug, Error)]
#[error("invalid OCPP-J frame: {kind}")]
pub struct FrameError {
    pub kind: FrameErrorKind,
    /// `Some(2|3|4)` if the first element was a valid message type.
    pub message_type: Option<u8>,
    /// The message id, if element 1 was a string.
    pub message_id: Option<MessageId>,
}

impl FrameError {
    fn new(kind: FrameErrorKind) -> Self {
        Self {
            kind,
            message_type: None,
            message_id: None,
        }
    }

    /// The `FormationViolation` CALLERROR to send back, if the broken frame was a
    /// CALL whose id could be read. Broken CALLRESULT/CALLERROR frames and frames
    /// without a readable id get no reply (same as the TS client).
    pub fn reply(&self) -> Option<Frame> {
        match (self.message_type, &self.message_id) {
            (Some(MESSAGE_TYPE_CALL), Some(id)) => Some(Frame::call_error(
                id.clone(),
                ErrorCode::FormationViolation,
                self.kind.to_string(),
            )),
            _ => None,
        }
    }
}

/// Marker that replaces a secret value in recorded frames.
pub const REDACTED: &str = "***";

/// Returns `frame` with `AuthorizationKey` values in `ChangeConfiguration` CALLs and
/// reply payloads replaced by [`REDACTED`]. Used only for the recorded copy of a frame
/// (logs, history, UI); the wire bytes are never touched.
///
/// The key is matched case-insensitively, like the station's configuration registry.
pub fn redact_secrets(frame: &Frame) -> Frame {
    let mut copy = frame.clone();
    match &mut copy {
        Frame::Call {
            action, payload, ..
        } if action == "ChangeConfiguration" => {
            redact_payload(payload);
        }
        Frame::CallResult { payload, .. } => redact_payload(payload),
        Frame::CallError { details, .. } => redact_payload(details),
        Frame::Call { .. } => {}
    }
    copy
}

/// Replaces the `value` beside a case-insensitive `"key": "AuthorizationKey"` anywhere
/// in a JSON payload with [`REDACTED`]. Other keys and values are preserved.
pub fn redact_payload(payload: &mut Value) {
    match payload {
        Value::Object(fields) => {
            if fields
                .get("key")
                .and_then(Value::as_str)
                .is_some_and(|key| key.eq_ignore_ascii_case("AuthorizationKey"))
                && let Some(value) = fields.get_mut("value")
            {
                *value = Value::from(REDACTED);
            }
            for child in fields.values_mut() {
                if child.is_object() || child.is_array() {
                    redact_payload(child);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_payload),
        _ => {}
    }
}

/// Direction of a frame relative to this charge point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Received from the CSMS.
    In,
    /// Sent by this charge point.
    Out,
}

/// A frame as it crossed the wire, for logs, the TUI and JSONL export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameRecord {
    pub ts: DateTime<Utc>,
    pub direction: Direction,
    /// `None` only for text that did not parse as a frame at all.
    pub msg_id: Option<MessageId>,
    /// Action name. For CALLRESULT/CALLERROR the connection layer fills in the
    /// action of the CALL they answer when it is known (correlation).
    pub action: Option<String>,
    /// The frame as JSON (`[2, ...]`), or a JSON string with the raw text if it
    /// was not a valid frame.
    pub frame: Value,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn roundtrip(frame: &Frame) {
        let text = frame.to_json_string();
        assert_eq!(&Frame::parse(&text).unwrap(), frame, "text form: {text}");
        let via_serde: Frame =
            serde_json::from_str(&serde_json::to_string(frame).unwrap()).unwrap();
        assert_eq!(&via_serde, frame);
    }

    #[test]
    fn redact_secrets_hides_only_the_authorization_key_value() {
        let secret = Frame::call(
            "1",
            "ChangeConfiguration",
            json!({"key": "AuthorizationKey", "value": "0123456789abcdef"}),
        );
        let redacted = redact_secrets(&secret);
        assert_eq!(
            redacted.to_value(),
            json!([2, "1", "ChangeConfiguration", {"key": "AuthorizationKey", "value": "***"}])
        );
        // The original is untouched.
        assert!(secret.to_json_string().contains("0123456789abcdef"));
        let lower = Frame::call(
            "2",
            "ChangeConfiguration",
            json!({"key": "authorizationkey", "value": "0123456789abcdef"}),
        );
        assert!(
            !redact_secrets(&lower)
                .to_json_string()
                .contains("0123456789")
        );

        let others = [
            Frame::call(
                "3",
                "ChangeConfiguration",
                json!({"key": "HeartbeatInterval", "value": "10"}),
            ),
            Frame::call("4", "Heartbeat", json!({})),
            Frame::call(
                "5",
                "DataTransfer",
                json!({"key": "AuthorizationKey", "value": "x"}),
            ),
            Frame::call_result("6", json!({"key": "HeartbeatInterval", "value": "60"})),
            Frame::call(
                "7",
                "ChangeConfiguration",
                json!({"key": "AuthorizationKey"}),
            ),
            Frame::call("8", "ChangeConfiguration", json!({"key": 5, "value": "x"})),
            Frame::call("9", "ChangeConfiguration", json!({})),
        ];
        for frame in others {
            assert_eq!(redact_secrets(&frame), frame);
        }
    }

    #[test]
    fn redact_secrets_hides_configuration_result_values_without_changing_the_wire() {
        let frame = Frame::call_result(
            "configuration",
            json!({"configurationKey": [
                {"key": "authorizationkey", "readonly": false, "value": "test-private-value"},
                {"key": "HeartbeatInterval", "readonly": false, "value": "60"}
            ]}),
        );
        let recorded = redact_secrets(&frame).to_value();
        assert_eq!(recorded[2]["configurationKey"][0]["value"], REDACTED);
        assert_eq!(recorded[2]["configurationKey"][1]["value"], "60");
        assert!(frame.to_json_string().contains("test-private-value"));
    }

    #[test]
    fn redact_secrets_hides_nested_call_error_details() {
        let frame = Frame::CallError {
            id: "configuration".into(),
            code: ErrorCode::InternalError,
            description: "configuration failed".to_owned(),
            details: json!({"request": {"key": "AUTHORIZATIONKEY", "value": "test-private-value"},
                "retry": false}),
        };
        let recorded = redact_secrets(&frame).to_value();
        assert_eq!(recorded[4]["request"]["value"], REDACTED);
        assert_eq!(recorded[4]["retry"], false);
        assert!(frame.to_json_string().contains("test-private-value"));
    }

    #[test]
    fn call_roundtrip_and_exact_wire_form() {
        let frame = Frame::call("19223201", "Heartbeat", json!({}));
        assert_eq!(frame.to_json_string(), r#"[2,"19223201","Heartbeat",{}]"#);
        roundtrip(&frame);
        assert_eq!(frame.id(), "19223201");
        assert_eq!(frame.action(), Some("Heartbeat"));
        assert_eq!(frame.message_type(), 2);
    }

    #[test]
    fn call_result_roundtrip_and_exact_wire_form() {
        let frame = Frame::call_result("abc", json!({"currentTime": "2026-01-01T00:00:00Z"}));
        assert_eq!(
            frame.to_json_string(),
            r#"[3,"abc",{"currentTime":"2026-01-01T00:00:00Z"}]"#
        );
        roundtrip(&frame);
        assert_eq!(frame.action(), None);
    }

    #[test]
    fn call_error_roundtrip_and_exact_wire_form() {
        let frame = Frame::CallError {
            id: "x".into(),
            code: ErrorCode::OccurenceConstraintViolation,
            description: "missing".into(),
            details: json!({"k": 1}),
        };
        assert_eq!(
            frame.to_json_string(),
            r#"[4,"x","OccurenceConstraintViolation","missing",{"k":1}]"#
        );
        roundtrip(&frame);
    }

    #[test]
    fn every_standard_error_code_roundtrips_and_is_standard() {
        for code in ErrorCode::STANDARD {
            assert!(code.is_standard());
            assert_eq!(ErrorCode::from_wire(code.as_str()), code);
            roundtrip(&Frame::call_error("1", code, "d"));
        }
        assert_eq!(
            ErrorCode::OccurenceConstraintViolation.as_str(),
            "OccurenceConstraintViolation"
        );
    }

    #[test]
    fn unknown_error_code_is_preserved_verbatim() {
        // Correct English spelling is NOT the 1.6 code.
        let text = r#"[4,"7","OccurrenceConstraintViolation","d",{}]"#;
        let frame = Frame::parse(text).unwrap();
        let Frame::CallError { code, .. } = &frame else {
            panic!("expected CALLERROR");
        };
        assert_eq!(
            *code,
            ErrorCode::Other("OccurrenceConstraintViolation".into())
        );
        assert!(!code.is_standard());
        assert_eq!(frame.to_json_string(), text);
    }

    #[test]
    fn payload_may_contain_nested_values() {
        roundtrip(&Frame::call(
            "n",
            "MeterValues",
            json!({"connectorId": 1, "meterValue": [{"sampledValue": [{"value": "1"}]}]}),
        ));
    }

    fn parse_err(text: &str) -> FrameError {
        Frame::parse(text).expect_err(text)
    }

    #[test]
    fn rejects_non_json_and_non_arrays() {
        assert!(matches!(
            parse_err("not json").kind,
            FrameErrorKind::InvalidJson(_)
        ));
        assert!(matches!(parse_err("{}").kind, FrameErrorKind::NotAnArray));
        assert!(matches!(
            parse_err("\"str\"").kind,
            FrameErrorKind::NotAnArray
        ));
        assert!(matches!(parse_err("null").kind, FrameErrorKind::NotAnArray));
        assert!(matches!(parse_err("[]").kind, FrameErrorKind::EmptyArray));
    }

    #[test]
    fn rejects_bad_message_types() {
        for text in [
            r#"["2","id","Heartbeat",{}]"#,
            r#"[2.0,"id","Heartbeat",{}]"#,
            r#"[5,"id","Heartbeat",{}]"#,
            r#"[-2,"id","Heartbeat",{}]"#,
            r#"[null,"id"]"#,
        ] {
            let err = parse_err(text);
            assert!(
                matches!(err.kind, FrameErrorKind::UnknownMessageType(_)),
                "{text}: {err}"
            );
            assert!(err.reply().is_none(), "{text}: no reply for unknown types");
        }
    }

    #[test]
    fn rejects_wrong_lengths() {
        for text in [
            r#"[2,"id","Heartbeat"]"#,
            r#"[2,"id","Heartbeat",{},1]"#,
            r#"[3,"id"]"#,
            r#"[3,"id",{},{}]"#,
            r#"[4,"id","GenericError","d"]"#,
            r#"[4,"id","GenericError","d",{},1]"#,
        ] {
            assert!(
                matches!(parse_err(text).kind, FrameErrorKind::WrongLength { .. }),
                "{text}"
            );
        }
    }

    #[test]
    fn rejects_bad_field_types() {
        for text in [
            r#"[2,1,"Heartbeat",{}]"#,
            r#"[2,"id",5,{}]"#,
            r#"[2,"id","Heartbeat",[]]"#,
            r#"[2,"id","Heartbeat",null]"#,
            r#"[2,"id","Heartbeat","x"]"#,
            r#"[3,"id",[]]"#,
            r"[3,7,{}]",
            r#"[4,"id",1,"d",{}]"#,
            r#"[4,"id","GenericError",1,{}]"#,
            r#"[4,"id","GenericError","d",null]"#,
            r#"[4,"id","GenericError","d",[]]"#,
        ] {
            assert!(
                matches!(parse_err(text).kind, FrameErrorKind::InvalidField { .. }),
                "{text}"
            );
        }
    }

    #[test]
    fn broken_call_with_readable_id_gets_formation_violation_reply() {
        let err = parse_err(r#"[2,"id-9","Heartbeat",[]]"#);
        assert_eq!(err.message_id.as_deref(), Some("id-9"));
        assert_eq!(err.message_type, Some(2));
        let Some(Frame::CallError { id, code, .. }) = err.reply() else {
            panic!("expected a CALLERROR reply");
        };
        assert_eq!(id, "id-9");
        assert_eq!(code, ErrorCode::FormationViolation);
    }

    #[test]
    fn broken_call_without_readable_id_or_broken_result_gets_no_reply() {
        assert!(parse_err(r#"[2,5,"Heartbeat",{}]"#).reply().is_none());
        assert!(parse_err(r"[2]").reply().is_none());
        let broken_result = parse_err(r#"[3,"id",[]]"#);
        assert_eq!(broken_result.message_id.as_deref(), Some("id"));
        assert!(broken_result.reply().is_none());
    }

    #[test]
    fn serde_deserialize_reports_frame_errors() {
        let err = serde_json::from_str::<Frame>(r"[9]").unwrap_err();
        assert!(err.to_string().contains("unsupported message type"));
    }

    #[test]
    fn frame_record_serializes_direction_lowercase() {
        let record = FrameRecord {
            ts: DateTime::<Utc>::UNIX_EPOCH,
            direction: Direction::Out,
            msg_id: Some("1".into()),
            action: Some("Heartbeat".into()),
            frame: Frame::call("1", "Heartbeat", json!({})).to_value(),
        };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["direction"], "out");
        assert_eq!(serde_json::from_value::<FrameRecord>(json).unwrap(), record);
    }
}
