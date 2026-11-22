//! The OCPP 2.0.1 table of the action registry: the 40 actions a central system sends and
//! the 25 actions a charge station sends. The names follow the payload layer
//! (`kirby_ocpp_core::ocpp::v2::registry`). The OCPP 2.1 table ([`super::ocpp21`]) builds on
//! this one.
//!
//! Twenty-two central-system actions have a typed payload and a form specification. The other
//! 18 have no fields: their payload is sent raw. 2.0.1 integrates the security extensions
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
pub(super) const fn raw(
    name: &'static str,
    title: &'static str,
    summary: &'static str,
) -> ActionSpec {
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
/// OCPP 2.1 added `ImmediateAndResume` to the reset types.
const RESET_TYPES_21: &[&str] = &["Immediate", "OnIdle", "ImmediateAndResume"];

const RESET_EVSE: FieldSpec = with_hint(
    number("evseId", "evse (optional)", false),
    "1 or higher; omit for the whole station",
);
const RESET: ActionSpec = action(
    "Reset",
    "Reset",
    "immediate reset or reset when idle, of the station or an EVSE",
    &[
        with_default(choice("type", "type", RESET_TYPES, true), "OnIdle"),
        RESET_EVSE,
    ],
);
/// The 2.1 form of `Reset`: the 2.0.1 one with the extra reset type.
pub(super) const RESET_21: ActionSpec = action(
    "Reset",
    "Reset",
    "immediate reset, reset when idle or reset and resume, of the station or an EVSE",
    &[
        with_default(choice("type", "type", RESET_TYPES_21, true), "OnIdle"),
        RESET_EVSE,
    ],
);
const UPDATE_TYPES: &[&str] = &["Differential", "Full"];
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
const PROFILE_PURPOSES: &[&str] = &[
    "ChargingStationExternalConstraints",
    "ChargingStationMaxProfile",
    "TxDefaultProfile",
    "TxProfile",
];
const RATE_UNITS: &[&str] = &["W", "A"];
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

pub(super) const CSMS_ACTIONS: &[ActionSpec] = &[
    action(
        "CancelReservation",
        "Cancel reservation",
        "CancelReservation by reservation id",
        &[number("reservationId", "reservationId", true)],
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
    action(
        "ClearChargingProfile",
        "Clear charging profile",
        "ClearChargingProfile by id or by criteria (all together without input)",
        &[
            number("chargingProfileId", "chargingProfileId (optional)", false),
            with_hint(
                number(
                    "chargingProfileCriteria.evseId",
                    "criteria: evse (optional)",
                    false,
                ),
                "0 for the profiles of the station itself",
            ),
            with_hint(
                choice(
                    "chargingProfileCriteria.chargingProfilePurpose",
                    "criteria: purpose (optional)",
                    PROFILE_PURPOSES,
                    false,
                ),
                "profiles of an external system are never cleared",
            ),
            number(
                "chargingProfileCriteria.stackLevel",
                "criteria: stack level (optional)",
                false,
            ),
        ],
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
    action(
        "CostUpdated",
        "Cost updated",
        "CostUpdated with the running cost of a transaction",
        &[
            with_hint(json("totalCost", "totalCost", true), "a number, e.g. 12.34"),
            text("transactionId", "transactionId", true),
        ],
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
    action(
        "GetChargingProfiles",
        "Get charging profiles",
        "GetChargingProfiles: the station reports the installed profiles that match",
        &[
            with_default(number("requestId", "requestId", true), "1"),
            with_hint(
                number("evseId", "evse (optional)", false),
                "0 for the profiles of the station itself; omit for all",
            ),
            with_hint(
                choice(
                    "chargingProfile.chargingProfilePurpose",
                    "purpose (optional)",
                    PROFILE_PURPOSES,
                    false,
                ),
                "set at least one filter",
            ),
            number(
                "chargingProfile.stackLevel",
                "stack level (optional)",
                false,
            ),
            with_hint(
                json(
                    "chargingProfile.chargingProfileId",
                    "profile ids (optional)",
                    false,
                ),
                "JSON array of ids, e.g. [1,2]",
            ),
            with_hint(
                json(
                    "chargingProfile.chargingLimitSource",
                    "limit sources (optional)",
                    false,
                ),
                "JSON array of EMS, Other, SO, CSO",
            ),
        ],
    ),
    action(
        "GetCompositeSchedule",
        "Get composite schedule",
        "GetCompositeSchedule: the limit that results from all profiles of an EVSE",
        &[
            with_default(number("duration", "duration (s)", true), "3600"),
            with_default(
                with_hint(
                    number("evseId", "evse (0 = station)", true),
                    "0 for the station as a whole",
                ),
                "0",
            ),
            choice(
                "chargingRateUnit",
                "chargingRateUnit (optional)",
                RATE_UNITS,
                false,
            ),
        ],
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
    action(
        "GetLocalListVersion",
        "Get local list version",
        "GetLocalListVersion: the version of the local authorization list",
        &[],
    ),
    raw("GetLog", "Get log", "GetLog (raw payload)"),
    raw(
        "GetMonitoringReport",
        "Get monitoring report",
        "GetMonitoringReport (raw payload)",
    ),
    action(
        "GetReport",
        "Get report",
        "GetReport: a report limited by component criteria and component variables",
        &[
            with_default(number("requestId", "requestId", true), "1"),
            with_hint(
                json("componentCriteria", "componentCriteria (optional)", false),
                "JSON array of Active, Available, Enabled, Problem",
            ),
            with_hint(
                json("componentVariable", "componentVariable (optional)", false),
                "JSON array of {component:{name}, variable:{name}}",
            ),
        ],
    ),
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
    action(
        "ReserveNow",
        "Reserve now",
        "ReserveNow: hold an EVSE, or any EVSE, for an id token until a time",
        &[
            with_default(number("id", "reservation id", true), "1"),
            with_hint(
                text("expiryDateTime", "expiryDateTime", true),
                "RFC 3339 UTC, e.g. 2030-01-01T00:00:00Z",
            ),
            with_hint(
                text("idToken.idToken", "idToken", true),
                "at most 36 characters",
            ),
            with_default(
                choice("idToken.type", "idToken type", ID_TOKEN_TYPES, true),
                "ISO14443",
            ),
            with_hint(number("evseId", "evse (optional)", false), "1 or higher"),
        ],
    ),
    RESET,
    action(
        "SendLocalList",
        "Send local list",
        "SendLocalList: full or differential local authorization list",
        &[
            number("versionNumber", "versionNumber", true),
            with_default(
                choice("updateType", "updateType", UPDATE_TYPES, true),
                "Full",
            ),
            with_hint(
                json(
                    "localAuthorizationList",
                    "localAuthorizationList (optional)",
                    false,
                ),
                "JSON array of {idToken:{idToken, type}, idTokenInfo:{status}}",
            ),
        ],
    ),
    action(
        "SetChargingProfile",
        "Set charging profile",
        "SetChargingProfile from JSON, checked against the schema",
        &[
            with_default(number("evseId", "evse (0 = station)", true), "0"),
            with_hint(
                json("chargingProfile", "chargingProfile (JSON)", true),
                "{id, stackLevel, chargingProfilePurpose, chargingProfileKind, chargingSchedule:[...]}",
            ),
        ],
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
pub(in crate::registry) mod tests {
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
        assert_eq!(typed, 22);
    }

    fn request_schema(protocol: Protocol, action: &str) -> Value {
        let registry = SchemaRegistry::for_protocol(protocol).expect("schemas");
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
        let typed: Vec<ActionSpec> = CSMS_ACTIONS
            .iter()
            .copied()
            .filter(|spec| core::is_typed(spec.name))
            .collect();
        assert_forms_match_schemas(Protocol::V201, &typed);
    }

    /// Every form field of `specs` exists in the request schema of `protocol` with the same
    /// type, choices and required flag, and the form covers the required keys.
    pub(in crate::registry) fn assert_forms_match_schemas(
        protocol: Protocol,
        specs: &[ActionSpec],
    ) {
        for spec in specs {
            let root = request_schema(protocol, spec.name);
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
                    // The 2.1 table reuses these forms. 2.1 added values to some enums (the
                    // reset type) and made others open strings (the id token type): every
                    // choice stays valid, but the list is no longer complete.
                    FieldType::Choice
                        if !field.choices.is_empty() && protocol != Protocol::V201 =>
                    {
                        if let Some(allowed) = &schema_choices {
                            assert!(
                                field.choices.iter().all(|choice| allowed.contains(choice)),
                                "{context}: choices {:?} not in {allowed:?}",
                                field.choices
                            );
                        }
                    }
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
    fn protocols_list_every_table_in_order() {
        assert_eq!(
            registry::protocols().collect::<Vec<_>>(),
            ["ocpp1.6", "ocpp2.0.1", "ocpp2.1"]
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
