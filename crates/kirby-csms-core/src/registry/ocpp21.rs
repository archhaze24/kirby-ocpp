//! The OCPP 2.1 table of the action registry: the 55 actions a central system sends and the
//! 36 actions a charge station sends as a CALL.
//!
//! OCPP 2.1 keeps every 2.0.1 action, so the table is the [2.0.1 one](super::ocpp201) with
//! the entries (and their forms) reused as they are, plus 15 central-system actions that
//! only 2.1 has. Of those, `SetDERControl` has no form (its payload is sent raw) and
//! `GetPeriodicEventStream` takes no input; the others have forms. The 12 station actions 2.1
//! added are 11 CALLs and the SEND message `NotifyPeriodicEventStream`, which is not a CALL
//! and so has no entry in the list of actions a station sends.
//! The names follow the payload layer (`kirby_ocpp_core::ocpp::v2::registry`).

use super::ocpp201::{
    self, ID_TOKEN_TYPES, action, choice, field, json, number, raw, text, with_default, with_hint,
};
use super::{ActionSpec, FieldSpec, FieldType, ProtocolActions};

const fn boolean(name: &'static str, label: &'static str, required: bool) -> FieldSpec {
    field(name, label, FieldType::Boolean, required)
}

/// A transaction of the station, picked from the running ones.
const fn transaction(required: bool) -> FieldSpec {
    choice("transactionId", "transaction", &[], required)
}

const DER_CONTROL_TYPES: &[&str] = &[
    "EnterService",
    "FreqDroop",
    "FreqWatt",
    "FixedPFAbsorb",
    "FixedPFInject",
    "FixedVar",
    "Gradients",
    "HFMustTrip",
    "HFMayTrip",
    "HVMustTrip",
    "HVMomCess",
    "HVMayTrip",
    "LimitMaxDischarge",
    "LFMustTrip",
    "LVMustTrip",
    "LVMomCess",
    "LVMayTrip",
    "PowerMonitoringMustTrip",
    "VoltVar",
    "VoltWatt",
    "WattPF",
    "WattVar",
];
const DER_CONTROL_ID: FieldSpec = with_hint(
    text("controlId", "controlId (optional)", false),
    "at most 36 characters",
);
const DER_CONTROL_TYPE: FieldSpec = choice(
    "controlType",
    "controlType (optional)",
    DER_CONTROL_TYPES,
    false,
);
/// A small valid tariff to start from: a price per kWh.
const TARIFF_EXAMPLE: &str =
    r#"{"tariffId":"T1","currency":"EUR","energy":{"prices":[{"priceKwh":0.3}]}}"#;
const TARIFF: FieldSpec = with_default(
    with_hint(
        json("tariff", "tariff", true),
        "a TariffType: tariffId, currency, prices",
    ),
    TARIFF_EXAMPLE,
);
/// The EVSE of a tariff action, where 0 stands for every EVSE.
const TARIFF_EVSE: FieldSpec = with_default(
    with_hint(number("evseId", "evse (0 = all)", true), "0 for every EVSE"),
    "0",
);

