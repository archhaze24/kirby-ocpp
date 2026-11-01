//! Remote start/stop, Reset, reservations, smart charging, maintenance, availability with
//! transactions and restoration of a running station (ports of the TS smoke scenarios
//! availability-reset-persistence, smart-charging, maintenance, negative-csms).

mod station_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::Duration as Delta;
use kirby_ocpp_core::ids::{ConnectorId, ReservationId, TxId};
use kirby_ocpp_core::ocpp::ErrorCode;
use kirby_ocpp_core::ocpp::types::{
    AvailabilityType, ConnectorStatus, DiagnosticsStatus, FirmwareStatus, RegistrationStatus,
    StopReason,
};
use kirby_ocpp_core::station::{Command, FirmwareOutcome, Outcome, TransactionState};
use kirby_ocpp_core::{ManualClock, Station};
use kirby_ocpp_testkit::Reply;
use serde_json::{Value, json};
use station_support::{Rig, STEP, t0, tag, test_config, wait_state};

const C1: ConnectorId = ConnectorId::new(1);
const C2: ConnectorId = ConnectorId::new(2);

async fn booted() -> Rig {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig
}

fn conn(rig: &Rig, id: ConnectorId) -> kirby_ocpp_core::station::ConnectorSnapshot {
    rig.station.snapshot().connector(id).unwrap().clone()
}

fn status_of(payload: &Value) -> &str {
    payload["status"].as_str().unwrap()
}

fn profile(id: i32, purpose: &str, stack: i32) -> Value {
    json!({
        "chargingProfileId": id, "stackLevel": stack,
        "chargingProfilePurpose": purpose, "chargingProfileKind": "Absolute",
        "chargingSchedule": {"chargingRateUnit": "A",
            "chargingSchedulePeriod": [{"startPeriod": 0, "limit": 16}]}
    })
}

// ---- remote start / stop ---------------------------------------------------------

#[tokio::test]
async fn remote_start_and_stop() {
    let rig = booted().await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "RemoteStartTransaction",
            json!({"connectorId": 1, "idTag": "TAG1"}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.csms
        .wait_for_call_after(mark, "StartTransaction", |_| true, STEP)
        .await
        .unwrap();
    wait_state(&rig.station, "active", |s| {
        s.connector(C1).unwrap().tx_id() == Some(TxId::Csms(123))
    })
    .await;
    // Busy connector, wrong transaction id.
    rig.csms
        .expect_status(
            "RemoteStartTransaction",
            json!({"connectorId": 1, "idTag": "TAG2"}),
            "Rejected",
        )
        .await
        .unwrap();
    rig.csms
        .expect_status(
            "RemoteStopTransaction",
            json!({"transactionId": 999}),
            "Rejected",
        )
        .await
        .unwrap();
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "RemoteStopTransaction",
            json!({"transactionId": 123}),
            "Accepted",
        )
        .await
        .unwrap();
    let stop = rig
        .csms
        .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(stop["reason"], "Remote");
    rig.shutdown().await;
}

#[tokio::test]
async fn remote_start_without_a_connector_uses_the_first_available_one() {
    let rig = booted().await;
    rig.begin(1, "TAG1").await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "RemoteStartTransaction",
            json!({"idTag": "TAG2"}),
            "Accepted",
        )
        .await
        .unwrap();
    let sent = rig
        .csms
        .wait_for_call_after(mark, "StartTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(sent["connectorId"], 2);
    rig.shutdown().await;
}

