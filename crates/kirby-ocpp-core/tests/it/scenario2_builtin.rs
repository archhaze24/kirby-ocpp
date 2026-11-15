//! The built-in OCPP 2.0.1 scenarios run against the mock CSMS: each one on its own fresh
//! station, mock and (absent) state file.

use std::time::Duration;

use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::scenario::{CancelToken, builtin, run_scenario};
use tokio::time::timeout;

use super::station2_remote::Rig;

#[tokio::test]
async fn every_builtin_passes_on_a_fresh_station() {
    let all = builtin::all_for(Protocol::V201);
    assert_eq!(all.len(), 8);
    for scenario in all {
        let rig = Rig::new().await;
        let report = timeout(
            Duration::from_secs(60),
            run_scenario(&rig.station, &scenario, &CancelToken::new(), None),
        )
        .await
        .unwrap_or_else(|_| panic!("{} ends", scenario.name));
        assert!(report.passed, "{}: {report:#?}", scenario.name);
        assert_eq!(
            report.steps.len(),
            scenario.steps.len(),
            "{}",
            scenario.name
        );
        rig.stop().await;
    }
}

#[test]
fn builtins_are_named_after_their_key_and_described() {
    for (scenario, (name, _)) in builtin::all_for(Protocol::V201)
        .iter()
        .zip(builtin::SOURCES_V201)
    {
        assert_eq!(scenario.name, *name);
        assert_eq!(scenario.ocpp_version, Some(Protocol::V201));
        assert!(scenario.tags.is_empty(), "{name}");
        assert!(!scenario.description.trim().is_empty(), "{name}");
        assert_eq!(
            scenario.source.as_deref(),
            Some(format!("builtin:{name}").as_str())
        );
    }
}

#[test]
fn lookups_are_per_protocol_and_finish_session_is_16_only() {
    assert!(builtin::get_for("finish-session", Protocol::V16).is_some());
    assert!(builtin::get_for("finish-session", Protocol::V201).is_none());
    assert!(builtin::source_for("finish-session", Protocol::V201).is_none());
    for scenario in builtin::all() {
        assert_ne!(
            scenario.ocpp_version,
            Some(Protocol::V201),
            "{}",
            scenario.name
        );
    }
    let v201 = builtin::get_for("charge_session", Protocol::V201).expect("2.0.1 charge-session");
    assert_eq!(v201.ocpp_version, Some(Protocol::V201));
    let v16 = builtin::get_for("charge-session", Protocol::V16).expect("1.6 charge-session");
    assert_ne!(v16, v201);
    assert_ne!(
        builtin::source_for("charge-session", Protocol::V201),
        builtin::source("charge-session")
    );
}
