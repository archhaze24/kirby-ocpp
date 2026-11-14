//! The catalog of all 64 OCPP 2.0.1 actions with the direction each one travels in and
//! whether [`messages`](super::messages) has a typed payload for it.
//!
//! The catalog is a static table: lookups borrow from it and never allocate. Having a
//! name here means the action exists in 2.0.1, not that a station or CSMS implements it.
//! The names are pinned to the embedded JSON schemas by the tests at the bottom of this
//! file, and the directions to the marker types in [`messages`](super::messages).

use crate::ocpp::frame::ErrorCode;

/// Who may send the CALL of an action. Relative to the charge station, also when the CSMS
/// is the sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionDirection {
    /// Only the station sends the CALL (`BootNotification`, ...).
    StationToCsms,
    /// Only the CSMS sends the CALL (`Reset`, ...).
    CsmsToStation,
    /// Either side (`DataTransfer`).
    Both,
}

impl ActionDirection {
    /// `true` if the station may send the CALL.
    pub const fn station_may_send(self) -> bool {
        matches!(self, Self::StationToCsms | Self::Both)
    }

    /// `true` if the CSMS may send the CALL.
    pub const fn csms_may_send(self) -> bool {
        matches!(self, Self::CsmsToStation | Self::Both)
    }
}

/// One entry of [`ACTIONS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionInfo {
    /// The action name as it appears in a CALL.
    pub name: &'static str,
    /// Who may send the CALL.
    pub direction: ActionDirection,
    /// `true` if [`messages`](super::messages) defines request and response structs and a
    /// marker for the action. Other actions are carried as raw JSON.
    pub typed: bool,
}

const STATION: ActionDirection = ActionDirection::StationToCsms;
const CSMS: ActionDirection = ActionDirection::CsmsToStation;

const fn info(name: &'static str, direction: ActionDirection, typed: bool) -> ActionInfo {
    ActionInfo {
        name,
        direction,
        typed,
    }
}

/// All 64 actions in byte-wise alphabetical order: 24 sent by the station, 39 by the CSMS
/// and `DataTransfer` by either. 20 of them are typed.
pub const ACTIONS: [ActionInfo; 64] = [
    info("Authorize", STATION, true),
    info("BootNotification", STATION, true),
    info("CancelReservation", CSMS, false),
    info("CertificateSigned", CSMS, false),
    info("ChangeAvailability", CSMS, true),
    info("ClearCache", CSMS, true),
    info("ClearChargingProfile", CSMS, false),
    info("ClearDisplayMessage", CSMS, false),
    info("ClearVariableMonitoring", CSMS, false),
    info("ClearedChargingLimit", STATION, false),
    info("CostUpdated", CSMS, false),
    info("CustomerInformation", CSMS, false),
    info("DataTransfer", ActionDirection::Both, true),
    info("DeleteCertificate", CSMS, false),
    info("FirmwareStatusNotification", STATION, false),
    info("Get15118EVCertificate", STATION, false),
    info("GetBaseReport", CSMS, true),
    info("GetCertificateStatus", STATION, false),
    info("GetChargingProfiles", CSMS, false),
    info("GetCompositeSchedule", CSMS, false),
    info("GetDisplayMessages", CSMS, false),
    info("GetInstalledCertificateIds", CSMS, false),
    info("GetLocalListVersion", CSMS, false),
    info("GetLog", CSMS, false),
    info("GetMonitoringReport", CSMS, false),
    info("GetReport", CSMS, false),
    info("GetTransactionStatus", CSMS, true),
    info("GetVariables", CSMS, true),
    info("Heartbeat", STATION, true),
    info("InstallCertificate", CSMS, false),
    info("LogStatusNotification", STATION, false),
    info("MeterValues", STATION, true),
    info("NotifyChargingLimit", STATION, false),
    info("NotifyCustomerInformation", STATION, false),
    info("NotifyDisplayMessages", STATION, false),
    info("NotifyEVChargingNeeds", STATION, false),
    info("NotifyEVChargingSchedule", STATION, false),
    info("NotifyEvent", STATION, false),
    info("NotifyMonitoringReport", STATION, false),
    info("NotifyReport", STATION, true),
    info("PublishFirmware", CSMS, false),
    info("PublishFirmwareStatusNotification", STATION, false),
    info("ReportChargingProfiles", STATION, false),
    info("RequestStartTransaction", CSMS, true),
    info("RequestStopTransaction", CSMS, true),
    info("ReservationStatusUpdate", STATION, false),
    info("ReserveNow", CSMS, false),
    info("Reset", CSMS, true),
    info("SecurityEventNotification", STATION, true),
    info("SendLocalList", CSMS, false),
    info("SetChargingProfile", CSMS, false),
    info("SetDisplayMessage", CSMS, false),
    info("SetMonitoringBase", CSMS, false),
    info("SetMonitoringLevel", CSMS, false),
    info("SetNetworkProfile", CSMS, false),
    info("SetVariableMonitoring", CSMS, false),
    info("SetVariables", CSMS, true),
    info("SignCertificate", STATION, false),
    info("StatusNotification", STATION, true),
    info("TransactionEvent", STATION, true),
    info("TriggerMessage", CSMS, true),
    info("UnlockConnector", CSMS, true),
    info("UnpublishFirmware", CSMS, false),
    info("UpdateFirmware", CSMS, false),
];

