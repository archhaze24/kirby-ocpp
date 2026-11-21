//! Smart charging of the OCPP 2.x station: `SetChargingProfile`, `ClearChargingProfile`,
//! `GetChargingProfiles`, `GetCompositeSchedule`, a profile in `RequestStartTransaction`,
//! external limits and the simulated meter (K01, K05, K08 to K13).
//!
//! Tests drive the station through the mock CSMS and the station clock. Waits are bounded by
//! `STEP`; nothing sleeps for a fixed time. The energy test uses a nominal power so high that a
//! few microseconds of charging show up in the register.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Duration, SecondsFormat, Timelike, Utc};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::frame::Frame;
use kirby_ocpp_core::ocpp::v2::types::{ChargingState, IdTokenType, StopReason};
use kirby_ocpp_core::station::{Direction, FrameRecord};
use kirby_ocpp_core::store::{FileKind, LoadOutcome2, PersistedStation2, StationStore};
use kirby_ocpp_core::{
    Clock, Command2, ManualClock, Outcome2, Station2, Station2Handle, StationConfig, StationError,
};
use kirby_ocpp_testkit::{CsmsOptions, ErrorCode, MockCsms, Reply, SubprotocolPolicy};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::station2_remote::{Frames, Rig};
use super::station2_support::{STEP, v201_csms, wait_booted, wait_snapshot};

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// An absolute profile with one period that started ten seconds before `now`.
fn profile(
    now: DateTime<Utc>,
    id: i64,
    stack: i64,
    purpose: &str,
    unit: &str,
    limit: f64,
) -> Value {
    json!({
        "id": id,
        "stackLevel": stack,
        "chargingProfilePurpose": purpose,
        "chargingProfileKind": "Absolute",
        "chargingSchedule": [{
            "id": 1,
            "startSchedule": stamp(now - Duration::seconds(10)),
            "chargingRateUnit": unit,
            "chargingSchedulePeriod": [{"startPeriod": 0, "limit": limit}],
        }],
    })
}

fn external_schedule(unit: &str, limit: f64) -> Value {
    json!({
        "id": 1,
        "chargingRateUnit": unit,
        "chargingSchedulePeriod": [{"startPeriod": 0, "limit": limit}],
    })
}

fn relative_zero_profile(now: DateTime<Utc>) -> Value {
    let mut raw = profile(now, 42, 0, "TxProfile", "W", 0.0);
    raw["chargingProfileKind"] = json!("Relative");
    let schedule = raw["chargingSchedule"][0].as_object_mut().unwrap();
    schedule.remove("startSchedule");
    schedule.insert("duration".to_owned(), json!(60));
    raw
}

fn is_call(record: &FrameRecord, action: &str) -> bool {
    record.direction == Direction::Out
        && record.frame[0] == 2
        && record.action.as_deref() == Some(action)
}

fn is_result(record: &FrameRecord, action: &str) -> bool {
    record.direction == Direction::Out
        && record.frame[0] == 3
        && record.action.as_deref() == Some(action)
}

impl Rig {
    fn now(&self) -> DateTime<Utc> {
        self.clock.now_utc()
    }
}

async fn set_profile(rig: &Rig, evse: i64, profile: Value) -> Value {
    rig.call(
        "SetChargingProfile",
        json!({"evseId": evse, "chargingProfile": profile}),
    )
    .await
}

async fn set_accepted(rig: &Rig, evse: i64, profile: Value) {
    let reply = set_profile(rig, evse, profile).await;
    assert_eq!(reply["status"], "Accepted", "{reply}");
}

fn reason_of(reply: &Value) -> (&str, &str) {
    (
        reply["status"].as_str().unwrap_or_default(),
        reply["statusInfo"]["reasonCode"]
            .as_str()
            .unwrap_or_default(),
    )
}

async fn variable(rig: &Rig, name: &str, instance: Option<&str>) -> String {
    let mut variable = json!({"name": name});
    if let Some(instance) = instance {
        variable["instance"] = json!(instance);
    }
    let reply = rig
        .call(
            "GetVariables",
            json!({"getVariableData": [{
                "component": {"name": "SmartChargingCtrlr"},
                "variable": variable,
            }]}),
        )
        .await;
    reply["getVariableResult"][0]["attributeValue"]
        .as_str()
        .unwrap_or_else(|| panic!("no value in {reply}"))
        .to_owned()
}

/// A round trip: everything the station sent before it has reached the CSMS afterwards.
async fn settle(rig: &Rig) {
    rig.call("GetLocalListVersion", json!({})).await;
}

/// Sends `GetChargingProfiles` and collects the `count` reports that follow.
async fn get_profiles(
    rig: &Rig,
    criteria: Value,
    evse: Option<i64>,
    count: usize,
) -> (Value, Vec<Value>) {
    let before = rig.calls("ReportChargingProfiles").len();
    let mut body = json!({"requestId": 9, "chargingProfile": criteria});
    if let Some(evse) = evse {
        body["evseId"] = json!(evse);
    }
    let reply = rig.call("GetChargingProfiles", body).await;
    if count > 0 {
        rig.wait_calls("ReportChargingProfiles", before + count)
            .await;
    }
    settle(rig).await;
    let reports = rig.calls("ReportChargingProfiles").split_off(before);
    assert_eq!(reports.len(), count, "{reports:#?}");
    (reply, reports)
}

fn profile_ids(report: &Value) -> Vec<i64> {
    report["chargingProfile"]
        .as_array()
        .map(|profiles| profiles.iter().filter_map(|p| p["id"].as_i64()).collect())
        .unwrap_or_default()
}

// ---- SmartChargingCtrlr ------------------------------------------------------------------

#[tokio::test]
async fn the_variables_describe_smart_charging() {
    let rig = Rig::new().await;
    assert_eq!(variable(&rig, "Enabled", None).await, "true");
    assert_eq!(variable(&rig, "Available", None).await, "true");
    assert_eq!(variable(&rig, "ProfileStackLevel", None).await, "10");
    assert_eq!(variable(&rig, "PeriodsPerSchedule", None).await, "24");
    assert_eq!(variable(&rig, "RateUnit", None).await, "A,W");
    assert_eq!(
        variable(&rig, "ExternalControlSignalsEnabled", None).await,
        "true"
    );
    assert_eq!(
        variable(&rig, "Entries", Some("ChargingProfiles")).await,
        "0"
    );
    let now = rig.now();
    set_accepted(&rig, 1, profile(now, 1, 0, "TxDefaultProfile", "A", 6.0)).await;
    assert_eq!(
        variable(&rig, "Entries", Some("ChargingProfiles")).await,
        "1"
    );
    let reply = rig
        .call(
            "SetVariables",
            json!({"setVariableData": [{
                "attributeValue": "false",
                "component": {"name": "SmartChargingCtrlr"},
                "variable": {"name": "Available"},
            }]}),
        )
        .await;
    assert_eq!(reply["setVariableResult"][0]["attributeStatus"], "Rejected");
    rig.stop().await;
}

#[tokio::test]
async fn smart_charging_off_answers_not_supported() {
    let rig = Rig::new().await;
    rig.set_variable("SmartChargingCtrlr", "Enabled", "false")
        .await;
    let now = rig.now();
    let body = json!({
        "evseId": 1,
        "chargingProfile": profile(now, 1, 0, "TxDefaultProfile", "A", 6.0),
    });
    rig.csms
        .expect_call_error("SetChargingProfile", body, &ErrorCode::NotSupported)
        .await
        .expect("NotSupported");
    rig.csms
        .expect_call_error("ClearChargingProfile", json!({}), &ErrorCode::NotSupported)
        .await
        .expect("NotSupported");
    rig.csms
        .expect_call_error(
            "GetChargingProfiles",
            json!({"requestId": 1, "chargingProfile": {}}),
            &ErrorCode::NotSupported,
        )
        .await
        .expect("NotSupported");
    rig.csms
        .expect_call_error(
            "GetCompositeSchedule",
            json!({"duration": 60, "evseId": 1}),
            &ErrorCode::NotSupported,
        )
        .await
        .expect("NotSupported");
    rig.set_variable("SmartChargingCtrlr", "Enabled", "true")
        .await;
    set_accepted(&rig, 1, profile(now, 1, 0, "TxDefaultProfile", "A", 6.0)).await;
    rig.stop().await;
}

