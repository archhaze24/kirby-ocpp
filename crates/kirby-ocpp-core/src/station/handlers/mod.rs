//! CSMS-originated CALLs.
//!
//! The connection layer has already validated the payload against the schema and hands the
//! actor an [`IncomingCall`] with a one-shot [`ReplyHandle`]. `dispatch_incoming` maps the
//! action name to a typed handler `fn(&mut StationActor, Responder<A>, A::Req)`:
//!
//! * the payload is decoded into `A::Req`; a schema-valid payload that still does not fit the
//!   typed request (e.g. an integer beyond `i32`) is answered with a `TypeConstraintViolation`
//!   CALLERROR, never dropped;
//! * a handler **consumes** its `Responder`, so it replies exactly once (the compiler enforces
//!   it; a handler that forgot would trigger the `InternalError` drop fallback);
//! * handlers never await; work they trigger (a `StatusNotification`, a `Boot`) is started
//!   *after* the reply was queued and completes through a continuation;
//! * unknown action names are answered `NotImplemented` (the connection layer normally
//!   filters them already).
//!
//! One file per topic: `config` (Get/ChangeConfiguration), `data_transfer`, `local_list`
//! (SendLocalList, GetLocalListVersion, ClearCache), `availability`
//! (ChangeAvailability, UnlockConnector), `trigger` (TriggerMessage), `remote` (RemoteStart/Stop,
//! Reset), `rules` (response rules applied before the handler), `reservation`, `smart` (charging profiles), `maintenance` (diagnostics, firmware), `security` (certificates).

#![allow(
    clippy::needless_pass_by_value,
    reason = "handlers share the signature `fn(&mut StationActor, Responder<A>, A::Req)`"
)]

mod availability;
mod config;
mod data_transfer;
mod local_list;
mod maintenance;
mod remote;
mod reservation;
mod rules;
mod security;
mod smart;
mod trigger;

use super::actor::StationActor;
use crate::ocpp::action::{Action, InboundAction};
use crate::ocpp::frame::ErrorCode;
use crate::ocpp::messages::{
    CancelReservation, ChangeAvailability, ChangeConfiguration, ClearCache, ClearChargingProfile,
    DataTransfer, GetCompositeSchedule, GetConfiguration, GetDiagnostics, GetLocalListVersion,
    RemoteStartTransaction, RemoteStopTransaction, ReserveNow, Reset, SendLocalList,
    SetChargingProfile, TriggerMessage, UnlockConnector, UpdateFirmware,
};
use crate::ocpp::messages::{
    CertificateSigned, DeleteCertificate, ExtendedTriggerMessage, GetInstalledCertificateIds,
    GetLog, InstallCertificate, SignedUpdateFirmware,
};
use crate::ocpp::responder::{ReplyHandle, Responder};
use crate::station::event::LogLevel;
use crate::station::response_rules::INBOUND_ACTIONS;
use crate::ws::IncomingCall;

/// A handler for the inbound action `A`.
type Handler<A> = fn(&mut StationActor, Responder<A>, <A as Action>::Req);

