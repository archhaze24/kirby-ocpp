//! User commands ([`Command`]) executed by the actor.
//!
//! Completion rules:
//!
//! * **State commands** (plug, unplug, status, fault, connectors, outcomes) are answered as
//!   soon as the local state changed. The resulting `StatusNotification` is queued by the
//!   status discipline (`status.rs`) and may be debounced or, offline, not sent at all.
//! * **CALL commands** (`Boot`, `Heartbeat`, `DataTransfer`, `*StatusNotification`,
//!   `StatusNotification`, `SendRaw`) are answered when the CSMS answered, with its payload
//!   in `Outcome::Response`, or with the error that stopped the CALL. They are subject to the
//!   registration gate (`calls.rs`), except `SendRaw`.
//! * Commands that belong to stage 2b answer `StationError::NotImplemented`.

use serde_json::Value;

use super::actor::StationActor;
use super::calls::{Origin, Reply};
use super::command::{Command, Outcome};
use super::connector::{
    STATUS_TEXT_MAX, charge_point_status, fault_error_code, new_connector, next_status,
    status_after_fault_clear, status_after_unplug, trim_text,
};
use super::event::LogLevel;
use crate::error::{CallError, StationError};
use crate::ids::ConnectorId;
use crate::ocpp::action::OutboundAction;
use crate::ocpp::messages::{
    DataTransfer, DataTransferRequest, DiagnosticsStatusNotification,
    DiagnosticsStatusNotificationRequest, FirmwareStatusNotification,
    FirmwareStatusNotificationRequest,
};
use crate::ocpp::types::{
    AvailabilityType, ChargePointErrorCode, ConnectorStatus, DiagnosticsStatus, FirmwareStatus,
};

/// How a failed CALL is reported to the caller of a command.
pub(super) fn call_failure(error: CallError) -> StationError {
    match error {
        CallError::NotConnected => StationError::NotConnected,
        other => StationError::Call(other),
    }
}

impl StationActor {
    /// Runs one command; the answer goes to `reply` now or when the CSMS has answered.
    pub(super) fn execute(&mut self, command: Command, reply: Reply) {
        match command {
            Command::Boot => {
                if self.link.conn.is_none() {
                    let _ = reply.send(Err(StationError::NotConnected));
                } else {
                    self.send_boot(Some(reply));
                }
            }
            Command::Heartbeat => self.send_heartbeat(Origin::Station, Some(reply)),
            Command::StatusNotification { connector } => {
                self.send_status_forced(connector, Origin::Station, Some(reply));
            }
            Command::DataTransfer {
                vendor_id,
                message_id,
                data,
            } => self.user_call::<DataTransfer>(
                &DataTransferRequest {
                    vendor_id,
                    message_id,
                    data,
                },
                reply,
            ),
            Command::DiagnosticsStatusNotification { status } => {
                self.state.diagnostics_status = status;
                self.user_call::<DiagnosticsStatusNotification>(
                    &DiagnosticsStatusNotificationRequest { status },
                    reply,
                );
            }
            Command::FirmwareStatusNotification { status } => {
                self.state.firmware_status = status;
                self.user_call::<FirmwareStatusNotification>(
                    &FirmwareStatusNotificationRequest { status },
                    reply,
                );
            }
            Command::SendRaw {
                action,
                payload,
                validate,
            } => self.send_raw(&action, payload, validate, reply),
            other => {
                // The remaining commands complete immediately.
                let result = self.execute_now(other);
                let _ = reply.send(result);
            }
        }
    }

