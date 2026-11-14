//! Routes [`Command2`]s to the module that implements them.
//!
//! Completion rules follow the 1.6 station: state commands are answered as soon as the local
//! state changed, CALL commands when the CSMS answered or the CALL failed.

#![allow(
    dead_code,
    reason = "the router has no caller until the actor loop is implemented; remove this allow then"
)]

use super::actor::Station2Actor;
use super::command::{Command2, Outcome2};
use super::types::Reply2;
use crate::error::StationError;

/// Answers a command whose module does not implement it yet.
pub(super) fn not_implemented(reply: Reply2, command: &'static str) {
    // The requester may have gone away; nothing to do about that.
    let _ = reply.send(Err(StationError::NotImplemented { command }));
}

impl Station2Actor {
    /// Runs one command; the answer goes to `reply` now or when the CSMS has answered.
    pub(super) fn execute(&mut self, command: Command2, reply: Reply2) {
        match command {
            Command2::Connect => self.cmd_connect(reply),
            Command2::Disconnect => self.cmd_disconnect(reply),
            Command2::Reconnect => self.cmd_reconnect(reply),
            Command2::Boot => self.cmd_boot(reply),
            Command2::Heartbeat => self.cmd_heartbeat(reply),
            Command2::PlugIn { evse } => self.cmd_plug_in(evse, reply),
            Command2::Unplug { evse } => self.cmd_unplug(evse, reply),
            Command2::PresentToken {
                evse,
                id_token,
                token_type,
            } => self.cmd_present_token(evse, id_token, token_type, reply),
            Command2::Authorize {
                id_token,
                token_type,
            } => self.cmd_authorize(id_token, token_type, reply),
            Command2::StopTransaction { evse, reason } => {
                self.cmd_stop_transaction(evse, reason, reply);
            }
            Command2::AdvanceMeter { evse, wh_delta } => {
                self.cmd_advance_meter(evse, wh_delta, reply);
            }
            Command2::SetChargingState { evse, state } => {
                self.cmd_set_charging_state(evse, state, reply);
            }
            Command2::Fault { evse, connector } => self.cmd_fault(evse, connector, reply),
            Command2::ClearFault { evse, connector } => {
                self.cmd_clear_fault(evse, connector, reply);
            }
            Command2::StatusNotification { evse, connector } => {
                self.cmd_status_notification(evse, connector, reply);
            }
            Command2::SendSecurityEvent {
                event_type,
                tech_info,
            } => self.cmd_send_security_event(event_type, tech_info, reply),
            Command2::DataTransfer {
                vendor_id,
                message_id,
                data,
            } => self.cmd_data_transfer(vendor_id, message_id, data, reply),
            Command2::SendRaw {
                action,
                payload,
                validate,
            } => self.cmd_send_raw(action, payload, validate, reply),
            Command2::SetVariable {
                component,
                variable,
                value,
            } => self.cmd_set_variable(component, variable, value, reply),
            // The actor loop stops the station before commands are executed.
            Command2::Shutdown => {
                let _ = reply.send(Ok(Outcome2::Done));
            }
        }
    }
}
