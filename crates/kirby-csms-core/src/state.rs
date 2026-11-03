//! Per-station protocol state, derived from the CALLs the engine answers.
//!
//! The engine does not interpret anything the station says beyond this read model: it
//! deserialises the CALLs it answered with a result into the typed messages of
//! `kirby-ocpp-core` and folds them into a [`StationState`]. A payload that does not
//! deserialise is answered as before but leaves the state untouched. CALLs answered with a
//! CALLERROR (a fault or an unsupported action) are not tracked either.
//!
//! The state is plain data: cheap to clone (bounded, see [`CsmsConfig::transaction_history_cap`]),
//! `Serialize` for JSON output and `PartialEq` only (meter readings are `f64`).
//!
//! [`CsmsConfig::transaction_history_cap`]: crate::CsmsConfig::transaction_history_cap

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use kirby_ocpp_core::ocpp::messages::{
    BootNotificationRequest, BootNotificationResponse, DiagnosticsStatusNotificationRequest,
    FirmwareStatusNotificationRequest, MeterValuesRequest, StartTransactionRequest,
    StartTransactionResponse, StatusNotificationRequest, StopTransactionRequest,
};
use kirby_ocpp_core::ocpp::types::{
    AuthorizationStatus, ChargePointErrorCode, ConnectorStatus, DiagnosticsStatus, FirmwareStatus,
    Measurand, RegistrationStatus, StopReason, UnitOfMeasure,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Most connector ids (including 0) tracked per station; reports for further ids are ignored.
pub const MAX_CONNECTORS: usize = 1024;
/// Most simultaneously active transactions tracked per station; beyond it the oldest active
/// one is dropped (a station that starts transactions and never stops them).
pub const MAX_ACTIVE_TRANSACTIONS: usize = 256;

/// What the engine knows about one station's protocol state. See the module docs.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StationState {
    /// The last `BootNotification` and our answer; `None` before the first boot.
    pub boot: Option<BootInfo>,
    /// Connector id (0 is the charge point itself) to its last `StatusNotification`.
    pub connectors: BTreeMap<u32, ConnectorState>,
    /// Transactions, oldest first: every active one plus at most
    /// `transaction_history_cap` stopped ones.
    pub transactions: Vec<TransactionRecord>,
    /// When we last answered a `Heartbeat` (our clock).
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    /// Last `DiagnosticsStatusNotification`.
    pub diagnostics_status: Option<Reported<DiagnosticsStatus>>,
    /// Last `FirmwareStatusNotification`.
    pub firmware_status: Option<Reported<FirmwareStatus>>,
    /// CSMS-originated calls to this station that ended in a CALLERROR or a timeout.
    pub failed_calls: u64,
    /// `StopTransaction`s for a transaction id that has no active transaction here. They
    /// are counted, not recorded.
    pub unknown_stops: u64,
}

/// A status the station reported, with the time we received it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reported<T> {
    pub status: T,
    /// Our clock.
    pub at: DateTime<Utc>,
}

/// `BootNotification` fields as sent, plus our answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BootInfo {
    pub vendor: String,
    pub model: String,
    pub charge_point_serial_number: Option<String>,
    pub charge_box_serial_number: Option<String>,
    pub firmware_version: Option<String>,
    pub iccid: Option<String>,
    pub imsi: Option<String>,
    pub meter_type: Option<String>,
    pub meter_serial_number: Option<String>,
    /// When the boot arrived (our clock).
    pub booted_at: DateTime<Utc>,
    /// The registration status we answered (`None` if the answer was not a valid
    /// `BootNotification.conf`, e.g. a custom responder).
    pub registration: Option<RegistrationStatus>,
    /// The `interval` we answered, seconds.
    pub interval: Option<i32>,
}

/// One connector's last `StatusNotification`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConnectorState {
    pub status: ConnectorStatus,
    pub error_code: ChargePointErrorCode,
    pub info: Option<String>,
    pub vendor_id: Option<String>,
    pub vendor_error_code: Option<String>,
    /// The timestamp the station put in the message, if any.
    pub timestamp: Option<DateTime<Utc>>,
    /// When we received it (our clock).
    pub received_at: DateTime<Utc>,
}

/// Whether a transaction is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionStatus {
    Active,
    Stopped,
}

/// One transaction, from `StartTransaction` to `StopTransaction`.
///
/// Ids come from the engine's allocator. With the default `Fixed` policy several
/// transactions share an id; a stop or a meter reading then applies to the most recent
/// active one with that id.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TransactionRecord {
    /// The id we answered in `StartTransaction.conf`.
    pub id: i32,
    pub connector: u32,
    pub id_tag: String,
    /// The `idTagInfo.status` we answered. A transaction stays `Active` even if it is not
    /// `Accepted`, until the station stops it.
    pub id_tag_status: AuthorizationStatus,
    pub meter_start: i64,
    /// The station's timestamp.
    pub started_at: DateTime<Utc>,
    pub status: TransactionStatus,
    pub meter_stop: Option<i64>,
    /// The station's timestamp.
    pub stopped_at: Option<DateTime<Utc>>,
    /// `None` while active, or when the station sent no reason.
    pub stop_reason: Option<StopReason>,
    /// Latest `Energy.Active.Import.Register` from `MeterValues`, Wh.
    pub energy_wh: Option<f64>,
    /// Latest `Power.Active.Import` from `MeterValues`, W.
    pub power_w: Option<f64>,
    /// The station's timestamp of the latest `MeterValues` entry that updated this record.
    pub last_meter_at: Option<DateTime<Utc>>,
}

