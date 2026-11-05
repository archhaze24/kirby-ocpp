//! `GetDiagnostics`, `UpdateFirmware` and their simulated lifecycles (plans: `station::maintenance`).
//!
//! The reply comes first; the status reports follow from `TimerKey::Diagnostics` /
//! `TimerKey::Firmware` (each kind has its own queue and timer, so a firmware update and a
//! diagnostics upload run side by side), one step per tick, with delays of at most one second. The selected outcome (`SetDiagnosticsOutcome` /
//! `SetFirmwareOutcome`) decides success or failure, failures repeat up to `retries` times
//! (at most 5). An unexpected disconnect, a `Disconnect` and a `Reset` drop the remaining steps.

use std::time::Duration;

use crate::ocpp::messages::{
    DiagnosticsStatusNotification, DiagnosticsStatusNotificationRequest,
    FirmwareStatusNotification, FirmwareStatusNotificationRequest, GetDiagnostics,
    GetDiagnosticsRequest, GetDiagnosticsResponse, GetLog, GetLogRequest, GetLogResponse,
    LogStatus, LogStatusNotification, LogStatusNotificationRequest, LogType, SignedFirmwareStatus,
    SignedFirmwareStatusNotification, SignedFirmwareStatusNotificationRequest,
    SignedUpdateFirmware, SignedUpdateFirmwareRequest, SignedUpdateFirmwareResponse,
    UpdateFirmware, UpdateFirmwareRequest, UpdateFirmwareResponse, UpdateFirmwareStatus,
    UploadLogStatus,
};
use crate::ocpp::responder::Responder;
use crate::ocpp::types::{DiagnosticsStatus, FirmwareStatus};
use crate::station::actor::{StationActor, TimerKey};
use crate::station::calls::Origin;
use crate::station::certificates::parse_pem_chain;
use crate::station::event::LogLevel;
use crate::station::maintenance::{
    MAX_DELAY, Step, plan_diagnostics, plan_firmware, plan_log, plan_signed, valid_log_location,
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
    actor.run_diagnostics_plan(plan);
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
    actor.last_firmware_signed = false;
    let plan = plan_firmware(
        actor.state.firmware_outcome,
        non_negative(request.retries),
        Duration::from_secs(u64::from(non_negative(request.retry_interval))),
    );
    let wait = (request.retrieve_date - actor.clock.now_utc())
        .to_std()
        .unwrap_or(Duration::ZERO)
        .min(MAX_DELAY);
    actor.run_firmware_plan(plan, wait);
}

/// Checks the signing certificate of a `SignedUpdateFirmware`: it must parse, be valid now and,
/// when a manufacturer root is installed, chain to one (issuer name match, no signature check).
fn check_signing_certificate(actor: &StationActor, pem: &str) -> Result<(), String> {
    let chain = parse_pem_chain(pem)?;
    let now = actor.clock.now_utc();
    if let Some(bad) = chain.iter().find(|cert| !cert.valid_at(now)) {
        return Err(format!("{} is expired or not yet valid", bad.subject));
    }
    if actor.certs.chains_to_manufacturer_root(&chain) == Some(false) {
        return Err("does not chain to an installed manufacturer root".to_owned());
    }
    Ok(())
}

/// `SignedUpdateFirmware` (Security Whitepaper): validation, then a simulated update that reports
/// `SignedFirmwareStatusNotification` with the `requestId` (plans: `plan_signed`; the outcome
/// comes from `SetFirmwareOutcome`). The signature is never verified for real. A request while
/// another signed update runs is `AcceptedCanceled`; the cancelled update sends no further status
/// (the enum has no cancelled value). No `RevokedCertificate` (no revocation data).
pub(super) fn signed_update_firmware(
    actor: &mut StationActor,
    responder: Responder<SignedUpdateFirmware>,
    request: SignedUpdateFirmwareRequest,
) {
    let firmware = &request.firmware;
    let refusal = if !valid_log_location(&firmware.location) {
        actor.log(
            LogLevel::Warn,
            "SignedUpdateFirmware rejected: location is not an ftp/ftps/http/https/sftp URL"
                .to_owned(),
        );
        Some(UpdateFirmwareStatus::Rejected)
    } else if let Err(reason) = check_signing_certificate(actor, &firmware.signing_certificate) {
        actor.log(
            LogLevel::Warn,
            format!("SignedUpdateFirmware: invalid signing certificate: {reason}"),
        );
        actor.security_event("InvalidFirmwareSigningCertificate", Some(&reason));
        Some(UpdateFirmwareStatus::InvalidCertificate)
    } else {
        None
    };
    if let Some(status) = refusal {
        actor.respond(responder, SignedUpdateFirmwareResponse { status });
        return;
    }
    let canceled = !actor.signed_fw.is_empty();
    actor.cancel_signed_firmware();
    actor.respond(
        responder,
        SignedUpdateFirmwareResponse {
            status: if canceled {
                UpdateFirmwareStatus::AcceptedCanceled
            } else {
                UpdateFirmwareStatus::Accepted
            },
        },
    );
    let now = actor.clock.now_utc();
    let until =
        |at: chrono::DateTime<chrono::Utc>| (at - now).to_std().ok().filter(|d| !d.is_zero());
    let retrieve_wait = until(firmware.retrieve_date_time);
    let install_wait = firmware.install_date_time.and_then(until);
    let plan = plan_signed(
        actor.state.firmware_outcome,
        non_negative(request.retries),
        Duration::from_secs(u64::from(non_negative(request.retry_interval))),
        retrieve_wait,
        install_wait,
    );
    actor.last_firmware_signed = true;
    actor.signed_fw_request_id = Some(request.request_id);
    actor.state.signed_firmware_request_id = Some(request.request_id);
    actor.state.signed_firmware_status = SignedFirmwareStatus::Idle;
    actor.signed_fw = plan.into();
    if let Some((delay, _)) = actor.signed_fw.front().copied() {
        actor.timers.set(TimerKey::SignedFirmware, delay);
    }
}

