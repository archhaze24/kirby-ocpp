//! StatusNotification behaviour: boot, plug/unplug/fault, MinimumStatusDuration, offline.

mod station_support;

use std::time::Duration;

use chrono::{DateTime, Utc};
use kirby_ocpp_core::StationError;
use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::ocpp::types::{ChargePointErrorCode, ConnectorStatus};
use kirby_ocpp_core::station::{Command, Outcome};
use serde_json::json;
use station_support::{Rig, STEP, t0, wait_state};

const C1: ConnectorId = ConnectorId::new(1);

fn parse_ts(value: &serde_json::Value) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value.as_str().unwrap())
        .unwrap()
        .with_timezone(&Utc)
}

#[tokio::test]
async fn plug_unplug_and_fault_send_status_notifications() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;

    let mark = rig.csms.mark();
    rig.station.plug_in(C1).await.unwrap();
    let preparing = rig.wait_status(mark, 1, "Preparing").await;
    assert_eq!(preparing["errorCode"], "NoError");
    assert_eq!(
        rig.station.snapshot().connector(C1).unwrap().status,
        ConnectorStatus::Preparing
    );

    let mark = rig.csms.mark();
    rig.send(Command::Unplug { connector: C1 }).await;
    rig.wait_status(mark, 1, "Available").await;

    let mark = rig.csms.mark();
    rig.send(Command::Fault {
        connector: C1,
        error_code: ChargePointErrorCode::GroundFailure,
        info: Some("  door open ".into()),
        vendor_error_code: Some("E-17".into()),
    })
    .await;
    let faulted = rig.wait_status(mark, 1, "Faulted").await;
    assert_eq!(faulted["errorCode"], "GroundFailure");
    assert_eq!(faulted["info"], "door open");
    assert_eq!(faulted["vendorErrorCode"], "E-17");
    assert_eq!(faulted["vendorId"], "archhaze24");

    let mark = rig.csms.mark();
    rig.send(Command::ClearFault { connector: C1 }).await;
    let cleared = rig.wait_status(mark, 1, "Available").await;
    assert_eq!(cleared["errorCode"], "NoError");
    assert!(cleared.get("info").is_none());
    rig.shutdown().await;
}

#[tokio::test]
async fn same_status_is_resent_when_the_error_code_changes() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    let mark = rig.csms.mark();
    for code in [
        ChargePointErrorCode::OtherError,
        ChargePointErrorCode::GroundFailure,
    ] {
        rig.send(Command::Fault {
            connector: C1,
            error_code: code,
            info: None,
            vendor_error_code: None,
        })
        .await;
    }
    let faults = rig
        .csms
        .wait_for_call_count_after(
            mark,
            "StatusNotification",
            2,
            |p| p["status"] == "Faulted",
            STEP,
        )
        .await
        .unwrap();
    assert_eq!(faults[0]["errorCode"], "OtherError");
    assert_eq!(faults[1]["errorCode"], "GroundFailure");
    rig.shutdown().await;
}

#[tokio::test]
async fn unchanged_state_is_not_notified_again() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    let mark = rig.csms.mark();
    // Available -> Available: set_status to the current status changes nothing observable.
    rig.send(Command::SetConnectorStatus {
        connector: C1,
        status: ConnectorStatus::Available,
    })
    .await;
    // A later, real change proves the earlier no-op produced nothing (calls arrive in order).
    rig.send(Command::SetConnectorStatus {
        connector: C1,
        status: ConnectorStatus::Charging,
    })
    .await;
    rig.wait_status(mark, 1, "Charging").await;
    assert_eq!(rig.calls("StatusNotification", mark).len(), 1);
    rig.shutdown().await;
}

#[tokio::test]
async fn status_commands_cycle_and_resend() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;

    let mark = rig.csms.mark();
    rig.send(Command::CycleConnectorStatus { connector: C1 })
        .await;
    rig.wait_status(mark, 1, "Preparing").await;

    // The explicit command re-sends the current state even though it is unchanged.
    let mark = rig.csms.mark();
    assert_eq!(
        rig.send(Command::StatusNotification { connector: C1 })
            .await,
        Outcome::Done
    );
    rig.wait_status(mark, 1, "Preparing").await;
    assert_eq!(
        rig.send(Command::StatusNotification {
            connector: ConnectorId::STATION
        })
        .await,
        Outcome::Done
    );
    let err = rig
        .station
        .send(Command::StatusNotification {
            connector: ConnectorId::new(9),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StationError::UnknownConnector(_)), "{err:?}");
    rig.shutdown().await;
}

#[tokio::test]
async fn invalid_plug_and_unplug_are_reported() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    let err = rig
        .station
        .send(Command::Unplug { connector: C1 })
        .await
        .unwrap_err();
    assert!(matches!(err, StationError::InvalidState { .. }), "{err:?}");
    rig.station.plug_in(C1).await.unwrap();
    let err = rig.station.plug_in(C1).await.unwrap_err();
    assert!(matches!(err, StationError::InvalidState { .. }), "{err:?}");
    let err = rig.station.plug_in(ConnectorId::STATION).await.unwrap_err();
    assert!(matches!(err, StationError::UnknownConnector(_)), "{err:?}");
    rig.shutdown().await;
}