#[tokio::test]
async fn remote_start_charging_profile_rules() {
    let rig = booted().await;
    // Only a TxProfile without transactionId is allowed.
    let mut with_tx = profile(1, "TxProfile", 0);
    with_tx["transactionId"] = json!(5);
    for bad in [profile(1, "TxDefaultProfile", 0), with_tx] {
        rig.csms
            .expect_status(
                "RemoteStartTransaction",
                json!({"connectorId": 1, "idTag": "TAG1", "chargingProfile": bad}),
                "Rejected",
            )
            .await
            .unwrap();
    }
    assert!(conn(&rig, C1).transaction.is_idle());
    rig.csms
        .expect_status(
            "RemoteStartTransaction",
            json!({"connectorId": 1, "idTag": "TAG1", "chargingProfile": profile(2, "TxProfile", 0)}),
            "Accepted",
        )
        .await
        .unwrap();
    wait_state(&rig.station, "active", |s| {
        s.connector(C1).unwrap().tx_id() == Some(TxId::Csms(123))
    })
    .await;
    // The profile was stored bound to the new transaction.
    let schedule = rig
        .csms_call(
            "GetCompositeSchedule",
            json!({"connectorId": 1, "duration": 600}),
        )
        .await;
    assert_eq!(status_of(&schedule), "Accepted");
    rig.shutdown().await;
}

#[tokio::test]
async fn authorize_remote_tx_requests_gates_the_start() {
    let rig = booted().await;
    rig.cfg("AuthorizeRemoteTxRequests", "true").await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "RemoteStartTransaction",
            json!({"connectorId": 1, "idTag": "BADTAG"}),
            "Rejected",
        )
        .await
        .unwrap();
    rig.assert_no_calls("StartTransaction", mark);
    assert!(conn(&rig, C1).transaction.is_idle());
    rig.shutdown().await;
}

// ---- Reset -------------------------------------------------------------------------

#[tokio::test]
async fn reset_stops_transactions_and_boots_again() {
    let rig = booted().await;
    rig.begin(1, "TAG1").await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status("Reset", json!({"type": "Soft"}), "Accepted")
        .await
        .unwrap();
    let stop = rig
        .csms
        .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(stop["reason"], "SoftReset");
    rig.csms
        .wait_for_call_after(mark, "BootNotification", |_| true, STEP)
        .await
        .unwrap();
    rig.wait_registration(RegistrationStatus::Accepted).await;
    let c = conn(&rig, C1);
    assert!(c.transaction.is_idle() && !c.ev_connected);
    assert_eq!(c.status, ConnectorStatus::Available);
    rig.shutdown().await;
}

#[tokio::test]
async fn reset_with_a_failing_stop_still_reboots_and_hard_reset_uses_its_reason() {
    let rig = booted().await;
    rig.begin(1, "TAG1").await;
    rig.csms
        .inject(kirby_ocpp_testkit::Fault::call_error("StopTransaction"));
    let mark = rig.csms.mark();
    rig.csms
        .expect_status("Reset", json!({"type": "Hard"}), "Accepted")
        .await
        .unwrap();
    let stop = rig
        .csms
        .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(stop["reason"], "HardReset");
    rig.csms
        .wait_for_call_after(mark, "BootNotification", |_| true, STEP)
        .await
        .unwrap();
    rig.wait_registration(RegistrationStatus::Accepted).await;
    // The failed stop was queued and is delivered after the new boot.
    rig.csms
        .wait_for_call_count_after(mark, "StopTransaction", 2, |_| true, STEP)
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn reset_repeats_the_boot_up_to_reset_retries_times() {
    let rig = booted().await;
    rig.cfg("ResetRetries", "2").await;
    let boots = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&boots);
    rig.csms.respond("BootNotification", move |_| {
        let status = if counter.fetch_add(1, Ordering::SeqCst) < 2 {
            "Pending"
        } else {
            "Accepted"
        };
        Reply::Result(
            json!({"status": status, "currentTime": "2026-03-01T12:00:00Z", "interval": 30}),
        )
    });
    rig.csms
        .expect_status("Reset", json!({"type": "Soft"}), "Accepted")
        .await
        .unwrap();
    // Pending twice at 500 ms spacing (not the 30 s the CSMS asked for), then Accepted.
    rig.wait_registration(RegistrationStatus::Accepted).await;
    assert_eq!(boots.load(Ordering::SeqCst), 3);
    rig.shutdown().await;
}

// ---- reservations -------------------------------------------------------------------

