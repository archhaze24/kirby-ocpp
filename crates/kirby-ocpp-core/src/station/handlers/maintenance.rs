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
    GetDiagnosticsRequest, GetDiagnosticsResponse, UpdateFirmware, UpdateFirmwareRequest,
    UpdateFirmwareResponse,
};
use crate::ocpp::responder::Responder;
use crate::ocpp::types::{DiagnosticsStatus, FirmwareStatus};
use crate::station::actor::{StationActor, TimerKey};
use crate::station::calls::Origin;
use crate::station::maintenance::{MAX_DELAY, Step, plan_diagnostics, plan_firmware};

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
