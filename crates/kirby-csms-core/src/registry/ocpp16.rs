//! The OCPP 1.6 table of the action registry, next to the typed 1.6 messages
//! (`typed.rs`): the 26 actions a central system sends, with the 7 security whitepaper
//! extensions marked, and the 14 actions a charge point sends.

use super::{ActionGroup, ActionSpec, FieldSpec, FieldType, ProtocolActions};

const fn field(
    name: &'static str,
    label: &'static str,
    ty: FieldType,
    required: bool,
) -> FieldSpec {
    FieldSpec {
        name,
        label,
        ty,
        choices: &[],
        default: None,
        required,
        hint: "",
    }
}

const fn text(name: &'static str, label: &'static str, required: bool) -> FieldSpec {
    field(name, label, FieldType::Text, required)
}

const fn number(name: &'static str, label: &'static str, required: bool) -> FieldSpec {
    field(name, label, FieldType::Integer, required)
}

const fn time(name: &'static str, label: &'static str, required: bool) -> FieldSpec {
    field(name, label, FieldType::Timestamp, required)
}

const fn pem(name: &'static str, label: &'static str) -> FieldSpec {
    FieldSpec {
        hint: "paste one or more PEM blocks",
        ..field(name, label, FieldType::Pem, true)
    }
}

const fn choice(
    name: &'static str,
    label: &'static str,
    choices: &'static [&'static str],
    required: bool,
) -> FieldSpec {
    FieldSpec {
        choices,
        ..field(name, label, FieldType::Choice, required)
    }
}

const fn with_default(spec: FieldSpec, default: &'static str) -> FieldSpec {
    FieldSpec {
        default: Some(default),
        ..spec
    }
}

const fn with_hint(spec: FieldSpec, hint: &'static str) -> FieldSpec {
    FieldSpec { hint, ..spec }
}

const fn action(
    name: &'static str,
    group: ActionGroup,
    title: &'static str,
    summary: &'static str,
    fields: &'static [FieldSpec],
) -> ActionSpec {
    ActionSpec {
        name,
        group,
        title,
        summary,
        fields,
    }
}

const CORE: ActionGroup = ActionGroup::Core;
const SECURITY: ActionGroup = ActionGroup::Security;

const CONNECTOR_OPTIONAL: FieldSpec = with_hint(
    number("connectorId", "connector (optional)", false),
    "1 or higher",
);
const RETRIES: FieldSpec = number("retries", "retries (optional)", false);
const RETRY_INTERVAL: FieldSpec = number("retryInterval", "retryInterval s (optional)", false);
const CERTIFICATE_USES: &[&str] = &[
    "CentralSystemRootCertificate",
    "ManufacturerRootCertificate",
];
const HASH_ALGORITHMS: &[&str] = &["SHA256", "SHA384", "SHA512"];
const PURPOSES: &[&str] = &["ChargePointMaxProfile", "TxDefaultProfile", "TxProfile"];
const TRIGGERS: &[&str] = &[
    "BootNotification",
    "DiagnosticsStatusNotification",
    "FirmwareStatusNotification",
    "Heartbeat",
    "MeterValues",
    "StatusNotification",
];
const EXTENDED_TRIGGERS: &[&str] = &[
    "BootNotification",
    "LogStatusNotification",
    "FirmwareStatusNotification",
    "Heartbeat",
    "MeterValues",
    "SignChargePointCertificate",
    "StatusNotification",
];

/// The actions a charge point sends (and so the ones response rules and faults can target),
/// sorted. Includes the four security whitepaper extensions.
pub const STATION_ACTIONS: &[&str] = &[
    "Authorize",
    "BootNotification",
    "DataTransfer",
    "DiagnosticsStatusNotification",
    "FirmwareStatusNotification",
    "Heartbeat",
    "LogStatusNotification",
    "MeterValues",
    "SecurityEventNotification",
    "SignCertificate",
    "SignedFirmwareStatusNotification",
    "StartTransaction",
    "StatusNotification",
    "StopTransaction",
];