    fn execute_now(&mut self, command: Command) -> Result<Outcome, StationError> {
        match command {
            Command::Connect => {
                self.connect();
                Ok(Outcome::Done)
            }
            Command::Disconnect => {
                self.disconnect();
                Ok(Outcome::Done)
            }
            Command::Reconnect => {
                self.disconnect();
                self.connect();
                Ok(Outcome::Done)
            }
            Command::AddConnector => Ok(self.add_connector()),
            Command::PlugIn { connector } => self.plug_in(connector),
            Command::Unplug { connector } => self.unplug(connector),
            Command::SetConnectorStatus { connector, status } => self.set_status(connector, status),
            Command::CycleConnectorStatus { connector } => {
                let current = self
                    .find_connector(connector)
                    .ok_or(StationError::UnknownConnector(connector))?
                    .status;
                self.set_status(connector, next_status(current))
            }
            Command::Fault {
                connector,
                error_code,
                info,
                vendor_error_code,
            } => self.fault(
                connector,
                error_code,
                info.as_deref(),
                vendor_error_code.as_deref(),
            ),
            Command::ClearFault { connector } => self.clear_fault(connector),
            Command::SetDiagnosticsOutcome { outcome } => {
                self.state.diagnostics_outcome = outcome;
                self.log(
                    LogLevel::Info,
                    format!("Diagnostics outcome set to {outcome:?}"),
                );
                Ok(Outcome::Done)
            }
            Command::SetFirmwareOutcome { outcome } => {
                self.state.firmware_outcome = outcome;
                self.log(
                    LogLevel::Info,
                    format!("Firmware outcome set to {outcome:?}"),
                );
                Ok(Outcome::Done)
            }
            // stage 2b: Authorize, StartTransaction, StopTransaction and MeterValues.
            other => Err(StationError::NotImplemented {
                command: other.name(),
            }),
        }
    }

    /// A user-initiated CALL answered with `Outcome::Response`.
    fn user_call<A>(&mut self, request: &A::Req, reply: Reply)
    where
        A: OutboundAction,
        A::Res: Send + 'static,
    {
        self.call::<A>(Origin::Station, request, move |actor, result| {
            let answer = match result {
                Ok(response) => {
                    let payload = serde_json::to_value(&response).unwrap_or(Value::Null);
                    actor.log_data_transfer_result(A::NAME, &payload);
                    Ok(Outcome::Response { payload })
                }
                Err(error) => Err(call_failure(error)),
            };
            let _ = reply.send(answer);
        });
    }

    fn log_data_transfer_result(&self, action: &str, payload: &Value) {
        if action != "DataTransfer" {
            return;
        }
        let status = payload
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("Unknown");
        let level = if status == "Accepted" {
            LogLevel::Success
        } else {
            LogLevel::Warn
        };
        self.log(level, format!("DataTransfer status: {status}"));
        if let Some(data) = payload.get("data").and_then(Value::as_str) {
            self.log(
                LogLevel::Info,
                format!("DataTransfer response data: {data}"),
            );
        }
    }

    fn send_raw(&mut self, action: &str, payload: Value, validate: bool, reply: Reply) {
        self.call_raw(
            Origin::Raw,
            action,
            payload,
            validate,
            Box::new(move |_, result| {
                let answer = match result {
                    Ok(payload) => Ok(Outcome::Response { payload }),
                    Err(error) => Err(call_failure(error)),
                };
                let _ = reply.send(answer);
            }),
        );
    }

    // ---- connectors ------------------------------------------------------------

    fn add_connector(&mut self) -> Outcome {
        let id = self
            .state
            .connectors
            .iter()
            .map(|c| c.id.get())
            .max()
            .unwrap_or(0)
            + 1;
        self.state.connectors.push(new_connector(id));
        self.registry.set_connector_count(id);
        self.flush_now();
        let connector = ConnectorId::new(id);
        self.log(LogLevel::Info, format!("connector {id} added"));
        self.status_changed(connector);
        Outcome::ConnectorAdded { connector }
    }

    fn plug_in(&mut self, id: ConnectorId) -> Result<Outcome, StationError> {
        let connector = self.connector(id)?;
        if connector.ev_connected {
            return Err(invalid(id, "an EV is already connected"));
        }
        if connector.availability == AvailabilityType::Inoperative
            || matches!(
                connector.status,
                ConnectorStatus::Unavailable | ConnectorStatus::Faulted
            )
        {
            return Err(invalid(
                id,
                format!("cannot accept an EV while {}", connector.status),
            ));
        }
        if !connector.transaction.is_idle() {
            // stage 2b: plugging in during an offline-started transaction resumes charging.
            return Err(StationError::NotImplemented {
                command: "plug_in (during a transaction)",
            });
        }
        let status = if connector.reservation.is_some() {
            ConnectorStatus::Reserved
        } else {
            ConnectorStatus::Preparing
        };
        let connector = self.connector_or_station_mut(id)?;
        connector.ev_connected = true;
        connector.status = status;
        self.status_changed(id);
        Ok(Outcome::Done)
    }