impl StationState {
    /// Folds one answered CALL into the state. `response` is the CALLRESULT payload we sent.
    /// Returns whether the state changed.
    pub(crate) fn apply(
        &mut self,
        action: &str,
        request: &Value,
        response: &Value,
        now: DateTime<Utc>,
        history_cap: usize,
    ) -> bool {
        match action {
            "BootNotification" => self.on_boot(request, response, now),
            "Heartbeat" => {
                // Recorded silently: a heartbeat is no state change worth an event (the
                // frame event already tells subscribers), and they come constantly.
                self.last_heartbeat_at = Some(now);
                None
            }
            "StatusNotification" => self.on_status(request, now),
            "StartTransaction" => self.on_start(request, response),
            "StopTransaction" => self.on_stop(request, history_cap),
            "MeterValues" => self.on_meter(request),
            "DiagnosticsStatusNotification" => {
                DiagnosticsStatusNotificationRequest::deserialize(request)
                    .map(|req| {
                        self.diagnostics_status = Some(Reported {
                            status: req.status,
                            at: now,
                        });
                    })
                    .ok()
            }
            "FirmwareStatusNotification" => FirmwareStatusNotificationRequest::deserialize(request)
                .map(|req| {
                    self.firmware_status = Some(Reported {
                        status: req.status,
                        at: now,
                    });
                })
                .ok(),
            _ => None,
        }
        .is_some()
    }

    fn on_boot(&mut self, request: &Value, response: &Value, now: DateTime<Utc>) -> Option<()> {
        let req = BootNotificationRequest::deserialize(request).ok()?;
        let res = BootNotificationResponse::deserialize(response).ok();
        self.boot = Some(BootInfo {
            vendor: req.charge_point_vendor,
            model: req.charge_point_model,
            charge_point_serial_number: req.charge_point_serial_number,
            charge_box_serial_number: req.charge_box_serial_number,
            firmware_version: req.firmware_version,
            iccid: req.iccid,
            imsi: req.imsi,
            meter_type: req.meter_type,
            meter_serial_number: req.meter_serial_number,
            booted_at: now,
            registration: res.as_ref().map(|res| res.status),
            interval: res.map(|res| res.interval),
        });
        Some(())
    }

    fn on_status(&mut self, request: &Value, now: DateTime<Utc>) -> Option<()> {
        let req = StatusNotificationRequest::deserialize(request).ok()?;
        let id = req.connector_id.get();
        if !self.connectors.contains_key(&id) && self.connectors.len() >= MAX_CONNECTORS {
            return None;
        }
        self.connectors.insert(
            id,
            ConnectorState {
                status: req.status,
                error_code: req.error_code,
                info: req.info,
                vendor_id: req.vendor_id,
                vendor_error_code: req.vendor_error_code,
                timestamp: req.timestamp,
                received_at: now,
            },
        );
        Some(())
    }

    fn on_start(&mut self, request: &Value, response: &Value) -> Option<()> {
        let req = StartTransactionRequest::deserialize(request).ok()?;
        let res = StartTransactionResponse::deserialize(response).ok()?;
        self.transactions.push(TransactionRecord {
            id: res.transaction_id,
            connector: req.connector_id.get(),
            id_tag: req.id_tag.to_string(),
            id_tag_status: res.id_tag_info.status,
            meter_start: req.meter_start,
            started_at: req.timestamp,
            status: TransactionStatus::Active,
            meter_stop: None,
            stopped_at: None,
            stop_reason: None,
            energy_wh: None,
            power_w: None,
            last_meter_at: None,
        });
        let active = self
            .transactions
            .iter()
            .filter(|tx| tx.status == TransactionStatus::Active)
            .count();
        if active > MAX_ACTIVE_TRANSACTIONS
            && let Some(oldest) = self
                .transactions
                .iter()
                .position(|tx| tx.status == TransactionStatus::Active)
        {
            self.transactions.remove(oldest);
        }
        Some(())
    }

    fn on_stop(&mut self, request: &Value, history_cap: usize) -> Option<()> {
        let req = StopTransactionRequest::deserialize(request).ok()?;
        let Some(tx) = self.active_mut(req.transaction_id) else {
            self.unknown_stops += 1;
            return Some(());
        };
        tx.status = TransactionStatus::Stopped;
        tx.meter_stop = Some(req.meter_stop);
        tx.stopped_at = Some(req.timestamp);
        tx.stop_reason = req.reason;
        let mut stopped = self
            .transactions
            .iter()
            .filter(|tx| tx.status == TransactionStatus::Stopped)
            .count();
        while stopped > history_cap {
            let Some(oldest) = self
                .transactions
                .iter()
                .position(|tx| tx.status == TransactionStatus::Stopped)
            else {
                break;
            };
            self.transactions.remove(oldest);
            stopped -= 1;
        }
        Some(())
    }

