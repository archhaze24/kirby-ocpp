//! The OCPP 2.0.1 table of the action registry: the 40 actions a central system sends and
//! the 25 actions a charge station sends. The names follow the payload layer
//! (`kirby_ocpp_core::ocpp::v2::registry`).
//!
//! Twelve central-system actions have a typed payload and a form specification. The other
//! 28 have no fields: their payload is sent raw. 2.0.1 integrates the security extensions
//! into the base specification, so every action is in the core group.

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

const fn json(name: &'static str, label: &'static str, required: bool) -> FieldSpec {
    field(name, label, FieldType::Json, required)
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
    title: &'static str,
    summary: &'static str,
    fields: &'static [FieldSpec],
) -> ActionSpec {
    ActionSpec {
        name,
        group: ActionGroup::Core,
        title,
        summary,
        fields,
    }
}

/// An action without form fields: the payload is sent raw.
const fn raw(name: &'static str, title: &'static str, summary: &'static str) -> ActionSpec {
    action(name, title, summary, &[])
}

const EVSE_ID: FieldSpec = with_hint(
    number("evse.id", "evse (optional)", false),
    "1 or higher; omit for the whole station",
);
const EVSE_CONNECTOR: FieldSpec = with_hint(
    number("evse.connectorId", "connector (optional)", false),
    "needs the evse",
);
const OPERATIONAL_STATUSES: &[&str] = &["Inoperative", "Operative"];
const RESET_TYPES: &[&str] = &["Immediate", "OnIdle"];
const ID_TOKEN_TYPES: &[&str] = &[
    "Central",
    "eMAID",
    "ISO14443",
    "ISO15693",
    "KeyCode",
    "Local",
    "MacAddress",
    "NoAuthorization",
];
const REPORT_BASES: &[&str] = &[
    "ConfigurationInventory",
    "FullInventory",
    "SummaryInventory",
];
const TRIGGERS: &[&str] = &[
    "BootNotification",
    "LogStatusNotification",
    "FirmwareStatusNotification",
    "Heartbeat",
    "MeterValues",
    "SignChargingStationCertificate",
    "SignV2GCertificate",
    "StatusNotification",
    "TransactionEvent",
    "SignCombinedCertificate",
    "PublishFirmwareStatusNotification",
];

/// The actions a charge station sends (and so the ones response rules and faults can
/// target), sorted.
pub const STATION_ACTIONS: &[&str] = &[
    "Authorize",
    "BootNotification",
    "ClearedChargingLimit",
    "DataTransfer",
    "FirmwareStatusNotification",
    "Get15118EVCertificate",
    "GetCertificateStatus",
    "Heartbeat",
    "LogStatusNotification",
    "MeterValues",
    "NotifyChargingLimit",
    "NotifyCustomerInformation",
    "NotifyDisplayMessages",
    "NotifyEVChargingNeeds",
    "NotifyEVChargingSchedule",
    "NotifyEvent",
    "NotifyMonitoringReport",
    "NotifyReport",
    "PublishFirmwareStatusNotification",
    "ReportChargingProfiles",
    "ReservationStatusUpdate",
    "SecurityEventNotification",
    "SignCertificate",
    "StatusNotification",
    "TransactionEvent",
];

/// The OCPP 2.0.1 table.
pub const ACTIONS: ProtocolActions = ProtocolActions {
    protocol: "ocpp2.0.1",
    csms_actions: CSMS_ACTIONS,
    station_actions: STATION_ACTIONS,
};