// ---- K01: SetChargingProfile ---------------------------------------------------------------

#[tokio::test]
async fn a_profile_is_accepted_replaced_and_reported_as_it_arrived() {
    let rig = Rig::new().await;
    let now = rig.now();
    let mut first = profile(now, 7, 0, "TxDefaultProfile", "A", 6.0);
    first["customData"] = json!({"vendorId": "acme", "note": [1, null]});
    set_accepted(&rig, 1, first.clone()).await;
    let (reply, reports) = get_profiles(&rig, json!({}), Some(1), 1).await;
    assert_eq!(reply["status"], "Accepted");
    assert_eq!(reports[0]["chargingProfile"], json!([first]));
    // The same id replaces the profile, on the same EVSE or another one (K01.FR.05).
    let second = profile(now, 7, 0, "TxDefaultProfile", "A", 9.0);
    set_accepted(&rig, 2, second.clone()).await;
    assert_eq!(
        variable(&rig, "Entries", Some("ChargingProfiles")).await,
        "1"
    );
    let (_, reports) = get_profiles(&rig, json!({}), None, 1).await;
    assert_eq!(reports[0]["evseId"], 2);
    assert_eq!(reports[0]["chargingProfile"], json!([second]));
    rig.stop().await;
}

#[tokio::test]
async fn refusals_carry_the_reason_code() {
    let rig = Rig::new().await;
    let now = rig.now();
    let default = |id: i64, stack: i64| profile(now, id, stack, "TxDefaultProfile", "A", 6.0);
    // An EVSE the station does not have.
    let reply = set_profile(&rig, 3, default(1, 0)).await;
    assert_eq!(reason_of(&reply), ("Rejected", "UnknownEVSE"));
    // A TxProfile of a transaction that does not run (K01.FR.09/33).
    let mut tx = profile(now, 2, 0, "TxProfile", "A", 6.0);
    tx["transactionId"] = json!("NOPE");
    let reply = set_profile(&rig, 1, tx.clone()).await;
    assert_eq!(reason_of(&reply), ("Rejected", "TxNotFound"));
    // On all EVSEs at once (K01.FR.16).
    let reply = set_profile(&rig, 0, tx).await;
    assert_eq!(reason_of(&reply), ("Rejected", "InvalidProfile"));
    // The CSMS may not set a constraint of an external system (K01.FR.22).
    let external = profile(now, 3, 0, "ChargingStationExternalConstraints", "A", 6.0);
    let reply = set_profile(&rig, 0, external).await;
    assert_eq!(reason_of(&reply), ("Rejected", "InvalidProfile"));
    // A max profile belongs on EVSE 0.
    let max = profile(now, 4, 0, "ChargingStationMaxProfile", "A", 6.0);
    let reply = set_profile(&rig, 1, max).await;
    assert_eq!(reason_of(&reply), ("Rejected", "InvalidProfile"));
    // A stack level above the limit.
    let reply = set_profile(&rig, 1, default(5, 11)).await;
    assert_eq!(reason_of(&reply), ("Rejected", "InvalidStackLevel"));
    // More periods than the schedule may have.
    rig.set_variable("SmartChargingCtrlr", "PeriodsPerSchedule", "1")
        .await;
    let mut long = default(6, 0);
    long["chargingSchedule"][0]["chargingSchedulePeriod"] =
        json!([{"startPeriod": 0, "limit": 6}, {"startPeriod": 60, "limit": 8}]);
    let reply = set_profile(&rig, 1, long).await;
    assert_eq!(reason_of(&reply), ("Rejected", "TooManyElements"));
    // A rate unit the station does not offer (K01.FR.26).
    rig.set_variable("SmartChargingCtrlr", "RateUnit", "A")
        .await;
    let watts = profile(now, 7, 0, "TxDefaultProfile", "W", 6000.0);
    let reply = set_profile(&rig, 1, watts).await;
    assert_eq!(reason_of(&reply), ("Rejected", "UnsupportedRateUnit"));
    // A phase to use, and more phases than the station has.
    let mut phase = default(8, 0);
    phase["chargingSchedule"][0]["chargingSchedulePeriod"][0]["phaseToUse"] = json!(1);
    assert_eq!(
        reason_of(&set_profile(&rig, 1, phase).await),
        ("Rejected", "InvalidSchedule")
    );
    let mut phases = default(9, 0);
    phases["chargingSchedule"][0]["chargingSchedulePeriod"][0]["numberPhases"] = json!(3);
    assert_eq!(
        reason_of(&set_profile(&rig, 1, phases).await),
        ("Rejected", "InvalidSchedule")
    );
    // Nothing was installed by any of this.
    assert_eq!(
        variable(&rig, "Entries", Some("ChargingProfiles")).await,
        "0"
    );
    rig.stop().await;
}

#[tokio::test]
async fn a_tx_default_stack_level_is_not_shared_between_evse_0_and_an_evse() {
    let rig = Rig::new().await;
    let now = rig.now();
    set_accepted(&rig, 1, profile(now, 1, 1, "TxDefaultProfile", "A", 6.0)).await;
    let clash = profile(now, 2, 1, "TxDefaultProfile", "A", 8.0);
    let reply = set_profile(&rig, 0, clash.clone()).await;
    assert_eq!(reason_of(&reply), ("Rejected", "DuplicateProfile"));
    // The same id is a replacement (K01.FR.52/53).
    let mut same = clash;
    same["id"] = json!(1);
    set_accepted(&rig, 0, same).await;
    assert_eq!(
        variable(&rig, "Entries", Some("ChargingProfiles")).await,
        "1"
    );
    rig.stop().await;
}

#[tokio::test]
async fn the_store_holds_ten_profiles() {
    let rig = Rig::new().await;
    let now = rig.now();
    for stack in 0..10 {
        set_accepted(
            &rig,
            1,
            profile(now, stack + 1, stack, "TxDefaultProfile", "A", 6.0),
        )
        .await;
    }
    let reply = set_profile(&rig, 1, profile(now, 50, 10, "TxDefaultProfile", "A", 6.0)).await;
    assert_eq!(reason_of(&reply), ("Rejected", "TooManyElements"));
    // A replacement needs no room.
    set_accepted(&rig, 1, profile(now, 60, 4, "TxDefaultProfile", "A", 9.0)).await;
    assert_eq!(
        variable(&rig, "Entries", Some("ChargingProfiles")).await,
        "10"
    );
    rig.stop().await;
}

#[tokio::test]
async fn profile_admission_caps_serialized_size() {
    let rig = Rig::new().await;
    let mut raw = profile(rig.now(), 1, 0, "TxDefaultProfile", "W", 500.0);
    raw["customData"] = json!({"vendorId": "test", "data": "x".repeat(70 * 1024)});
    let refused = set_profile(&rig, 1, raw.clone()).await;
    assert_eq!(reason_of(&refused), ("Rejected", "TooLarge"));
    assert_eq!(
        get_profiles(&rig, json!({}), Some(1), 0).await.0["status"],
        "NoProfiles"
    );
    raw["chargingProfilePurpose"] = json!("TxProfile");
    let refused = rig
        .call("RequestStartTransaction", start_body(1, Some(raw.clone())))
        .await;
    assert_eq!(reason_of(&refused), ("Rejected", "TooLarge"));
    assert!(matches!(
        rig.station.set_external_limit(1, raw.clone()).await,
        Err(StationError::InvalidArgument(message)) if message.contains("TooLarge")
    ));
    raw["chargingProfilePurpose"] = json!("TxDefaultProfile");
    raw["customData"]["data"] = json!("");
    let overhead = raw.to_string().len();
    raw["customData"]["data"] = json!("x".repeat(64 * 1024 - overhead - 1));
    set_accepted(&rig, 1, raw).await;
    rig.stop().await;
}

// ---- K09: GetChargingProfiles ----------------------------------------------------------------

