//! `Reset`, `TriggerMessage`, `UnlockConnector`, `ClearCache` and `DataTransfer`, plus the
//! outbound `DataTransfer` command.

use serde_json::{Value, json};

use crate::ocpp::responder::Responder;
use crate::ocpp::v2::messages::MessageTrigger;
use crate::ocpp::v2::messages::{
    BootReason, ClearCache, ClearCacheRequest, ClearCacheResponse, ClearCacheStatus, DataTransfer,
    DataTransferRequest, DataTransferResponse, DataTransferStatus, Reset, ResetKind, ResetRequest,
    ResetResponse, ResetStatus, TransactionEventKind, TriggerMessage, TriggerMessageRequest,
    TriggerMessageResponse, TriggerMessageStatus, TriggerReason, UnlockConnector,
    UnlockConnectorRequest, UnlockConnectorResponse, UnlockStatus,
};
use crate::ocpp::v2::types::StatusInfo;
use crate::station::LogLevel;
use crate::station2::actor::Station2Actor;
use crate::station2::calls::call_failure;
use crate::station2::command::Outcome2;
use crate::station2::transactions::EventExtra;
use crate::station2::types::{Origin2, Reply2};

fn status_info(reason: &str) -> StatusInfo {
    StatusInfo {
        reason_code: reason.to_owned(),
        additional_info: None,
        custom_data: None,
    }
}

// ---- Reset -----------------------------------------------------------------------

pub(in crate::station2) fn reset(
    actor: &mut Station2Actor,
    responder: Responder<Reset>,
    request: ResetRequest,
) {
    // Any `evseId` (even an invalid one) asks for an EVSE reset, which is not supported.
    let evse = request.evse_id.map(|id| u32::try_from(id).unwrap_or(0));
    let status = actor.start_reset(request.kind, evse);
    actor.log(
        LogLevel::Info,
        format!("Reset {:?} -> {status:?}", request.kind),
    );
    let unsupported = evse.is_some() || request.kind == ResetKind::ImmediateAndResume;
    let status_info =
        (unsupported && status == ResetStatus::Rejected).then(|| status_info("UnsupportedParam"));
    actor.respond(
        responder,
        ResetResponse {
            status,
            status_info,
            custom_data: None,
        },
    );
}

// ---- TriggerMessage --------------------------------------------------------------

/// What an accepted `TriggerMessage` sends once the response is out.
enum Trigger {
    Boot,
    Heartbeat,
    Status {
        evse: u32,
        connector: u32,
    },
    Meter(Option<u32>),
    TransactionEvents(Vec<u32>),
    /// A message with a fixed `Idle` status: `LogStatusNotification`,
    /// `FirmwareStatusNotification`.
    Idle(&'static str),
}

/// A positive id from the wire.
fn positive(id: i32) -> Option<u32> {
    u32::try_from(id).ok().filter(|id| *id > 0)
}

impl Station2Actor {
    /// Decides what a `TriggerMessage` does (F06).
    fn plan_trigger(
        &self,
        requested: MessageTrigger,
        evse: Option<&crate::ocpp::v2::types::Evse>,
    ) -> Result<Trigger, TriggerMessageStatus> {
        let rejected = TriggerMessageStatus::Rejected;
        // `evse` 0 or absent: all EVSEs where the message is per EVSE (F06.FR.11).
        let one_evse = evse.and_then(|e| positive(e.id));
        let known = |id: u32| self.evse(id).is_some();
        match requested {
            // F06.FR.17: a boot that was accepted is not repeated on request.
            MessageTrigger::BootNotification if self.registration_accepted() => Err(rejected),
            MessageTrigger::BootNotification => Ok(Trigger::Boot),
            MessageTrigger::Heartbeat => Ok(Trigger::Heartbeat),
            MessageTrigger::StatusNotification => {
                // F06.FR.12: a connector is needed.
                let evse_id = one_evse.ok_or(rejected)?;
                let connector = evse
                    .and_then(|e| e.connector_id)
                    .and_then(positive)
                    .ok_or(rejected)?;
                self.connector_status_text(evse_id, connector)
                    .map(|_| Trigger::Status {
                        evse: evse_id,
                        connector,
                    })
                    .ok_or(rejected)
            }
            MessageTrigger::MeterValues => match one_evse {
                Some(id) if known(id) => Ok(Trigger::Meter(Some(id))),
                Some(_) => Err(rejected),
                None => Ok(Trigger::Meter(None)),
            },
            MessageTrigger::TransactionEvent => {
                let ids: Vec<u32> = match one_evse {
                    Some(id) => vec![id],
                    None => self.evses.iter().map(|e| e.id).collect(),
                };
                let running: Vec<u32> = ids
                    .into_iter()
                    .filter(|id| self.active_tx(*id).is_some())
                    .collect();
                if running.is_empty() {
                    Err(rejected)
                } else {
                    Ok(Trigger::TransactionEvents(running))
                }
            }
            MessageTrigger::LogStatusNotification => Ok(Trigger::Idle("LogStatusNotification")),
            MessageTrigger::FirmwareStatusNotification => {
                Ok(Trigger::Idle("FirmwareStatusNotification"))
            }
            // F06.FR.08
            MessageTrigger::SignChargingStationCertificate
            | MessageTrigger::SignV2gCertificate
            | MessageTrigger::SignCombinedCertificate
            | MessageTrigger::PublishFirmwareStatusNotification
            // OCPP 2.1 additions the station has no message for.
            | MessageTrigger::SignV2g20Certificate
            | MessageTrigger::CustomTrigger => {
                Err(TriggerMessageStatus::NotImplemented)
            }
        }
    }