fn reserve(id: i32, connector: u32, id_tag: &str, expiry_in: Delta) -> Value {
    json!({
        "connectorId": connector, "reservationId": id, "idTag": id_tag,
        "expiryDate": (t0() + expiry_in).to_rfc3339(),
    })
}

#[tokio::test]
async fn reservation_lifecycle() {
    let rig = booted().await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "ReserveNow",
            reserve(7, 1, "RES", Delta::hours(1)),
            "Accepted",
        )
        .await
        .unwrap();
    rig.wait_status(mark, 1, "Reserved").await;
    assert_eq!(
        conn(&rig, C1).reservation.unwrap().id,
        ReservationId::new(7)
    );
    // Another reservation on the same connector is occupied; the other tag cannot start.
    rig.csms
        .expect_status(
            "ReserveNow",
            reserve(8, 1, "OTHER", Delta::hours(1)),
            "Occupied",
        )
        .await
        .unwrap();
    let err = rig
        .station
        .send(Command::StartTransaction {
            connector: C1,
            id_tag: tag("OTHER"),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, kirby_ocpp_core::StationError::InvalidState { .. }),
        "{err:?}"
    );
    // The reserved tag can; the reservation id goes out with the start and is consumed.
    let mark = rig.csms.mark();
    rig.begin(1, "RES").await;
    let sent = rig
        .csms
        .wait_for_call_after(mark, "StartTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(sent["reservationId"], 7);
    assert!(conn(&rig, C1).reservation.is_none());
    rig.shutdown().await;
}

#[tokio::test]
async fn cancel_and_move_a_reservation() {
    let rig = booted().await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "ReserveNow",
            reserve(7, 1, "RES", Delta::hours(1)),
            "Accepted",
        )
        .await
        .unwrap();
    // Same id on another connector moves the reservation.
    rig.csms
        .expect_status(
            "ReserveNow",
            reserve(7, 2, "RES", Delta::hours(1)),
            "Accepted",
        )
        .await
        .unwrap();
    rig.wait_status(mark, 2, "Reserved").await;
    assert!(conn(&rig, C1).reservation.is_none());
    assert_eq!(conn(&rig, C1).status, ConnectorStatus::Available);
    assert!(conn(&rig, C2).reservation.is_some());

    let mark = rig.csms.mark();
    rig.csms
        .expect_status("CancelReservation", json!({"reservationId": 7}), "Accepted")
        .await
        .unwrap();
    rig.wait_status(mark, 2, "Available").await;
    rig.csms
        .expect_status("CancelReservation", json!({"reservationId": 7}), "Rejected")
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn reservation_rejections_follow_the_documented_order() {
    let rig = booted().await;
    rig.csms
        .expect_status(
            "ReserveNow",
            reserve(1, 9, "X", Delta::hours(1)),
            "Unavailable",
        )
        .await
        .unwrap();
    rig.csms
        .expect_status(
            "ReserveNow",
            reserve(1, 1, "X", Delta::hours(-1)),
            "Rejected",
        )
        .await
        .unwrap();
    rig.send(Command::Fault {
        connector: C2,
        error_code: kirby_ocpp_core::ocpp::types::ChargePointErrorCode::OtherError,
        info: None,
        vendor_error_code: None,
    })
    .await;
    rig.csms
        .expect_status("ReserveNow", reserve(1, 2, "X", Delta::hours(1)), "Faulted")
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn an_expired_reservation_is_released() {
    let rig = booted().await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "ReserveNow",
            reserve(3, 1, "RES", Delta::seconds(1)),
            "Accepted",
        )
        .await
        .unwrap();
    rig.wait_status(mark, 1, "Reserved").await;
    rig.clock.advance(Delta::seconds(5));
    rig.wait_status(mark, 1, "Available").await;
    assert!(conn(&rig, C1).reservation.is_none());
    rig.shutdown().await;
}