/// The OCPP 1.6 table.
pub const ACTIONS: ProtocolActions = ProtocolActions {
    protocol: "ocpp1.6",
    csms_actions: CSMS_ACTIONS,
    station_actions: STATION_ACTIONS,
};

const CSMS_ACTIONS: &[ActionSpec] = &[
    action(
        "RemoteStartTransaction",
        CORE,
        "Remote start transaction",
        "RemoteStartTransaction with an idTag, optionally on a connector",
        &[
            with_hint(text("idTag", "idTag", true), "at most 20 characters"),
            CONNECTOR_OPTIONAL,
        ],
    ),
    action(
        "RemoteStopTransaction",
        CORE,
        "Remote stop transaction",
        "RemoteStopTransaction for one of the active transactions",
        &[with_hint(
            choice("transactionId", "transaction", &[], true),
            "the active transactions of the station",
        )],
    ),
    action(
        "Reset",
        CORE,
        "Reset",
        "soft or hard reset of the station (asks first)",
        &[with_default(
            choice("type", "type", &["Hard", "Soft"], true),
            "Soft",
        )],
    ),
    action(
        "ChangeConfiguration",
        CORE,
        "Change configuration",
        "ChangeConfiguration: set one key",
        &[
            text("key", "key", true),
            with_hint(text("value", "value", false), "may be empty"),
        ],
    ),
    action(
        "GetConfiguration",
        CORE,
        "Get configuration",
        "GetConfiguration: read keys, the answer opens in a popup",
        &[with_hint(
            text("key", "keys (optional, empty = all)", false),
            "separate keys with commas or spaces",
        )],
    ),
    action(
        "TriggerMessage",
        CORE,
        "Trigger message",
        "TriggerMessage: ask the station to send a message",
        &[
            choice("requestedMessage", "message", TRIGGERS, true),
            CONNECTOR_OPTIONAL,
        ],
    ),
    action(
        "UnlockConnector",
        CORE,
        "Unlock connector",
        "UnlockConnector",
        &[with_default(number("connectorId", "connector", true), "1")],
    ),
    action(
        "ChangeAvailability",
        CORE,
        "Change availability",
        "ChangeAvailability: operative or inoperative",
        &[
            with_default(number("connectorId", "connector (0 = station)", true), "0"),
            choice("type", "type", &["Operative", "Inoperative"], true),
        ],
    ),
    action(
        "ClearCache",
        CORE,
        "Clear cache",
        "ClearCache: empty the authorization cache",
        &[],
    ),
    action(
        "SetChargingProfile",
        CORE,
        "Set charging profile",
        "SetChargingProfile from JSON, checked against the schema",
        &[
            with_default(number("connectorId", "connector (0 = station)", true), "0"),
            field(
                "csChargingProfiles",
                "csChargingProfiles (JSON)",
                FieldType::Json,
                true,
            ),
        ],
    ),
    action(
        "DataTransfer",
        CORE,
        "Data transfer",
        "DataTransfer to the station",
        &[
            text("vendorId", "vendorId", true),
            text("messageId", "messageId (optional)", false),
            text("data", "data (optional)", false),
        ],
    ),
    action(
        "GetLog",
        SECURITY,
        "Get log (security)",
        "GetLog: ask the station to upload its diagnostics or security log",
        &[
            choice(
                "logType",
                "logType",
                &["DiagnosticsLog", "SecurityLog"],
                true,
            ),
            number("requestId", "requestId", true),
            text("log.remoteLocation", "remoteLocation (upload URL)", true),
            RETRIES,
            RETRY_INTERVAL,
            time("log.oldestTimestamp", "oldestTimestamp (optional)", false),
            time("log.latestTimestamp", "latestTimestamp (optional)", false),
        ],
    ),
    action(
        "InstallCertificate",
        SECURITY,
        "Install certificate (security)",
        "InstallCertificate: install a root certificate from pasted PEM",
        &[
            choice("certificateType", "certificateType", CERTIFICATE_USES, true),
            pem("certificate", "certificate (PEM, paste)"),
        ],
    ),
    action(
        "DeleteCertificate",
        SECURITY,
        "Delete certificate (security)",
        "DeleteCertificate by hash data",
        &[
            choice(
                "certificateHashData.hashAlgorithm",
                "hashAlgorithm",
                HASH_ALGORITHMS,
                true,
            ),
            text(
                "certificateHashData.issuerNameHash",
                "issuerNameHash (hex)",
                true,
            ),
            text(
                "certificateHashData.issuerKeyHash",
                "issuerKeyHash (hex)",
                true,
            ),
            text(
                "certificateHashData.serialNumber",
                "serialNumber (hex)",
                true,
            ),
        ],
    ),
    action(
        "GetInstalledCertificateIds",
        SECURITY,
        "Installed certificates (security)",
        "GetInstalledCertificateIds, the answer opens in a popup",
        &[choice(
            "certificateType",
            "certificateType",
            CERTIFICATE_USES,
            true,
        )],
    ),
    action(
        "ExtendedTriggerMessage",
        SECURITY,
        "Extended trigger message (security)",
        "ExtendedTriggerMessage, e.g. SignChargePointCertificate",
        &[
            with_default(
                choice("requestedMessage", "message", EXTENDED_TRIGGERS, true),
                "SignChargePointCertificate",
            ),
            CONNECTOR_OPTIONAL,
        ],
    ),
    action(
        "SignedUpdateFirmware",
        SECURITY,
        "Signed update firmware (security)",
        "SignedUpdateFirmware with signing certificate and signature",
        &[
            text("firmware.location", "location (firmware URL)", true),
            time("firmware.retrieveDateTime", "retrieveDateTime", true),
            number("requestId", "requestId", true),
            RETRIES,
            RETRY_INTERVAL,
            pem(
                "firmware.signingCertificate",
                "signingCertificate (PEM, paste)",
            ),
            text("firmware.signature", "signature (base64)", true),
            time(
                "firmware.installDateTime",
                "installDateTime (optional)",
                false,
            ),
        ],
    ),
    action(
        "CertificateSigned",
        SECURITY,
        "Certificate signed (security)",
        "CertificateSigned: hand the station its certificate chain (PEM)",
        &[pem("certificateChain", "certificateChain (PEM, paste)")],
    ),
    action(
        "ClearChargingProfile",
        CORE,
        "Clear charging profile",
        "ClearChargingProfile by id, connector, purpose or stack level",
        &[
            number("id", "id (optional)", false),
            number("connectorId", "connector (optional, 0 = station)", false),
            choice(
                "chargingProfilePurpose",
                "chargingProfilePurpose",
                PURPOSES,
                false,
            ),
            number("stackLevel", "stackLevel (optional)", false),
        ],
    ),
    action(
        "GetCompositeSchedule",
        CORE,
        "Get composite schedule",
        "GetCompositeSchedule: the schedule in force, the answer opens in a popup",
        &[
            with_default(number("connectorId", "connector (0 = station)", true), "1"),
            with_default(number("duration", "duration s", true), "3600"),
            choice("chargingRateUnit", "chargingRateUnit", &["A", "W"], false),
        ],
    ),
    action(
        "ReserveNow",
        CORE,
        "Reserve now",
        "ReserveNow: reserve a connector for an idTag",
        &[
            with_default(number("connectorId", "connector (0 = any)", true), "1"),
            time("expiryDate", "expiryDate", true),
            with_hint(text("idTag", "idTag", true), "at most 20 characters"),
            with_hint(
                text("parentIdTag", "parentIdTag (optional)", false),
                "at most 20 characters",
            ),
            number("reservationId", "reservationId", true),
        ],
    ),
    action(
        "UpdateFirmware",
        CORE,
        "Update firmware",
        "UpdateFirmware: download from a URL at a given time",
        &[
            text("location", "location (firmware URL)", true),
            time("retrieveDate", "retrieveDate", true),
            RETRIES,
            RETRY_INTERVAL,
        ],
    ),
    action(
        "GetDiagnostics",
        CORE,
        "Get diagnostics",
        "GetDiagnostics: ask the station to upload its diagnostics file",
        &[
            text("location", "location (upload URL)", true),
            RETRIES,
            RETRY_INTERVAL,
            time("startTime", "startTime (optional)", false),
            time("stopTime", "stopTime (optional)", false),
        ],
    ),
    action(
        "SendLocalList",
        CORE,
        "Send local list",
        "SendLocalList: full or differential local authorization list",
        &[
            choice("updateType", "updateType", &["Differential", "Full"], true),
            with_default(number("listVersion", "listVersion", true), "1"),
            with_hint(
                text(
                    "localAuthorizationList",
                    "entries: idTag[:status], comma separated (:- deletes)",
                    false,
                ),
                "status defaults to Accepted; at most 100 entries",
            ),
        ],
    ),
    action(
        "CancelReservation",
        CORE,
        "Cancel reservation",
        "CancelReservation by reservation id",
        &[number("reservationId", "reservationId", true)],
    ),
    action(
        "GetLocalListVersion",
        CORE,
        "Get local list version",
        "GetLocalListVersion: the version of the local authorization list",
        &[],
    ),
];

