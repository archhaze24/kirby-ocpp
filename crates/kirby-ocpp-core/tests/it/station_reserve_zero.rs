//! `ReserveNow` on connector 0 (`ReserveConnectorZeroSupported`): no connector is locked, but
//! at least one connector stays available for the reserved idTag (OCPP 1.6 section 5.13).

use std::sync::Arc;

use crate::station_support::{Rig, STEP, t0, tag, test_config, wait_state};
use chrono::Duration as Delta;
use kirby_ocpp_core::ids::{ConnectorId, ReservationId};
use kirby_ocpp_core::ocpp::types::ConnectorStatus;
use kirby_ocpp_core::station::{Command, Outcome};
use kirby_ocpp_core::{ManualClock, Station, StationError};
use kirby_ocpp_testkit::{CsmsOptions, Fault};
use serde_json::{Value, json};

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

fn reserve_zero(id: i32, id_tag: &str, expiry_in: Delta) -> Value {
    json!({
        "connectorId": 0, "reservationId": id, "idTag": id_tag,
        "expiryDate": (t0() + expiry_in).to_rfc3339(),
    })
}

async fn reserve(rig: &Rig, request: Value, status: &str) {
    rig.csms
        .expect_status("ReserveNow", request, status)
        .await
        .unwrap();
}

#[tokio::test]
async fn another_tag_may_use_a_connector_until_one_is_left_then_it_is_held() {
    let rig = booted().await;
    reserve(&rig, reserve_zero(20, "RES", Delta::hours(1)), "Accepted").await;
    // Nothing is locked while two connectors are free.
    assert_eq!(conn(&rig, C1).status, ConnectorStatus::Available);
    assert_eq!(conn(&rig, C2).status, ConnectorStatus::Available);
    assert_eq!(
        rig.station
            .snapshot()
            .charge_point
            .reservation
            .as_ref()
            .map(|r| r.id),
        Some(ReservationId::new(20))
    );
    let mark = rig.csms.mark();
    let outcome = rig.begin(1, "TAG1").await;
    assert!(
        matches!(outcome, Outcome::TransactionStarted { .. }),
        "{outcome:?}"
    );
    // Only connector 2 is left: it is kept for the reserved idTag.
    rig.wait_status(mark, 2, "Reserved").await;
    let err = rig
        .station
        .send(Command::StartTransaction {
            connector: C2,
            id_tag: tag("OTHER"),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StationError::InvalidState { .. }), "{err:?}");
    // The reserved idTag starts there; the reservation id goes out and is consumed.
    let mark = rig.csms.mark();
    let outcome = rig.begin(2, "RES").await;
    assert!(
        matches!(outcome, Outcome::TransactionStarted { .. }),
        "{outcome:?}"
    );
    let sent = rig
        .csms
        .wait_for_call_after(mark, "StartTransaction", |p| p["connectorId"] == 2, STEP)
        .await
        .unwrap();
    assert_eq!(sent["reservationId"], 20);
    let snapshot = rig.station.snapshot();
    assert!(snapshot.charge_point.reservation.is_none());
    assert!(conn(&rig, C2).reservation.is_none());
    rig.shutdown().await;
}

