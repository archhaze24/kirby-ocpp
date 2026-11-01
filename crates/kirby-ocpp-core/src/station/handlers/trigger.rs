//! `TriggerMessage` (TS `handleTriggerMessage`, with the fixes from `core-analysis.md`).
//!
//! * All six requestable messages are supported. `MeterValues` sends one one-off sample
//!   (context `Trigger`, measurands from `MeterValuesSampledData`) built from a transient
//!   `SimulatedMeter` at the connector's stored energy; it does not advance the meter and
//!   carries the `transactionId` of a running (non-local) transaction. Connector 0 reports the sum of all connectors;
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
    DiagnosticsStatusNotification, DiagnosticsStatusNotificationRequest,
    FirmwareStatusNotification, FirmwareStatusNotificationRequest, MeterValues, MeterValuesRequest,
    TriggerMessage, TriggerMessageRequest, TriggerMessageResponse, TriggerMessageStatus,
};
use crate::ocpp::responder::Responder;
use crate::ocpp::types::{MessageTrigger, ReadingContext, RegistrationStatus};
use crate::station::actor::StationActor;
use crate::station::calls::Origin;
use crate::station::config_registry::METER_VALUES_SAMPLED_DATA;
use crate::station::event::LogLevel;
use crate::station::metering::SimulatedMeter;

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
    if request.requested_message != MessageTrigger::BootNotification
        && !matches!(
            actor.state.registration,
            Some(RegistrationStatus::Accepted | RegistrationStatus::Pending)
        )
    {
        return TriggerMessageStatus::Rejected;
    }
    if matches!(
        request.requested_message,
        MessageTrigger::StatusNotification | MessageTrigger::MeterValues
    ) && let Some(id) = request.connector_id
        && actor.find_connector(id).is_none()
    {
        return TriggerMessageStatus::Rejected;
    }
    TriggerMessageStatus::Accepted
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
    let (energy_wh, status, active_tx) = if id.is_station() {
        let total: u64 = actor.state.connectors.iter().map(|c| c.meter_wh).sum();
        (total, actor.state.charge_point.status, None)
    } else {
        let Some(connector) = actor.find_connector(id) else {
            return;
        };
        (connector.meter_wh, connector.status, connector.tx_id())
    };
    let measurands = actor.registry.measurands(METER_VALUES_SAMPLED_DATA);
    #[allow(
        clippy::cast_precision_loss,
        reason = "Wh counters stay far below 2^53"
    )]
    let mut meter = SimulatedMeter::with_energy_wh(energy_wh as f64);
    let sample = meter.sample(
        actor.clock.now_utc(),
        status,
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
