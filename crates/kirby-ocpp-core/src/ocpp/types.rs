//! Shared OCPP 1.6 vocabulary enums.
//!
//! Every enum serializes to / parses from the exact OCPP wire string and has
//! `ALL`, `as_str`, `Display` and `FromStr`, so UIs can build pick-lists and
//! the CLI can parse flags without a second source of truth. They are used both
//! by the typed messages in [`crate::ocpp::messages`] and by the station state.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// A string that is not a valid value of the named enum.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("`{value}` is not a valid {type_name}")]
pub struct ParseEnumError {
    pub type_name: &'static str,
    pub value: String,
}

macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $($variant:ident => $wire:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            /// Every variant, in OCPP specification order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The exact OCPP wire string.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ParseEnumError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($wire => Ok(Self::$variant),)+
                    _ => Err(ParseEnumError {
                        type_name: stringify!($name),
                        value: s.to_owned(),
                    }),
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

wire_enum! {
    /// `ChargePointStatus` (called `ConnectorStatus` here, as in the TS code).
    ConnectorStatus {
        Available => "Available",
        Preparing => "Preparing",
        Charging => "Charging",
        SuspendedEVSE => "SuspendedEVSE",
        SuspendedEV => "SuspendedEV",
        Finishing => "Finishing",
        Reserved => "Reserved",
        Unavailable => "Unavailable",
        Faulted => "Faulted",
    }
}

wire_enum! {
    /// `ChargePointErrorCode`.
    ChargePointErrorCode {
        ConnectorLockFailure => "ConnectorLockFailure",
        EVCommunicationError => "EVCommunicationError",
        GroundFailure => "GroundFailure",
        HighTemperature => "HighTemperature",
        InternalError => "InternalError",
        LocalListConflict => "LocalListConflict",
        NoError => "NoError",
        OtherError => "OtherError",
        OverCurrentFailure => "OverCurrentFailure",
        PowerMeterFailure => "PowerMeterFailure",
        PowerSwitchFailure => "PowerSwitchFailure",
        ReaderFailure => "ReaderFailure",
        ResetFailure => "ResetFailure",
        UnderVoltage => "UnderVoltage",
        OverVoltage => "OverVoltage",
        WeakSignal => "WeakSignal",
    }
}

wire_enum! {
    /// `AvailabilityType`.
    AvailabilityType {
        Operative => "Operative",
        Inoperative => "Inoperative",
    }
}

wire_enum! {
    /// `RegistrationStatus` of `BootNotification.conf`.
    RegistrationStatus {
        Accepted => "Accepted",
        Pending => "Pending",
        Rejected => "Rejected",
    }
}

wire_enum! {
    /// `AuthorizationStatus` (the `status` of an `IdTagInfo`).
    AuthorizationStatus {
        Accepted => "Accepted",
        Blocked => "Blocked",
        Expired => "Expired",
        Invalid => "Invalid",
        ConcurrentTx => "ConcurrentTx",
    }
}

wire_enum! {
    /// `DiagnosticsStatus`.
    DiagnosticsStatus {
        Idle => "Idle",
        Uploaded => "Uploaded",
        UploadFailed => "UploadFailed",
        Uploading => "Uploading",
    }
}

wire_enum! {
    /// `FirmwareStatus`.
    FirmwareStatus {
        Downloaded => "Downloaded",
        DownloadFailed => "DownloadFailed",
        Downloading => "Downloading",
        Idle => "Idle",
        InstallationFailed => "InstallationFailed",
        Installing => "Installing",
        Installed => "Installed",
    }
}

wire_enum! {
    /// `Reason` of `StopTransaction.req`.
    StopReason {
        EmergencyStop => "EmergencyStop",
        EVDisconnected => "EVDisconnected",
        HardReset => "HardReset",
        Local => "Local",
        Other => "Other",
        PowerLoss => "PowerLoss",
        Reboot => "Reboot",
        Remote => "Remote",
        SoftReset => "SoftReset",
        UnlockCommand => "UnlockCommand",
        DeAuthorized => "DeAuthorized",
    }
}

wire_enum! {
    /// `Measurand` of a sampled value.
    Measurand {
        CurrentExport => "Current.Export",
        CurrentImport => "Current.Import",
        CurrentOffered => "Current.Offered",
        EnergyActiveExportRegister => "Energy.Active.Export.Register",
        EnergyActiveImportRegister => "Energy.Active.Import.Register",
        EnergyReactiveExportRegister => "Energy.Reactive.Export.Register",
        EnergyReactiveImportRegister => "Energy.Reactive.Import.Register",
        EnergyActiveExportInterval => "Energy.Active.Export.Interval",
        EnergyActiveImportInterval => "Energy.Active.Import.Interval",
        EnergyReactiveExportInterval => "Energy.Reactive.Export.Interval",
        EnergyReactiveImportInterval => "Energy.Reactive.Import.Interval",
        Frequency => "Frequency",
        PowerActiveExport => "Power.Active.Export",
        PowerActiveImport => "Power.Active.Import",
        PowerFactor => "Power.Factor",
        PowerOffered => "Power.Offered",
        PowerReactiveExport => "Power.Reactive.Export",
        PowerReactiveImport => "Power.Reactive.Import",
        Rpm => "RPM",
        SoC => "SoC",
        Temperature => "Temperature",
        Voltage => "Voltage",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_strings_roundtrip_through_serde_and_fromstr() {
        for status in ConnectorStatus::ALL {
            let json = serde_json::to_string(status).unwrap();
            assert_eq!(json, format!("\"{}\"", status.as_str()));
            assert_eq!(
                serde_json::from_str::<ConnectorStatus>(&json).unwrap(),
                *status
            );
            assert_eq!(status.as_str().parse::<ConnectorStatus>().unwrap(), *status);
        }
        for measurand in Measurand::ALL {
            assert_eq!(
                measurand.to_string().parse::<Measurand>().unwrap(),
                *measurand
            );
        }
    }

    #[test]
    fn irregular_wire_strings_are_exact() {
        assert_eq!(ConnectorStatus::SuspendedEVSE.as_str(), "SuspendedEVSE");
        assert_eq!(Measurand::Rpm.as_str(), "RPM");
        assert_eq!(Measurand::CurrentImport.as_str(), "Current.Import");
        assert_eq!(StopReason::EVDisconnected.as_str(), "EVDisconnected");
    }

    #[test]
    fn enum_sizes_match_the_spec() {
        assert_eq!(ConnectorStatus::ALL.len(), 9);
        assert_eq!(ChargePointErrorCode::ALL.len(), 16);
        assert_eq!(StopReason::ALL.len(), 11);
        assert_eq!(Measurand::ALL.len(), 22);
    }

    #[test]
    fn unknown_and_wrongly_cased_values_are_rejected() {
        let err = "available".parse::<ConnectorStatus>().unwrap_err();
        assert_eq!(err.type_name, "ConnectorStatus");
        assert!(serde_json::from_str::<AvailabilityType>("\"Nope\"").is_err());
        assert!(serde_json::from_str::<AvailabilityType>("1").is_err());
    }
}