#[tokio::test]
async fn profiles_are_reported_after_the_answer_per_evse_and_source() {
    let rig = Rig::new().await;
    let now = rig.now();
    set_accepted(
        &rig,
        0,
        profile(now, 1, 0, "ChargingStationMaxProfile", "W", 9000.0),
    )
    .await;
    set_accepted(&rig, 1, profile(now, 2, 0, "TxDefaultProfile", "A", 6.0)).await;
    set_accepted(&rig, 1, profile(now, 3, 1, "TxDefaultProfile", "A", 8.0)).await;
    rig.station
        .set_external_limit(2, external_schedule("W", 3000.0))
        .await
        .expect("external limit");
    let mut frames = Frames::new(&rig.station);
    let (reply, reports) = get_profiles(&rig, json!({}), None, 3).await;
    assert_eq!(reply["status"], "Accepted");
    let seen = frames
        .until("the first report", |r| is_call(r, "ReportChargingProfiles"))
        .await;
    assert!(
        seen.iter().any(|r| is_result(r, "GetChargingProfiles")),
        "the answer goes out before the first report: {seen:#?}"
    );
    // By EVSE, then by source; `tbc` on all but the last.
    let shape: Vec<(i64, &str, Vec<i64>, Option<bool>)> = reports
        .iter()
        .map(|r| {
            (
                r["evseId"].as_i64().expect("evse"),
                r["chargingLimitSource"].as_str().expect("source"),
                profile_ids(r),
                r["tbc"].as_bool(),
            )
        })
        .collect();
    assert_eq!(shape[0], (0, "CSO", vec![1], Some(true)));
    assert_eq!(shape[1].0, 1);
    assert_eq!(shape[1].1, "CSO");
    assert_eq!(shape[1].3, Some(true));
    let mut ids = shape[1].2.clone();
    ids.sort_unstable();
    assert_eq!(ids, [2, 3]);
    assert_eq!((shape[2].0, shape[2].1, shape[2].3), (2, "EMS", None));
    assert!(reports.iter().all(|r| r["requestId"] == 9));
    // The external profile is reported with its purpose and a negative id (K11.FR.06).
    assert!(shape[2].2.iter().all(|id| *id < 0));
    assert_eq!(
        reports[2]["chargingProfile"][0]["chargingProfilePurpose"],
        "ChargingStationExternalConstraints"
    );
    rig.stop().await;
}

#[tokio::test]
async fn the_criteria_select_the_profiles_to_report() {
    let rig = Rig::new().await;
    let now = rig.now();
    set_accepted(
        &rig,
        0,
        profile(now, 1, 0, "ChargingStationMaxProfile", "W", 9000.0),
    )
    .await;
    set_accepted(&rig, 1, profile(now, 2, 0, "TxDefaultProfile", "A", 6.0)).await;
    set_accepted(&rig, 1, profile(now, 3, 1, "TxDefaultProfile", "A", 8.0)).await;
    rig.station
        .set_external_limit(1, external_schedule("W", 3000.0))
        .await
        .expect("external limit");
    // EVSE 0 reports its own profiles only (K09.FR.05).
    let (_, reports) = get_profiles(&rig, json!({}), Some(0), 1).await;
    assert_eq!(profile_ids(&reports[0]), [1]);
    assert_eq!(reports[0]["tbc"], Value::Null);
    // An EVSE above 0, narrowed by stack level, purpose, id and source.
    let (_, reports) = get_profiles(&rig, json!({"stackLevel": 1}), Some(1), 1).await;
    assert_eq!(profile_ids(&reports[0]), [3]);
    let (_, reports) = get_profiles(
        &rig,
        json!({"chargingProfilePurpose": "ChargingStationExternalConstraints"}),
        Some(1),
        1,
    )
    .await;
    assert_eq!(reports[0]["chargingLimitSource"], "EMS");
    let (_, reports) = get_profiles(&rig, json!({"chargingProfileId": [1, 2]}), None, 2).await;
    assert_eq!(
        reports
            .iter()
            .map(|r| r["evseId"].as_i64().expect("evse"))
            .collect::<Vec<_>>(),
        [0, 1]
    );
    let (_, reports) = get_profiles(&rig, json!({"chargingLimitSource": ["EMS"]}), None, 1).await;
    assert_eq!(reports[0]["evseId"], 1);
    // Nothing matches: no reports and a status that says so.
    let (reply, _) = get_profiles(&rig, json!({"stackLevel": 5}), None, 0).await;
    assert_eq!(reply["status"], "NoProfiles");
    assert_eq!(reply["statusInfo"]["reasonCode"], "NotFound");
    rig.stop().await;
}

// ---- K08: GetCompositeSchedule ---------------------------------------------------------------

#[tokio::test]
async fn without_profiles_the_composite_schedule_shows_the_nominal_power() {
    let rig = Rig::new().await;
    let reply = rig
        .call(
            "GetCompositeSchedule",
            json!({"duration": 300, "evseId": 0}),
        )
        .await;
    assert_eq!(reply["status"], "Accepted", "{reply}");
    let schedule = &reply["schedule"];
    assert_eq!(schedule["evseId"], 0);
    assert_eq!(schedule["duration"], 300);
    assert_eq!(schedule["chargingRateUnit"], "W");
    assert_eq!(schedule["chargingSchedulePeriod"][0]["startPeriod"], 0);
    assert_eq!(schedule["chargingSchedulePeriod"][0]["limit"], 7200.0);
    // The schedule starts at the whole second of the request.
    let start: DateTime<Utc> = schedule["scheduleStart"]
        .as_str()
        .expect("a start")
        .parse()
        .expect("a time");
    let late = rig.now() - start;
    assert!(
        late >= Duration::zero() && late < Duration::seconds(1),
        "{late}"
    );
    rig.stop().await;
}

#[tokio::test]
async fn the_composite_schedule_stacks_and_caps() {
    let rig = Rig::new().await;
    let now = rig.now();
    set_accepted(&rig, 1, profile(now, 1, 0, "TxDefaultProfile", "A", 10.0)).await;
    let limit_of = |reply: &Value| {
        assert_eq!(reply["status"], "Accepted", "{reply}");
        reply["schedule"]["chargingSchedulePeriod"][0]["limit"]
            .as_f64()
            .expect("a limit")
    };
    let ask = |evse: i64| {
        rig.call(
            "GetCompositeSchedule",
            json!({"duration": 300, "evseId": evse, "chargingRateUnit": "A"}),
        )
    };
    assert!((limit_of(&ask(1).await) - 10.0).abs() < 1e-9);
    // A higher stack level wins (K01.FR.17), another EVSE has no default.
    set_accepted(&rig, 1, profile(now, 2, 1, "TxDefaultProfile", "A", 6.0)).await;
    assert!((limit_of(&ask(1).await) - 6.0).abs() < 1e-9);
    let other = ask(2).await;
    assert!(limit_of(&other) > 6.0, "{other}");
    // The max profile caps the result (K08.FR.04).
    set_accepted(
        &rig,
        0,
        profile(now, 3, 0, "ChargingStationMaxProfile", "A", 4.0),
    )
    .await;
    assert!((limit_of(&ask(1).await) - 4.0).abs() < 1e-9);
    assert!((limit_of(&ask(2).await) - 4.0).abs() < 1e-9);
    // A profile of the other unit does not take part in the composite.
    set_accepted(&rig, 1, profile(now, 4, 2, "TxDefaultProfile", "W", 1000.0)).await;
    assert!((limit_of(&ask(1).await) - 4.0).abs() < 1e-9);
    rig.stop().await;
}

#[tokio::test]
async fn composite_selects_the_unit_of_a_future_profile_in_the_window() {
    let rig = Rig::new().await;
    rig.clock.set(rig.now().with_nanosecond(0).unwrap());
    let start = stamp(rig.now() + Duration::seconds(10));
    let mut raw = profile(rig.now(), 1, 0, "TxDefaultProfile", "W", 500.0);
    raw["validFrom"] = json!(start);
    raw["chargingSchedule"][0]["startSchedule"] = json!(start);
    set_accepted(&rig, 1, raw).await;
    let reply = rig
        .call("GetCompositeSchedule", json!({"evseId": 1, "duration": 60}))
        .await;
    assert_eq!(reply["schedule"]["chargingRateUnit"], "W");
    let period = &reply["schedule"]["chargingSchedulePeriod"][1];
    assert_eq!(period["startPeriod"], 10);
    assert_eq!(period["limit"], 500.0);
    rig.stop().await;
}

