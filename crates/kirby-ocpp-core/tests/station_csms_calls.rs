//! CSMS-originated CALLs against a running station: configuration, TriggerMessage,
//! availability, DataTransfer, local list, cache, error mapping, exactly-one-reply.

mod station_support;

use std::collections::BTreeMap;

use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::ocpp::types::{ConnectorStatus, DiagnosticsStatus, FirmwareStatus};
use kirby_ocpp_core::ocpp::{ErrorCode, Frame};
use kirby_ocpp_core::station::{Command, Direction, Outcome};
use serde_json::{Value, json};
use station_support::{Rig, STEP};

const C1: ConnectorId = ConnectorId::new(1);

async fn booted() -> Rig {
    let mut rig = Rig::start().await;
    rig.connect_booted().await;
    rig
}

async fn payload(rig: &Rig, action: &str, request: Value) -> Value {
    match rig.csms.send_call(action, request).await.unwrap() {
        Frame::CallResult { payload, .. } => payload,
        other => panic!(
            "{action}: expected a CALLRESULT, got {}",
            other.to_json_string()
        ),
    }
}

async fn config_value(rig: &Rig, key: &str) -> Option<String> {
    let result = payload(rig, "GetConfiguration", json!({"key": [key]})).await;
    result["configurationKey"][0]["value"]
        .as_str()
        .map(str::to_owned)
}

// ---- configuration -------------------------------------------------------------

#[tokio::test]
async fn get_configuration_returns_all_known_and_reports_unknown_keys() {
    let rig = booted().await;
    let all = payload(&rig, "GetConfiguration", json!({})).await;
    let keys: Vec<&str> = all["configurationKey"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["key"].as_str().unwrap())
        .collect();
    for key in [
        "ClockAlignedDataInterval",
        "SupportedFeatureProfiles",
        "StopTransactionOnEVSideDisconnect",
        "ChargeProfileMaxStackLevel",
    ] {
        assert!(keys.contains(&key), "{key} missing");
    }
    assert!(all.get("unknownKey").is_none());

    let partial = payload(
        &rig,
        "GetConfiguration",
        json!({"key": ["HeartbeatInterval", "TotallyUnknownConfigurationKey"]}),
    )
    .await;
    assert_eq!(partial["configurationKey"][0]["key"], "HeartbeatInterval");
    assert_eq!(partial["configurationKey"][0]["readonly"], false);
    assert_eq!(
        partial["unknownKey"],
        json!(["TotallyUnknownConfigurationKey"])
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn configuration_keys_are_case_insensitive() {
    let rig = booted().await;
    let result = payload(
        &rig,
        "GetConfiguration",
        json!({"key": ["heartbeatINTERVAL"]}),
    )
    .await;
    assert_eq!(result["configurationKey"][0]["key"], "HeartbeatInterval");
    rig.csms
        .expect_status(
            "ChangeConfiguration",
            json!({"key": "authorizationcacheenabled", "value": "false"}),
            "Accepted",
        )
        .await
        .unwrap();
    assert_eq!(
        config_value(&rig, "AuthorizationCacheEnabled")
            .await
            .as_deref(),
        Some("false")
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn too_many_configuration_keys_is_a_call_error() {
    let rig = booted().await;
    let keys: Vec<String> = (0..51).map(|i| format!("Key{i}")).collect();
    rig.csms
        .expect_call_error(
            "GetConfiguration",
            json!({"key": keys}),
            &ErrorCode::OccurenceConstraintViolation,
        )
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn change_configuration_matrix() {
    let rig = booted().await;
    let cases = [
        ("TotallyUnknownConfigurationKey", "1", "NotSupported"),
        ("NumberOfConnectors", "7", "Rejected"),
        ("StopTransactionOnEVSideDisconnect", "maybe", "Rejected"),
        (
            "MeterValuesSampledData",
            "Energy.Active.Import.Register,Totally.Not.Real",
            "Rejected",
        ),
        ("HeartbeatInterval", "0", "Rejected"),
        ("MeterValueSampleInterval", "-1", "Rejected"),
        ("AuthorizationCacheEnabled", "false", "Accepted"),
        ("AuthorizationCacheEnabled", "true", "Accepted"),
        ("LocalPreAuthorize", "true", "Accepted"),
        (
            "MeterValuesSampledData",
            "Energy.Active.Import.Register,Voltage",
            "Accepted",
        ),
    ];
    for (key, value, expected) in cases {
        rig.csms
            .expect_status(
                "ChangeConfiguration",
                json!({"key": key, "value": value}),
                expected,
            )
            .await
            .unwrap_or_else(|e| panic!("{key}={value}: {e}"));
    }
    assert_eq!(
        config_value(&rig, "LocalPreAuthorize").await.as_deref(),
        Some("true")
    );
    assert_eq!(
        config_value(&rig, "MeterValuesSampledData")
            .await
            .as_deref(),
        Some("Energy.Active.Import.Register,Voltage")
    );
    assert_eq!(
        config_value(&rig, "NumberOfConnectors").await.as_deref(),
        Some("2")
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn changing_the_heartbeat_interval_rearms_the_heartbeat() {
    let rig = booted().await;
    rig.csms
        .expect_status(
            "ChangeConfiguration",
            json!({"key": "HeartbeatInterval", "value": "1"}),
            "Accepted",
        )
        .await
        .unwrap();
    assert_eq!(rig.station.snapshot().heartbeat_interval_secs, 1);
    rig.csms.wait_for_call("Heartbeat", STEP).await.unwrap();
    rig.shutdown().await;
}

// ---- TriggerMessage ------------------------------------------------------------

#[tokio::test]
async fn trigger_message_matrix() {
    let rig = booted().await;

    // BootNotification and Heartbeat.
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "BootNotification"}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.csms
        .wait_for_call_after(mark, "BootNotification", |_| true, STEP)
        .await
        .unwrap();
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "Heartbeat"}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.csms
        .wait_for_call_after(mark, "Heartbeat", |_| true, STEP)
        .await
        .unwrap();

    // StatusNotification: connector 0, a connector, an unknown connector, and all.
    for id in [0_u32, 1] {
        let mark = rig.csms.mark();
        rig.csms
            .expect_status(
                "TriggerMessage",
                json!({"requestedMessage": "StatusNotification", "connectorId": id}),
                "Accepted",
            )
            .await
            .unwrap();
        let sent = rig
            .csms
            .wait_for_call_after(mark, "StatusNotification", |_| true, STEP)
            .await
            .unwrap();
        assert_eq!(sent["connectorId"], id);
        assert_eq!(sent["status"], "Available");
    }
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "StatusNotification", "connectorId": 9}),
            "Rejected",
        )
        .await
        .unwrap();
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "StatusNotification"}),
            "Accepted",
        )
        .await
        .unwrap();
    let all = rig
        .csms
        .wait_for_call_count_after(mark, "StatusNotification", 3, |_| true, STEP)
        .await
        .unwrap();
    let mut ids: Vec<u64> = all
        .iter()
        .map(|p| p["connectorId"].as_u64().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        [0, 1, 2],
        "without connectorId every connector, including 0"
    );

    rig.shutdown().await;
}

