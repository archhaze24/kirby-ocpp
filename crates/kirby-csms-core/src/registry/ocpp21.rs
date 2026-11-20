//! The OCPP 2.1 table of the action registry: the 55 actions a central system sends and the
//! 36 actions a charge station sends as a CALL.
//!
//! OCPP 2.1 keeps every 2.0.1 action, so the table is the [2.0.1 one](super::ocpp201) with
//! the entries (and their forms) reused as they are, plus 15 central-system actions that
//! only 2.1 has. Those have no form: their payload is sent raw. The 12 station actions 2.1
//! added are 11 CALLs and the SEND message `NotifyPeriodicEventStream`, which is not a CALL
//! and so has no entry in the list of actions a station sends.
//! The names follow the payload layer (`kirby_ocpp_core::ocpp::v2::registry`).

use super::ocpp201::{self, raw};
use super::{ActionSpec, ProtocolActions};

/// The central-system actions OCPP 2.1 added, sorted by name.
const NEW_CSMS_ACTIONS: [ActionSpec; 15] = [
    raw(
        "AFRRSignal",
        "aFRR signal",
        "AFRRSignal: send an aFRR balancing signal to the station (raw payload)",
    ),
    raw(
        "AdjustPeriodicEventStream",
        "Adjust event stream",
        "AdjustPeriodicEventStream: change the parameters of an open stream (raw payload)",
    ),
    raw(
        "ChangeTransactionTariff",
        "Change transaction tariff",
        "ChangeTransactionTariff: replace the tariff of a running transaction (raw payload)",
    ),
    raw(
        "ClearDERControl",
        "Clear DER control",
        "ClearDERControl: remove DER controls from the station (raw payload)",
    ),
    raw(
        "ClearTariffs",
        "Clear tariffs",
        "ClearTariffs: remove tariffs from the station (raw payload)",
    ),
    raw(
        "GetDERControl",
        "Get DER control",
        "GetDERControl: ask for the DER controls the station holds (raw payload)",
    ),
    raw(
        "GetPeriodicEventStream",
        "Get event streams",
        "GetPeriodicEventStream: list the open periodic event streams (raw payload)",
    ),
    raw(
        "GetTariffs",
        "Get tariffs",
        "GetTariffs: ask for the tariffs of an EVSE (raw payload)",
    ),
    raw(
        "NotifyAllowedEnergyTransfer",
        "Allowed energy transfer",
        "NotifyAllowedEnergyTransfer: set the energy transfer modes of a transaction (raw payload)",
    ),
    raw(
        "NotifyWebPaymentStarted",
        "Web payment started",
        "NotifyWebPaymentStarted: tell the station a web payment began on an EVSE (raw payload)",
    ),
    raw(
        "RequestBatterySwap",
        "Request battery swap",
        "RequestBatterySwap: ask the station to start a battery swap (raw payload)",
    ),
    raw(
        "SetDERControl",
        "Set DER control",
        "SetDERControl: install a DER control (raw payload)",
    ),
    raw(
        "SetDefaultTariff",
        "Set default tariff",
        "SetDefaultTariff: set the default tariff of an EVSE (raw payload)",
    ),
    raw(
        "UpdateDynamicSchedule",
        "Update dynamic schedule",
        "UpdateDynamicSchedule: update a dynamic charging profile (raw payload)",
    ),
    raw(
        "UsePriorityCharging",
        "Use priority charging",
        "UsePriorityCharging: activate or cancel priority charging of a transaction (raw payload)",
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
            merged[next] = old[from_old];
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
        for spec in ocpp201::CSMS_ACTIONS {
            assert_eq!(registry::csms_action("ocpp2.1", spec.name), Some(spec));
        }
        for spec in &NEW_CSMS_ACTIONS {
            assert!(spec.fields.is_empty(), "{}", spec.name);
            assert!(spec.summary.contains("raw payload"), "{}", spec.name);
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
    fn typed_forms_match_the_2_1_request_schemas() {
        let typed: Vec<ActionSpec> = CSMS_ACTIONS
            .iter()
            .copied()
            .filter(|spec| {
                core::lookup_for(Protocol::V21, spec.name).is_some_and(|entry| entry.typed)
            })
            .collect();
        assert_eq!(typed.len(), 17);
        assert_forms_match_schemas(Protocol::V21, &typed);
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