#[tokio::test]
async fn composite_refuses_overlong_windows_on_both_protocols() {
    for protocol in [Protocol::V201, Protocol::V21] {
        let (csms, station, join, _) = rig_for(protocol, |_| {}).await;
        let ask = |duration| {
            call(
                &csms,
                "GetCompositeSchedule",
                json!({"evseId": 1, "duration": duration, "chargingRateUnit": "W"}),
            )
        };
        assert_eq!(
            reason_of(&ask(604_801).await),
            ("Rejected", "UnsupportedRequest")
        );
        let reply = ask(604_800).await;
        assert_eq!(reply["status"], "Accepted");
        assert_eq!(reply["schedule"]["duration"], 604_800);
        stop_21(csms, station, join).await;
    }
}

#[tokio::test]
async fn composite_refuses_a_schedule_that_exceeds_the_period_cap() {
    let rig = Rig::new().await;
    rig.set_variable("SmartChargingCtrlr", "PeriodsPerSchedule", "150")
        .await;
    rig.clock.set(rig.now().with_nanosecond(0).unwrap());
    let mut raw = profile(rig.now(), 1, 0, "TxDefaultProfile", "W", 500.0);
    raw["chargingProfileKind"] = json!("Recurring");
    raw["recurrencyKind"] = json!("Daily");
    let schedule = &mut raw["chargingSchedule"][0];
    schedule["startSchedule"] = json!(stamp(rig.now()));
    schedule["chargingSchedulePeriod"] = json!(
        (0..150)
            .map(|offset| { json!({"startPeriod": offset, "limit": 500 + offset % 2}) })
            .collect::<Vec<_>>()
    );
    set_accepted(&rig, 1, raw).await;
    let ask = || {
        rig.call(
            "GetCompositeSchedule",
            json!({"evseId": 1, "duration": 604_800, "chargingRateUnit": "W"}),
        )
    };
    assert_eq!(reason_of(&ask().await), ("Rejected", "UnsupportedRequest"));
    rig.station
        .set_external_limit(1, external_schedule("W", 400.0))
        .await
        .expect("external limit");
    let reply = ask().await;
    assert_eq!(reply["status"], "Accepted", "{reply}");
    assert_eq!(reply["schedule"]["duration"], 604_800);
    assert_eq!(
        reply["schedule"]["chargingSchedulePeriod"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    rig.stop().await;
}

#[tokio::test]
async fn a_composite_schedule_that_cannot_be_made_is_rejected() {
    let rig = Rig::new().await;
    let reply = rig
        .call("GetCompositeSchedule", json!({"duration": 60, "evseId": 3}))
        .await;
    assert_eq!(reason_of(&reply), ("Rejected", "UnknownEVSE"));
    let reply = rig
        .call("GetCompositeSchedule", json!({"duration": 0, "evseId": 1}))
        .await;
    assert_eq!(reply["status"], "Rejected");
    rig.set_variable("SmartChargingCtrlr", "RateUnit", "A")
        .await;
    let reply = rig
        .call(
            "GetCompositeSchedule",
            json!({"duration": 60, "evseId": 1, "chargingRateUnit": "W"}),
        )
        .await;
    assert_eq!(reason_of(&reply), ("Rejected", "UnsupportedRateUnit"));
    rig.stop().await;
}

// ---- K10: ClearChargingProfile ---------------------------------------------------------------

async fn clear(rig: &Rig, body: Value) -> String {
    rig.status_of("ClearChargingProfile", body).await
}

#[tokio::test]
async fn profiles_are_cleared_by_id_and_by_criteria() {
    let rig = Rig::new().await;
    let now = rig.now();
    set_accepted(
        &rig,
        0,
        profile(now, 1, 0, "ChargingStationMaxProfile", "A", 9.0),
    )
    .await;
    set_accepted(&rig, 1, profile(now, 2, 0, "TxDefaultProfile", "A", 6.0)).await;
    set_accepted(&rig, 1, profile(now, 3, 1, "TxDefaultProfile", "A", 8.0)).await;
    set_accepted(&rig, 2, profile(now, 4, 2, "TxDefaultProfile", "A", 8.0)).await;
    assert_eq!(
        clear(&rig, json!({"chargingProfileId": 2})).await,
        "Accepted"
    );
    assert_eq!(
        clear(&rig, json!({"chargingProfileId": 2})).await,
        "Unknown"
    );
    // The criteria are combined with AND.
    let miss = json!({"chargingProfileCriteria": {"evseId": 2, "stackLevel": 1}});
    assert_eq!(clear(&rig, miss).await, "Unknown");
    let hit = json!({"chargingProfileCriteria": {"evseId": 1, "stackLevel": 1}});
    assert_eq!(clear(&rig, hit).await, "Accepted");
    let by_purpose =
        json!({"chargingProfileCriteria": {"chargingProfilePurpose": "ChargingStationMaxProfile"}});
    assert_eq!(clear(&rig, by_purpose).await, "Accepted");
    assert_eq!(
        variable(&rig, "Entries", Some("ChargingProfiles")).await,
        "1"
    );
    // Nothing selected clears what is left.
    assert_eq!(clear(&rig, json!({})).await, "Accepted");
    let reply = rig
        .call("ClearChargingProfile", json!({"chargingProfileId": 4}))
        .await;
    assert_eq!(reason_of(&reply), ("Unknown", "NotFound"));
    rig.stop().await;
}

#[tokio::test]
async fn a_clear_leaves_the_external_constraints_alone() {
    let rig = Rig::new().await;
    let now = rig.now();
    rig.station
        .set_external_limit(0, external_schedule("W", 3000.0))
        .await
        .expect("external limit");
    let (_, reports) = get_profiles(&rig, json!({}), None, 1).await;
    let id = reports[0]["chargingProfile"][0]["id"]
        .as_i64()
        .expect("an id");
    assert_eq!(
        clear(&rig, json!({"chargingProfileId": id})).await,
        "Unknown"
    );
    let by_purpose = json!({"chargingProfileCriteria":
        {"chargingProfilePurpose": "ChargingStationExternalConstraints"}});
    assert_eq!(clear(&rig, by_purpose).await, "Unknown");
    assert_eq!(clear(&rig, json!({})).await, "Unknown");
    // The CSMS cannot replace one either (K01.FR.05).
    let reply = set_profile(
        &rig,
        1,
        profile(now, id, 0, "TxDefaultProfile", "W", 5000.0),
    )
    .await;
    assert_eq!(reason_of(&reply), ("Rejected", "InvalidProfileId"));
    assert_eq!(
        variable(&rig, "Entries", Some("ChargingProfiles")).await,
        "1"
    );
    rig.stop().await;
}

// ---- transactions --------------------------------------------------------------------------

async fn wait_ended(rig: &Rig) {
    rig.wait_event("Ended").await;
}

#[tokio::test]
async fn a_tx_profile_ends_with_its_transaction() {
    let rig = Rig::new().await;
    let now = rig.now();
    rig.start_transaction(1).await;
    let id = rig.transaction_id(1);
    let mut tx = profile(now, 5, 0, "TxProfile", "A", 6.0);
    tx["transactionId"] = json!(id);
    set_accepted(&rig, 1, tx.clone()).await;
    let (_, reports) = get_profiles(
        &rig,
        json!({"chargingProfilePurpose": "TxProfile"}),
        Some(1),
        1,
    )
    .await;
    assert_eq!(reports[0]["chargingProfile"], json!([tx]));
    // The composite of the EVSE follows the TxProfile while the transaction runs.
    let reply = rig
        .call(
            "GetCompositeSchedule",
            json!({"duration": 60, "evseId": 1, "chargingRateUnit": "A"}),
        )
        .await;
    assert_eq!(reply["schedule"]["chargingSchedulePeriod"][0]["limit"], 6.0);
    rig.send(Command2::StopTransaction {
        evse: 1,
        reason: StopReason::Local,
    })
    .await;
    wait_ended(&rig).await;
    let (reply, _) = get_profiles(&rig, json!({}), None, 0).await;
    assert_eq!(reply["status"], "NoProfiles");
    assert_eq!(
        variable(&rig, "Entries", Some("ChargingProfiles")).await,
        "0"
    );
    rig.stop().await;
}

fn start_body(evse: u32, profile: Option<Value>) -> Value {
    let mut body = json!({
        "evseId": evse,
        "idToken": {"idToken": "REMOTE", "type": "Central"},
        "remoteStartId": 42,
    });
    if let Some(profile) = profile {
        body["chargingProfile"] = profile;
    }
    body
}

#[tokio::test]
async fn a_remote_start_installs_its_profile_for_the_new_transaction() {
    let rig = Rig::new().await;
    let now = rig.now();
    rig.send(Command2::PlugIn { evse: 1 }).await;
    let tx = profile(now, 8, 0, "TxProfile", "A", 6.0);
    let reply = rig
        .call("RequestStartTransaction", start_body(1, Some(tx.clone())))
        .await;
    assert_eq!(reply["status"], "Accepted", "{reply}");
    let started = rig.wait_event("Started").await;
    let id = started["transactionInfo"]["transactionId"]
        .as_str()
        .expect("an id")
        .to_owned();
    let (_, reports) = get_profiles(&rig, json!({}), Some(1), 1).await;
    let mut named = tx;
    named["transactionId"] = json!(id);
    assert_eq!(reports[0]["chargingProfile"], json!([named]));
    rig.stop().await;
}

#[tokio::test]
async fn a_pending_remote_start_reserves_a_profile_slot() {
    let rig = Rig::new().await;
    rig.set_variable("TxCtrlr", "TxStartPoint", "PowerPathClosed")
        .await;
    let now = rig.now();
    for stack in 0..9 {
        set_accepted(
            &rig,
            1,
            profile(now, stack + 1, stack, "TxDefaultProfile", "W", 1000.0),
        )
        .await;
    }
    let reply = rig
        .call(
            "RequestStartTransaction",
            start_body(1, Some(relative_zero_profile(now))),
        )
        .await;
    assert_eq!(reply["status"], "Accepted");
    let reply = set_profile(
        &rig,
        1,
        profile(now, 10, 9, "TxDefaultProfile", "W", 1000.0),
    )
    .await;
    assert_eq!(reason_of(&reply), ("Rejected", "TooManyElements"));
    let reply = rig
        .call(
            "RequestStartTransaction",
            start_body(2, Some(profile(now, 11, 0, "TxProfile", "W", 500.0))),
        )
        .await;
    assert_eq!(reason_of(&reply), ("Rejected", "TooManyElements"));
    rig.send(Command2::PlugIn { evse: 1 }).await;
    wait_charging(&rig, 1).await;
    assert_eq!(rig.station.snapshot().evse(1).unwrap().offered_w, Some(0.0));
    rig.stop().await;
}

#[tokio::test]
async fn a_rejected_remote_start_preserves_the_earlier_profile() {
    let rig = Rig::new().await;
    rig.set_variable("AuthCtrlr", "AuthorizeRemoteStart", "true")
        .await;
    rig.set_variable("TxCtrlr", "TxStartPoint", "PowerPathClosed")
        .await;
    rig.csms.respond("Authorize", |_| Reply::Silence);
    let now = rig.now();
    let mut a = start_body(1, Some(relative_zero_profile(now)));
    a["remoteStartId"] = json!(7);
    a["idToken"]["type"] = json!("ISO14443");
    assert_eq!(
        rig.call("RequestStartTransaction", a).await["status"],
        "Accepted"
    );
    rig.wait_calls("Authorize", 1).await;
    let auth = rig
        .csms
        .received()
        .into_iter()
        .find(|call| call.action == "Authorize")
        .expect("authorize");
    let mut b = start_body(1, Some(profile(now, 43, 0, "TxDefaultProfile", "W", 500.0)));
    b["remoteStartId"] = json!(8);
    assert_eq!(
        rig.call("RequestStartTransaction", b).await["status"],
        "Rejected"
    );
    rig.csms
        .send_raw(json!([3, auth.message_id, {"idTokenInfo": {"status": "Accepted"}}]).to_string())
        .expect("authorize response");
    rig.send(Command2::PlugIn { evse: 1 }).await;
    wait_charging(&rig, 1).await;
    let snapshot = rig.station.snapshot();
    let evse = snapshot.evse(1).unwrap();
    assert_eq!(evse.transaction.as_ref().unwrap().remote_start_id, Some(7));
    assert_eq!(evse.offered_w, Some(0.0));
    rig.stop().await;
}

#[tokio::test]
async fn a_remote_start_with_a_profile_that_is_not_valid_is_rejected() {
    let rig = Rig::new().await;
    let now = rig.now();
    rig.send(Command2::PlugIn { evse: 1 }).await;
    // Only a TxProfile (K05.FR.02), and one without a transaction id.
    let default = profile(now, 8, 0, "TxDefaultProfile", "A", 6.0);
    let reply = rig
        .call("RequestStartTransaction", start_body(1, Some(default)))
        .await;
    assert_eq!(reason_of(&reply), ("Rejected", "InvalidProfile"));
    let mut named = profile(now, 8, 0, "TxProfile", "A", 6.0);
    named["transactionId"] = json!("T1");
    let reply = rig
        .call("RequestStartTransaction", start_body(1, Some(named)))
        .await;
    assert_eq!(reason_of(&reply), ("Rejected", "InvalidProfile"));
    let mut shape = profile(now, 8, 0, "TxProfile", "A", 6.0);
    shape["chargingSchedule"][0]["chargingSchedulePeriod"][0]["startPeriod"] = json!(5);
    let reply = rig
        .call("RequestStartTransaction", start_body(1, Some(shape)))
        .await;
    assert_eq!(reason_of(&reply), ("Rejected", "InvalidSchedule"));
    assert_eq!(rig.calls("TransactionEvent").len(), 0);
    // With smart charging off the profile is ignored and the transaction starts (K05.FR.04).
    rig.set_variable("SmartChargingCtrlr", "Enabled", "false")
        .await;
    let tx = profile(now, 9, 0, "TxProfile", "A", 6.0);
    let reply = rig
        .call("RequestStartTransaction", start_body(1, Some(tx)))
        .await;
    assert_eq!(reply["status"], "Accepted");
    rig.wait_event("Started").await;
    rig.set_variable("SmartChargingCtrlr", "Enabled", "true")
        .await;
    assert_eq!(
        variable(&rig, "Entries", Some("ChargingProfiles")).await,
        "0"
    );
    rig.stop().await;
}

// ---- K11 to K13: external limits -------------------------------------------------------------

#[tokio::test]
async fn an_external_limit_is_notified_installed_and_released() {
    let rig = Rig::new().await;
    let before = rig.calls("NotifyChargingLimit").len();
    let outcome = rig
        .station
        .set_external_limit(1, external_schedule("W", 3000.0))
        .await
        .expect("external limit");
    assert!(matches!(outcome, Outcome2::Response { .. }));
    let sent = rig.wait_calls("NotifyChargingLimit", before + 1).await;
    let notice = &sent[before];
    assert_eq!(notice["chargingLimit"]["chargingLimitSource"], "EMS");
    assert_eq!(notice["evseId"], 1);
    // The schedule goes along while `NotifyChargingLimitWithSchedules` is true (K11.FR.03).
    assert_eq!(
        notice["chargingSchedule"][0]["chargingSchedulePeriod"][0]["limit"],
        3000.0
    );
    // The limit is in force: the composite of the EVSE shows it, another EVSE is free.
    let ask = |evse: i64| {
        rig.call(
            "GetCompositeSchedule",
            json!({"duration": 60, "evseId": evse}),
        )
    };
    let limit_of = |reply: &Value| reply["schedule"]["chargingSchedulePeriod"][0]["limit"].as_f64();
    assert_eq!(limit_of(&ask(1).await), Some(3000.0));
    assert_eq!(limit_of(&ask(2).await), Some(7200.0));
    // A second limit on the EVSE replaces the first.
    rig.station
        .set_external_limit(1, external_schedule("W", 2000.0))
        .await
        .expect("replaced");
    assert_eq!(limit_of(&ask(1).await), Some(2000.0));
    assert_eq!(
        variable(&rig, "Entries", Some("ChargingProfiles")).await,
        "1"
    );
    // K13: the release is notified and the limit ends.
    let cleared = rig.calls("ClearedChargingLimit").len();
    rig.station.clear_external_limit(1).await.expect("released");
    let sent = rig.wait_calls("ClearedChargingLimit", cleared + 1).await;
    assert_eq!(
        sent[cleared],
        json!({"chargingLimitSource": "EMS", "evseId": 1})
    );
    assert_eq!(limit_of(&ask(1).await), Some(7200.0));
    let again = rig.station.clear_external_limit(1).await;
    assert!(
        matches!(again, Err(StationError::InvalidState { .. })),
        "{again:?}"
    );
    rig.stop().await;
}

#[tokio::test]
async fn generated_external_ids_do_not_replace_csms_profiles() {
    let rig = Rig::new().await;
    rig.start_transaction(1).await;
    set_accepted(
        &rig,
        1,
        profile(rig.now(), -1, 0, "TxDefaultProfile", "W", 1000.0),
    )
    .await;
    let offered = || rig.station.snapshot().evse(1).unwrap().offered_w;
    settle(&rig).await;
    assert_eq!(offered(), Some(1000.0));
    rig.station
        .set_external_limit(1, external_schedule("W", 500.0))
        .await
        .expect("external limit");
    settle(&rig).await;
    assert_eq!(offered(), Some(500.0));
    rig.station.clear_external_limit(1).await.expect("cleared");
    settle(&rig).await;
    assert_eq!(offered(), Some(1000.0));
    rig.stop().await;
}

#[tokio::test]
async fn explicit_external_ids_cannot_replace_csms_profiles() {
    let rig = Rig::new().await;
    let raw = profile(rig.now(), -1, 0, "TxDefaultProfile", "W", 1000.0);
    set_accepted(&rig, 1, raw.clone()).await;
    let result = rig.station.set_external_limit(1, raw.clone()).await;
    assert!(
        matches!(result, Err(StationError::InvalidArgument(_))),
        "{result:?}"
    );
    let (_, reports) =
        get_profiles(&rig, json!({"chargingLimitSource": ["CSO"]}), Some(1), 1).await;
    assert_eq!(reports[0]["chargingProfile"], json!([raw]));
    rig.stop().await;
}

#[tokio::test]
async fn the_snapshot_shows_the_limit_in_force_and_the_profile_count() {
    let rig = Rig::new().await;
    let free = wait_snapshot(&rig.station, "a booted station", |s| {
        s.booted() && s.evse(1).is_some()
    })
    .await;
    assert_eq!(free.charging_profiles, 0);
    assert_eq!(free.evse(1).and_then(|e| e.offered_w), None);
    rig.station
        .set_external_limit(1, external_schedule("W", 3000.0))
        .await
        .expect("external limit");
    let limited = wait_snapshot(&rig.station, "the external limit", |s| {
        s.evse(1).is_some_and(|e| e.external_limit)
    })
    .await;
    assert_eq!(limited.charging_profiles, 1);
    assert_eq!(limited.evse(1).and_then(|e| e.offered_w), Some(3000.0));
    // Another EVSE is not covered.
    assert_eq!(limited.evse(2).map(|e| e.external_limit), Some(false));
    assert_eq!(limited.evse(2).and_then(|e| e.offered_w), None);
    // A limit on the whole station covers every EVSE.
    rig.station.clear_external_limit(1).await.expect("released");
    rig.station
        .set_external_limit(0, external_schedule("W", 2000.0))
        .await
        .expect("station limit");
    let whole = wait_snapshot(&rig.station, "the station limit", |s| {
        s.evse(2).is_some_and(|e| e.external_limit)
    })
    .await;
    assert_eq!(whole.evse(1).map(|e| e.external_limit), Some(true));
    assert_eq!(whole.evse(2).and_then(|e| e.offered_w), Some(2000.0));
    rig.station.clear_external_limit(0).await.expect("released");
    let after = wait_snapshot(&rig.station, "the release", |s| s.charging_profiles == 0).await;
    assert_eq!(after.evse(1).map(|e| e.external_limit), Some(false));
    assert_eq!(after.evse(1).and_then(|e| e.offered_w), None);
    rig.stop().await;
}

#[tokio::test]
async fn an_external_limit_notice_leaves_the_schedule_out_when_the_variable_says_so() {
    let rig = Rig::new().await;
    rig.set_variable(
        "SmartChargingCtrlr",
        "NotifyChargingLimitWithSchedules",
        "false",
    )
    .await;
    rig.station
        .set_external_limit(0, external_schedule("A", 16.0))
        .await
        .expect("external limit");
    let sent = rig.wait_calls("NotifyChargingLimit", 1).await;
    assert_eq!(
        sent[0],
        json!({"chargingLimit": {"chargingLimitSource": "EMS"}})
    );
    rig.stop().await;
}

#[tokio::test]
async fn external_limits_need_external_control_signals() {
    let rig = Rig::new().await;
    rig.set_variable(
        "SmartChargingCtrlr",
        "ExternalControlSignalsEnabled",
        "false",
    )
    .await;
    let refused = rig
        .station
        .set_external_limit(1, external_schedule("W", 3000.0))
        .await;
    assert!(
        matches!(refused, Err(StationError::InvalidState { .. })),
        "{refused:?}"
    );
    rig.set_variable(
        "SmartChargingCtrlr",
        "ExternalControlSignalsEnabled",
        "true",
    )
    .await;
    // What cannot be a limit is refused before anything is sent.
    let bad = rig.station.set_external_limit(1, json!({"x": 1})).await;
    assert!(
        matches!(bad, Err(StationError::InvalidArgument(_))),
        "{bad:?}"
    );
    let unknown = rig
        .station
        .set_external_limit(5, external_schedule("W", 3000.0))
        .await;
    assert!(
        matches!(unknown, Err(StationError::InvalidArgument(_))),
        "{unknown:?}"
    );
    assert_eq!(rig.calls("NotifyChargingLimit").len(), 0);
    rig.stop().await;
}

#[tokio::test]
async fn an_external_limit_that_is_not_a_valid_profile_is_refused_before_it_is_kept() {
    let rig = Rig::new().await;
    // A field the schema does not know would make every later report and notice invalid.
    let mut schedule = external_schedule("W", 3000.0);
    schedule["bogus"] = json!(1);
    let refused = rig.station.set_external_limit(1, schedule).await;
    assert!(
        matches!(refused, Err(StationError::InvalidArgument(_))),
        "{refused:?}"
    );
    let mut limit = external_schedule("W", 3000.0);
    limit["chargingSchedulePeriod"][0]["limit"] = json!("a lot");
    let refused = rig.station.set_external_limit(1, limit).await;
    assert!(
        matches!(refused, Err(StationError::InvalidArgument(_))),
        "{refused:?}"
    );
    assert_eq!(
        variable(&rig, "Entries", Some("ChargingProfiles")).await,
        "0"
    );
    assert_eq!(rig.calls("NotifyChargingLimit").len(), 0);
    rig.stop().await;
}

#[tokio::test]
async fn an_external_limit_given_as_a_whole_profile_keeps_its_id_and_stack_level() {
    let rig = Rig::new().await;
    let now = rig.now();
    let mut whole = profile(now, -7, 3, "TxDefaultProfile", "W", 2500.0);
    // Whatever purpose it carries, it becomes an external constraint.
    whole["chargingProfileKind"] = json!("Absolute");
    rig.station
        .set_external_limit(2, whole)
        .await
        .expect("external limit");
    let (_, reports) = get_profiles(&rig, json!({}), Some(2), 1).await;
    let reported = &reports[0]["chargingProfile"][0];
    assert_eq!(reported["id"], -7);
    assert_eq!(reported["stackLevel"], 3);
    assert_eq!(
        reported["chargingProfilePurpose"],
        "ChargingStationExternalConstraints"
    );
    rig.stop().await;
}

// ---- the meter ---------------------------------------------------------------------------

async fn wait_charging(rig: &Rig, evse: u32) {
    wait_snapshot(&rig.station, "a charging EVSE", |s| {
        s.evse(evse)
            .and_then(|e| e.transaction.as_ref())
            .is_some_and(|t| t.charging_state == ChargingState::Charging)
    })
    .await;
}

fn register(rig: &Rig, evse: u32) -> u64 {
    rig.station.snapshot().evse(evse).map_or(0, |e| e.meter_wh)
}

#[tokio::test]
async fn the_energy_register_stops_under_a_zero_limit() {
    // 3.6e9 W: a microsecond of charging is one watt hour.
    let rig = Rig::start("Accepted", |config| config.nominal_power_w = 3_600_000_000).await;
    rig.start_transaction(1).await;
    rig.start_transaction(2).await;
    wait_charging(&rig, 1).await;
    wait_charging(&rig, 2).await;
    let now = rig.now();
    let id = rig.transaction_id(1);
    let mut zero = profile(now, 1, 0, "TxProfile", "W", 0.0);
    zero["transactionId"] = json!(id);
    set_accepted(&rig, 1, zero).await;
    // After the round trip the snapshot holds the state the profile left.
    settle(&rig).await;
    let (first, free_first) = (register(&rig, 1), register(&rig, 2));
    settle(&rig).await;
    settle(&rig).await;
    let (second, free_second) = (register(&rig, 1), register(&rig, 2));
    assert_eq!(second, first, "the limited EVSE draws nothing");
    assert!(free_second > free_first, "the other EVSE keeps charging");
    // The power in the samples follows the limit too.
    rig.send(Command2::SampleMeter { evse: 1 }).await;
    let samples = rig
        .csms
        .wait_for_call_count_after(
            0,
            "TransactionEvent",
            1,
            |p| p["triggerReason"] == "MeterValuePeriodic",
            STEP,
        )
        .await
        .expect("a sample");
    let power = samples[0]["meterValue"][0]["sampledValue"]
        .as_array()
        .and_then(|values| {
            values
                .iter()
                .find(|v| v["measurand"] == "Power.Active.Import")
        })
        .and_then(|value| value["value"].as_f64());
    assert_eq!(power, Some(0.0));
    rig.stop().await;
}

// ---- persistence -----------------------------------------------------------------------------

const ID: &str = "CP-SMART";

fn spawn(csms: &MockCsms, dir: &Path, clock: &ManualClock) -> (Station2Handle, JoinHandle<()>) {
    let mut config = StationConfig::new(csms.url())
        .with_charge_point_id(ID)
        .with_connectors(2)
        .with_call_timeout_ms(2_000)
        .with_heartbeat_interval_secs(3600)
        .with_reconnect(ReconnectConfig {
            enabled: true,
            initial_delay_ms: 30,
            max_delay_ms: 100,
            max_attempts: 0,
        })
        .with_state_dir(dir);
    config.ws_ping_interval_secs = 0;
    Protocol::V201
        .token()
        .clone_into(&mut config.ws_subprotocol);
    config.protocol = Protocol::V201;
    Station2::spawn(config, Arc::new(clock.clone()))
}

async fn run_once(
    csms: &MockCsms,
    dir: &Path,
    clock: &ManualClock,
    body: impl AsyncFnOnce(&Station2Handle),
) {
    let (station, join) = spawn(csms, dir, clock);
    station.send(Command2::Connect).await.expect("connect");
    wait_booted(&station).await;
    body(&station).await;
    station.shutdown().await.expect("shutdown");
    timeout(STEP, join).await.expect("stops").expect("joins");
}

async fn call(csms: &MockCsms, action: &str, payload: Value) -> Value {
    let Frame::CallResult { payload, .. } =
        csms.send_call(action, payload).await.expect("answered")
    else {
        panic!("not a CALLRESULT");
    };
    payload
}

#[tokio::test]
async fn profiles_survive_a_restart() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().expect("dir");
    let clock = ManualClock::new(Utc::now());
    let now = clock.now_utc();
    let default = profile(now, 1, 0, "TxDefaultProfile", "A", 6.0);
    let max = profile(now, 2, 0, "ChargingStationMaxProfile", "A", 12.0);
    run_once(&csms, dir.path(), &clock, async |station| {
        for (evse, raw) in [(1, &default), (0, &max)] {
            let reply = call(
                &csms,
                "SetChargingProfile",
                json!({"evseId": evse, "chargingProfile": raw}),
            )
            .await;
            assert_eq!(reply["status"], "Accepted");
        }
        station
            .set_external_limit(2, external_schedule("W", 3000.0))
            .await
            .expect("external limit");
    })
    .await;
    run_once(&csms, dir.path(), &clock, async |_| {
        let before = csms.calls_since(0).len();
        let reply = call(
            &csms,
            "GetChargingProfiles",
            json!({"requestId": 3, "chargingProfile": {}}),
        )
        .await;
        assert_eq!(reply["status"], "Accepted");
        csms.wait_for_call_count_after(0, "ReportChargingProfiles", 3, |_| true, STEP)
            .await
            .expect("three reports");
        let reports: Vec<Value> = csms
            .calls_since(before)
            .into_iter()
            .filter(|call| call.action == "ReportChargingProfiles")
            .map(|call| call.payload)
            .collect();
        let shape: Vec<(i64, &str)> = reports
            .iter()
            .map(|r| {
                (
                    r["evseId"].as_i64().expect("evse"),
                    r["chargingLimitSource"].as_str().expect("source"),
                )
            })
            .collect();
        assert_eq!(shape, [(0, "CSO"), (1, "CSO"), (2, "EMS")]);
        // The profiles come back as they were stored.
        assert_eq!(reports[0]["chargingProfile"], json!([max]));
        assert_eq!(reports[1]["chargingProfile"], json!([default]));
        let reply = call(
            &csms,
            "GetVariables",
            json!({"getVariableData": [{
                "component": {"name": "SmartChargingCtrlr"},
                "variable": {"name": "Entries", "instance": "ChargingProfiles"},
            }]}),
        )
        .await;
        assert_eq!(reply["getVariableResult"][0]["attributeValue"], "3");
    })
    .await;
    csms.shutdown().await;
}

#[tokio::test]
async fn a_cleared_profile_stays_cleared_after_a_restart() {
    let csms = v201_csms().await;
    let dir = tempfile::tempdir().expect("dir");
    let clock = ManualClock::new(Utc::now());
    let now = clock.now_utc();
    run_once(&csms, dir.path(), &clock, async |_| {
        let body = json!({"evseId": 1, "chargingProfile": profile(now, 1, 0, "TxDefaultProfile", "A", 6.0)});
        assert_eq!(call(&csms, "SetChargingProfile", body).await["status"], "Accepted");
        let reply = call(&csms, "ClearChargingProfile", json!({"chargingProfileId": 1})).await;
        assert_eq!(reply["status"], "Accepted");
    })
    .await;
    run_once(&csms, dir.path(), &clock, async |_| {
        let reply = call(
            &csms,
            "ClearChargingProfile",
            json!({"chargingProfileId": 1}),
        )
        .await;
        assert_eq!(reply["status"], "Unknown");
    })
    .await;
    csms.shutdown().await;
}

#[tokio::test]
async fn a_state_file_without_charging_profiles_still_loads() {
    let dir = tempfile::tempdir().expect("dir");
    let store = StationStore::new_for(FileKind::V2, dir.path(), ID);
    std::fs::write(
        store.path(),
        format!(r#"{{"version": 2, "charge_point_id": "{ID}"}}"#),
    )
    .expect("write");
    let LoadOutcome2::Loaded(state) = store.load2().expect("loads") else {
        panic!("the file did not load");
    };
    assert_eq!(state.charging_profiles.profiles.len(), 0);
    let fresh = PersistedStation2::new(ID);
    assert_eq!(fresh.charging_profiles.profiles.len(), 0);
}

// ---- OCPP 2.1 --------------------------------------------------------------------------------

/// A booted OCPP 2.1 station on a mock CSMS that answers the station's calls with `{}`.
async fn rig_21() -> (MockCsms, Station2Handle, JoinHandle<()>, ManualClock) {
    rig_for(Protocol::V21, |_| {}).await
}

async fn rig_for(
    protocol: Protocol,
    tweak: impl FnOnce(&mut StationConfig),
) -> (MockCsms, Station2Handle, JoinHandle<()>, ManualClock) {
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Require(protocol.token().to_owned()),
        ..CsmsOptions::default()
    })
    .await
    .expect("mock CSMS starts");
    csms.respond("BootNotification", |_| {
        Reply::Result(json!({
            "status": "Accepted",
            "currentTime": Utc::now().to_rfc3339(),
            "interval": 3600,
        }))
    });
    for action in [
        "StatusNotification",
        "ReportChargingProfiles",
        "NotifyChargingLimit",
        "ClearedChargingLimit",
        "TransactionEvent",
    ] {
        csms.respond(action, |_| Reply::Result(json!({})));
    }
    let clock = ManualClock::new(Utc::now());
    let mut config = StationConfig::new(csms.url())
        .with_connectors(2)
        .with_call_timeout_ms(2_000)
        .with_heartbeat_interval_secs(3600)
        .without_persistence();
    config.ws_ping_interval_secs = 0;
    protocol.token().clone_into(&mut config.ws_subprotocol);
    config.protocol = protocol;
    tweak(&mut config);
    let (station, join) = Station2::spawn(config, Arc::new(clock.clone()));
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    (csms, station, join, clock)
}