#[tokio::test]
async fn trigger_meter_values_matrix() {
    let rig = booted().await;
    // MeterValues: a one-off sample (context Trigger), no transactionId, meter not advanced.
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "MeterValues", "connectorId": 1}),
            "Accepted",
        )
        .await
        .unwrap();
    let sent = rig
        .csms
        .wait_for_call_after(mark, "MeterValues", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(sent["connectorId"], 1);
    assert!(sent.get("transactionId").is_none());
    let samples = sent["meterValue"][0]["sampledValue"].as_array().unwrap();
    assert_ne!(samples.len(), 0);
    assert!(
        samples.iter().all(|v| v["context"] == "Trigger"),
        "{samples:?}"
    );
    assert_eq!(rig.station.snapshot().connector(C1).unwrap().meter_wh, 0);
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "MeterValues", "connectorId": 0}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.csms
        .wait_for_call_after(mark, "MeterValues", |p| p["connectorId"] == 0, STEP)
        .await
        .unwrap();
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "MeterValues", "connectorId": 9}),
            "Rejected",
        )
        .await
        .unwrap();
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "MeterValues"}),
            "Accepted",
        )
        .await
        .unwrap();
    let all = rig
        .csms
        .wait_for_call_count_after(mark, "MeterValues", 2, |_| true, STEP)
        .await
        .unwrap();
    let mut ids: Vec<u64> = all
        .iter()
        .map(|p| p["connectorId"].as_u64().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, [1, 2], "without connectorId every physical connector");
    rig.shutdown().await;
}

