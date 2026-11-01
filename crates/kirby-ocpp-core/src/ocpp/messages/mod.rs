//! Typed request/response payloads, one module per OCPP 1.6 action.
//!
//! Only [`Heartbeat`] exists so far; it is the template for the other 27 (see
//! the conventions in [`crate::ocpp::action`]).

mod heartbeat;

pub use heartbeat::{Heartbeat, HeartbeatRequest, HeartbeatResponse};