#[cfg(test)]
mod tests {
    use kirby_ocpp_core::ocpp::messages::registry::ACTIONS as CORE_ACTIONS;
    use kirby_ocpp_core::ocpp::types::{
        AvailabilityType, ChargingProfilePurposeType, ChargingRateUnitType, MessageTrigger,
        ResetType, UpdateType,
    };

    use super::*;

    fn names<T: 'static>(
        all: &'static [T],
        name: impl Fn(&T) -> &'static str,
    ) -> Vec<&'static str> {
        all.iter().map(name).collect()
    }

    #[test]
    fn covers_every_action_the_typed_layer_knows() {
        let mut registered: Vec<&str> = CSMS_ACTIONS.iter().map(|spec| spec.name).collect();
        registered.sort_unstable();
        let mut known: Vec<&str> = CORE_ACTIONS
            .iter()
            .filter(|info| info.direction.central_system_may_send())
            .map(|info| info.name)
            .collect();
        known.sort_unstable();
        assert_eq!(registered, known);
        let mut stations: Vec<&str> = CORE_ACTIONS
            .iter()
            .filter(|info| info.direction.charge_point_may_send())
            .map(|info| info.name)
            .collect();
        stations.sort_unstable();
        assert_eq!(STATION_ACTIONS, stations.as_slice());
        assert!(STATION_ACTIONS.is_sorted());
    }

    #[test]
    fn choices_match_the_typed_enums() {
        let choices = |action: &str, field: &str| {
            let spec = super::super::csms_action("ocpp1.6", action).unwrap();
            spec.field(field).unwrap().choices.to_vec()
        };
        assert_eq!(
            choices("Reset", "type"),
            names(ResetType::ALL, |t| t.as_str())
        );
        assert_eq!(
            choices("TriggerMessage", "requestedMessage"),
            names(MessageTrigger::ALL, |t| t.as_str())
        );
        assert_eq!(
            choices("ChangeAvailability", "type"),
            names(AvailabilityType::ALL, |t| t.as_str())
        );
        assert_eq!(
            choices("ClearChargingProfile", "chargingProfilePurpose"),
            names(ChargingProfilePurposeType::ALL, |t| t.as_str())
        );
        assert_eq!(
            choices("GetCompositeSchedule", "chargingRateUnit"),
            names(ChargingRateUnitType::ALL, |t| t.as_str())
        );
        assert_eq!(
            choices("SendLocalList", "updateType"),
            names(UpdateType::ALL, |t| t.as_str())
        );
    }

    #[test]
    fn specs_are_consistent() {
        for spec in CSMS_ACTIONS {
            let mut seen = std::collections::HashSet::new();
            for f in spec.fields {
                assert!(
                    seen.insert(f.name),
                    "{}: duplicate field {}",
                    spec.name,
                    f.name
                );
                assert_eq!(
                    f.ty == FieldType::Choice,
                    !f.choices.is_empty() || f.name == "transactionId",
                    "{}.{}",
                    spec.name,
                    f.name
                );
                if let (Some(default), true) = (f.default, f.ty == FieldType::Choice) {
                    assert!(f.choices.contains(&default), "{}.{}", spec.name, f.name);
                }
            }
        }
    }
}