async fn relative_profile_after_late_authorization(protocol: Protocol, offered_w: Option<f64>) {
    let (csms, station, join, clock) = rig_for(protocol, |_| {}).await;
    let reply = call(
        &csms,
        "SetVariables",
        json!({"setVariableData": [{
            "component": {"name": "TxCtrlr"}, "variable": {"name": "TxStartPoint"},
            "attributeValue": "EVConnected"
        }]}),
    )
    .await;
    assert_eq!(reply["setVariableResult"][0]["attributeStatus"], "Accepted");
    station
        .send(Command2::PlugIn { evse: 1 })
        .await
        .expect("plugged in");
    let snapshot = wait_snapshot(&station, "an unauthorized transaction", |s| {
        s.evse(1)
            .and_then(|e| e.transaction.as_ref())
            .is_some_and(|t| t.charging_state == ChargingState::EvConnected)
    })
    .await;
    let transaction = snapshot.evse(1).unwrap().transaction.as_ref().unwrap();
    let mut raw = relative_zero_profile(clock.now_utc());
    raw["transactionId"] = json!(transaction.transaction_id);
    let reply = call(
        &csms,
        "SetChargingProfile",
        json!({"evseId": 1, "chargingProfile": raw}),
    )
    .await;
    assert_eq!(reply["status"], "Accepted");
    clock.advance(Duration::seconds(61));
    station
        .send(Command2::PresentToken {
            evse: Some(1),
            id_token: "LATE".to_owned(),
            token_type: IdTokenType::Central,
        })
        .await
        .expect("authorized");
    let snapshot = wait_snapshot(&station, "a charging EVSE", |s| {
        s.evse(1)
            .and_then(|e| e.transaction.as_ref())
            .is_some_and(|t| t.charging_state == ChargingState::Charging)
    })
    .await;
    assert_eq!(snapshot.evse(1).unwrap().offered_w, offered_w);
    stop_21(csms, station, join).await;
}