#[tokio::test]
async fn add_connector_notifies_and_updates_the_configuration() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    let mark = rig.csms.mark();
    let Outcome::ConnectorAdded { connector } = rig.send(Command::AddConnector).await else {
        panic!("expected ConnectorAdded");
    };
    assert_eq!(connector, ConnectorId::new(3));
    rig.wait_status(mark, 3, "Available").await;
    assert_eq!(rig.station.snapshot().connectors.len(), 3);

    let frame = rig
        .csms
        .send_call(
            "GetConfiguration",
            json!({"key": ["NumberOfConnectors", "ConnectorPhaseRotation"]}),
        )
        .await
        .unwrap();
    let text = frame.to_json_string();
    assert!(text.contains(r#""value":"3""#), "{text}");
    assert!(text.contains("3.Unknown"), "{text}");
    rig.shutdown().await;
}

#[tokio::test]
async fn statuses_changed_while_offline_are_sent_after_the_boot() {
    let rig = Rig::start().await;
    // Offline: the change happens, nothing can be sent.
    rig.station.plug_in(C1).await.unwrap();
    rig.station.connect().await.unwrap();
    let mark = 0;
    rig.wait_status(mark, 1, "Preparing").await;
    rig.wait_status(mark, 2, "Available").await;
    rig.wait_status(mark, 0, "Available").await;
    // Exactly one notification per connector: nothing was queued while offline.
    assert_eq!(rig.calls("StatusNotification", 0).len(), 3);
    rig.shutdown().await;
}

#[tokio::test]
async fn minimum_status_duration_uses_the_time_of_the_change() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig.csms
        .expect_status(
            "ChangeConfiguration",
            json!({"key": "MinimumStatusDuration", "value": "1"}),
            "Accepted",
        )
        .await
        .unwrap();

    let mark = rig.csms.mark();
    rig.station.plug_in(C1).await.unwrap();
    // The wall clock moves on while the debounce waits.
    rig.clock.advance(chrono::Duration::seconds(10));
    let sent = rig.wait_status(mark, 1, "Preparing").await;
    assert_eq!(
        parse_ts(&sent["timestamp"]),
        t0(),
        "timestamp is the change time"
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn a_change_reverted_within_the_window_sends_nothing() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig.csms
        .expect_status(
            "ChangeConfiguration",
            json!({"key": "MinimumStatusDuration", "value": "1"}),
            "Accepted",
        )
        .await
        .unwrap();

    let mark = rig.csms.mark();
    rig.station.plug_in(C1).await.unwrap();
    rig.send(Command::Unplug { connector: C1 }).await;
    // A different connector's change after the window proves the debounce has fired.
    rig.station.plug_in(ConnectorId::new(2)).await.unwrap();
    rig.wait_status(mark, 2, "Preparing").await;
    let ones: Vec<_> = rig
        .calls("StatusNotification", mark)
        .into_iter()
        .filter(|p| p["connectorId"] == 1)
        .collect();
    assert!(
        ones.is_empty(),
        "connector 1 returned to Available: {ones:?}"
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn setting_the_minimum_duration_to_zero_flushes_what_is_waiting() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig.csms
        .expect_status(
            "ChangeConfiguration",
            json!({"key": "MinimumStatusDuration", "value": "3600"}),
            "Accepted",
        )
        .await
        .unwrap();
    let mark = rig.csms.mark();
    rig.station.plug_in(C1).await.unwrap();
    rig.clock.advance(chrono::Duration::seconds(5));
    assert!(
        rig.csms
            .wait_for_call_after(
                mark,
                "StatusNotification",
                |_| true,
                Duration::from_millis(200)
            )
            .await
            .is_err(),
        "still debounced"
    );
    rig.csms
        .expect_status(
            "ChangeConfiguration",
            json!({"key": "MinimumStatusDuration", "value": "0"}),
            "Accepted",
        )
        .await
        .unwrap();
    let sent = rig.wait_status(mark, 1, "Preparing").await;
    assert_eq!(parse_ts(&sent["timestamp"]), t0());
    rig.shutdown().await;
}

#[tokio::test]
async fn connector_zero_fault_is_reported() {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    let mark = rig.csms.mark();
    rig.send(Command::Fault {
        connector: ConnectorId::STATION,
        error_code: ChargePointErrorCode::HighTemperature,
        info: None,
        vendor_error_code: None,
    })
    .await;
    let sent = rig.wait_status(mark, 0, "Faulted").await;
    assert_eq!(sent["errorCode"], "HighTemperature");
    let snapshot = wait_state(&rig.station, "charge point faulted", |s| {
        s.charge_point.status == ConnectorStatus::Faulted
    })
    .await;
    assert_eq!(
        snapshot.charge_point.error_code,
        ChargePointErrorCode::HighTemperature
    );
    rig.shutdown().await;
}