const CSMS_ACTIONS: &[ActionSpec] = &[
    raw(
        "CancelReservation",
        "Cancel reservation",
        "CancelReservation (raw payload)",
    ),
    raw(
        "CertificateSigned",
        "Certificate signed",
        "CertificateSigned with a signed certificate chain (raw payload)",
    ),
    action(
        "ChangeAvailability",
        "Change availability",
        "ChangeAvailability of the station, an EVSE or a connector",
        &[
            with_default(
                choice(
                    "operationalStatus",
                    "operationalStatus",
                    OPERATIONAL_STATUSES,
                    true,
                ),
                "Operative",
            ),
            EVSE_ID,
            EVSE_CONNECTOR,
        ],
    ),
    action(
        "ClearCache",
        "Clear cache",
        "ClearCache of the authorization cache (no input)",
        &[],
    ),
    raw(
        "ClearChargingProfile",
        "Clear charging profile",
        "ClearChargingProfile (raw payload)",
    ),
    raw(
        "ClearDisplayMessage",
        "Clear display message",
        "ClearDisplayMessage (raw payload)",
    ),
    raw(
        "ClearVariableMonitoring",
        "Clear variable monitoring",
        "ClearVariableMonitoring (raw payload)",
    ),
    raw(
        "CostUpdated",
        "Cost updated",
        "CostUpdated with the running cost of a transaction (raw payload)",
    ),
    raw(
        "CustomerInformation",
        "Customer information",
        "CustomerInformation request or clear (raw payload)",
    ),
    action(
        "DataTransfer",
        "Data transfer",
        "DataTransfer with a vendor id, an optional message id and optional data",
        &[
            with_hint(text("vendorId", "vendorId", true), "at most 255 characters"),
            with_hint(
                text("messageId", "messageId (optional)", false),
                "at most 50 characters",
            ),
            with_hint(json("data", "data (optional)", false), "any JSON value"),
        ],
    ),
    raw(
        "DeleteCertificate",
        "Delete certificate",
        "DeleteCertificate by hash data (raw payload)",
    ),
    action(
        "GetBaseReport",
        "Get base report",
        "GetBaseReport; the station answers with NotifyReport messages",
        &[
            number("requestId", "requestId", true),
            with_default(
                choice("reportBase", "reportBase", REPORT_BASES, true),
                "FullInventory",
            ),
        ],
    ),
    raw(
        "GetChargingProfiles",
        "Get charging profiles",
        "GetChargingProfiles (raw payload)",
    ),
    raw(
        "GetCompositeSchedule",
        "Get composite schedule",
        "GetCompositeSchedule (raw payload)",
    ),
    raw(
        "GetDisplayMessages",
        "Get display messages",
        "GetDisplayMessages (raw payload)",
    ),
    raw(
        "GetInstalledCertificateIds",
        "Get installed certificate ids",
        "GetInstalledCertificateIds (raw payload)",
    ),
    raw(
        "GetLocalListVersion",
        "Get local list version",
        "GetLocalListVersion (raw payload)",
    ),
    raw("GetLog", "Get log", "GetLog (raw payload)"),
    raw(
        "GetMonitoringReport",
        "Get monitoring report",
        "GetMonitoringReport (raw payload)",
    ),
    raw("GetReport", "Get report", "GetReport (raw payload)"),
    action(
        "GetTransactionStatus",
        "Get transaction status",
        "GetTransactionStatus of one transaction, or of the station when no id is given",
        &[with_hint(
            text("transactionId", "transactionId (optional)", false),
            "at most 36 characters",
        )],
    ),
    action(
        "GetVariables",
        "Get variables",
        "GetVariables for a list of component and variable pairs",
        &[with_hint(
            json("getVariableData", "getVariableData", true),
            "JSON array of {component:{name}, variable:{name}}",
        )],
    ),
    raw(
        "InstallCertificate",
        "Install certificate",
        "InstallCertificate with a PEM certificate (raw payload)",
    ),
    raw(
        "PublishFirmware",
        "Publish firmware",
        "PublishFirmware (raw payload)",
    ),
    action(
        "RequestStartTransaction",
        "Request start transaction",
        "RequestStartTransaction with an id token, optionally on an EVSE",
        &[
            with_hint(
                text("idToken.idToken", "idToken", true),
                "at most 36 characters",
            ),
            with_default(
                choice("idToken.type", "idToken type", ID_TOKEN_TYPES, true),
                "ISO14443",
            ),
            with_default(number("remoteStartId", "remoteStartId", true), "1"),
            with_hint(number("evseId", "evse (optional)", false), "1 or higher"),
        ],
    ),
    action(
        "RequestStopTransaction",
        "Request stop transaction",
        "RequestStopTransaction for one of the active transactions",
        &[with_hint(
            choice("transactionId", "transaction", &[], true),
            "the active transactions of the station",
        )],
    ),
    raw("ReserveNow", "Reserve now", "ReserveNow (raw payload)"),
    action(
        "Reset",
        "Reset",
        "immediate reset or reset when idle, of the station or an EVSE",
        &[
            with_default(choice("type", "type", RESET_TYPES, true), "OnIdle"),
            with_hint(
                number("evseId", "evse (optional)", false),
                "1 or higher; omit for the whole station",
            ),
        ],
    ),
    raw(
        "SendLocalList",
        "Send local list",
        "SendLocalList (raw payload)",
    ),
    raw(
        "SetChargingProfile",
        "Set charging profile",
        "SetChargingProfile (raw payload)",
    ),
    raw(
        "SetDisplayMessage",
        "Set display message",
        "SetDisplayMessage (raw payload)",
    ),
    raw(
        "SetMonitoringBase",
        "Set monitoring base",
        "SetMonitoringBase (raw payload)",
    ),
    raw(
        "SetMonitoringLevel",
        "Set monitoring level",
        "SetMonitoringLevel (raw payload)",
    ),
    raw(
        "SetNetworkProfile",
        "Set network profile",
        "SetNetworkProfile (raw payload)",
    ),
    raw(
        "SetVariableMonitoring",
        "Set variable monitoring",
        "SetVariableMonitoring (raw payload)",
    ),
    action(
        "SetVariables",
        "Set variables",
        "SetVariables for a list of component, variable and value triples",
        &[with_hint(
            json("setVariableData", "setVariableData", true),
            "JSON array of {component:{name}, variable:{name}, attributeValue}",
        )],
    ),
    action(
        "TriggerMessage",
        "Trigger message",
        "TriggerMessage: ask the station to send one message, optionally for an EVSE",
        &[
            with_default(
                choice("requestedMessage", "requestedMessage", TRIGGERS, true),
                "StatusNotification",
            ),
            EVSE_ID,
            EVSE_CONNECTOR,
        ],
    ),
    action(
        "UnlockConnector",
        "Unlock connector",
        "UnlockConnector of one connector of an EVSE",
        &[
            with_hint(number("evseId", "evse", true), "1 or higher"),
            with_hint(number("connectorId", "connector", true), "1 or higher"),
        ],
    ),
    raw(
        "UnpublishFirmware",
        "Unpublish firmware",
        "UnpublishFirmware (raw payload)",
    ),
    raw(
        "UpdateFirmware",
        "Update firmware",
        "UpdateFirmware (raw payload)",
    ),
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use kirby_ocpp_core::ocpp::v2::registry as core;
    use kirby_ocpp_core::ocpp::{PayloadKind, Protocol, SchemaRegistry};
    use serde_json::Value;

    use super::*;
    use crate::registry;

    fn csms_names() -> Vec<&'static str> {
        CSMS_ACTIONS.iter().map(|spec| spec.name).collect()
    }

    fn is_sorted_and_unique(names: &[&str]) -> bool {
        names.windows(2).all(|pair| pair[0] < pair[1])
    }

    #[test]
    fn csms_actions_are_the_ones_a_csms_may_send() {
        let expected: BTreeSet<&str> = core::iter()
            .filter(|entry| entry.direction.csms_may_send())
            .map(|entry| entry.name)
            .collect();
        let names = csms_names();
        assert_eq!(names.len(), 40);
        assert_eq!(names.iter().copied().collect::<BTreeSet<_>>(), expected);
        assert!(is_sorted_and_unique(&names));
        assert!(
            CSMS_ACTIONS
                .iter()
                .all(|spec| spec.group == ActionGroup::Core)
        );
    }

    #[test]
    fn station_actions_are_the_ones_a_station_may_send() {
        let expected: BTreeSet<&str> = core::iter()
            .filter(|entry| entry.direction.station_may_send())
            .map(|entry| entry.name)
            .collect();
        assert_eq!(STATION_ACTIONS.len(), 25);
        assert_eq!(
            STATION_ACTIONS.iter().copied().collect::<BTreeSet<_>>(),
            expected
        );
        assert!(is_sorted_and_unique(STATION_ACTIONS));
    }

    #[test]
    fn typed_actions_have_fields_and_raw_ones_have_none() {
        for spec in CSMS_ACTIONS {
            let typed = core::is_typed(spec.name);
            if !typed {
                assert!(
                    spec.fields.is_empty(),
                    "{}: raw action with fields",
                    spec.name
                );
                assert!(
                    spec.summary.contains("raw payload"),
                    "{}: summary names the raw payload",
                    spec.name
                );
            }
        }
        let typed = CSMS_ACTIONS
            .iter()
            .filter(|spec| core::is_typed(spec.name))
            .count();
        assert_eq!(typed, 12);
    }

    fn request_schema(action: &str) -> Value {
        let registry = SchemaRegistry::for_protocol(Protocol::V201).expect("2.0.1 schemas");
        let source = registry
            .schema_source(action, PayloadKind::Request)
            .expect("request schema");
        serde_json::from_str(source).expect("schema is JSON")
    }

    /// The schema node of the property `name` of `object`, with a `$ref` resolved.
    fn property<'a>(root: &'a Value, object: &'a Value, name: &str) -> Option<&'a Value> {
        let node = object.get("properties")?.get(name)?;
        Some(resolve(root, node))
    }

    fn resolve<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
        match node.get("$ref").and_then(Value::as_str) {
            Some(reference) => reference
                .strip_prefix("#/definitions/")
                .and_then(|key| root.get("definitions")?.get(key))
                .expect("local definition"),
            None => node,
        }
    }

    fn is_required(object: &Value, name: &str) -> bool {
        object
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|list| list.iter().any(|item| item == name))
    }

    #[test]
    fn typed_actions_match_the_request_schema() {
        for spec in CSMS_ACTIONS.iter().filter(|spec| core::is_typed(spec.name)) {
            let root = request_schema(spec.name);
            let mut covered = BTreeSet::new();
            for field in spec.fields {
                let (top, nested) = match field.name.split_once('.') {
                    Some((top, nested)) => (top, Some(nested)),
                    None => (field.name, None),
                };
                let context = format!("{}.{}", spec.name, field.name);
                let parent = property(&root, &root, top)
                    .unwrap_or_else(|| panic!("{context}: not in the schema"));
                covered.insert(top);
                let (node, required) = match nested {
                    None => (parent, is_required(&root, top)),
                    Some(key) => {
                        let node = property(&root, parent, key)
                            .unwrap_or_else(|| panic!("{context}: not in the schema"));
                        let required = is_required(&root, top) && is_required(parent, key);
                        (node, required)
                    }
                };
                assert_eq!(field.required, required, "{context}: required flag");
                let schema_choices: Option<Vec<&str>> = node
                    .get("enum")
                    .and_then(Value::as_array)
                    .map(|list| list.iter().filter_map(Value::as_str).collect());
                match field.ty {
                    FieldType::Choice if !field.choices.is_empty() => {
                        assert_eq!(
                            schema_choices.as_deref(),
                            Some(field.choices),
                            "{context}: choices"
                        );
                    }
                    FieldType::Choice => {}
                    _ => assert_eq!(schema_choices, None, "{context}: enum not a choice"),
                }
                if let Some(default) = field.default.filter(|_| !field.choices.is_empty()) {
                    assert!(field.choices.contains(&default), "{context}: default");
                }
                let kind = node.get("type").and_then(Value::as_str);
                match field.ty {
                    FieldType::Integer => assert_eq!(kind, Some("integer"), "{context}: type"),
                    FieldType::Text | FieldType::Choice => {
                        assert_eq!(kind, Some("string"), "{context}: type");
                    }
                    _ => {}
                }
            }
            let required_top: BTreeSet<&str> = root
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            assert!(
                required_top.is_subset(&covered),
                "{}: schema requires {:?}, form covers {:?}",
                spec.name,
                required_top,
                covered
            );
        }
    }

    #[test]
    fn protocols_list_both_tables_in_order() {
        assert_eq!(
            registry::protocols().collect::<Vec<_>>(),
            ["ocpp1.6", "ocpp2.0.1"]
        );
        assert_eq!(
            registry::protocol_actions("ocpp2.0.1").map(|table| table.csms_actions.len()),
            Some(40)
        );
        assert!(registry::csms_action("ocpp2.0.1", "RequestStartTransaction").is_some());
        assert!(registry::csms_action("ocpp2.0.1", "RemoteStartTransaction").is_none());
    }

    #[test]
    fn station_action_lookup_is_per_protocol() {
        assert!(registry::is_station_action("ocpp2.0.1", "TransactionEvent"));
        assert!(!registry::is_station_action(
            "ocpp2.0.1",
            "StartTransaction"
        ));
        assert!(!registry::is_station_action("ocpp1.6", "TransactionEvent"));
    }
}
