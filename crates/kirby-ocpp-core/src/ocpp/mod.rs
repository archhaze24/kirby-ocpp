//! The OCPP 1.6J protocol layer: frames, typed messages, schema validation.
//!
//! Nothing in here knows about stations, sockets or timers.

pub mod action;
pub mod frame;
pub mod messages;
pub mod protocol;
pub mod responder;
pub mod schema;
mod schema_v201;
pub mod types;
pub mod v2;

pub use action::{Action, InboundAction, OutboundAction};
pub use frame::{
    Direction, ErrorCode, Frame, FrameError, FrameRecord, MessageId, REDACTED, redact_payload,
    redact_secrets,
};
pub use protocol::Protocol;
pub use responder::{ReplyError, ReplyHandle, Responder};
pub use schema::{
    PayloadKind, SchemaError, SchemaIssue, SchemaKeyword, SchemaLoadError, SchemaRegistry,
    SchemaViolation,
};
