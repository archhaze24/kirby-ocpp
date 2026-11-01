//! Shared OCPP 1.6 vocabulary enums.
//!
//! Every enum serializes to / parses from the exact OCPP wire string and has
//! `ALL`, `as_str`, `Display` and `FromStr`, so UIs can build pick-lists and
//! the CLI can parse flags without a second source of truth. They are used both
//! by the typed messages in [`crate::ocpp::messages`] and by the station state.

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

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::ocpp::types::ParseEnumError;

            fn from_str(s: &str) -> ::std::result::Result<Self, Self::Err> {
                match s {
                    $($wire => ::std::result::Result::Ok(Self::$variant),)+
                    _ => Err($crate::ocpp::types::ParseEnumError {
                        type_name: stringify!($name),
                        value: ::std::string::ToString::to_string(s),
                    }),
                }
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> ::std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> ::std::result::Result<Self, D::Error> {
                let value = <::std::string::String as ::serde::Deserialize>::deserialize(deserializer)?;
                value.parse().map_err(<D::Error as ::serde::de::Error>::custom)
            }
        }
    };
}

pub(crate) use wire_enum;

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

wire_enum! {
    /// `Phase` of a sampled value.
    Phase {
        L1 => "L1",
        L2 => "L2",
        L3 => "L3",
        N => "N",
        L1N => "L1-N",
        L2N => "L2-N",
        L3N => "L3-N",
        L1L2 => "L1-L2",
        L2L3 => "L2-L3",
        L3L1 => "L3-L1",
    }
}

wire_enum! {
    /// `Location` of a measurement.
    Location {
        Cable => "Cable",
        EV => "EV",
        Inlet => "Inlet",
        Outlet => "Outlet",
        Body => "Body",
    }
}

wire_enum! {
    /// `UnitOfMeasure` of a sampled value. Both spellings `Celcius` (sic, as in
    /// the 1.6 schema) and `Celsius` are valid on the wire.
    UnitOfMeasure {
        Wh => "Wh",
        KWh => "kWh",
        Varh => "varh",
        Kvarh => "kvarh",
        W => "W",
        KW => "kW",
        VA => "VA",
        KVA => "kVA",
        Var => "var",
        Kvar => "kvar",
        A => "A",
        V => "V",
        K => "K",
        Celcius => "Celcius",
        Celsius => "Celsius",
        Fahrenheit => "Fahrenheit",
        Percent => "Percent",
    }
}

wire_enum! {
    /// `ReadingContext` of a sampled value.
    ReadingContext {
        InterruptionBegin => "Interruption.Begin",
        InterruptionEnd => "Interruption.End",
        SampleClock => "Sample.Clock",
        SamplePeriodic => "Sample.Periodic",
        TransactionBegin => "Transaction.Begin",
        TransactionEnd => "Transaction.End",
        Trigger => "Trigger",
        Other => "Other",
    }
}

wire_enum! {
    /// `ValueFormat` of a sampled value.
    ValueFormat {
        Raw => "Raw",
        SignedData => "SignedData",
    }
}

wire_enum! {
    /// `ResetType` of `Reset.req`.
    ResetType {
        Hard => "Hard",
        Soft => "Soft",
    }
}

wire_enum! {
    /// `ConfigurationStatus` of `ChangeConfiguration.conf`.
    ConfigurationStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
        RebootRequired => "RebootRequired",
        NotSupported => "NotSupported",
    }
}

wire_enum! {
    /// `ChargingProfilePurposeType`.
    ChargingProfilePurposeType {
        ChargePointMaxProfile => "ChargePointMaxProfile",
        TxDefaultProfile => "TxDefaultProfile",
        TxProfile => "TxProfile",
    }
}

wire_enum! {
    /// `ChargingProfileKindType`.
    ChargingProfileKindType {
        Absolute => "Absolute",
        Recurring => "Recurring",
        Relative => "Relative",
    }
}

wire_enum! {
    /// `RecurrencyKindType`.
    RecurrencyKindType {
        Daily => "Daily",
        Weekly => "Weekly",
    }
}

wire_enum! {
    /// `ChargingRateUnitType`.
    ChargingRateUnitType {
        A => "A",
        W => "W",
    }
}

wire_enum! {
    /// `MessageTrigger`: the `requestedMessage` of `TriggerMessage.req`.
    MessageTrigger {
        BootNotification => "BootNotification",
        DiagnosticsStatusNotification => "DiagnosticsStatusNotification",
        FirmwareStatusNotification => "FirmwareStatusNotification",
        Heartbeat => "Heartbeat",
        MeterValues => "MeterValues",
        StatusNotification => "StatusNotification",
    }
}

wire_enum! {
    /// `UpdateType` of `SendLocalList.req`.
    UpdateType {
        Differential => "Differential",
        Full => "Full",
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
