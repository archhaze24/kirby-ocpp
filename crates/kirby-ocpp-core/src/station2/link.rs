//! The link to the CSMS: connect, reconnect, boot, registration and heartbeat.

#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use super::actor::Station2Actor;
use super::commands::not_implemented;
use super::types::Reply2;
use crate::ocpp::responder::ReplyHandle;
use crate::ocpp::v2::messages::BootReason;

/// The connection to the CSMS and everything about keeping it.
#[derive(Debug, Default)]
pub(crate) struct Link2 {}

impl Station2Actor {
    /// `Command2::Connect`.
    pub(super) fn cmd_connect(&mut self, reply: Reply2) {
        not_implemented(reply, "connect");
    }

    /// `Command2::Disconnect`.
    pub(super) fn cmd_disconnect(&mut self, reply: Reply2) {
        not_implemented(reply, "disconnect");
    }

    /// `Command2::Reconnect`.
    pub(super) fn cmd_reconnect(&mut self, reply: Reply2) {
        not_implemented(reply, "reconnect");
    }

    /// `Command2::Boot`.
    pub(super) fn cmd_boot(&mut self, reply: Reply2) {
        not_implemented(reply, "boot");
    }

    /// `Command2::Heartbeat`.
    pub(super) fn cmd_heartbeat(&mut self, reply: Reply2) {
        not_implemented(reply, "heartbeat");
    }

    /// Sends a `BootNotification` with `reason`; the answer goes to `reply` when given.
    pub(super) fn send_boot(&mut self, _reason: BootReason, _reply: Option<Reply2>) {}

    /// The heartbeat period elapsed.
    pub(super) fn on_heartbeat_timer(&mut self) {}

    /// The reconnect delay elapsed.
    pub(super) fn on_reconnect_timer(&mut self) {}

    /// The boot retry delay elapsed.
    pub(super) fn on_boot_retry_timer(&mut self) {}

    /// Registration gate for an incoming CALL: hands the reply back when the call may be
    /// dispatched, or answers it and returns `None`.
    pub(super) fn gate_incoming(
        &mut self,
        _action: &str,
        reply: ReplyHandle,
    ) -> Option<ReplyHandle> {
        Some(reply)
    }
}