#[tokio::test]
async fn a_parent_id_tag_may_use_a_reservation() {
    let rig = booted().await;
    // Authorize caches PARENTAUTH's parentIdTag (PARENTTAG).
    rig.send(Command::Authorize {
        id_tag: tag("PARENTAUTH"),
    })
    .await;
    let mut request = reserve(4, 1, "RESERVEDTAG", Delta::hours(1));
    request["parentIdTag"] = json!("PARENTTAG");
    rig.csms
        .expect_status("ReserveNow", request, "Accepted")
        .await
        .unwrap();
    let outcome = rig.begin(1, "PARENTAUTH").await;
    assert!(
        matches!(outcome, Outcome::TransactionStarted { .. }),
        "{outcome:?}"
    );
    rig.shutdown().await;
}

// ---- smart charging -------------------------------------------------------------------

#[tokio::test]
async fn charging_profiles_set_clear_and_composite() {
    let rig = booted().await;
    let set = |id, purpose: &str, stack, connector: u32| json!({"connectorId": connector, "csChargingProfiles": profile(id, purpose, stack)});
    rig.csms
        .expect_status(
            "SetChargingProfile",
            set(14, "TxDefaultProfile", 0, 1),
            "Accepted",
        )
        .await
        .unwrap();
    // Limits: stack level above ChargeProfileMaxStackLevel (10), purpose/connector rules.
    for (request, expected) in [
        (set(15, "TxDefaultProfile", 99, 1), "Rejected"),
        (set(16, "ChargePointMaxProfile", 0, 1), "Rejected"),
        (set(17, "TxProfile", 0, 1), "Rejected"), // no running transaction
        (set(18, "TxDefaultProfile", 1, 9), "Rejected"),
    ] {
        rig.csms
            .expect_status("SetChargingProfile", request, expected)
            .await
            .unwrap();
    }
    let schedule = rig
        .csms_call(
            "GetCompositeSchedule",
            json!({"connectorId": 1, "duration": 3600}),
        )
        .await;
    assert_eq!(status_of(&schedule), "Accepted");
    assert_eq!(
        schedule["chargingSchedule"]["chargingSchedulePeriod"][0]["limit"],
        16.0
    );
    let missing = rig
        .csms_call(
            "GetCompositeSchedule",
            json!({"connectorId": 9, "duration": 60}),
        )
        .await;
    assert_eq!(status_of(&missing), "Rejected");

    rig.csms
        .expect_status("ClearChargingProfile", json!({"id": 14}), "Accepted")
        .await
        .unwrap();
    rig.csms
        .expect_status("ClearChargingProfile", json!({"id": 14}), "Unknown")
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn a_tx_profile_needs_a_running_transaction_and_dies_with_it() {
    let rig = booted().await;
    rig.begin(1, "TAG1").await;
    rig.csms
        .expect_status(
            "SetChargingProfile",
            json!({"connectorId": 1, "csChargingProfiles": profile(20, "TxProfile", 0)}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.stop(1, StopReason::Local).await;
    // The profile was bound to transaction 123 and is gone with it.
    rig.csms
        .expect_status("ClearChargingProfile", json!({"id": 20}), "Unknown")
        .await
        .unwrap();
    rig.shutdown().await;
}

// ---- maintenance ----------------------------------------------------------------------

async fn statuses(rig: &Rig, action: &str, count: usize) -> Vec<String> {
    rig.csms
        .wait_for_call_count_after(0, action, count, |_| true, STEP)
        .await
        .unwrap()
        .iter()
        .map(|p| p["status"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn diagnostics_lifecycle_follows_the_selected_outcome() {
    let rig = booted().await;
    let reply = rig
        .csms_call(
            "GetDiagnostics",
            json!({"location": "ftp://example.com/logs"}),
        )
        .await;
    assert!(reply["fileName"].is_string());
    assert_eq!(
        statuses(&rig, "DiagnosticsStatusNotification", 2).await,
        ["Uploading", "Uploaded"]
    );
    wait_state(&rig.station, "uploaded", |s| {
        s.diagnostics_status == DiagnosticsStatus::Uploaded
    })
    .await;
    rig.shutdown().await;
}

#[tokio::test]
async fn failed_diagnostics_retry_a_bounded_number_of_times() {
    let rig = booted().await;
    rig.send(Command::SetDiagnosticsOutcome {
        outcome: kirby_ocpp_core::station::DiagnosticsOutcome::UploadFailure,
    })
    .await;
    rig.csms_call(
        "GetDiagnostics",
        json!({"location": "ftp://example.com/logs", "retries": 1, "retryInterval": 1}),
    )
    .await;
    assert_eq!(
        statuses(&rig, "DiagnosticsStatusNotification", 4).await,
        ["Uploading", "UploadFailed", "Uploading", "UploadFailed"]
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn firmware_lifecycles() {
    for (outcome, expected) in [
        (
            FirmwareOutcome::Success,
            vec!["Downloading", "Downloaded", "Installing", "Installed"],
        ),
        (
            FirmwareOutcome::DownloadFailure,
            vec!["Downloading", "DownloadFailed"],
        ),
        (
            FirmwareOutcome::InstallationFailure,
            vec![
                "Downloading",
                "Downloaded",
                "Installing",
                "InstallationFailed",
            ],
        ),
    ] {
        let rig = booted().await;
        rig.send(Command::SetFirmwareOutcome { outcome }).await;
        rig.csms_call(
            "UpdateFirmware",
            json!({"location": "ftp://example.com/fw.bin", "retrieveDate": (t0()).to_rfc3339()}),
        )
        .await;
        assert_eq!(
            statuses(&rig, "FirmwareStatusNotification", expected.len()).await,
            expected
        );
        wait_state(&rig.station, "final status", |s| {
            s.firmware_status.to_string() == *expected.last().unwrap()
        })
        .await;
        rig.shutdown().await;
    }
}

#[tokio::test]
async fn an_unexpected_disconnect_cancels_the_simulation() {
    let rig = booted().await;
    rig.csms_call(
        "UpdateFirmware",
        json!({"location": "ftp://example.com/fw.bin", "retrieveDate": t0().to_rfc3339()}),
    )
    .await;
    rig.csms
        .wait_for_call("FirmwareStatusNotification", STEP)
        .await
        .unwrap();
    rig.station.disconnect().await.unwrap();
    rig.station.connect().await.unwrap();
    rig.wait_registration(RegistrationStatus::Accepted).await;
    // Deliberate negative wait: the old simulation must not resume.
    let seen = rig.calls("FirmwareStatusNotification", 0).len();
    assert!(seen < 4);
    assert!(
        rig.csms
            .wait_for_call_count_after(
                0,
                "FirmwareStatusNotification",
                seen + 1,
                |_| true,
                std::time::Duration::from_millis(500)
            )
            .await
            .is_err()
    );
    assert_ne!(
        rig.station.snapshot().firmware_status,
        FirmwareStatus::Installed
    );
    rig.shutdown().await;
}

// ---- availability and unlock with transactions ---------------------------------------

#[tokio::test]
async fn availability_change_with_a_running_transaction_is_scheduled_until_it_ends() {
    let rig = booted().await;
    rig.begin(1, "TAG1").await;
    rig.csms
        .expect_status(
            "ChangeAvailability",
            json!({"connectorId": 1, "type": "Inoperative"}),
            "Scheduled",
        )
        .await
        .unwrap();
    let c = conn(&rig, C1);
    assert_eq!(c.pending_availability, Some(AvailabilityType::Inoperative));
    assert_eq!(c.status, ConnectorStatus::Charging);
    let mark = rig.csms.mark();
    rig.stop(1, StopReason::Local).await;
    rig.wait_status(mark, 1, "Unavailable").await;
    let c = conn(&rig, C1);
    assert_eq!(c.availability, AvailabilityType::Inoperative);
    assert_eq!(c.pending_availability, None);
    rig.shutdown().await;
}

#[tokio::test]
async fn unlock_connector_stops_a_running_transaction() {
    let rig = booted().await;
    rig.begin(1, "TAG1").await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status("UnlockConnector", json!({"connectorId": 1}), "Unlocked")
        .await
        .unwrap();
    let stop = rig
        .csms
        .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(stop["reason"], "UnlockCommand");
    rig.shutdown().await;
}

// ---- restoration -------------------------------------------------------------------------

#[tokio::test]
async fn a_running_station_is_restored_after_a_restart() {
    let mut rig = booted().await;
    rig.begin(1, "TAG1").await;
    rig.send(Command::MeterValues {
        connector: C1,
        wh_delta: 40,
        measurands: None,
    })
    .await;
    rig.csms
        .expect_status(
            "ReserveNow",
            reserve(5, 2, "RES", Delta::hours(1)),
            "Accepted",
        )
        .await
        .unwrap();
    rig.csms
        .expect_status(
            "SetChargingProfile",
            json!({"connectorId": 1, "csChargingProfiles": profile(30, "TxDefaultProfile", 0)}),
            "Accepted",
        )
        .await
        .unwrap();

    rig.station.shutdown().await.unwrap();
    rig.join.take().unwrap().await.unwrap();
    let config = test_config(rig.csms.url(), &rig.dir);
    let (station, join) = Station::spawn(config, Arc::new(ManualClock::new(t0())));
    let snapshot = station.snapshot();
    let c = snapshot.connector(C1).unwrap();
    assert_eq!(c.tx_id(), Some(TxId::Csms(123)));
    assert!(matches!(c.transaction, TransactionState::Active(_)));
    assert_eq!(
        (c.meter_wh, c.status, c.ev_connected),
        (40, ConnectorStatus::Charging, true)
    );
    assert_eq!(
        snapshot
            .connector(C2)
            .unwrap()
            .reservation
            .as_ref()
            .unwrap()
            .id,
        ReservationId::new(5)
    );

    let mark = rig.csms.mark();
    station.connect().await.unwrap();
    wait_state(
        &station,
        "booted",
        kirby_ocpp_core::station::Snapshot::booted,
    )
    .await;
    station
        .send(Command::StopTransaction {
            connector: C1,
            reason: StopReason::Local,
            id_tag: None,
        })
        .await
        .unwrap();
    let stop = rig
        .csms
        .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(stop["transactionId"], 123);
    assert_eq!(stop["meterStop"], 40);
    let schedule = rig
        .csms_call(
            "GetCompositeSchedule",
            json!({"connectorId": 1, "duration": 60}),
        )
        .await;
    assert_eq!(status_of(&schedule), "Accepted");
    station.shutdown().await.unwrap();
    join.await.unwrap();
    rig.csms.shutdown().await;
}

#[tokio::test]
async fn reservations_that_expired_while_down_are_dropped_on_restore() {
    let mut rig = booted().await;
    rig.csms
        .expect_status(
            "ReserveNow",
            reserve(5, 2, "RES", Delta::hours(1)),
            "Accepted",
        )
        .await
        .unwrap();
    rig.station.shutdown().await.unwrap();
    rig.join.take().unwrap().await.unwrap();
    let config = test_config(rig.csms.url(), &rig.dir);
    let later = ManualClock::new(t0() + Delta::hours(2));
    let (station, join) = Station::spawn(config, Arc::new(later));
    let snapshot = wait_state(&station, "reservation pruned", |s| {
        s.connector(C2).unwrap().reservation.is_none()
    })
    .await;
    assert_eq!(
        snapshot.connector(C2).unwrap().status,
        ConnectorStatus::Available
    );
    station.shutdown().await.unwrap();
    join.await.unwrap();
    rig.csms.shutdown().await;
}

#[tokio::test]
async fn unknown_csms_calls_after_the_changes_still_get_exactly_one_error() {
    let rig = booted().await;
    rig.csms
        .expect_call_error("Nonsense", json!({}), &ErrorCode::NotImplemented)
        .await
        .unwrap();
    rig.shutdown().await;
}