#[tokio::test]
async fn the_reserved_tag_may_start_on_any_connector_and_consumes_the_reservation() {
    let rig = booted().await;
    reserve(&rig, reserve_zero(21, "RES", Delta::hours(1)), "Accepted").await;
    let mark = rig.csms.mark();
    rig.begin(2, "RES").await;
    let sent = rig
        .csms
        .wait_for_call_after(mark, "StartTransaction", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(sent["reservationId"], 21);
    assert!(rig.station.snapshot().charge_point.reservation.is_none());
    // Consumed: the other connector is not held for anybody.
    let outcome = rig.begin(1, "TAG1").await;
    assert!(
        matches!(outcome, Outcome::TransactionStarted { .. }),
        "{outcome:?}"
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn a_held_connector_waits_for_start_acceptance_before_charging() {
    tokio::time::timeout(STEP, async {
        let mut rig = Rig::with(CsmsOptions::default(), |mut config| {
            config.call_timeout_ms = 6_000;
            config
        })
        .await;
        rig.connect_booted().await;
        reserve(&rig, reserve_zero(29, "RES", Delta::hours(1)), "Accepted").await;
        let mark = rig.csms.mark();
        rig.begin(1, "TAG1").await;
        rig.wait_status(mark, 2, "Reserved").await;

        let mark = rig.csms.mark();
        let meter_start = conn(&rig, C2).meter_wh;
        rig.csms.inject(Fault::silence("StartTransaction").times(1));
        let station = rig.station.clone();
        let start = tokio::spawn(async move {
            station
                .send(Command::StartTransaction {
                    connector: C2,
                    id_tag: tag("RES"),
                })
                .await
        });
        rig.csms
            .wait_for_call_after(mark, "StartTransaction", |_| true, STEP)
            .await
            .unwrap();
        let request = rig
            .csms
            .calls_since(mark)
            .into_iter()
            .find(|call| call.action == "StartTransaction")
            .unwrap();
        let other_meter = conn(&rig, C1).meter_wh;
        rig.cfg("MeterValueSampleInterval", "1").await;
        let sampled = wait_state(&rig.station, "meter tick while the start is pending", |s| {
            s.connector(C1).unwrap().meter_wh >= other_meter + 2
        })
        .await;
        let meter = sampled.connector(C2).unwrap().meter_wh;
        rig.csms
            .send_raw(
                json!([3, request.message_id, {
                    "transactionId": 300,
                    "idTagInfo": {"status": "Blocked"}
                }])
                .to_string(),
            )
            .unwrap();
        assert!(matches!(
            start.await.unwrap().unwrap(),
            Outcome::TransactionRejected { .. }
        ));
        rig.csms
            .wait_for_call_after(mark, "StopTransaction", |_| true, STEP)
            .await
            .unwrap();
        assert!(
            rig.calls("StatusNotification", mark)
                .iter()
                .all(|call| call["connectorId"] != 2 || call["status"] != "Charging")
        );
        assert_eq!(meter, meter_start);
        assert_eq!(conn(&rig, C2).meter_wh, meter_start);
        let retry = rig.csms.mark();
        assert!(matches!(
            rig.begin(2, "RES").await,
            Outcome::TransactionStarted { .. }
        ));
        let request = rig
            .csms
            .wait_for_call_after(retry, "StartTransaction", |_| true, STEP)
            .await
            .unwrap();
        assert_eq!(request["reservationId"], 29);
        rig.shutdown().await;
    })
    .await
    .expect("reservation start scenario");
}

#[tokio::test]
async fn concurrent_starts_carry_a_connector_zero_reservation_only_once() {
    tokio::time::timeout(STEP, async {
        let rig = booted().await;
        reserve(&rig, reserve_zero(30, "RES", Delta::hours(1)), "Accepted").await;
        let mark = rig.csms.mark();
        rig.csms.inject(Fault::silence("StartTransaction").times(1));
        let station = rig.station.clone();
        let first = tokio::spawn(async move {
            station
                .send(Command::StartTransaction {
                    connector: C1,
                    id_tag: tag("RES"),
                })
                .await
        });
        let sent = rig
            .csms
            .wait_for_call_after(mark, "StartTransaction", |p| p["connectorId"] == 1, STEP)
            .await
            .unwrap();
        assert_eq!(sent["reservationId"], 30);

        let station = rig.station.clone();
        let second = tokio::spawn(async move {
            station
                .send(Command::StartTransaction {
                    connector: C2,
                    id_tag: tag("RES"),
                })
                .await
        });
        wait_state(&rig.station, "second start queued", |s| {
            !s.connector(C2).unwrap().transaction.is_idle()
        })
        .await;
        assert!(rig.station.snapshot().charge_point.reservation.is_some());

        let request = rig
            .csms
            .calls_since(mark)
            .into_iter()
            .find(|call| call.action == "StartTransaction" && call.payload["connectorId"] == 1)
            .unwrap();
        rig.csms
            .send_raw(
                json!([3, request.message_id, {
                    "transactionId": 301,
                    "idTagInfo": {"status": "Accepted"}
                }])
                .to_string(),
            )
            .unwrap();
        assert!(matches!(
            first.await.unwrap().unwrap(),
            Outcome::TransactionStarted { .. }
        ));
        assert!(matches!(
            second.await.unwrap().unwrap(),
            Outcome::TransactionStarted { .. }
        ));
        let calls = rig.calls("StartTransaction", mark);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls.iter().filter(|p| p["reservationId"] == 30).count(), 1);
        assert!(rig.station.snapshot().charge_point.reservation.is_none());
        rig.shutdown().await;
    })
    .await
    .expect("concurrent reservation starts");
}

#[tokio::test]
async fn cancel_releases_the_held_connector() {
    let rig = booted().await;
    reserve(&rig, reserve_zero(22, "RES", Delta::hours(1)), "Accepted").await;
    let mark = rig.csms.mark();
    rig.begin(1, "TAG1").await;
    rig.wait_status(mark, 2, "Reserved").await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "CancelReservation",
            json!({"reservationId": 22}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.wait_status(mark, 2, "Available").await;
    assert!(rig.station.snapshot().charge_point.reservation.is_none());
    assert!(conn(&rig, C2).reservation.is_none());
    rig.csms
        .expect_status(
            "CancelReservation",
            json!({"reservationId": 22}),
            "Rejected",
        )
        .await
        .unwrap();
    let outcome = rig.begin(2, "OTHER").await;
    assert!(
        matches!(outcome, Outcome::TransactionStarted { .. }),
        "{outcome:?}"
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn expiry_releases_the_held_connector() {
    let rig = booted().await;
    reserve(&rig, reserve_zero(23, "RES", Delta::seconds(1)), "Accepted").await;
    let mark = rig.csms.mark();
    rig.begin(1, "TAG1").await;
    rig.wait_status(mark, 2, "Reserved").await;
    rig.clock.advance(Delta::seconds(5));
    rig.wait_status(mark, 2, "Available").await;
    assert!(rig.station.snapshot().charge_point.reservation.is_none());
    rig.shutdown().await;
}

#[tokio::test]
async fn a_finished_transaction_frees_a_connector_and_the_hold_moves_away() {
    let rig = booted().await;
    reserve(&rig, reserve_zero(24, "RES", Delta::hours(1)), "Accepted").await;
    let mark = rig.csms.mark();
    rig.begin(1, "TAG1").await;
    rig.wait_status(mark, 2, "Reserved").await;
    let mark = rig.csms.mark();
    rig.stop(1, kirby_ocpp_core::ocpp::types::StopReason::Local)
        .await;
    // `Finishing` until the EV is unplugged: still only one free connector.
    rig.send(Command::Unplug { connector: C1 }).await;
    // Two connectors are free again, nothing is held.
    rig.wait_status(mark, 2, "Available").await;
    assert_eq!(conn(&rig, C2).status, ConnectorStatus::Available);
    rig.shutdown().await;
}

#[tokio::test]
async fn a_busy_station_is_occupied() {
    let rig = booted().await;
    rig.begin(1, "TAG1").await;
    rig.begin(2, "TAG2").await;
    reserve(&rig, reserve_zero(25, "RES", Delta::hours(1)), "Occupied").await;
    rig.shutdown().await;
}

#[tokio::test]
async fn a_second_connector_zero_reservation_is_occupied_and_the_same_id_replaces() {
    let rig = booted().await;
    reserve(&rig, reserve_zero(26, "RES", Delta::hours(1)), "Accepted").await;
    reserve(&rig, reserve_zero(27, "OTHER", Delta::hours(1)), "Occupied").await;
    reserve(
        &rig,
        reserve_zero(26, "NEWTAG", Delta::hours(2)),
        "Accepted",
    )
    .await;
    let snapshot = rig.station.snapshot();
    let stored = snapshot.charge_point.reservation.as_ref().unwrap();
    assert_eq!(stored.id_tag.as_str(), "NEWTAG");
    rig.shutdown().await;
}

#[tokio::test]
async fn the_reservation_survives_a_restart() {
    let mut rig = booted().await;
    reserve(&rig, reserve_zero(28, "RES", Delta::hours(1)), "Accepted").await;
    let mark = rig.csms.mark();
    rig.begin(1, "TAG1").await;
    rig.wait_status(mark, 2, "Reserved").await;
    rig.station.shutdown().await.unwrap();
    rig.join.take().unwrap().await.unwrap();
    let config = test_config(rig.csms.url(), &rig.dir);
    let (station, join) = Station::spawn(config, Arc::new(ManualClock::new(t0())));
    let snapshot = wait_state(&station, "restored", |s| {
        s.charge_point.reservation.is_some()
    })
    .await;
    assert_eq!(
        snapshot.charge_point.reservation.as_ref().unwrap().id,
        ReservationId::new(28)
    );
    assert_eq!(
        snapshot.connector(C2).unwrap().status,
        ConnectorStatus::Reserved
    );
    station.shutdown().await.unwrap();
    join.await.unwrap();
    rig.csms.shutdown().await;
}
