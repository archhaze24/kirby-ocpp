//! `TriggerMessage` (TS `handleTriggerMessage`, with the fixes from `core-analysis.md`).
//!
//! * All six requestable messages are supported. `MeterValues` sends one one-off sample
//!   (context `Trigger`, measurands from `MeterValuesSampledData`) built with `meter_value`
//!   from `meter_reading`: the energy integrated so far is flushed into the connector's
//!   register, and the sample carries the `transactionId` of a running (non-local)
//!   transaction. Connector 0 reports the sum of all connectors;
//!   without `connectorId` every physical connector (`1..=n`) is sampled.
//! * `Diagnostics`/`FirmwareStatusNotification` report the **current** status, not `Idle`.
//! * `StatusNotification` / `MeterValues` are connector specific: `connectorId` 0 is the
//!   charge point, an unknown connector is `Rejected`; without a `connectorId`,
//!   `StatusNotification` covers connector 0 and every connector.
//! * Registration gate: `BootNotification` may always be triggered. Everything else is
//!   `Rejected` unless the station is `Accepted`, or `Pending` (OCPP 1.6 section 4.2 lets a
//!   `Pending` station send what the CSMS triggered).
//! * The reply is queued **before** the triggered message is started, and the connection
//!   sends our replies ahead of queued CALLs, so the `.conf` precedes the message on the wire.

use crate::ids::ConnectorId;
use crate::ocpp::messages::{
    DiagnosticsStatusNotification, DiagnosticsStatusNotificationRequest, ExtendedMessageTrigger,
    ExtendedTriggerMessage, ExtendedTriggerMessageRequest, ExtendedTriggerMessageResponse,
    FirmwareStatusNotification, FirmwareStatusNotificationRequest, LogStatusNotification,
    LogStatusNotificationRequest, MeterValues, MeterValuesRequest, TriggerMessage,
    TriggerMessageRequest, TriggerMessageResponse, TriggerMessageStatus, UploadLogStatus,
};
use crate::ocpp::messages::{
    SignedFirmwareStatus, SignedFirmwareStatusNotification, SignedFirmwareStatusNotificationRequest,
};
use crate::ocpp::responder::Responder;
use crate::ocpp::types::{MessageTrigger, ReadingContext, RegistrationStatus};
use crate::station::actor::StationActor;
use crate::station::calls::Origin;
use crate::station::config_registry::METER_VALUES_SAMPLED_DATA;
use crate::station::event::LogLevel;
use crate::station::metering::meter_value;

pub(super) fn trigger_message(
    actor: &mut StationActor,
    responder: Responder<TriggerMessage>,
    request: TriggerMessageRequest,
) {
    let message = request.requested_message;
    let status = decide(actor, &request);
    actor.respond(responder, TriggerMessageResponse { status });
    actor.log(
        LogLevel::Info,
        format!("TriggerMessage {message} -> {status}"),
    );
    if status == TriggerMessageStatus::Accepted {
        start(actor, message, request.connector_id);
    }
}

fn decide(actor: &StationActor, request: &TriggerMessageRequest) -> TriggerMessageStatus {
    decide_for(
        actor,
        request.requested_message == MessageTrigger::BootNotification,
        matches!(
            request.requested_message,
            MessageTrigger::StatusNotification | MessageTrigger::MeterValues
        ),
        request.connector_id,
    )
}

/// Shared rules: only `BootNotification` may be triggered before registration (`Accepted` or
/// `Pending`); connector specific messages refuse an unknown `connectorId`.
fn decide_for(
    actor: &StationActor,
    is_boot: bool,
    connector_specific: bool,
    connector: Option<ConnectorId>,
) -> TriggerMessageStatus {
    if !is_boot
        && !matches!(
            actor.state.registration,
            Some(RegistrationStatus::Accepted | RegistrationStatus::Pending)
        )
    {
        return TriggerMessageStatus::Rejected;
    }
    if connector_specific
        && let Some(id) = connector
        && actor.find_connector(id).is_none()
    {
        return TriggerMessageStatus::Rejected;
    }
    TriggerMessageStatus::Accepted
}

