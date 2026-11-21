use std::path::Path;
use std::sync::Arc;

use chrono::{Duration, Utc};
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::frame::{ErrorCode, Frame};
use kirby_ocpp_core::ocpp::v2::types::{ChargingState, IdTokenType};
use kirby_ocpp_core::{
    Clock, Command2, ManualClock, Outcome2, Station2, Station2Handle, StationError,
};
use kirby_ocpp_testkit::{MockCsms, Reply};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use super::station2_persistence::config;
use super::station2_support::{STEP, v201_csms, wait_booted, wait_snapshot};
use super::station2_transactions::{call, set_variables, stop};
use super::station21::v21_csms;

pub(super) async fn boot(
    csms: &MockCsms,
    protocol: Protocol,
    dir: &Path,
    clock: Arc<dyn Clock>,
) -> (Station2Handle, JoinHandle<()>) {
    let mut config = config(csms, dir);
    config.protocol = protocol;
    protocol.token().clone_into(&mut config.ws_subprotocol);
    let (station, task) = Station2::spawn(config, clock);
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    (station, task)
}

pub(super) async fn set_profile(csms: &MockCsms, evse: u32, raw: &Value) {
    let reply = call(
        csms,
        "SetChargingProfile",
        json!({"evseId": evse, "chargingProfile": raw}),
    )
    .await;
    assert_eq!(reply["status"], "Accepted", "{reply}");
}

pub(super) async fn expect_offered(station: &Station2Handle, watts: f64) {
    wait_snapshot(station, "the expected offered power", |snapshot| {
        snapshot
            .evse(1)
            .is_some_and(|evse| evse.offered_w == Some(watts))
    })
    .await;
}

pub(super) async fn start_tx(station: &Station2Handle) -> String {
    station
        .send(Command2::PlugIn { evse: 1 })
        .await
        .expect("plug in");
    station
        .send(Command2::PresentToken {
            evse: Some(1),
            token_type: IdTokenType::Local,
            id_token: "DRIVER".to_owned(),
        })
        .await
        .expect("present token");
    let snapshot = wait_snapshot(station, "a charging transaction", |snapshot| {
        snapshot
            .evse(1)
            .and_then(|evse| evse.transaction.as_ref())
            .is_some_and(|tx| tx.charging_state == ChargingState::Charging)
    })
    .await;
    snapshot
        .evse(1)
        .unwrap()
        .transaction
        .as_ref()
        .unwrap()
        .transaction_id
        .clone()
}

pub(super) fn absolute(clock: &dyn Clock, id: i32, purpose: &str, watts: f64) -> Value {
    json!({
        "id": id,
        "stackLevel": 0,
        "chargingProfilePurpose": purpose,
        "chargingProfileKind": "Absolute",
        "chargingSchedule": [{
            "id": 1,
            "chargingRateUnit": "W",
            "startSchedule": clock.now_utc() - Duration::seconds(1),
            "chargingSchedulePeriod": [{"startPeriod": 0, "limit": watts}]
        }]
    })
}

async fn use_priority(csms: &MockCsms, tx: &str, activate: bool) -> Value {
    call(
        csms,
        "UsePriorityCharging",
        json!({"transactionId": tx, "activate": activate}),
    )
    .await
}

async fn notice(csms: &MockCsms, mark: usize, tx: &str, activated: bool) {
    let request = csms
        .wait_for_call_after(mark, "NotifyPriorityCharging", |_| true, STEP)
        .await
        .expect("priority notification");
    assert_eq!(
        request,
        json!({"transactionId": tx, "activated": activated})
    );
}

