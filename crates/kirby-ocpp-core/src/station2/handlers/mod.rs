//! CSMS-originated CALLs.
//!
//! The connection layer has already validated the payload against the 2.0.1 schemas and
//! hands the actor an [`IncomingCall`] with a one-shot [`ReplyHandle`]. `dispatch_incoming`
//! maps the action name to a typed handler `fn(&mut Station2Actor, Responder<A>, A::Req)`:
//!
//! * the payload is decoded into `A::Req`; a schema-valid payload that still does not fit the
//!   typed request is answered with a `TypeConstraintViolation` CALLERROR, never dropped;
//! * a handler **consumes** its `Responder`, so it replies exactly once;
//! * handlers never await; work they trigger is started *after* the reply was queued and
//!   completes through a continuation;
//! * an armed scripted reply (see `scripted`) answers the CALL instead of the handler;
//! * an action without a handler is answered with `NotSupported` (`NotImplemented` for a name
//!   2.0.1 does not define), so no CALL is left unanswered.
//!
//! One file per topic: `device_model` (variables and reports), `availability`, `tx`
//! (transaction status) and `control` (reset, trigger, unlock, cache, data transfer). The
//! remote start and stop handlers live in `remote`.

#![allow(
    clippy::needless_pass_by_value,
    reason = "handlers share the signature `fn(&mut Station2Actor, Responder<A>, A::Req)`"
)]

pub(super) mod availability;
pub(super) mod control;
pub(super) mod device_model;
pub(super) mod tx;

use super::actor::Station2Actor;
use super::link::sanitize;
use super::local_list;
use super::remote;
use super::reservation;
use crate::ocpp::action::{Action, InboundAction};
use crate::ocpp::frame::ErrorCode;
use crate::ocpp::responder::{ReplyHandle, Responder};
use crate::ocpp::v2::messages::{
    CancelReservation, ChangeAvailability, ClearCache, DataTransfer, GetBaseReport,
    GetLocalListVersion, GetReport, GetTransactionStatus, GetVariables, RequestStartTransaction,
    RequestStopTransaction, ReserveNow, Reset, SendLocalList, SetVariables, TriggerMessage,
    UnlockConnector,
};
use crate::ocpp::v2::registry::unhandled_error_code_for;
use crate::station::LogLevel;
use crate::ws::IncomingCall;

/// A handler for the inbound action `A`.
type Handler<A> = fn(&mut Station2Actor, Responder<A>, <A as Action>::Req);

impl Station2Actor {
    /// Routes an incoming CALL to its handler.
    pub(super) fn dispatch_incoming(&mut self, call: IncomingCall) {
        let IncomingCall {
            action,
            payload,
            reply,
            ..
        } = call;
        self.log(LogLevel::Info, format!("CSMS requests {action}"));
        let Some(reply) = self.gate_incoming(&action, reply) else {
            return;
        };
        // A scripted reply answers the CALL itself; the handler does not run.
        let Some(reply) = self.apply_scripted_reply(&action, reply) else {
            return;
        };
        match action.as_str() {
            GetVariables::NAME => {
                self.invoke::<GetVariables>(payload, reply, device_model::get_variables);
            }
            SetVariables::NAME => {
                self.invoke::<SetVariables>(payload, reply, device_model::set_variables);
            }
            GetBaseReport::NAME => {
                self.invoke::<GetBaseReport>(payload, reply, device_model::get_base_report);
            }
            GetReport::NAME => {
                self.invoke::<GetReport>(payload, reply, device_model::get_report);
            }
            ChangeAvailability::NAME => {
                self.invoke::<ChangeAvailability>(
                    payload,
                    reply,
                    availability::change_availability,
                );
            }
            GetTransactionStatus::NAME => {
                self.invoke::<GetTransactionStatus>(payload, reply, tx::get_transaction_status);
            }
            RequestStartTransaction::NAME => {
                self.invoke::<RequestStartTransaction>(
                    payload,
                    reply,
                    remote::request_start_transaction,
                );
            }
            RequestStopTransaction::NAME => {
                self.invoke::<RequestStopTransaction>(
                    payload,
                    reply,
                    remote::request_stop_transaction,
                );
            }
            Reset::NAME => self.invoke::<Reset>(payload, reply, control::reset),
            TriggerMessage::NAME => {
                self.invoke::<TriggerMessage>(payload, reply, control::trigger_message);
            }
            UnlockConnector::NAME => {
                self.invoke::<UnlockConnector>(payload, reply, control::unlock_connector);
            }
            SendLocalList::NAME => {
                self.invoke::<SendLocalList>(payload, reply, local_list::send_local_list);
            }
            GetLocalListVersion::NAME => {
                self.invoke::<GetLocalListVersion>(
                    payload,
                    reply,
                    local_list::get_local_list_version,
                );
            }
            ReserveNow::NAME => self.invoke::<ReserveNow>(payload, reply, reservation::reserve_now),
            CancelReservation::NAME => {
                self.invoke::<CancelReservation>(payload, reply, reservation::cancel_reservation);
            }
            ClearCache::NAME => self.invoke::<ClearCache>(payload, reply, control::clear_cache),
            DataTransfer::NAME => {
                self.invoke::<DataTransfer>(payload, reply, control::data_transfer);
            }
            other => {
                let code = unhandled_error_code_for(self.protocol(), other);
                let text = if code == ErrorCode::NotSupported {
                    format!("{other} is not supported")
                } else {
                    format!("{other} is not implemented")
                };
                self.log(
                    LogLevel::Warn,
                    format!("{} is not handled", sanitize(other)),
                );
                let _ = reply.reject(code, text);
            }
        }
    }

    /// Decodes the request and calls `handler`, or answers the decoding failure.
    fn invoke<A: InboundAction>(
        &mut self,
        payload: serde_json::Value,
        reply: ReplyHandle,
        handler: Handler<A>,
    ) {
        let responder = Responder::<A>::new(reply);
        match serde_json::from_value::<A::Req>(payload) {
            Ok(request) => handler(self, responder, request),
            Err(error) => {
                self.log(
                    LogLevel::Warn,
                    format!(
                        "{} payload cannot be decoded: {}",
                        A::NAME,
                        sanitize(&error.to_string())
                    ),
                );
                let _ = responder.reject_error(
                    ErrorCode::TypeConstraintViolation,
                    format!("invalid {} payload: {error}", A::NAME),
                );
            }
        }
    }

    /// Sends `response` as the CALLRESULT; a vanished connection is logged, not fatal.
    pub(super) fn respond<A: Action>(&self, responder: Responder<A>, response: A::Res) {
        if let Err(error) = responder.accept(response) {
            self.log(
                LogLevel::Warn,
                format!("could not reply to {}: {error}", A::NAME),
            );
        }
    }

    /// Sends a CALLERROR as the answer.
    pub(super) fn respond_error<A: Action>(
        &self,
        responder: Responder<A>,
        code: ErrorCode,
        description: impl Into<String>,
    ) {
        if let Err(error) = responder.reject_error(code, description) {
            self.log(
                LogLevel::Warn,
                format!("could not reply to {}: {error}", A::NAME),
            );
        }
    }
}