/// `ExtendedTriggerMessage` (Security Whitepaper): the 1.6 `TriggerMessage` rules, plus
/// `LogStatusNotification` (reports `Uploading` with the request id while a `GetLog` runs, else
/// `Idle`) and `SignChargePointCertificate` (starts the `SignCertificate` flow). Like
/// `FirmwareStatusNotification`, the reported status is the current one.
pub(super) fn extended_trigger_message(
    actor: &mut StationActor,
    responder: Responder<ExtendedTriggerMessage>,
    request: ExtendedTriggerMessageRequest,
) {
    use ExtendedMessageTrigger as E;
    let message = request.requested_message;
    let common = match message {
        E::BootNotification => Some(MessageTrigger::BootNotification),
        E::FirmwareStatusNotification => Some(MessageTrigger::FirmwareStatusNotification),
        E::Heartbeat => Some(MessageTrigger::Heartbeat),
        E::MeterValues => Some(MessageTrigger::MeterValues),
        E::StatusNotification => Some(MessageTrigger::StatusNotification),
        E::LogStatusNotification | E::SignChargePointCertificate => None,
    };
    let status = decide_for(
        actor,
        message == E::BootNotification,
        matches!(message, E::StatusNotification | E::MeterValues),
        request.connector_id,
    );
    actor.respond(responder, ExtendedTriggerMessageResponse { status });
    actor.log(
        LogLevel::Info,
        format!("ExtendedTriggerMessage {message} -> {status}"),
    );
    if status != TriggerMessageStatus::Accepted {
        return;
    }
    match common {
        Some(MessageTrigger::FirmwareStatusNotification) if actor.last_firmware_signed => {
            // The current firmware update is a signed one: report it in its own message.
            let status = actor.state.signed_firmware_status;
            let request_id = if status == SignedFirmwareStatus::Idle {
                None
            } else {
                actor.signed_fw_request_id
            };
            actor.call::<SignedFirmwareStatusNotification>(
                Origin::Triggered,
                &SignedFirmwareStatusNotificationRequest { status, request_id },
                |_, _| {},
            );
        }
        Some(trigger) => start(actor, trigger, request.connector_id),
        None if message == E::LogStatusNotification => {
            let status = if actor.log_upload.is_empty() {
                UploadLogStatus::Idle
            } else {
                UploadLogStatus::Uploading
            };
            let request_id = if status == UploadLogStatus::Idle {
                None
            } else {
                actor.log_request_id
            };
            actor.call::<LogStatusNotification>(
                Origin::Triggered,
                &LogStatusNotificationRequest { status, request_id },
                |_, _| {},
            );
        }
        None => {
            // The command result is not needed: the flow logs its own outcome.
            let (reply, _) = tokio::sync::oneshot::channel();
            actor.request_certificate(reply);
        }
    }
}

/// Starts the triggered message (after the reply was queued).
fn start(actor: &mut StationActor, message: MessageTrigger, connector: Option<ConnectorId>) {
    match message {
        MessageTrigger::BootNotification => actor.send_boot(None),
        MessageTrigger::Heartbeat => actor.send_heartbeat(Origin::Triggered, None),
        MessageTrigger::DiagnosticsStatusNotification => {
            let status = actor.diagnostics_status();
            actor.call::<DiagnosticsStatusNotification>(
                Origin::Triggered,
                &DiagnosticsStatusNotificationRequest { status },
                |_, _| {},
            );
        }
        MessageTrigger::FirmwareStatusNotification => {
            let status = actor.firmware_status();
            actor.call::<FirmwareStatusNotification>(
                Origin::Triggered,
                &FirmwareStatusNotificationRequest { status },
                |_, _| {},
            );
        }
        MessageTrigger::StatusNotification => {
            let ids: Vec<ConnectorId> = match connector {
                Some(id) => vec![id],
                None => std::iter::once(ConnectorId::STATION)
                    .chain(actor.state.connectors.iter().map(|c| c.id))
                    .collect(),
            };
            for id in ids {
                actor.send_status_forced(id, Origin::Triggered, None);
            }
        }
        MessageTrigger::MeterValues => {
            let ids: Vec<ConnectorId> = match connector {
                Some(id) => vec![id],
                None => actor.state.connectors.iter().map(|c| c.id).collect(),
            };
            for id in ids {
                send_meter_values(actor, id);
            }
        }
    }
}

/// One triggered `MeterValues` sample for `id` (0 = the whole station).
fn send_meter_values(actor: &mut StationActor, id: ConnectorId) {
    let active_tx = if id.is_station() {
        None
    } else {
        let Some(connector) = actor.find_connector(id) else {
            return;
        };
        connector.tx_id()
    };
    let measurands = actor.registry.measurands(METER_VALUES_SAMPLED_DATA);
    let reading = actor.meter_reading(id, false);
    let sample = meter_value(
        &actor.power_model(),
        &reading,
        actor.clock.now_utc(),
        &measurands,
        ReadingContext::Trigger,
    );
    actor.call::<MeterValues>(
        Origin::Triggered,
        &MeterValuesRequest {
            connector_id: id,
            transaction_id: active_tx.and_then(crate::ids::TxId::wire),
            meter_value: vec![sample],
        },
        |_, _| {},
    );
}
