use std::sync::Arc;

use chrono::{Duration, Utc};
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::{Clock, ManualClock, Station2Handle, SystemClock};
use kirby_ocpp_testkit::{Fault, MockCsms, Reply};
use serde_json::{Value, json};

use super::station2_priority::{absolute, boot, expect_offered, set_profile, start_tx};
use super::station2_remote::Frames;
use super::station2_support::STEP;
use super::station2_transactions::{call, stop};
use super::station21::v21_csms;

fn dynamic(id: i32, limit: f64) -> Value {
    json!({
        "id": id,
        "stackLevel": 1,
        "chargingProfilePurpose": "TxDefaultProfile",
        "chargingProfileKind": "Dynamic",
        "chargingSchedule": [{
            "id": 1,
            "chargingRateUnit": "W",
            "chargingSchedulePeriod": [{"startPeriod": 0, "limit": limit}]
        }]
    })
}

async fn update(csms: &MockCsms, id: i32, schedule_update: Value) -> Value {
    call(
        csms,
        "UpdateDynamicSchedule",
        json!({
            "chargingProfileId": id,
            "scheduleUpdate": schedule_update,
        }),
    )
    .await
}

async fn reported(csms: &MockCsms, station: &Station2Handle, id: i32) -> Value {
    let mut events = Frames::new(station);
    let mark = csms.mark();
    let reply = call(
        csms,
        "GetChargingProfiles",
        json!({
            "requestId": id,
            "chargingProfile": {"chargingProfileId": [id]},
        }),
    )
    .await;
    assert_eq!(reply["status"], "Accepted", "{reply}");
    let report = csms
        .wait_for_call_after(
            mark,
            "ReportChargingProfiles",
            |payload| payload["requestId"] == id,
            STEP,
        )
        .await
        .expect("dynamic profile report");
    events
        .log_containing("ReportChargingProfiles accepted")
        .await;
    report["chargingProfile"][0].clone()
}

#[tokio::test]
async fn dynamic_push_updates_the_limit_timestamp_and_persisted_profile() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), Arc::new(clock.clone())).await;
    set_profile(&csms, 1, &absolute(&clock, 1, "TxDefaultProfile", 3000.0)).await;
    set_profile(&csms, 1, &dynamic(2, 2000.0)).await;
    start_tx(&station).await;
    expect_offered(&station, 2000.0).await;
    assert_eq!(
        reported(&csms, &station, 2).await["dynUpdateTime"],
        json!(clock.now_utc())
    );
    for (id, payload, reason) in [
        (99, json!({"limit": 1000}), "NotFound"),
        (1, json!({"limit": 1000}), "InvalidProfile"),
        (
            2,
            json!({"limit": 1000, "setpoint": 500}),
            "UnsupportedParam",
        ),
        (2, json!({"limit": -1}), "InvalidValue"),
    ] {
        let reply = update(&csms, id, payload).await;
        assert_eq!(reply["status"], "Rejected", "{reply}");
        assert_eq!(reply["statusInfo"]["reasonCode"], reason);
    }
    clock.advance(Duration::seconds(1));
    assert_eq!(
        update(&csms, 2, json!({"limit": 1000})).await["status"],
        "Accepted"
    );
    expect_offered(&station, 1000.0).await;
    stop(&station, task).await;

    let (station, task) = boot(&csms, Protocol::V21, dir.path(), Arc::new(clock.clone())).await;
    let saved = reported(&csms, &station, 2).await;
    assert_eq!(saved["dynUpdateTime"], json!(clock.now_utc()));
    assert_eq!(
        saved["chargingSchedule"][0]["chargingSchedulePeriod"][0]["limit"],
        1000
    );
    expect_offered(&station, 1000.0).await;
    stop(&station, task).await;
}

#[tokio::test]
async fn dynamic_pull_applies_the_csms_answer() {
    let csms = v21_csms().await;
    csms.respond("PullDynamicScheduleUpdate", |_| {
        Reply::Result(json!({
            "status": "Accepted",
            "scheduleUpdate": {"limit": 500},
        }))
    });
    let dir = tempfile::tempdir().expect("state directory");
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), Arc::new(SystemClock)).await;
    start_tx(&station).await;
    let mut raw = dynamic(99, 2000.0);
    raw["dynUpdateInterval"] = json!(1);
    set_profile(&csms, 1, &raw).await;
    let request = csms
        .wait_for_call("PullDynamicScheduleUpdate", STEP)
        .await
        .expect("a dynamic pull");
    assert_eq!(request, json!({"chargingProfileId": 99}));
    expect_offered(&station, 500.0).await;
    stop(&station, task).await;
}

