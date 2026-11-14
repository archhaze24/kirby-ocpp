//! Outbound CALLs of the station actor.
//!
//! The actor never awaits a CSMS round trip: a CALL stores a continuation that runs inside
//! the actor loop when the answer arrives, the CALL fails or the link goes away. Whether a
//! message may leave at all depends on the registration state and on why it is sent
//! ([`Origin2`]).

#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use serde_json::Value;

use super::actor::Station2Actor;
use super::commands::not_implemented;
use super::types::{Continuation2, Origin2, Reply2};
use crate::error::CallError;
use crate::ocpp::action::OutboundAction;

/// CALLs handed to the connection and not completed yet.
#[derive(Debug, Default)]
pub(crate) struct Calls2 {}

impl Station2Actor {
    /// Sends a CALL with a JSON payload; `cont` runs when it completed.
    pub(super) fn call_raw(
        &mut self,
        _origin: Origin2,
        _action: &str,
        _payload: Value,
        _validate: bool,
        _cont: Continuation2,
    ) {
    }

    /// Sends the typed CALL of `A`; `cont` runs when it completed.
    pub(super) fn call<A: OutboundAction>(
        &mut self,
        _origin: Origin2,
        _req: &A::Req,
        _cont: impl FnOnce(&mut Self, Result<A::Res, CallError>) + Send + 'static,
    ) {
    }

    /// The registration gate: whether `action` may be sent now.
    pub(super) fn may_send(&self, _action: &str, _origin: Origin2) -> Result<(), CallError> {
        Ok(())
    }

    /// `Command2::SendRaw`.
    pub(super) fn cmd_send_raw(
        &mut self,
        _action: String,
        _payload: Value,
        _validate: bool,
        reply: Reply2,
    ) {
        not_implemented(reply, "send_raw");
    }

    /// `Command2::SendSecurityEvent`.
    pub(super) fn cmd_send_security_event(
        &mut self,
        _event_type: String,
        _tech_info: Option<String>,
        reply: Reply2,
    ) {
        not_implemented(reply, "send_security_event");
    }
}