    /// Sends the message of an accepted trigger, as `Origin2::Triggered`.
    fn run_trigger(&mut self, trigger: Trigger) {
        match trigger {
            Trigger::Boot => self.send_boot(BootReason::Triggered, None),
            Trigger::Heartbeat => self.send_heartbeat(Origin2::Triggered, None),
            Trigger::Status { evse, connector } => {
                self.send_status(evse, connector, Origin2::Triggered);
            }
            Trigger::Meter(evse) => self.send_meter_values(evse, Origin2::Triggered),
            Trigger::TransactionEvents(ids) => {
                for id in ids {
                    self.tx_event(
                        id,
                        TransactionEventKind::Updated,
                        TriggerReason::Trigger,
                        EventExtra::default(),
                    );
                }
            }
            Trigger::Idle(action) => self.call_raw(
                Origin2::Triggered,
                action,
                json!({"status": "Idle"}),
                true,
                Box::new(move |actor, result| {
                    if let Err(error) = result {
                        actor.log(LogLevel::Warn, format!("{action} failed: {error}"));
                    }
                }),
            ),
        }
    }
}

pub(in crate::station2) fn trigger_message(
    actor: &mut Station2Actor,
    responder: Responder<TriggerMessage>,
    request: TriggerMessageRequest,
) {
    let plan = actor.plan_trigger(request.requested_message, request.evse.as_ref());
    let status = match &plan {
        Ok(_) => TriggerMessageStatus::Accepted,
        Err(status) => *status,
    };
    actor.log(
        LogLevel::Info,
        format!(
            "TriggerMessage {:?} -> {status:?}",
            request.requested_message
        ),
    );
    // F06.FR.04: the response first, then the message.
    actor.respond(
        responder,
        TriggerMessageResponse {
            status,
            status_info: None,
            custom_data: None,
        },
    );
    if let Ok(trigger) = plan {
        actor.run_trigger(trigger);
    }
}

// ---- UnlockConnector -------------------------------------------------------------

pub(in crate::station2) fn unlock_connector(
    actor: &mut Station2Actor,
    responder: Responder<UnlockConnector>,
    request: UnlockConnectorRequest,
) {
    let evse = positive(request.evse_id);
    let connector = positive(request.connector_id);
    let known = evse
        .zip(connector)
        .is_some_and(|(e, c)| actor.connector_status_text(e, c).is_some());
    let status = match evse {
        _ if !known => UnlockStatus::UnknownConnector,
        Some(e) if actor.active_tx(e).is_some_and(|tx| tx.authorized) => {
            UnlockStatus::OngoingAuthorizedTransaction
        }
        _ => UnlockStatus::Unlocked,
    };
    actor.respond(
        responder,
        UnlockConnectorResponse {
            status,
            status_info: None,
            custom_data: None,
        },
    );
}

// ---- ClearCache ------------------------------------------------------------------

pub(in crate::station2) fn clear_cache(
    actor: &mut Station2Actor,
    responder: Responder<ClearCache>,
    _request: ClearCacheRequest,
) {
    let status = if actor.cache_enabled() {
        actor.cache_clear();
        ClearCacheStatus::Accepted
    } else {
        ClearCacheStatus::Rejected
    };
    actor.respond(
        responder,
        ClearCacheResponse {
            status,
            status_info: None,
            custom_data: None,
        },
    );
}

// ---- DataTransfer ----------------------------------------------------------------

/// Data for the station's own vendor id is accepted and echoed, any other vendor id is
/// `UnknownVendorId`; the `messageId` is not interpreted (as in the 1.6 emulator).
pub(in crate::station2) fn data_transfer(
    actor: &mut Station2Actor,
    responder: Responder<DataTransfer>,
    request: DataTransferRequest,
) {
    let response = if request.vendor_id == actor.config.vendor {
        DataTransferResponse {
            status: DataTransferStatus::Accepted,
            status_info: None,
            data: request.data,
            custom_data: None,
        }
    } else {
        DataTransferResponse {
            status: DataTransferStatus::UnknownVendorId,
            status_info: None,
            data: None,
            custom_data: None,
        }
    };
    actor.respond(responder, response);
}

impl Station2Actor {
    /// `Command2::DataTransfer`.
    pub(in crate::station2) fn cmd_data_transfer(
        &mut self,
        vendor_id: String,
        message_id: Option<String>,
        data: Option<Value>,
        reply: Reply2,
    ) {
        let request = DataTransferRequest {
            message_id,
            data,
            vendor_id,
            custom_data: None,
        };
        self.call::<DataTransfer>(Origin2::Station, &request, move |_actor, result| {
            let outcome = match result {
                Ok(response) => Ok(Outcome2::Response {
                    payload: serde_json::to_value(&response).unwrap_or_default(),
                }),
                Err(error) => Err(call_failure(error)),
            };
            // The requester may have gone away; nothing to do about that.
            let _ = reply.send(outcome);
        });
    }
}