/// The central-system actions OCPP 2.1 added, sorted by name.
const NEW_CSMS_ACTIONS: [ActionSpec; 15] = [
    action(
        "AFRRSignal",
        "aFRR signal",
        "AFRRSignal: send an aFRR balancing signal to the station",
        &[
            with_hint(
                field("timestamp", "timestamp", FieldType::Timestamp, true),
                "when the signal takes effect",
            ),
            with_hint(
                json("signal", "signal", true),
                "a whole number, may be negative",
            ),
        ],
    ),
    action(
        "AdjustPeriodicEventStream",
        "Adjust event stream",
        "AdjustPeriodicEventStream: change the interval or size of an open stream",
        &[
            with_hint(
                number("id", "stream id", true),
                "Get event streams lists the open ones",
            ),
            with_hint(
                number("params.interval", "interval (s, optional)", false),
                "set the interval, the values or both",
            ),
            with_hint(
                number("params.values", "values (optional)", false),
                "values per message",
            ),
        ],
    ),
    action(
        "ChangeTransactionTariff",
        "Change transaction tariff",
        "ChangeTransactionTariff: replace the tariff of a running transaction",
        &[transaction(true), TARIFF],
    ),
    action(
        "ClearDERControl",
        "Clear DER control",
        "ClearDERControl: remove default or scheduled DER controls",
        &[
            with_default(
                with_hint(
                    boolean("isDefault", "isDefault", true),
                    "true: default controls; false: scheduled ones",
                ),
                "false",
            ),
            DER_CONTROL_TYPE,
            DER_CONTROL_ID,
        ],
    ),
    action(
        "ClearTariffs",
        "Clear tariffs",
        "ClearTariffs: remove tariffs from the station (all of them without input)",
        &[
            with_hint(
                json("tariffIds", "tariffIds (optional)", false),
                r#"JSON array, e.g. ["T1"]; omit for all"#,
            ),
            with_hint(
                number("evseId", "evse (optional)", false),
                "0 for station-wide tariffs; omit for all",
            ),
        ],
    ),
    action(
        "GetDERControl",
        "Get DER control",
        "GetDERControl: the station reports the DER controls it holds",
        &[
            number("requestId", "requestId", true),
            with_hint(
                boolean("isDefault", "isDefault (optional)", false),
                "true: default controls; false: scheduled ones",
            ),
            DER_CONTROL_TYPE,
            DER_CONTROL_ID,
        ],
    ),
    action(
        "GetPeriodicEventStream",
        "Get event streams",
        "GetPeriodicEventStream: list the open periodic event streams (no input)",
        &[],
    ),
    action(
        "GetTariffs",
        "Get tariffs",
        "GetTariffs: the tariffs installed for an EVSE or for all of them",
        &[TARIFF_EVSE],
    ),
    action(
        "NotifyAllowedEnergyTransfer",
        "Allowed energy transfer",
        "NotifyAllowedEnergyTransfer: the energy transfer modes a transaction may use",
        &[
            transaction(true),
            with_default(
                with_hint(
                    json("allowedEnergyTransfer", "allowed modes", true),
                    r#"JSON array, e.g. ["AC_three_phase","DC"]"#,
                ),
                r#"["AC_three_phase"]"#,
            ),
        ],
    ),
    action(
        "NotifyWebPaymentStarted",
        "Web payment started",
        "NotifyWebPaymentStarted: tell the station a web payment began on an EVSE",
        &[
            with_default(number("evseId", "evse", true), "1"),
            with_default(
                with_hint(
                    number("timeout", "timeout (s)", true),
                    "how long the payment may take",
                ),
                "60",
            ),
        ],
    ),
    action(
        "RequestBatterySwap",
        "Request battery swap",
        "RequestBatterySwap: ask the station to start a battery swap for a token",
        &[
            with_hint(
                text("idToken.idToken", "idToken", true),
                "at most 255 characters",
            ),
            with_default(
                choice("idToken.type", "idToken type", ID_TOKEN_TYPES, true),
                "ISO14443",
            ),
            number("requestId", "requestId", true),
        ],
    ),
    raw(
        "SetDERControl",
        "Set DER control",
        "SetDERControl: install a DER control (raw payload)",
    ),
    action(
        "SetDefaultTariff",
        "Set default tariff",
        "SetDefaultTariff: the tariff of an EVSE, or of every EVSE",
        &[TARIFF_EVSE, TARIFF],
    ),
    action(
        "UpdateDynamicSchedule",
        "Update dynamic schedule",
        "UpdateDynamicSchedule: new values for a Dynamic charging profile",
        &[
            with_hint(
                number("chargingProfileId", "profile id", true),
                "a Dynamic profile installed on the station",
            ),
            with_hint(
                json("scheduleUpdate.limit", "limit (optional)", false),
                "a number in the profile's unit; set at least one",
            ),
            with_hint(
                json("scheduleUpdate.setpoint", "setpoint (optional)", false),
                "a number in the profile's unit",
            ),
            with_hint(
                json(
                    "scheduleUpdate.dischargeLimit",
                    "discharge limit (optional)",
                    false,
                ),
                "a number, 0 or below",
            ),
            with_hint(
                json(
                    "scheduleUpdate.setpointReactive",
                    "reactive setpoint (optional)",
                    false,
                ),
                "a number",
            ),
        ],
    ),
    action(
        "UsePriorityCharging",
        "Use priority charging",
        "UsePriorityCharging: turn priority charging of a transaction on or off",
        &[
            transaction(true),
            with_default(
                with_hint(
                    boolean("activate", "activate", true),
                    "false ends priority charging",
                ),
                "true",
            ),
        ],
    ),
];

