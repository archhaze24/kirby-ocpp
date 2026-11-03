//! `GetDiagnostics`, `UpdateFirmware` and their simulated lifecycles (plans: `station::maintenance`).
//!
//! The reply comes first; the status reports follow from `TimerKey::Maintenance`, one step per
//! tick, with delays of at most one second. The selected outcome (`SetDiagnosticsOutcome` /
//! `SetFirmwareOutcome`) decides success or failure, failures repeat up to `retries` times
//! (at most 5). An unexpected disconnect, a `Disconnect` and a `Reset` drop the remaining steps.

use std::time::Duration;

use crate::ocpp::messages::{
    DiagnosticsStatusNotification, DiagnosticsStatusNotificationRequest,
    FirmwareStatusNotification, FirmwareStatusNotificationRequest, GetDiagnostics,
    GetDiagnosticsRequest, GetDiagnosticsResponse, GetLog, GetLogRequest, GetLogResponse,
    LogStatus, LogStatusNotification, LogStatusNotificationRequest, LogType, UpdateFirmware,
    UpdateFirmwareRequest, UpdateFirmwareResponse, UploadLogStatus,
};
use crate::ocpp::responder::Responder;
use crate::ocpp::types::{DiagnosticsStatus, FirmwareStatus};
use crate::station::actor::{StationActor, TimerKey};
use crate::station::calls::Origin;
use crate::station::event::LogLevel;
use crate::station::maintenance::{
    MAX_DELAY, Step, plan_diagnostics, plan_firmware, plan_log, valid_log_location,
};

fn non_negative(value: Option<i32>) -> u32 {
    value.and_then(|v| u32::try_from(v).ok()).unwrap_or(0)
}

pub(super) fn get_diagnostics(
    actor: &mut StationActor,
    responder: Responder<GetDiagnostics>,
    request: GetDiagnosticsRequest,
) {
    let stamp = actor.clock.now_utc().format("%Y%m%dT%H%M%SZ");
    let file_name = format!("diagnostics-{}-{stamp}.log", actor.config.charge_point_id);
    actor.respond(
        responder,
        GetDiagnosticsResponse {
            file_name: Some(file_name),
        },
    );
    let plan = plan_diagnostics(
        actor.state.diagnostics_outcome,
        non_negative(request.retries),
        Duration::from_secs(u64::from(non_negative(request.retry_interval))),
    );
    actor.run_plan(plan, None);
}

/// `GetLog`: the reply comes first, then a simulated upload (`Uploading`, then `Uploaded` or
/// `UploadFailure` per the diagnostics outcome knob). A new request while an upload runs cancels
/// it (`AcceptedCanceled`); the cancelled upload gets no further status, because
/// `UploadLogStatusEnumType` has no cancellation value. An unsupported `remoteLocation` is
/// `Rejected`.
pub(super) fn get_log(
    actor: &mut StationActor,
    responder: Responder<GetLog>,
    request: GetLogRequest,
) {
    if !valid_log_location(&request.log.remote_location) {
        actor.log(
            LogLevel::Warn,
            format!(
                "GetLog {} rejected: remoteLocation is not an ftp/ftps/http/https/sftp URL",
                request.request_id
            ),
        );
        actor.respond(
            responder,
            GetLogResponse {
                status: LogStatus::Rejected,
                filename: None,
            },
        );
        return;
    }
    let canceled = !actor.log_upload.is_empty();
    actor.cancel_log_upload();
    let stamp = actor.clock.now_utc().format("%Y%m%dT%H%M%SZ");
    let prefix = match request.log_type {
        LogType::DiagnosticsLog => "diagnostics-log",
        LogType::SecurityLog => "security-log",
    };
    let filename = format!("{prefix}-{}-{stamp}.log", actor.config.charge_point_id);
    actor.respond(
        responder,
        GetLogResponse {
            status: if canceled {
                LogStatus::AcceptedCanceled
            } else {
                LogStatus::Accepted
            },
            filename: Some(filename),
        },
    );
    actor.state.log_request_id = Some(request.request_id);
    actor.state.log_status = UploadLogStatus::Idle;
    actor.log_request_id = Some(request.request_id);
    let plan = plan_log(
        actor.state.diagnostics_outcome,
        non_negative(request.retries),
        Duration::from_secs(u64::from(non_negative(request.retry_interval))),
    );
    actor.log_upload = plan.into();
    if let Some((delay, _)) = actor.log_upload.front().copied() {
        actor.timers.set(TimerKey::LogUpload, delay);
    }
}

pub(super) fn update_firmware(
    actor: &mut StationActor,
    responder: Responder<UpdateFirmware>,
    request: UpdateFirmwareRequest,
) {
    actor.respond(responder, UpdateFirmwareResponse {});
    let plan = plan_firmware(
        actor.state.firmware_outcome,
        non_negative(request.retries),
        Duration::from_secs(u64::from(non_negative(request.retry_interval))),
    );
    let wait = (request.retrieve_date - actor.clock.now_utc())
        .to_std()
        .unwrap_or(Duration::ZERO)
        .min(MAX_DELAY);
    actor.run_plan(plan, Some(wait));
}

impl StationActor {
    fn run_plan(&mut self, plan: Vec<(Duration, Step)>, first_delay: Option<Duration>) {
        self.maintenance = plan.into();
        if let Some((delay, _)) = self.maintenance.front().copied() {
            self.timers
                .set(TimerKey::Maintenance, first_delay.unwrap_or(delay));
        }
    }

    pub(in crate::station) fn on_maintenance_timer(&mut self) {
        let Some((_, step)) = self.maintenance.pop_front() else {
            return;
        };
        match step {
            Step::Diagnostics(status) => self.report_diagnostics(status),
            Step::Firmware(status) => self.report_firmware(status),
        }
        if let Some((delay, _)) = self.maintenance.front().copied() {
            self.timers.set(TimerKey::Maintenance, delay);
        }
    }

    /// Drops the remaining `GetLog` statuses (disconnect, reset, a newer `GetLog`).
    pub(in crate::station) fn cancel_log_upload(&mut self) {
        self.timers.cancel(&TimerKey::LogUpload);
        self.log_upload.clear();
    }

    pub(in crate::station) fn on_log_timer(&mut self) {
        let Some((_, status)) = self.log_upload.pop_front() else {
            return;
        };
        self.report_log(status);
        if let Some((delay, _)) = self.log_upload.front().copied() {
            self.timers.set(TimerKey::LogUpload, delay);
        }
    }

    /// Sends one `LogStatusNotification` and remembers it as the current status.
    pub(in crate::station) fn report_log(&mut self, status: UploadLogStatus) {
        self.state.log_status = status;
        let request_id = if status == UploadLogStatus::Idle {
            None
        } else {
            self.log_request_id
        };
        self.call::<LogStatusNotification>(
            Origin::Station,
            &LogStatusNotificationRequest { status, request_id },
            |_, _| {},
        );
    }

    fn report_diagnostics(&mut self, status: DiagnosticsStatus) {
        self.state.diagnostics_status = status;
        self.call::<DiagnosticsStatusNotification>(
            Origin::Station,
            &DiagnosticsStatusNotificationRequest { status },
            |_, _| {},
        );
    }

    fn report_firmware(&mut self, status: FirmwareStatus) {
        self.state.firmware_status = status;
        self.call::<FirmwareStatusNotification>(
            Origin::Station,
            &FirmwareStatusNotificationRequest { status },
            |_, _| {},
        );
    }
}