impl StationActor {
    /// Routes an incoming CALL to its handler.
    #[allow(clippy::too_many_lines, reason = "one flat dispatch table")]
    pub(super) fn dispatch_incoming(&mut self, call: IncomingCall) {
        let IncomingCall {
            action,
            payload,
            reply,
            ..
        } = call;
        self.log(LogLevel::Info, format!("CSMS requests {action}"));
        let reply = if INBOUND_ACTIONS.contains(&action.as_str()) {
            match self.apply_response_rule(&action, reply) {
                Some(reply) => reply,
                None => return,
            }
        } else {
            reply
        };
        match action.as_str() {
            ChangeConfiguration::NAME => {
                self.invoke::<ChangeConfiguration>(payload, reply, config::change_configuration);
            }
            GetConfiguration::NAME => {
                self.invoke::<GetConfiguration>(payload, reply, config::get_configuration);
            }
            ClearCache::NAME => self.invoke::<ClearCache>(payload, reply, local_list::clear_cache),
            DataTransfer::NAME => {
                self.invoke::<DataTransfer>(payload, reply, data_transfer::data_transfer);
            }
            GetLocalListVersion::NAME => {
                self.invoke::<GetLocalListVersion>(payload, reply, local_list::get_version);
            }
            SendLocalList::NAME => {
                self.invoke::<SendLocalList>(payload, reply, local_list::send_local_list);
            }
            ChangeAvailability::NAME => {
                self.invoke::<ChangeAvailability>(
                    payload,
                    reply,
                    availability::change_availability,
                );
            }
            UnlockConnector::NAME => {
                self.invoke::<UnlockConnector>(payload, reply, availability::unlock_connector);
            }
            TriggerMessage::NAME => {
                self.invoke::<TriggerMessage>(payload, reply, trigger::trigger_message);
            }
            CancelReservation::NAME => self.invoke::<CancelReservation>(
                payload,
                reply,
                reservation::cancel_reservation_handler,
            ),
            ReserveNow::NAME => {
                self.invoke::<ReserveNow>(payload, reply, reservation::reserve_now_handler);
            }
            ClearChargingProfile::NAME => {
                self.invoke::<ClearChargingProfile>(payload, reply, smart::clear_profile);
            }
            GetCompositeSchedule::NAME => {
                self.invoke::<GetCompositeSchedule>(payload, reply, smart::composite_schedule);
            }
            SetChargingProfile::NAME => {
                self.invoke::<SetChargingProfile>(payload, reply, smart::set_profile);
            }
            GetDiagnostics::NAME => {
                self.invoke::<GetDiagnostics>(payload, reply, maintenance::get_diagnostics);
            }
            GetLog::NAME => self.invoke::<GetLog>(payload, reply, maintenance::get_log),
            ExtendedTriggerMessage::NAME => self.invoke::<ExtendedTriggerMessage>(
                payload,
                reply,
                trigger::extended_trigger_message,
            ),
            SignedUpdateFirmware::NAME => self.invoke::<SignedUpdateFirmware>(
                payload,
                reply,
                maintenance::signed_update_firmware,
            ),
            UpdateFirmware::NAME => {
                self.invoke::<UpdateFirmware>(payload, reply, maintenance::update_firmware);
            }
            RemoteStartTransaction::NAME => {
                self.invoke::<RemoteStartTransaction>(payload, reply, remote::remote_start);
            }
            RemoteStopTransaction::NAME => {
                self.invoke::<RemoteStopTransaction>(payload, reply, remote::remote_stop);
            }
            Reset::NAME => self.invoke::<Reset>(payload, reply, remote::reset),
            InstallCertificate::NAME => {
                self.invoke::<InstallCertificate>(payload, reply, security::install_certificate);
            }
            DeleteCertificate::NAME => {
                self.invoke::<DeleteCertificate>(payload, reply, security::delete_certificate);
            }
            GetInstalledCertificateIds::NAME => self.invoke::<GetInstalledCertificateIds>(
                payload,
                reply,
                security::get_installed_certificate_ids,
            ),
            CertificateSigned::NAME => {
                self.invoke::<CertificateSigned>(payload, reply, security::certificate_signed);
            }
            other => {
                // Security Whitepaper actions (GetLog, InstallCertificate, ...) pass the schema
                // check but have no behaviour yet: refuse them with exactly one CALLERROR.
                let (code, text) = if crate::ocpp::messages::registry::find(other)
                    .is_some_and(|info| info.direction.central_system_may_send())
                {
                    (ErrorCode::NotSupported, format!("{other} is not supported"))
                } else {
                    (
                        ErrorCode::NotImplemented,
                        format!("{other} is not implemented"),
                    )
                };
                self.log(LogLevel::Warn, format!("{other} is not handled: {text}"));
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
                    format!("{} payload cannot be decoded: {error}", A::NAME),
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