#[tokio::test]
async fn a_relative_profile_on_21_starts_when_authorized_and_connected() {
    relative_profile_after_late_authorization(Protocol::V21, Some(0.0)).await;
}

#[tokio::test]
async fn a_relative_profile_on_201_starts_with_the_transaction() {
    relative_profile_after_late_authorization(Protocol::V201, None).await;
}

#[tokio::test]
async fn profiles_are_restored_only_on_the_protocol_that_stored_them() {
    let dir = tempfile::tempdir().expect("dir");
    let persistent = |config: &mut StationConfig| {
        config.persist = true;
        ID.clone_into(&mut config.charge_point_id);
        config.state_dir = Some(dir.path().to_owned());
    };
    let (csms, station, join, clock) = rig_for(Protocol::V21, persistent).await;
    let mut raw = profile(clock.now_utc(), 1, 0, "TxDefaultProfile", "W", 500.0);
    raw["chargingSchedule"][0]["powerTolerance"] = json!(2.5);
    let reply = call(
        &csms,
        "SetChargingProfile",
        json!({"evseId": 1, "chargingProfile": raw}),
    )
    .await;
    assert_eq!(reply["status"], "Accepted");
    stop_21(csms, station, join).await;

    let criteria = json!({"requestId": 1, "chargingProfile": {}});
    let (csms, station, join, _) = rig_for(Protocol::V21, persistent).await;
    let reply = call(&csms, "GetChargingProfiles", criteria.clone()).await;
    assert_eq!(reply["status"], "Accepted");
    let reports = csms
        .wait_for_call_count_after(0, "ReportChargingProfiles", 1, |_| true, STEP)
        .await
        .expect("profile restored");
    assert_eq!(reports[0]["chargingProfile"], json!([raw]));
    stop_21(csms, station, join).await;

    let csms = v201_csms().await;
    run_once(&csms, dir.path(), &clock, async |_| {
        let reply = call(&csms, "GetChargingProfiles", criteria).await;
        assert_eq!(reply["status"], "NoProfiles");
    })
    .await;
    csms.shutdown().await;
}