/// The entry of action `name` (case-sensitive), or `None` if 2.0.1 has no such action.
pub fn lookup(name: &str) -> Option<&'static ActionInfo> {
    ACTIONS
        .binary_search_by(|entry| entry.name.cmp(name))
        .ok()
        .and_then(|index| ACTIONS.get(index))
}

/// All actions in table order.
pub fn iter() -> impl Iterator<Item = &'static ActionInfo> {
    ACTIONS.iter()
}

/// `true` if `name` is an OCPP 2.0.1 action.
pub fn is_known(name: &str) -> bool {
    lookup(name).is_some()
}

/// `true` if `name` is an OCPP 2.0.1 action with a typed payload in
/// [`messages`](super::messages).
pub fn is_typed(name: &str) -> bool {
    lookup(name).is_some_and(|entry| entry.typed)
}

/// Who may send the CALL of `name`, or `None` for an unknown action.
pub fn direction(name: &str) -> Option<ActionDirection> {
    lookup(name).map(|entry| entry.direction)
}

/// The CALLERROR code for an incoming CALL of `name` that has no handler: `NotImplemented`
/// for a name 2.0.1 does not define, `NotSupported` for any defined action. A typed
/// payload alone does not make an action handled, so typed actions count as `NotSupported`
/// too; the dispatcher decides which actions have a handler before it asks.
pub fn unhandled_error_code(name: &str) -> ErrorCode {
    if is_known(name) {
        ErrorCode::NotSupported
    } else {
        ErrorCode::NotImplemented
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::messages as m;
    use super::*;
    use crate::ocpp::action::{InboundAction, OutboundAction};
    use crate::ocpp::{Protocol, SchemaRegistry};

    fn station_sent<A: OutboundAction>() -> &'static str {
        A::NAME
    }

    fn csms_sent<A: InboundAction>() -> &'static str {
        A::NAME
    }

    /// Typed markers by the direction traits they implement.
    fn typed_markers() -> (Vec<&'static str>, Vec<&'static str>) {
        let outbound = vec![
            station_sent::<m::BootNotification>(),
            station_sent::<m::Heartbeat>(),
            station_sent::<m::StatusNotification>(),
            station_sent::<m::Authorize>(),
            station_sent::<m::TransactionEvent>(),
            station_sent::<m::MeterValues>(),
            station_sent::<m::NotifyReport>(),
            station_sent::<m::SecurityEventNotification>(),
            station_sent::<m::DataTransfer>(),
        ];
        let inbound = vec![
            csms_sent::<m::Reset>(),
            csms_sent::<m::ChangeAvailability>(),
            csms_sent::<m::RequestStartTransaction>(),
            csms_sent::<m::RequestStopTransaction>(),
            csms_sent::<m::UnlockConnector>(),
            csms_sent::<m::TriggerMessage>(),
            csms_sent::<m::GetVariables>(),
            csms_sent::<m::SetVariables>(),
            csms_sent::<m::GetBaseReport>(),
            csms_sent::<m::ClearCache>(),
            csms_sent::<m::GetTransactionStatus>(),
            csms_sent::<m::DataTransfer>(),
        ];
        (outbound, inbound)
    }

    #[test]
    fn names_equal_the_schema_table_exactly() {
        let registry = SchemaRegistry::for_protocol(Protocol::V201).unwrap();
        let schema: Vec<&str> = registry.action_names().collect();
        let catalog: Vec<&str> = iter().map(|entry| entry.name).collect();
        assert_eq!(catalog.len(), 64);
        assert_eq!(catalog, schema);
        let unique: BTreeSet<&str> = catalog.iter().copied().collect();
        assert_eq!(unique.len(), 64, "duplicate action name");
    }

    #[test]
    fn table_is_strictly_sorted_for_binary_search() {
        assert!(ACTIONS.windows(2).all(|pair| pair[0].name < pair[1].name));
        for entry in iter() {
            assert_eq!(lookup(entry.name), Some(entry));
        }
    }

    #[test]
    fn typed_entries_match_the_marker_types_and_their_direction() {
        let (outbound, inbound) = typed_markers();
        let typed: BTreeSet<&str> = iter()
            .filter(|entry| entry.typed)
            .map(|entry| entry.name)
            .collect();
        assert_eq!(typed.len(), 20);
        let markers: BTreeSet<&str> = outbound.iter().chain(&inbound).copied().collect();
        assert_eq!(markers, typed);
        for name in &typed {
            let expected = match (outbound.contains(name), inbound.contains(name)) {
                (true, true) => ActionDirection::Both,
                (true, false) => ActionDirection::StationToCsms,
                (false, true) => ActionDirection::CsmsToStation,
                (false, false) => unreachable!("typed set is built from the markers"),
            };
            assert_eq!(direction(name), Some(expected), "{name}");
        }
    }

    #[test]
    fn direction_counts() {
        let count = |wanted| iter().filter(|entry| entry.direction == wanted).count();
        assert_eq!(count(ActionDirection::StationToCsms), 24);
        assert_eq!(count(ActionDirection::CsmsToStation), 39);
        assert_eq!(count(ActionDirection::Both), 1);
        assert_eq!(direction("DataTransfer"), Some(ActionDirection::Both));
        assert!(ActionDirection::Both.station_may_send());
        assert!(ActionDirection::Both.csms_may_send());
        assert!(ActionDirection::StationToCsms.station_may_send());
        assert!(!ActionDirection::StationToCsms.csms_may_send());
        assert!(ActionDirection::CsmsToStation.csms_may_send());
        assert!(!ActionDirection::CsmsToStation.station_may_send());
    }

    #[test]
    fn untyped_directions_follow_the_specification() {
        for name in [
            "ClearedChargingLimit",
            "FirmwareStatusNotification",
            "Get15118EVCertificate",
            "GetCertificateStatus",
            "LogStatusNotification",
            "NotifyChargingLimit",
            "NotifyCustomerInformation",
            "NotifyDisplayMessages",
            "NotifyEVChargingNeeds",
            "NotifyEVChargingSchedule",
            "NotifyEvent",
            "NotifyMonitoringReport",
            "PublishFirmwareStatusNotification",
            "ReportChargingProfiles",
            "ReservationStatusUpdate",
            "SignCertificate",
        ] {
            assert_eq!(
                direction(name),
                Some(ActionDirection::StationToCsms),
                "{name}"
            );
        }
        for name in [
            "CancelReservation",
            "CertificateSigned",
            "CostUpdated",
            "CustomerInformation",
            "GetChargingProfiles",
            "GetLog",
            "GetReport",
            "InstallCertificate",
            "PublishFirmware",
            "ReserveNow",
            "SetNetworkProfile",
            "UpdateFirmware",
        ] {
            assert_eq!(
                direction(name),
                Some(ActionDirection::CsmsToStation),
                "{name}"
            );
        }
    }

    #[test]
    fn version_specific_names_are_unknown_to_the_other_version() {
        let v16 = SchemaRegistry::for_protocol(Protocol::V16).unwrap();
        for name in [
            "ChangeConfiguration",
            "GetConfiguration",
            "StartTransaction",
            "StopTransaction",
            "RemoteStartTransaction",
            "RemoteStopTransaction",
            "DiagnosticsStatusNotification",
            "GetDiagnostics",
            "ExtendedTriggerMessage",
            "SignedUpdateFirmware",
            "SignedFirmwareStatusNotification",
        ] {
            assert!(v16.knows_action(name), "{name} is a 1.6 action");
            assert!(!is_known(name), "{name} must be unknown in 2.0.1");
            assert_eq!(unhandled_error_code(name), ErrorCode::NotImplemented);
        }
        for name in [
            "TransactionEvent",
            "GetVariables",
            "SetVariables",
            "NotifyReport",
            "GetBaseReport",
            "RequestStartTransaction",
            "RequestStopTransaction",
        ] {
            assert!(!v16.knows_action(name), "{name} must be unknown in 1.6");
        }
        let shared: Vec<&str> = iter()
            .map(|entry| entry.name)
            .filter(|name| v16.knows_action(name))
            .collect();
        for name in [
            "BootNotification",
            "Heartbeat",
            "StatusNotification",
            "Authorize",
            "DataTransfer",
            "MeterValues",
            "Reset",
            "ChangeAvailability",
            "TriggerMessage",
            "UnlockConnector",
            "ClearCache",
        ] {
            assert!(shared.contains(&name), "{name} is shared");
        }
    }

    #[test]
    fn shared_names_keep_their_own_payload_shapes() {
        use serde_json::json;

        let v16 = SchemaRegistry::for_protocol(Protocol::V16).unwrap();
        let v201 = SchemaRegistry::for_protocol(Protocol::V201).unwrap();
        // (action, 1.6 request, 2.0.1 request)
        let cases = [
            (
                "BootNotification",
                json!({"chargePointVendor": "v", "chargePointModel": "m"}),
                json!({"reason": "PowerUp", "chargingStation": {"model": "m", "vendorName": "v"}}),
            ),
            (
                "Authorize",
                json!({"idTag": "TAG"}),
                json!({"idToken": {"idToken": "TAG", "type": "ISO14443"}}),
            ),
            (
                "StatusNotification",
                json!({"connectorId": 1, "errorCode": "NoError", "status": "Available"}),
                json!({
                    "timestamp": "2026-05-06T07:08:09Z",
                    "connectorStatus": "Available",
                    "evseId": 1,
                    "connectorId": 1
                }),
            ),
            (
                "ChangeAvailability",
                json!({"connectorId": 1, "type": "Operative"}),
                json!({"operationalStatus": "Operative"}),
            ),
            (
                "MeterValues",
                json!({"connectorId": 1, "meterValue": [
                    {"timestamp": "2026-05-06T07:08:09Z", "sampledValue": [{"value": "1"}]}
                ]}),
                json!({"evseId": 1, "meterValue": [
                    {"timestamp": "2026-05-06T07:08:09Z", "sampledValue": [{"value": 1}]}
                ]}),
            ),
        ];
        for (name, old, new) in cases {
            v16.validate_request(name, &old).unwrap();
            v201.validate_request(name, &new).unwrap();
            assert!(
                v201.validate_request(name, &old).is_err(),
                "{name}: 1.6 shape in 2.0.1"
            );
            assert!(
                v16.validate_request(name, &new).is_err(),
                "{name}: 2.0.1 shape in 1.6"
            );
        }
        // Same field, different enumeration.
        v16.validate_request("Reset", &json!({"type": "Hard"}))
            .unwrap();
        v201.validate_request("Reset", &json!({"type": "Immediate"}))
            .unwrap();
        assert!(
            v201.validate_request("Reset", &json!({"type": "Hard"}))
                .is_err()
        );
        assert!(
            v16.validate_request("Reset", &json!({"type": "Immediate"}))
                .is_err()
        );
    }

    #[test]
    fn lookups() {
        assert!(is_known("GetLog"));
        assert!(!is_typed("GetLog"));
        assert!(is_typed("Heartbeat"));
        assert!(!is_known("heartbeat"));
        assert!(!is_known(""));
        assert_eq!(direction("NoSuchAction"), None);
        assert_eq!(lookup("Reset").map(|entry| entry.typed), Some(true));
    }

    #[test]
    fn unhandled_codes() {
        assert_eq!(
            unhandled_error_code("Frobnicate"),
            ErrorCode::NotImplemented
        );
        assert_eq!(unhandled_error_code("GetLog"), ErrorCode::NotSupported);
        assert_eq!(unhandled_error_code("Reset"), ErrorCode::NotSupported);
    }
}
