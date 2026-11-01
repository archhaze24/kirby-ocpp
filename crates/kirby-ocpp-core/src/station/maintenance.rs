//! Simulated diagnostics upload and firmware update.
//!
//! TS reference: `src/station/maintenance-lifecycle.ts`.
//!
//! Bounded retries (<= 5), delays <= 1 s, selectable outcome. Sequences:
//! Downloading -> Downloaded -> Installing -> Installed, plus the failure variants.
//! Timers are cancelled on unexpected disconnect (a TS bug). `TriggerMessage` for
//! Diagnostics/Firmware must report the real status, not always `Idle`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ocpp::types::{DiagnosticsStatus, FirmwareStatus};

/// Result the emulator should simulate for the next `GetDiagnostics`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticsOutcome {
    #[default]
    Success,
    UploadFailure,
}

impl DiagnosticsOutcome {
    pub const ALL: &'static [Self] = &[Self::Success, Self::UploadFailure];
}

/// Result the emulator should simulate for the next `UpdateFirmware`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FirmwareOutcome {
    #[default]
    Success,
    DownloadFailure,
    InstallationFailure,
}

impl FirmwareOutcome {
    pub const ALL: &'static [Self] = &[
        Self::Success,
        Self::DownloadFailure,
        Self::InstallationFailure,
    ];
}

/// Delay between two simulated steps.
pub const STEP_DELAY: Duration = Duration::from_millis(100);
/// Upper bound of any simulated delay (retry interval, wait for `retrieveDate`).
pub const MAX_DELAY: Duration = Duration::from_secs(1);
/// Upper bound of simulated retries.
pub const MAX_RETRIES: u32 = 5;

/// One simulated status report, `delay` after the previous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Diagnostics(DiagnosticsStatus),
    Firmware(FirmwareStatus),
}

fn repeat<T: Copy>(attempt: &[T], retries: u32) -> Vec<T> {
    (0..=retries.min(MAX_RETRIES))
        .flat_map(|_| attempt.iter().copied())
        .collect()
}

/// The status reports of a simulated diagnostics upload. A failing outcome repeats the
/// attempt `retries` times (at most [`MAX_RETRIES`]), `retry_interval` apart (at most 1 s).
pub fn plan_diagnostics(
    outcome: DiagnosticsOutcome,
    retries: u32,
    retry_interval: Duration,
) -> Vec<(Duration, Step)> {
    let (attempt, retries) = match outcome {
        DiagnosticsOutcome::Success => (
            vec![DiagnosticsStatus::Uploading, DiagnosticsStatus::Uploaded],
            0,
        ),
        DiagnosticsOutcome::UploadFailure => (
            vec![
                DiagnosticsStatus::Uploading,
                DiagnosticsStatus::UploadFailed,
            ],
            retries,
        ),
    };
    with_delays(
        &repeat(&attempt, retries)
            .into_iter()
            .map(Step::Diagnostics)
            .collect::<Vec<_>>(),
        attempt.len(),
        retry_interval,
    )
}

/// The status reports of a simulated firmware update (see [`plan_diagnostics`] for retries).
pub fn plan_firmware(
    outcome: FirmwareOutcome,
    retries: u32,
    retry_interval: Duration,
) -> Vec<(Duration, Step)> {
    use FirmwareStatus as F;
    let (attempt, retries): (Vec<F>, u32) = match outcome {
        FirmwareOutcome::Success => (
            vec![F::Downloading, F::Downloaded, F::Installing, F::Installed],
            0,
        ),
        FirmwareOutcome::DownloadFailure => (vec![F::Downloading, F::DownloadFailed], retries),
        FirmwareOutcome::InstallationFailure => (
            vec![
                F::Downloading,
                F::Downloaded,
                F::Installing,
                F::InstallationFailed,
            ],
            retries,
        ),
    };
    with_delays(
        &repeat(&attempt, retries)
            .into_iter()
            .map(Step::Firmware)
            .collect::<Vec<_>>(),
        attempt.len(),
        retry_interval,
    )
}

fn with_delays(
    steps: &[Step],
    attempt_len: usize,
    retry_interval: Duration,
) -> Vec<(Duration, Step)> {
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let delay = if index > 0 && index % attempt_len == 0 {
                retry_interval.min(MAX_DELAY)
            } else {
                STEP_DELAY
            };
            (delay, *step)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_plans_have_no_retries() {
        let plan = plan_diagnostics(DiagnosticsOutcome::Success, 3, Duration::ZERO);
        let steps: Vec<_> = plan.iter().map(|(_, s)| *s).collect();
        assert_eq!(
            steps,
            [
                Step::Diagnostics(DiagnosticsStatus::Uploading),
                Step::Diagnostics(DiagnosticsStatus::Uploaded)
            ]
        );
        assert_eq!(
            plan_firmware(FirmwareOutcome::Success, 0, Duration::ZERO).len(),
            4
        );
    }

    #[test]
    fn failures_retry_a_bounded_number_of_times_with_a_bounded_delay() {
        let plan = plan_firmware(
            FirmwareOutcome::DownloadFailure,
            99,
            Duration::from_secs(60),
        );
        assert_eq!(plan.len(), 2 * (MAX_RETRIES as usize + 1));
        assert_eq!(plan[2].0, MAX_DELAY, "retry interval is clamped");
        assert_eq!(plan[1].0, STEP_DELAY);
        assert_eq!(
            plan.last().unwrap().1,
            Step::Firmware(FirmwareStatus::DownloadFailed)
        );
        let none = plan_diagnostics(DiagnosticsOutcome::UploadFailure, 0, Duration::ZERO);
        assert_eq!(none.len(), 2);
    }

    #[test]
    fn installation_failure_ends_with_installation_failed() {
        let plan = plan_firmware(
            FirmwareOutcome::InstallationFailure,
            1,
            Duration::from_millis(10),
        );
        assert_eq!(plan.len(), 8);
        assert_eq!(plan[4].0, Duration::from_millis(10));
        assert_eq!(
            plan[7].1,
            Step::Firmware(FirmwareStatus::InstallationFailed)
        );
    }

    #[test]
    fn outcomes_serialize_in_the_ts_spelling() {
        assert_eq!(
            serde_json::to_string(&DiagnosticsOutcome::UploadFailure).unwrap(),
            "\"uploadFailure\""
        );
        assert_eq!(
            serde_json::to_string(&FirmwareOutcome::InstallationFailure).unwrap(),
            "\"installationFailure\""
        );
        assert_eq!(FirmwareOutcome::default(), FirmwareOutcome::Success);
    }
}