impl StationActor {
    /// Drops the remaining signed-update statuses (disconnect, reset, a newer request).
    pub(in crate::station) fn cancel_signed_firmware(&mut self) {
        self.timers.cancel(&TimerKey::SignedFirmware);
        if !std::mem::take(&mut self.signed_fw).is_empty() {
            self.state.signed_firmware_status = SignedFirmwareStatus::Idle;
        }
    }

    pub(in crate::station) fn on_signed_firmware_timer(&mut self) {
        let Some((_, status)) = self.signed_fw.pop_front() else {
            return;
        };
        self.report_signed_firmware(status);
        match status {
            SignedFirmwareStatus::InvalidSignature => self.security_event(
                "InvalidFirmwareSignature",
                self.signed_fw_request_id
                    .map(|id| format!("requestId {id}"))
                    .as_deref(),
            ),
            SignedFirmwareStatus::Installed => self.security_event("FirmwareUpdated", None),
            _ => {}
        }
        if let Some((delay, _)) = self.signed_fw.front().copied() {
            self.timers.set(TimerKey::SignedFirmware, delay);
        }
    }

    /// Sends one `SignedFirmwareStatusNotification` and remembers it as the current status.
    pub(in crate::station) fn report_signed_firmware(&mut self, status: SignedFirmwareStatus) {
        self.state.signed_firmware_status = status;
        let request_id = if status == SignedFirmwareStatus::Idle {
            None
        } else {
            self.signed_fw_request_id
        };
        self.call::<SignedFirmwareStatusNotification>(
            Origin::Station,
            &SignedFirmwareStatusNotificationRequest { status, request_id },
            |_, _| {},
        );
    }

    fn run_diagnostics_plan(&mut self, plan: Vec<(Duration, Step)>) {
        self.diagnostics_plan = plan.into();
        if let Some((delay, _)) = self.diagnostics_plan.front().copied() {
            self.timers.set(TimerKey::Diagnostics, delay);
        }
    }

    fn run_firmware_plan(&mut self, plan: Vec<(Duration, Step)>, first_delay: Duration) {
        self.firmware_plan = plan.into();
        if self.firmware_plan.front().is_some() {
            self.timers.set(TimerKey::Firmware, first_delay);
        }
    }

    /// Drops the remaining `GetDiagnostics` and `UpdateFirmware` statuses (disconnect, reset).
    /// A status that was only an intermediate step of the cancelled plan goes back to `Idle`,
    /// so `TriggerMessage` does not report an operation that no longer runs.
    pub(in crate::station) fn cancel_maintenance(&mut self) {
        self.timers.cancel(&TimerKey::Diagnostics);
        self.timers.cancel(&TimerKey::Firmware);
        if !std::mem::take(&mut self.diagnostics_plan).is_empty() {
            self.state.diagnostics_status = DiagnosticsStatus::Idle;
        }
        if !std::mem::take(&mut self.firmware_plan).is_empty() {
            self.state.firmware_status = FirmwareStatus::Idle;
        }
    }

    pub(in crate::station) fn on_diagnostics_timer(&mut self) {
        let Some((_, step)) = self.diagnostics_plan.pop_front() else {
            return;
        };
        if let Step::Diagnostics(status) = step {
            self.report_diagnostics(status);
        }
        if let Some((delay, _)) = self.diagnostics_plan.front().copied() {
            self.timers.set(TimerKey::Diagnostics, delay);
        }
    }

    pub(in crate::station) fn on_firmware_timer(&mut self) {
        let Some((_, step)) = self.firmware_plan.pop_front() else {
            return;
        };
        if let Step::Firmware(status) = step {
            self.report_firmware(status);
        }
        if let Some((delay, _)) = self.firmware_plan.front().copied() {
            self.timers.set(TimerKey::Firmware, delay);
        }
    }

    /// Drops the remaining `GetLog` statuses (disconnect, reset, a newer `GetLog`).
    pub(in crate::station) fn cancel_log_upload(&mut self) {
        self.timers.cancel(&TimerKey::LogUpload);
        if !std::mem::take(&mut self.log_upload).is_empty() {
            self.state.log_status = UploadLogStatus::Idle;
        }
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