/// The station-to-CSMS CALLs OCPP 2.1 added, sorted by name.
const NEW_STATION_ACTIONS: [&str; 11] = [
    "BatterySwap",
    "ClosePeriodicEventStream",
    "GetCertificateChainStatus",
    "NotifyDERAlarm",
    "NotifyDERStartStop",
    "NotifyPriorityCharging",
    "NotifySettlement",
    "OpenPeriodicEventStream",
    "PullDynamicScheduleUpdate",
    "ReportDERControl",
    "VatNumberValidation",
];

const CSMS_COUNT: usize = ocpp201::CSMS_ACTIONS.len() + NEW_CSMS_ACTIONS.len();
const STATION_COUNT: usize = ocpp201::STATION_ACTIONS.len() + NEW_STATION_ACTIONS.len();

/// The 2.0.1 central-system actions and the new ones, merged by name.
const CSMS_ACTIONS: [ActionSpec; CSMS_COUNT] = merge_specs();
/// The 2.0.1 station actions and the new ones, merged by name.
const STATION_ACTIONS: [&str; STATION_COUNT] = merge_names();

/// The OCPP 2.1 table.
pub const ACTIONS: ProtocolActions = ProtocolActions {
    protocol: "ocpp2.1",
    csms_actions: &CSMS_ACTIONS,
    station_actions: &STATION_ACTIONS,
};

/// Whether `a` sorts before `b` byte by byte (`str` comparison is not `const`).
const fn name_before(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut index = 0;
    while index < a.len() && index < b.len() {
        if a[index] != b[index] {
            return a[index] < b[index];
        }
        index += 1;
    }
    a.len() < b.len()
}

const fn merge_specs() -> [ActionSpec; CSMS_COUNT] {
    let old = ocpp201::CSMS_ACTIONS;
    let mut merged = [NEW_CSMS_ACTIONS[0]; CSMS_COUNT];
    let (mut from_old, mut from_new, mut next) = (0, 0, 0);
    while next < CSMS_COUNT {
        let take_old = from_new == NEW_CSMS_ACTIONS.len()
            || (from_old < old.len()
                && name_before(old[from_old].name, NEW_CSMS_ACTIONS[from_new].name));
        if take_old {
            merged[next] = if name_before(old[from_old].name, "Reset")
                || name_before("Reset", old[from_old].name)
            {
                old[from_old]
            } else {
                ocpp201::RESET_21
            };
            from_old += 1;
        } else {
            merged[next] = NEW_CSMS_ACTIONS[from_new];
            from_new += 1;
        }
        next += 1;
    }
    merged
}