    fn unplug(&mut self, id: ConnectorId) -> Result<Outcome, StationError> {
        let connector = self.connector(id)?;
        if !connector.transaction.is_idle() {
            // stage 2b: unplugging stops the transaction (reason EVDisconnected) or suspends it.
            return Err(StationError::NotImplemented {
                command: "unplug (during a transaction)",
            });
        }
        if !connector.ev_connected && connector.status != ConnectorStatus::Finishing {
            return Err(invalid(id, "no EV is connected"));
        }
        let status = status_after_unplug(connector);
        let connector = self.connector_or_station_mut(id)?;
        connector.ev_connected = false;
        connector.status = status;
        self.status_changed(id);
        Ok(Outcome::Done)
    }

    fn set_status(
        &mut self,
        id: ConnectorId,
        status: ConnectorStatus,
    ) -> Result<Outcome, StationError> {
        let vendor = self.config.vendor.clone();
        let connector = self.connector_or_station_mut(id)?;
        connector.status = status;
        if status == ConnectorStatus::Faulted {
            connector.error_code = ChargePointErrorCode::OtherError;
            connector.info = Some("Manual fault".to_owned());
            connector.vendor_id = Some(vendor);
        } else {
            connector.error_code = ChargePointErrorCode::NoError;
            connector.info = None;
            connector.vendor_id = None;
        }
        connector.vendor_error_code = None;
        self.status_changed(id);
        Ok(Outcome::Done)
    }

    fn fault(
        &mut self,
        id: ConnectorId,
        error_code: ChargePointErrorCode,
        info: Option<&str>,
        vendor_error_code: Option<&str>,
    ) -> Result<Outcome, StationError> {
        let current = self
            .find_connector(id)
            .ok_or(StationError::UnknownConnector(id))?;
        if !current.transaction.is_idle() {
            // stage 2b: a fault stops a running transaction with reason `Other` first.
            return Err(StationError::NotImplemented {
                command: "fault (during a transaction)",
            });
        }
        let vendor = self.config.vendor.clone();
        let connector = self.connector_or_station_mut(id)?;
        connector.status = ConnectorStatus::Faulted;
        connector.error_code = fault_error_code(error_code);
        connector.info = info.and_then(|text| trim_text(text, STATUS_TEXT_MAX));
        connector.vendor_id = Some(vendor);
        connector.vendor_error_code =
            vendor_error_code.and_then(|text| trim_text(text, STATUS_TEXT_MAX));
        self.status_changed(id);
        Ok(Outcome::Done)
    }

    fn clear_fault(&mut self, id: ConnectorId) -> Result<Outcome, StationError> {
        let connector = self
            .find_connector(id)
            .ok_or(StationError::UnknownConnector(id))?;
        let status = if id.is_station() {
            charge_point_status(connector.availability, ConnectorStatus::Available)
        } else {
            status_after_fault_clear(connector)
        };
        let connector = self.connector_or_station_mut(id)?;
        connector.status = status;
        connector.error_code = ChargePointErrorCode::NoError;
        connector.info = None;
        connector.vendor_id = None;
        connector.vendor_error_code = None;
        self.status_changed(id);
        Ok(Outcome::Done)
    }

    /// Current diagnostics status (for `TriggerMessage`).
    pub(super) fn diagnostics_status(&self) -> DiagnosticsStatus {
        self.state.diagnostics_status
    }

    /// Current firmware status (for `TriggerMessage`).
    pub(super) fn firmware_status(&self) -> FirmwareStatus {
        self.state.firmware_status
    }
}

fn invalid(connector: ConnectorId, reason: impl Into<String>) -> StationError {
    StationError::InvalidState {
        connector: Some(connector),
        reason: format!("connector {connector}: {}", reason.into()),
    }
}
