//! Typed OCPP actions.
//!
//! An [`Action`] ties an action *name* to its request and response payload
//! types, so `connection.call::<Heartbeat>(HeartbeatRequest {})` returns a
//! `HeartbeatResponse` and a handler for an incoming `Reset` gets a
//! `Responder<Reset>` that only accepts a `ResetResponse`.
//!
//! # How the message structs are defined (`ocpp/messages/`)
//!
//! OCPP 1.6 has 28 distinct actions: 10 initiated by the charge point, 19 by
//! the central system, and `DataTransfer` which goes both ways; the Security
//! Whitepaper adds 11 more (4 + 7). The list with directions is
//! `messages::registry::ACTIONS`. The `messages` module groups
//! them by profile; for each action `Foo` it defines:
//!
//! * `pub struct FooRequest { .. }` and `pub struct FooResponse { .. }`,
//!   `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]`,
//!   `#[serde(rename_all = "camelCase")]`, optional fields as `Option<T>` with
//!   `#[serde(skip_serializing_if = "Option::is_none")]`, wire enums from
//!   [`crate::ocpp::types`] and ids from [`crate::ids`] instead of bare numbers
//!   and strings, timestamps as `chrono::DateTime<Utc>`;
//! * a marker `pub struct Foo;` (declared with the `action!` macro) implementing [`Action`] and one or both of
//!   [`OutboundAction`] (charge point -> CSMS) / [`InboundAction`] (CSMS ->
//!   charge point);
//! * a row in the table-driven test of `messages::registry`, which validates a
//!   typed sample of every request and response against the bundled schemas and
//!   roundtrips it.
//!
//! The structs are **not** the source of truth for validation: the bundled JSON
//! schemas are (see [`crate::ocpp::schema`]). The connection layer validates
//! the raw `serde_json::Value` against the schema first and only then calls
//! `serde_json::from_value`, so a schema violation maps to the right CALLERROR
//! code instead of an opaque serde error. `Heartbeat` is the smallest example.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// An OCPP action: name plus request/response payload types.
pub trait Action {
    /// The action name exactly as it appears in a CALL (e.g. `"Heartbeat"`).
    const NAME: &'static str;
    /// Payload of the CALL.
    type Req: Serialize + DeserializeOwned;
    /// Payload of the CALLRESULT.
    type Res: Serialize + DeserializeOwned;
}

/// Marker: the charge point may send this action (`Connection::call_typed` accepts it).
pub trait OutboundAction: Action {}

/// Marker: the central system may send this action (a handler gets a `Responder` for it).
pub trait InboundAction: Action {}