    /// `MeterValues` update the active transaction they name; readings without a
    /// transaction id, or for an unknown one, are not tracked.
    fn on_meter(&mut self, request: &Value) -> Option<()> {
        let req = MeterValuesRequest::deserialize(request).ok()?;
        let tx = self.active_mut(req.transaction_id?)?;
        let mut changed = false;
        for meter in &req.meter_value {
            let mut touched = false;
            for sample in meter.sampled_value.iter().filter(|s| s.phase.is_none()) {
                let Some(value) = sample_value(sample) else {
                    continue;
                };
                match sample
                    .measurand
                    .unwrap_or(Measurand::EnergyActiveImportRegister)
                {
                    Measurand::EnergyActiveImportRegister => {
                        let wh = match sample.unit.unwrap_or(UnitOfMeasure::Wh) {
                            UnitOfMeasure::Wh => value,
                            UnitOfMeasure::KWh => value * 1000.0,
                            _ => continue,
                        };
                        tx.energy_wh = Some(wh);
                        touched = true;
                    }
                    Measurand::PowerActiveImport => {
                        let w = match sample.unit.unwrap_or(UnitOfMeasure::W) {
                            UnitOfMeasure::W => value,
                            UnitOfMeasure::KW => value * 1000.0,
                            _ => continue,
                        };
                        tx.power_w = Some(w);
                        touched = true;
                    }
                    _ => {}
                }
            }
            if touched {
                tx.last_meter_at = Some(meter.timestamp);
                changed = true;
            }
        }
        changed.then_some(())
    }

    fn active_mut(&mut self, id: i32) -> Option<&mut TransactionRecord> {
        self.transactions
            .iter_mut()
            .rev()
            .find(|tx| tx.id == id && tx.status == TransactionStatus::Active)
    }
}

/// A raw numeric sample as `f64`; signed data and non-numbers are skipped.
fn sample_value(sample: &kirby_ocpp_core::ocpp::messages::SampledValue) -> Option<f64> {
    if sample.format == Some(kirby_ocpp_core::ocpp::types::ValueFormat::SignedData) {
        return None;
    }
    sample
        .value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn start(state: &mut StationState, id: i32) {
        let req = json!({"connectorId": 1, "idTag": "T", "meterStart": 10,
            "timestamp": "2026-01-01T00:00:00.000Z"});
        let res = json!({"transactionId": id, "idTagInfo": {"status": "Accepted"}});
        assert!(state.apply("StartTransaction", &req, &res, now(), 2));
    }

    fn stop(state: &mut StationState, id: i32) {
        let req = json!({"transactionId": id, "meterStop": 99,
            "timestamp": "2026-01-01T01:00:00.000Z", "reason": "Remote"});
        assert!(state.apply("StopTransaction", &req, &json!({}), now(), 2));
    }

    #[test]
    fn history_is_capped_and_active_kept() {
        let mut state = StationState::default();
        start(&mut state, 1);
        for id in 2..=5 {
            start(&mut state, id);
            stop(&mut state, id);
        }
        let ids: Vec<i32> = state.transactions.iter().map(|tx| tx.id).collect();
        assert_eq!(ids, [1, 4, 5]);
        assert_eq!(state.transactions[0].status, TransactionStatus::Active);
    }

    #[test]
    fn unknown_stop_is_counted_not_recorded() {
        let mut state = StationState::default();
        stop(&mut state, 7);
        assert_eq!(state.transactions.len(), 0);
        assert_eq!(state.unknown_stops, 1);
    }

    #[test]
    fn meter_values_convert_units_and_skip_junk() {
        let mut state = StationState::default();
        start(&mut state, 3);
        let req = json!({"connectorId": 1, "transactionId": 3, "meterValue": [{
        "timestamp": "2026-01-01T00:10:00.000Z",
        "sampledValue": [
            {"value": "1.5", "unit": "kWh"},
            {"value": "7000", "measurand": "Power.Active.Import", "unit": "W"},
            {"value": "NaN", "measurand": "Power.Active.Import"},
            {"value": "5", "measurand": "Power.Active.Import", "phase": "L1"}
        ]}]});
        assert!(state.apply("MeterValues", &req, &json!({}), now(), 2));
        let tx = &state.transactions[0];
        assert_eq!(tx.energy_wh, Some(1500.0));
        assert_eq!(tx.power_w, Some(7000.0));
        // No transaction id, or an unknown one: nothing changes.
        let other = json!({"connectorId": 1, "transactionId": 4, "meterValue": [{
            "timestamp": "2026-01-01T00:10:00.000Z", "sampledValue": [{"value": "1"}]}]});
        assert!(!state.apply("MeterValues", &other, &json!({}), now(), 2));
    }

    #[test]
    fn invalid_payload_changes_nothing() {
        let mut state = StationState::default();
        assert!(!state.apply("StatusNotification", &json!({"x": 1}), &json!({}), now(), 2));
        assert_eq!(state, StationState::default());
    }
}