const fn merge_names() -> [&'static str; STATION_COUNT] {
    let old = ocpp201::STATION_ACTIONS;
    let mut merged = [NEW_STATION_ACTIONS[0]; STATION_COUNT];
    let (mut from_old, mut from_new, mut next) = (0, 0, 0);
    while next < STATION_COUNT {
        let take_old = from_new == NEW_STATION_ACTIONS.len()
            || (from_old < old.len() && name_before(old[from_old], NEW_STATION_ACTIONS[from_new]));
        if take_old {
            merged[next] = old[from_old];
            from_old += 1;
        } else {
            merged[next] = NEW_STATION_ACTIONS[from_new];
            from_new += 1;
        }
        next += 1;
    }
    merged
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use kirby_ocpp_core::ocpp::v2::registry as core;
    use kirby_ocpp_core::ocpp::{Protocol, SchemaRegistry};

    use super::super::ocpp201::tests::assert_forms_match_schemas;
    use super::*;
    use crate::registry;

    fn is_sorted_and_unique(names: &[&str]) -> bool {
        names.windows(2).all(|pair| pair[0] < pair[1])
    }

    #[test]
    fn csms_actions_are_the_ones_a_csms_may_send() {
        let expected: BTreeSet<&str> = core::actions_for(Protocol::V21)
            .iter()
            .filter(|entry| entry.direction.csms_may_send())
            .map(|entry| entry.name)
            .collect();
        let names: Vec<&str> = CSMS_ACTIONS.iter().map(|spec| spec.name).collect();
        assert_eq!(names.len(), 55);
        assert_eq!(names.iter().copied().collect::<BTreeSet<_>>(), expected);
        assert!(is_sorted_and_unique(&names));
    }

    #[test]
    fn station_actions_are_the_calls_a_station_may_send() {
        let expected: BTreeSet<&str> = core::actions_for(Protocol::V21)
            .iter()
            .filter(|entry| entry.direction.station_may_send() && !entry.send_only)
            .map(|entry| entry.name)
            .collect();
        assert_eq!(STATION_ACTIONS.len(), 36);
        assert_eq!(
            STATION_ACTIONS.iter().copied().collect::<BTreeSet<_>>(),
            expected
        );
        assert!(is_sorted_and_unique(&STATION_ACTIONS));
        assert!(!STATION_ACTIONS.contains(&"NotifyPeriodicEventStream"));
        assert!(
            ocpp201::STATION_ACTIONS
                .iter()
                .all(|name| STATION_ACTIONS.contains(name))
        );
    }

    #[test]
    fn the_2_0_1_entries_are_reused_unchanged() {
        for spec in ocpp201::CSMS_ACTIONS.iter().filter(|s| s.name != "Reset") {
            assert_eq!(registry::csms_action("ocpp2.1", spec.name), Some(spec));
        }
        let reset = registry::csms_action("ocpp2.1", "Reset").expect("Reset");
        assert_eq!(
            reset.field("type").map(|f| f.choices),
            Some(&["Immediate", "OnIdle", "ImmediateAndResume"][..])
        );
        let old = registry::csms_action("ocpp2.0.1", "Reset").expect("Reset");
        assert_eq!(
            old.field("type").map(|f| f.choices),
            Some(&["Immediate", "OnIdle"][..])
        );
        for spec in &NEW_CSMS_ACTIONS {
            // Only SetDERControl is raw; the summary says so.
            let is_raw = spec.name == "SetDERControl";
            assert_eq!(
                spec.summary.contains("raw payload"),
                is_raw,
                "{}",
                spec.name
            );
            let no_input = spec.name == "GetPeriodicEventStream";
            assert_eq!(spec.fields.is_empty(), is_raw || no_input, "{}", spec.name);
            assert!(
                ocpp201::CSMS_ACTIONS
                    .iter()
                    .all(|old| old.name != spec.name),
                "{}",
                spec.name
            );
        }
    }

    #[test]
    fn forms_match_the_2_1_request_schemas() {
        let forms: Vec<ActionSpec> = CSMS_ACTIONS
            .iter()
            .copied()
            .filter(|spec| !spec.fields.is_empty())
            .collect();
        // The 20 forms of 2.0.1 and 13 of the actions new in 2.1.
        assert_eq!(forms.len(), 33);
        assert_forms_match_schemas(Protocol::V21, &forms);
    }

    #[test]
    fn every_action_has_a_request_schema() {
        let schemas = SchemaRegistry::for_protocol(Protocol::V21).unwrap();
        for spec in &CSMS_ACTIONS {
            assert!(schemas.knows_action(spec.name), "{}", spec.name);
        }
        for name in &STATION_ACTIONS {
            assert!(schemas.knows_action(name), "{name}");
        }
    }

    #[test]
    fn protocols_list_the_three_tables_in_order() {
        assert_eq!(
            registry::protocols().collect::<Vec<_>>(),
            ["ocpp1.6", "ocpp2.0.1", "ocpp2.1"]
        );
        assert_eq!(
            registry::protocol_actions("ocpp2.1").map(|table| table.csms_actions.len()),
            Some(55)
        );
        assert!(registry::csms_action("ocpp2.1", "RequestStartTransaction").is_some());
        assert!(registry::csms_action("ocpp2.1", "RequestBatterySwap").is_some());
        assert!(registry::csms_action("ocpp2.0.1", "RequestBatterySwap").is_none());
        assert!(registry::csms_action("ocpp2.1", "RemoteStartTransaction").is_none());
    }

    #[test]
    fn station_action_lookup_is_per_protocol() {
        assert!(registry::is_station_action("ocpp2.1", "TransactionEvent"));
        assert!(registry::is_station_action("ocpp2.1", "NotifySettlement"));
        assert!(!registry::is_station_action(
            "ocpp2.0.1",
            "NotifySettlement"
        ));
        assert!(!registry::is_station_action("ocpp2.1", "StartTransaction"));
        assert!(!registry::is_station_action(
            "ocpp2.1",
            "RequestBatterySwap"
        ));
    }
}