#[tokio::test]
async fn triggered_diagnostics_and_firmware_report_their_current_status() {
    let rig = booted().await;
    rig.send(Command::DiagnosticsStatusNotification {
        status: DiagnosticsStatus::Uploaded,
    })
    .await;
    rig.send(Command::FirmwareStatusNotification {
        status: FirmwareStatus::Installing,
    })
    .await;
    let snapshot = rig.station.snapshot();
    assert_eq!(snapshot.diagnostics_status, DiagnosticsStatus::Uploaded);
    assert_eq!(snapshot.firmware_status, FirmwareStatus::Installing);

    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "DiagnosticsStatusNotification"}),
            "Accepted",
        )
        .await
        .unwrap();
    let sent = rig
        .csms
        .wait_for_call_after(mark, "DiagnosticsStatusNotification", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(sent["status"], "Uploaded");
    rig.csms
        .expect_status(
            "TriggerMessage",
            json!({"requestedMessage": "FirmwareStatusNotification"}),
            "Accepted",
        )
        .await
        .unwrap();
    let sent = rig
        .csms
        .wait_for_call_after(mark, "FirmwareStatusNotification", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(sent["status"], "Installing");
    rig.shutdown().await;
}

#[tokio::test]
async fn the_trigger_reply_precedes_the_triggered_message_on_the_wire() {
    let mut rig = booted().await;
    rig.observed.drain();
    let reply = rig
        .csms
        .send_call(
            "TriggerMessage",
            json!({"requestedMessage": "StatusNotification", "connectorId": 1}),
        )
        .await
        .unwrap();
    let reply_id = reply.id().to_owned();
    // The CALLRESULT and then a StatusNotification CALL must appear, in this order.
    rig.observed
        .wait_until("the CALLRESULT followed by the StatusNotification", |o| {
            call_after_result(&o.frames, &reply_id)
        })
        .await;
    rig.shutdown().await;
}

/// `true` if the outgoing frames contain the CALLRESULT of `reply_id` and, after it, a
/// `StatusNotification` CALL.
fn call_after_result(frames: &[kirby_ocpp_core::station::FrameRecord], reply_id: &str) -> bool {
    let out: Vec<_> = frames
        .iter()
        .filter(|f| f.direction == Direction::Out)
        .collect();
    let Some(result) = out
        .iter()
        .position(|f| f.msg_id.as_deref() == Some(reply_id) && f.frame[0] == 3)
    else {
        return false;
    };
    out[result + 1..]
        .iter()
        .any(|f| f.action.as_deref() == Some("StatusNotification") && f.frame[0] == 2)
}

// ---- availability and unlock -----------------------------------------------------

#[tokio::test]
async fn change_availability_of_one_connector() {
    let rig = booted().await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "ChangeAvailability",
            json!({"connectorId": 1, "type": "Inoperative"}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.wait_status(mark, 1, "Unavailable").await;
    let c1 = rig.station.snapshot().connector(C1).unwrap().clone();
    assert_eq!(c1.status, ConnectorStatus::Unavailable);
    assert_eq!(
        c1.availability,
        kirby_ocpp_core::ocpp::types::AvailabilityType::Inoperative
    );

    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "ChangeAvailability",
            json!({"connectorId": 1, "type": "Operative"}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.wait_status(mark, 1, "Available").await;
    rig.csms
        .expect_status(
            "ChangeAvailability",
            json!({"connectorId": 9, "type": "Operative"}),
            "Rejected",
        )
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn change_availability_of_connector_zero_covers_everything() {
    let rig = booted().await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "ChangeAvailability",
            json!({"connectorId": 0, "type": "Inoperative"}),
            "Accepted",
        )
        .await
        .unwrap();
    for id in 0..=2 {
        rig.wait_status(mark, id, "Unavailable").await;
    }
    let snapshot = rig.station.snapshot();
    assert_eq!(snapshot.charge_point.status, ConnectorStatus::Unavailable);
    assert!(
        snapshot
            .connectors
            .iter()
            .all(|c| c.status == ConnectorStatus::Unavailable)
    );

    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "ChangeAvailability",
            json!({"connectorId": 0, "type": "Operative"}),
            "Accepted",
        )
        .await
        .unwrap();
    for id in 0..=2 {
        rig.wait_status(mark, id, "Available").await;
    }
    rig.shutdown().await;
}

#[tokio::test]
async fn a_faulted_connector_stays_faulted_on_availability_changes() {
    let rig = booted().await;
    rig.send(Command::Fault {
        connector: C1,
        error_code: kirby_ocpp_core::ocpp::types::ChargePointErrorCode::OtherError,
        info: None,
        vendor_error_code: None,
    })
    .await;
    rig.csms
        .expect_status(
            "ChangeAvailability",
            json!({"connectorId": 1, "type": "Inoperative"}),
            "Accepted",
        )
        .await
        .unwrap();
    assert_eq!(
        rig.station.snapshot().connector(C1).unwrap().status,
        ConnectorStatus::Faulted
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn unlock_connector() {
    let rig = booted().await;
    rig.csms
        .expect_status("UnlockConnector", json!({"connectorId": 1}), "Unlocked")
        .await
        .unwrap();
    for id in [0, 9] {
        rig.csms
            .expect_status(
                "UnlockConnector",
                json!({"connectorId": id}),
                "NotSupported",
            )
            .await
            .unwrap();
    }
    rig.shutdown().await;
}

// ---- DataTransfer -----------------------------------------------------------------

#[tokio::test]
async fn inbound_data_transfer_checks_the_vendor_id() {
    let rig = booted().await;
    let ok = payload(
        &rig,
        "DataTransfer",
        json!({"vendorId": "archhaze24", "messageId": "x", "data": "hello"}),
    )
    .await;
    assert_eq!(ok, json!({"status": "Accepted", "data": "hello"}));
    let unknown = payload(&rig, "DataTransfer", json!({"vendorId": "someone-else"})).await;
    assert_eq!(unknown, json!({"status": "UnknownVendorId"}));
    rig.shutdown().await;
}

#[tokio::test]
async fn user_data_transfer_returns_the_csms_answer() {
    let rig = booted().await;
    let Outcome::Response { payload } = rig
        .send(Command::DataTransfer {
            vendor_id: "archhaze24".into(),
            message_id: Some("ping".into()),
            data: None,
        })
        .await
    else {
        panic!("expected a response");
    };
    assert_eq!(payload, json!({"status": "Accepted", "data": "pong"}));
    let sent = rig.csms.wait_for_call("DataTransfer", STEP).await.unwrap();
    assert_eq!(sent["messageId"], "ping");
    rig.shutdown().await;
}

#[tokio::test]
async fn send_raw_can_bypass_local_validation() {
    let rig = booted().await;
    // A schema-invalid request is refused locally...
    let err = rig
        .station
        .send(Command::SendRaw {
            action: "Authorize".into(),
            payload: json!({}),
            validate: true,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            kirby_ocpp_core::StationError::Call(kirby_ocpp_core::CallError::Schema(_))
        ),
        "{err:?}"
    );
    rig.assert_no_calls("Authorize", 0);
    // ...and goes out when validation is switched off.
    rig.send(Command::SendRaw {
        action: "Authorize".into(),
        payload: json!({}),
        validate: false,
    })
    .await;
    assert_eq!(rig.calls("Authorize", 0).len(), 1);
    rig.shutdown().await;
}

// ---- local authorization list and cache -------------------------------------------

#[tokio::test]
async fn send_local_list_and_get_local_list_version() {
    let rig = booted().await;
    let version = |rig: &Rig| rig.station.snapshot().local_list_version;
    assert_eq!(
        payload(&rig, "GetLocalListVersion", json!({})).await["listVersion"],
        0
    );

    let status = |value: Value| value["status"].as_str().unwrap().to_owned();
    let full = json!({
        "listVersion": 1,
        "updateType": "Full",
        "localAuthorizationList": [{"idTag": "TAG1", "idTagInfo": {"status": "Accepted"}}],
    });
    assert_eq!(
        status(payload(&rig, "SendLocalList", full).await),
        "Accepted"
    );
    assert_eq!(
        payload(&rig, "GetLocalListVersion", json!({})).await["listVersion"],
        1
    );
    assert_eq!(version(&rig), 1);

    let delete = json!({"listVersion": 2, "updateType": "Differential", "localAuthorizationList": [{"idTag": "TAG1"}]});
    assert_eq!(
        status(payload(&rig, "SendLocalList", delete.clone()).await),
        "Accepted"
    );
    // Same version again: a differential update must increase it.
    assert_eq!(
        status(payload(&rig, "SendLocalList", delete).await),
        "VersionMismatch"
    );
    assert_eq!(version(&rig), 2);

    let too_long: Vec<Value> = (0..1001)
        .map(|i| json!({"idTag": format!("BULK{i}"), "idTagInfo": {"status": "Accepted"}}))
        .collect();
    let bulk =
        json!({"listVersion": 7, "updateType": "Differential", "localAuthorizationList": too_long});
    assert_eq!(status(payload(&rig, "SendLocalList", bulk).await), "Failed");
    assert_eq!(version(&rig), 2, "a failed update changes nothing");

    // Malformed entry: the schema, not the station, answers.
    rig.csms
        .expect_call_error(
            "SendLocalList",
            json!({"listVersion": 6, "updateType": "Differential", "localAuthorizationList": [{}]}),
            &ErrorCode::OccurenceConstraintViolation,
        )
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn disabled_local_list_is_not_supported_and_reports_minus_one() {
    let rig = booted().await;
    rig.csms
        .expect_status(
            "ChangeConfiguration",
            json!({"key": "LocalAuthListEnabled", "value": "false"}),
            "Accepted",
        )
        .await
        .unwrap();
    rig.csms
        .expect_status(
            "SendLocalList",
            json!({"listVersion": 5, "updateType": "Differential",
                   "localAuthorizationList": [{"idTag": "X", "idTagInfo": {"status": "Accepted"}}]}),
            "NotSupported",
        )
        .await
        .unwrap();
    assert_eq!(
        payload(&rig, "GetLocalListVersion", json!({})).await["listVersion"],
        -1
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn clear_cache_is_accepted() {
    let rig = booted().await;
    rig.csms
        .expect_status("ClearCache", json!({}), "Accepted")
        .await
        .unwrap();
    rig.shutdown().await;
}

// ---- error mapping and exactly-one-reply ------------------------------------------

#[tokio::test]
async fn schema_invalid_calls_get_the_matching_call_error() {
    let rig = booted().await;
    let cases = [
        (
            "ChangeConfiguration",
            json!({"key": "HeartbeatInterval"}),
            ErrorCode::OccurenceConstraintViolation,
        ),
        (
            "ChangeConfiguration",
            json!({"key": 5, "value": "x"}),
            ErrorCode::TypeConstraintViolation,
        ),
        (
            "GetConfiguration",
            json!({"surprise": true}),
            ErrorCode::PropertyConstraintViolation,
        ),
        (
            "ChangeAvailability",
            json!({"connectorId": 1, "type": "Maybe"}),
            ErrorCode::PropertyConstraintViolation,
        ),
    ];
    for (action, request, code) in cases {
        rig.csms
            .expect_call_error(action, request.clone(), &code)
            .await
            .unwrap_or_else(|e| panic!("{action} {request}: {e}"));
    }
    // The station is unharmed.
    rig.csms
        .expect_status("ClearCache", json!({}), "Accepted")
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn unknown_actions_are_not_implemented() {
    let rig = booted().await;
    rig.csms
        .expect_call_error("SelfDestruct", json!({}), &ErrorCode::NotImplemented)
        .await
        .unwrap();
    rig.shutdown().await;
}

/// Security Whitepaper actions pass schema validation but have no behaviour yet (S2): each
/// gets exactly one CALLERROR `NotSupported`, and the station stays usable afterwards.
#[tokio::test]
async fn security_whitepaper_actions_get_one_not_supported_reply() {
    let rig = booted().await;
    let hash = json!({"hashAlgorithm": "SHA256", "issuerNameHash": "ab", "issuerKeyHash": "cd", "serialNumber": "01"});
    let calls = [
        ("CertificateSigned", json!({"certificateChain": "PEM"})),
        ("DeleteCertificate", json!({"certificateHashData": hash})),
        (
            "ExtendedTriggerMessage",
            json!({"requestedMessage": "SignChargePointCertificate"}),
        ),
        (
            "GetInstalledCertificateIds",
            json!({"certificateType": "CentralSystemRootCertificate"}),
        ),
        (
            "GetLog",
            json!({"logType": "SecurityLog", "requestId": 1, "log": {"remoteLocation": "https://example.com/logs"}}),
        ),
        (
            "InstallCertificate",
            json!({"certificateType": "ManufacturerRootCertificate", "certificate": "PEM"}),
        ),
        (
            "SignedUpdateFirmware",
            json!({"requestId": 1, "firmware": {"location": "https://example.com/fw", "retrieveDateTime": "2031-01-01T00:00:00Z", "signingCertificate": "PEM", "signature": "c2ln"}}),
        ),
    ];
    for (action, request) in calls {
        rig.csms
            .expect_call_error(action, request, &ErrorCode::NotSupported)
            .await
            .unwrap();
    }
    // A schema violation is still answered by the validator, not by the handler.
    rig.csms
        .expect_call_error(
            "GetLog",
            json!({}),
            &ErrorCode::OccurenceConstraintViolation,
        )
        .await
        .unwrap();
    rig.csms
        .expect_status("ClearCache", json!({}), "Accepted")
        .await
        .unwrap();
    rig.shutdown().await;
}

/// One valid request of every action that earlier stages did not support.
fn stage_2b_requests() -> Vec<(&'static str, Value)> {
    vec![
        ("CancelReservation", json!({"reservationId": 1})),
        ("ClearChargingProfile", json!({})),
        (
            "GetCompositeSchedule",
            json!({"connectorId": 1, "duration": 60}),
        ),
        (
            "GetDiagnostics",
            json!({"location": "ftp://example.com/logs"}),
        ),
        ("RemoteStartTransaction", json!({"idTag": "TAG"})),
        ("RemoteStopTransaction", json!({"transactionId": 1})),
        (
            "ReserveNow",
            json!({"connectorId": 1, "expiryDate": "2031-01-01T00:00:00Z", "idTag": "TAG", "reservationId": 1}),
        ),
        ("Reset", json!({"type": "Soft"})),
        (
            "SetChargingProfile",
            json!({"connectorId": 1, "csChargingProfiles": {
                "chargingProfileId": 1, "stackLevel": 0,
                "chargingProfilePurpose": "TxDefaultProfile", "chargingProfileKind": "Absolute",
                "chargingSchedule": {"chargingRateUnit": "A", "chargingSchedulePeriod": [{"startPeriod": 0, "limit": 16}]}}}),
        ),
        (
            "UpdateFirmware",
            json!({"location": "ftp://example.com/fw.bin", "retrieveDate": "2031-01-01T00:00:00Z"}),
        ),
    ]
}

#[tokio::test]
async fn every_csms_call_gets_exactly_one_reply() {
    let mut rig = booted().await;
    rig.observed.drain();
    rig.observed.frames.clear();

    let mut sent = 0_usize;
    let mut calls: Vec<(&str, Value)> = stage_2b_requests();
    calls.extend([
        (
            "ChangeAvailability",
            json!({"connectorId": 1, "type": "Operative"}),
        ),
        (
            "ChangeConfiguration",
            json!({"key": "HeartbeatInterval", "value": "9"}),
        ),
        ("ChangeConfiguration", json!({"key": "Nope", "value": "9"})),
        ("ClearCache", json!({})),
        ("DataTransfer", json!({"vendorId": "archhaze24"})),
        ("GetConfiguration", json!({})),
        ("GetLocalListVersion", json!({})),
        (
            "SendLocalList",
            json!({"listVersion": 1, "updateType": "Full", "localAuthorizationList": []}),
        ),
        ("TriggerMessage", json!({"requestedMessage": "Heartbeat"})),
        ("TriggerMessage", json!({"requestedMessage": "MeterValues"})),
        ("UnlockConnector", json!({"connectorId": 1})),
        // Invalid and unknown ones are answered by the connection layer.
        ("ChangeConfiguration", json!({})),
        ("Bogus", json!({})),
    ]);
    for (action, request) in calls {
        rig.csms.send_call(action, request).await.unwrap();
        sent += 1;
    }

    let reply_frames = |o: &station_support::Observed| {
        o.frames
            .iter()
            .filter(|r| r.direction == Direction::Out && matches!(r.frame[0].as_u64(), Some(3 | 4)))
            .count()
    };
    rig.observed
        .wait_until("all reply frames", |o| reply_frames(o) >= sent)
        .await;
    let mut replies: BTreeMap<String, usize> = BTreeMap::new();
    for record in &rig.observed.frames {
        let kind = record.frame[0].as_u64();
        if record.direction == Direction::Out && matches!(kind, Some(3 | 4)) {
            *replies.entry(record.msg_id.clone().unwrap()).or_default() += 1;
        }
    }
    assert_eq!(replies.len(), sent, "one reply frame per CALL: {replies:?}");
    assert!(replies.values().all(|count| *count == 1), "{replies:?}");
    rig.shutdown().await;
}