#[tokio::test]
async fn priority_charging_replaces_transaction_profiles_and_keeps_station_caps() {
    let csms = v21_csms().await;
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), Arc::new(clock.clone())).await;
    set_profile(&csms, 1, &absolute(&clock, 1, "TxDefaultProfile", 1000.0)).await;
    let tx = start_tx(&station).await;
    expect_offered(&station, 1000.0).await;
    assert_eq!(use_priority(&csms, &tx, true).await["status"], "NoProfile");
    assert_eq!(
        use_priority(&csms, &tx, true).await["statusInfo"]["reasonCode"],
        "NotFound"
    );
    assert!(matches!(station.set_priority_charging(1, true).await,
        Err(StationError::InvalidArgument(message)) if message.contains("no priority charging profile applies")));
    assert!(matches!(
        station.set_priority_charging(2, false).await,
        Err(StationError::InvalidArgument(_))
    ));
    let absent = use_priority(&csms, "not-running", false).await;
    assert_eq!(absent["status"], "Rejected");
    assert_eq!(absent["statusInfo"]["reasonCode"], "TxNotFound");

    set_profile(&csms, 0, &absolute(&clock, 2, "PriorityCharging", 7000.0)).await;
    let mark = csms.mark();
    assert_eq!(use_priority(&csms, &tx, true).await["status"], "Accepted");
    notice(&csms, mark, &tx, true).await;
    expect_offered(&station, 7000.0).await;
    assert_eq!(use_priority(&csms, &tx, true).await["status"], "Accepted");
    set_profile(
        &csms,
        0,
        &absolute(&clock, 3, "ChargingStationMaxProfile", 3000.0),
    )
    .await;
    expect_offered(&station, 3000.0).await;
    let composite = call(
        &csms,
        "GetCompositeSchedule",
        json!({"evseId": 1, "duration": 10, "chargingRateUnit": "W"}),
    )
    .await;
    assert_eq!(
        composite["schedule"]["chargingSchedulePeriod"][0]["limit"],
        3000.0
    );
    let report_mark = csms.mark();
    let report = call(
        &csms,
        "GetChargingProfiles",
        json!({"requestId": 1, "chargingProfile": {"chargingProfilePurpose": "PriorityCharging"}}),
    )
    .await;
    assert_eq!(report["status"], "Accepted");
    let report = csms
        .wait_for_call_after(report_mark, "ReportChargingProfiles", |_| true, STEP)
        .await
        .expect("priority profile report");
    assert_eq!(
        report["chargingProfile"][0]["chargingProfilePurpose"],
        "PriorityCharging"
    );

    let deactivate = csms.mark();
    assert_eq!(use_priority(&csms, &tx, false).await["status"], "Accepted");
    notice(&csms, deactivate, &tx, false).await;
    expect_offered(&station, 1000.0).await;
    assert_eq!(
        csms.calls_since(mark)
            .iter()
            .filter(|call| call.action == "NotifyPriorityCharging")
            .count(),
        2
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn priority_charging_command_queues_offline_and_survives_transaction_resume() {
    let csms = v21_csms().await;
    csms.respond("NotifyPriorityCharging", |_| Reply::Result(json!({})));
    let dir = tempfile::tempdir().expect("state directory");
    let clock = ManualClock::new(Utc::now());
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), Arc::new(clock.clone())).await;
    set_variables(&csms, &[("TxCtrlr", "TxResumptionTimeout", None, "60")]).await;
    set_profile(&csms, 1, &absolute(&clock, 1, "TxDefaultProfile", 1000.0)).await;
    set_profile(&csms, 0, &absolute(&clock, 2, "PriorityCharging", 7000.0)).await;
    let tx = start_tx(&station).await;

    station.disconnect().await.expect("disconnect");
    let mark = csms.mark();
    assert_eq!(
        station.set_priority_charging(1, true).await.unwrap(),
        Outcome2::Done
    );
    assert_eq!(
        station.set_priority_charging(1, true).await.unwrap(),
        Outcome2::Done
    );
    expect_offered(&station, 7000.0).await;
    assert!(
        csms.calls_since(mark)
            .iter()
            .all(|call| call.action != "NotifyPriorityCharging")
    );
    station.connect().await.expect("reconnect");
    notice(&csms, mark, &tx, true).await;
    stop(&station, task).await;
    clock.advance(Duration::seconds(1));

    let mark = csms.mark();
    let (station, task) = boot(&csms, Protocol::V21, dir.path(), Arc::new(clock)).await;
    let resumed = csms
        .wait_for_call_after(
            mark,
            "TransactionEvent",
            |payload| payload["triggerReason"] == "TxResumed",
            STEP,
        )
        .await
        .expect("transaction resumed");
    assert_eq!(resumed["transactionInfo"]["transactionId"], tx);
    expect_offered(&station, 7000.0).await;
    let mark = csms.mark();
    assert_eq!(
        station.set_priority_charging(1, false).await.unwrap(),
        Outcome2::Done
    );
    notice(&csms, mark, &tx, false).await;
    expect_offered(&station, 1000.0).await;
    stop(&station, task).await;
}

#[tokio::test]
async fn priority_and_dynamic_capabilities_are_21_only() {
    for protocol in [Protocol::V201, Protocol::V21] {
        let csms = if protocol == Protocol::V21 {
            v21_csms().await
        } else {
            v201_csms().await
        };
        let dir = tempfile::tempdir().expect("state directory");
        let (station, task) = boot(
            &csms,
            protocol,
            dir.path(),
            Arc::new(ManualClock::new(Utc::now())),
        )
        .await;
        let reply = call(&csms, "GetVariables", json!({"getVariableData": [
            {"component": {"name": "SmartChargingCtrlr"}, "variable": {"name": "SupportedAdditionalPurposes"}},
            {"component": {"name": "SmartChargingCtrlr"}, "variable": {"name": "SupportsFeature", "instance": "DynamicProfiles"}}
        ]})).await;
        let results = reply["getVariableResult"].as_array().unwrap();
        if protocol == Protocol::V21 {
            assert_eq!(results[0]["attributeValue"], "PriorityCharging");
            assert_eq!(results[1]["attributeValue"], "true");
            set_variables(&csms, &[("SmartChargingCtrlr", "Enabled", None, "false")]).await;
        } else {
            assert!(
                results
                    .iter()
                    .all(|result| result["attributeStatus"] == "UnknownVariable")
            );
            assert!(matches!(
                station.set_priority_charging(1, true).await,
                Err(StationError::InvalidArgument(_))
            ));
        }
        let expected = if protocol == Protocol::V21 {
            ErrorCode::NotSupported
        } else {
            ErrorCode::NotImplemented
        };
        for (action, payload) in [
            (
                "UsePriorityCharging",
                json!({"transactionId": "TX", "activate": true}),
            ),
            (
                "UpdateDynamicSchedule",
                json!({"chargingProfileId": 1, "scheduleUpdate": {}}),
            ),
        ] {
            let frame = csms
                .send_call(action, payload)
                .await
                .expect("station answers");
            assert!(
                matches!(&frame, Frame::CallError { code, .. } if *code == expected),
                "{frame:?}"
            );
        }
        stop(&station, task).await;
    }
}