#[tokio::test]
async fn dynamic_pull_retries_after_bad_answers_without_refreshing_the_profile() {
    const PULL: &str = "PullDynamicScheduleUpdate";
    let csms = v21_csms().await;
    csms.inject(Fault::reply(
        PULL,
        Reply::Result(json!({
            "status": "Accepted",
            "scheduleUpdate": {"setpoint": 500},
        })),
    ));
    csms.inject(Fault::reply(
        PULL,
        Reply::Result(json!({"status": "Rejected"})),
    ));
    csms.inject(Fault::call_error(PULL));
    csms.respond(PULL, |_| {
        Reply::Error(
            kirby_ocpp_testkit::ErrorCode::InternalError,
            "retry".to_owned(),
        )
    });
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), Arc::new(clock.clone())).await;
    let mut raw = dynamic(2, 2000.0);
    let original_time = clock.now_utc() - Duration::seconds(1);
    raw["dynUpdateTime"] = json!(original_time);
    raw["dynUpdateInterval"] = json!(1);
    set_profile(&csms, 1, &raw).await;
    let pulls = csms
        .wait_for_call_count_after(0, PULL, 3, |_| true, STEP)
        .await
        .expect("three attempts");
    assert_eq!(pulls, vec![json!({"chargingProfileId": 2}); 3]);
    assert_eq!(
        reported(&csms, &station, 2).await["dynUpdateTime"],
        json!(original_time)
    );
    let mut events = Frames::new(&station);
    csms.respond(PULL, |_| Reply::Result(json!({"status": "Accepted"})));
    events
        .log_containing("PullDynamicScheduleUpdate accepted")
        .await;
    let saved = reported(&csms, &station, 2).await;
    assert_eq!(saved["dynUpdateTime"], json!(clock.now_utc()));
    assert_eq!(
        saved["chargingSchedule"][0]["chargingSchedulePeriod"][0]["limit"],
        2000.0
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn dynamic_expiry_falls_back_and_an_update_reactivates_the_profile() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), Arc::new(SystemClock)).await;
    set_profile(
        &csms,
        1,
        &absolute(&SystemClock, 1, "TxDefaultProfile", 3000.0),
    )
    .await;
    start_tx(&station).await;
    let mut raw = dynamic(2, 2000.0);
    raw["chargingSchedule"][0]["duration"] = json!(2);
    set_profile(&csms, 1, &raw).await;
    expect_offered(&station, 2000.0).await;
    expect_offered(&station, 3000.0).await;
    assert_eq!(
        reported(&csms, &station, 2).await["chargingProfileKind"],
        "Dynamic"
    );
    assert_eq!(
        update(&csms, 2, json!({"limit": 1500})).await["status"],
        "Accepted"
    );
    expect_offered(&station, 1500.0).await;
    stop(&station, task).await;
}

#[tokio::test]
async fn dynamic_profile_restores_expired_and_reactivates_after_update() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), Arc::new(clock.clone())).await;
    set_profile(&csms, 1, &absolute(&clock, 1, "TxDefaultProfile", 3000.0)).await;
    let mut raw = dynamic(2, 2000.0);
    raw["chargingSchedule"][0]["duration"] = json!(2);
    set_profile(&csms, 1, &raw).await;
    assert_eq!(
        update(&csms, 2, json!({"limit": 1000})).await["status"],
        "Accepted"
    );
    stop(&station, task).await;
    clock.advance(Duration::seconds(3));

    let (station, task) = boot(&csms, Protocol::V21, dir.path(), Arc::new(clock)).await;
    expect_offered(&station, 3000.0).await;
    let saved = reported(&csms, &station, 2).await;
    assert_eq!(
        saved["chargingSchedule"][0]["chargingSchedulePeriod"][0]["limit"],
        1000
    );
    assert_eq!(update(&csms, 2, json!({})).await["status"], "Accepted");
    expect_offered(&station, 1000.0).await;
    stop(&station, task).await;
}
