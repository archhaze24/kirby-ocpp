//! Simulated diagnostics upload and firmware update.
//!
//! TS reference: `src/station/maintenance-lifecycle.ts`.
//!
//! Bounded retries (<= 5), delays <= 1 s, selectable outcome. Sequences:
//! Downloading -> Downloaded -> Installing -> Installed, plus the failure variants.
//! Timers are cancelled on unexpected disconnect (a TS bug). `TriggerMessage` for
//! Diagnostics/Firmware must report the real status, not always `Idle`.

use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;

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