async fn stop_21(csms: MockCsms, station: Station2Handle, join: JoinHandle<()>) {
    station.shutdown().await.expect("shutdown");
    timeout(STEP, join).await.expect("stops").expect("joins");
    csms.shutdown().await;
}

#[tokio::test]
async fn an_ocpp_21_station_keeps_the_profile_with_its_21_fields() {
    let (csms, station, join, clock) = rig_21().await;
    let now = clock.now_utc();
    let mut raw = profile(now, 1, 0, "TxDefaultProfile", "W", 4000.0);
    // OCPP 2.1 fields the station accepts and reports back unchanged.
    raw["chargingSchedule"][0]["chargingSchedulePeriod"][0]["operationMode"] =
        json!("ChargingOnly");
    raw["chargingSchedule"][0]["powerTolerance"] = json!(2.5);
    let install = |evse: i64, profile: &Value| {
        call(
            &csms,
            "SetChargingProfile",
            json!({"evseId": evse, "chargingProfile": profile}),
        )
    };
    assert_eq!(install(1, &raw).await["status"], "Accepted");
    let reply = call(
        &csms,
        "GetChargingProfiles",
        json!({"requestId": 2, "evseId": 1, "chargingProfile": {"chargingLimitSource": ["CSO"]}}),
    )
    .await;
    assert_eq!(reply["status"], "Accepted");
    let sent = csms
        .wait_for_call_count_after(0, "ReportChargingProfiles", 1, |_| true, STEP)
        .await
        .expect("a report");
    assert_eq!(sent[0]["chargingLimitSource"], "CSO");
    assert_eq!(sent[0]["chargingProfile"], json!([raw]));
    // The composite follows the profile.
    let reply = call(
        &csms,
        "GetCompositeSchedule",
        json!({"duration": 120, "evseId": 1, "chargingRateUnit": "W"}),
    )
    .await;
    assert_eq!(
        reply["schedule"]["chargingSchedulePeriod"][0]["limit"],
        4000.0
    );
    // What the station does not implement is refused with the code of OCPP 2.1.
    let reason = |reply: &Value| {
        reply["statusInfo"]["reasonCode"]
            .as_str()
            .map(str::to_owned)
    };
    let mut priority = profile(now, 2, 0, "PriorityCharging", "W", 4000.0);
    assert_eq!(
        reason(&install(1, &priority).await).as_deref(),
        Some("UnsupportedPurpose")
    );
    priority["chargingProfilePurpose"] = json!("TxDefaultProfile");
    priority["chargingProfileKind"] = json!("Dynamic");
    assert_eq!(
        reason(&install(1, &priority).await).as_deref(),
        Some("UnsupportedKind")
    );
    let mut mode = profile(now, 3, 1, "TxDefaultProfile", "W", 4000.0);
    mode["chargingSchedule"][0]["chargingSchedulePeriod"][0]["operationMode"] =
        json!("CentralSetpoint");
    assert_eq!(
        reason(&install(1, &mode).await).as_deref(),
        Some("InvalidOperationMode")
    );
    stop_21(csms, station, join).await;
}

#[tokio::test]
async fn an_ocpp_21_station_notifies_an_external_limit() {
    let (csms, station, join, _clock) = rig_21().await;
    station
        .set_external_limit(1, external_schedule("A", 10.0))
        .await
        .expect("external limit");
    let sent = csms
        .wait_for_call_count_after(0, "NotifyChargingLimit", 1, |_| true, STEP)
        .await
        .expect("a notice");
    assert_eq!(sent[0]["chargingLimit"]["chargingLimitSource"], "EMS");
    assert_eq!(sent[0]["evseId"], 1);
    station.clear_external_limit(1).await.expect("released");
    let sent = csms
        .wait_for_call_count_after(0, "ClearedChargingLimit", 1, |_| true, STEP)
        .await
        .expect("a release");
    assert_eq!(sent[0]["chargingLimitSource"], "EMS");
    stop_21(csms, station, join).await;
}
