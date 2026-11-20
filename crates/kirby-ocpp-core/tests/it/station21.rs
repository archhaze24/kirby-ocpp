//! The OCPP 2.1 station: the 2.x actor on the `ocpp2.1` subprotocol with the 2.1 schemas.
//!
//! Each test runs against a mock CSMS that selects `ocpp2.1` and answers with 2.1-valid
//! payloads. Where a value exists only in 2.1, the same request goes to a 2.0.1 station too, to
//! show that its schemas still refuse it. Waits are on recorded CALLs and snapshots with
//! deadlines.

use std::sync::Arc;

use chrono::Utc;
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::frame::Frame;
use kirby_ocpp_core::ocpp::v2::types::{ChargingState, IdTokenType, StopReason};
use kirby_ocpp_core::ocpp::{Protocol, SchemaRegistry};
use kirby_ocpp_core::{Command2, ManualClock, Station2, Station2Handle, StationConfig};
use kirby_ocpp_testkit::{CsmsOptions, ErrorCode, MockCsms, Reply, SubprotocolPolicy};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::station2_support::{STEP, v201_csms, wait_booted, wait_snapshot};

/// A mock CSMS that accepts only `ocpp2.1`. Its answers do not depend on the mock's own
/// 2.1 defaults: every action the station sends gets a 2.1-valid answer from here.
async fn v21_csms() -> MockCsms {
    let csms = MockCsms::start(CsmsOptions {
        subprotocol: SubprotocolPolicy::Require("ocpp2.1".to_owned()),
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
    csms.respond("Heartbeat", |_| {
        Reply::Result(json!({"currentTime": Utc::now().to_rfc3339()}))
    });
    csms.respond("Authorize", |_| {
        Reply::Result(json!({"idTokenInfo": {"status": "Accepted"}}))
    });
    for action in [
        "TransactionEvent",
        "StatusNotification",
        "MeterValues",
        "NotifyEvent",
        "SecurityEventNotification",
    ] {
        csms.respond(action, |_| Reply::Result(json!({})));
    }
    csms
}

/// A station of `protocol` against `csms`: two EVSEs, no persistence, a long heartbeat. It does
/// not connect by itself.
fn spawn(csms: &MockCsms, protocol: Protocol) -> (Station2Handle, JoinHandle<()>) {
    let mut config = StationConfig::new(csms.url())
        .with_connectors(2)
        .with_call_timeout_ms(2_000)
        .with_heartbeat_interval_secs(3600)
        .with_reconnect(ReconnectConfig {
            enabled: true,
            initial_delay_ms: 30,
            max_delay_ms: 100,
            max_attempts: 0,
        })
        .without_persistence();
    config.ws_ping_interval_secs = 0;
    protocol.token().clone_into(&mut config.ws_subprotocol);
    config.protocol = protocol;
    Station2::spawn(config, Arc::new(ManualClock::new(Utc::now())))
}

/// A connected, booted station of `protocol`.
async fn booted(csms: &MockCsms, protocol: Protocol) -> (Station2Handle, JoinHandle<()>) {
    let (station, task) = spawn(csms, protocol);
    station.connect().await.expect("connect");
    wait_booted(&station).await;
    (station, task)
}

async fn stop(station: &Station2Handle, task: JoinHandle<()>) {
    station.shutdown().await.expect("the station stops");
    timeout(STEP, task)
        .await
        .expect("the station task ends in time")
        .expect("the station task does not panic");
}

/// Sends a CSMS CALL and returns the answer frame.
async fn ask(csms: &MockCsms, action: &str, payload: Value) -> Frame {
    csms.send_call(action, payload)
        .await
        .expect("the station answers")
}

/// The payload of a CALLRESULT.
fn result_of(frame: Frame) -> Value {
    match frame {
        Frame::CallResult { payload, .. } => payload,
        other => panic!("expected a CALLRESULT, got {other:?}"),
    }
}

/// The code of a CALLERROR.
fn error_of(frame: &Frame) -> &ErrorCode {
    match frame {
        Frame::CallError { code, .. } => code,
        other => panic!("expected a CALLERROR, got {other:?}"),
    }
}

/// Checks every CALL the CSMS received against the request schemas of `protocol`.
fn assert_calls_valid(csms: &MockCsms, protocol: Protocol) {
    let registry = SchemaRegistry::for_protocol(protocol).expect("schemas");
    let calls = csms.calls_since(0);
    assert!(!calls.is_empty(), "the station sent nothing");
    for call in calls {
        if let Err(violation) = registry.validate_request(&call.action, &call.payload) {
            panic!(
                "{} is not valid for OCPP {}: {violation:?}",
                call.action,
                protocol.ocpp_version()
            );
        }
    }
}

#[tokio::test]
async fn a_2_1_station_negotiates_ocpp2_1_and_runs_a_session_with_2_1_valid_frames() {
    let csms = v21_csms().await;
    let (station, task) = booted(&csms, Protocol::V21).await;
    let connection = csms
        .connection_log()
        .into_iter()
        .next()
        .expect("one connection");
    assert_eq!(connection.subprotocol.as_deref(), Some("ocpp2.1"));
    assert_eq!(station.snapshot().protocol, "ocpp2.1");

    let started = {
        // Start at the cable: no token is needed.
        let set = result_of(
            ask(
                &csms,
                "SetVariables",
                json!({"setVariableData": [{
                    "attributeValue": "EVConnected",
                    "component": {"name": "TxCtrlr"},
                    "variable": {"name": "TxStartPoint"},
                }]}),
            )
            .await,
        );
        assert_eq!(set["setVariableResult"][0]["attributeStatus"], "Accepted");
        station
            .send(Command2::PlugIn { evse: 1 })
            .await
            .expect("plug in");
        csms.wait_for_call_after(0, "TransactionEvent", |p| p["eventType"] == "Started", STEP)
            .await
            .expect("the Started event")
    };
    assert_eq!(started["triggerReason"], "CablePluggedIn");

    let authorized = station
        .send(Command2::Authorize {
            id_token: "CARD-1".to_owned(),
            token_type: IdTokenType::Iso14443,
        })
        .await
        .expect("authorize");
    assert!(
        format!("{authorized:?}").contains("Accepted"),
        "{authorized:?}"
    );

    station
        .send(Command2::SetChargingState {
            evse: 1,
            state: ChargingState::Charging,
        })
        .await
        .expect("charging");
    station
        .send(Command2::AdvanceMeter {
            evse: 1,
            wh_delta: 500,
        })
        .await
        .expect("meter");
    station
        .send(Command2::StopTransaction {
            evse: 1,
            reason: StopReason::Local,
        })
        .await
        .expect("stop");
    csms.wait_for_call_after(0, "TransactionEvent", |p| p["eventType"] == "Ended", STEP)
        .await
        .expect("the Ended event");
    wait_snapshot(&station, "the transaction gone", |s| {
        s.evse(1).is_some_and(|e| e.transaction.is_none())
    })
    .await;

    assert_calls_valid(&csms, Protocol::V21);
    stop(&station, task).await;
}

#[tokio::test]
async fn an_action_only_2_1_defines_gets_not_supported() {
    let csms = v21_csms().await;
    let (station, task) = booted(&csms, Protocol::V21).await;
    let request = json!({"timestamp": "2026-03-01T12:00:00Z", "signal": 1});
    let frame = ask(&csms, "AFRRSignal", request.clone()).await;
    assert_eq!(error_of(&frame), &ErrorCode::NotSupported, "{frame:?}");
    // A name no version defines stays NotImplemented.
    let frame = ask(&csms, "Frobnicate", json!({})).await;
    assert_eq!(error_of(&frame), &ErrorCode::NotImplemented, "{frame:?}");
    stop(&station, task).await;

    // 2.0.1 does not define the action at all.
    let csms = v201_csms().await;
    let (station, task) = booted(&csms, Protocol::V201).await;
    let frame = ask(&csms, "AFRRSignal", request).await;
    assert_eq!(error_of(&frame), &ErrorCode::NotImplemented, "{frame:?}");
    stop(&station, task).await;
}

#[tokio::test]
async fn reset_immediate_and_resume_is_rejected_on_2_1_and_refused_by_schema_on_2_0_1() {
    let csms = v21_csms().await;
    let (station, task) = booted(&csms, Protocol::V21).await;
    let reply = result_of(ask(&csms, "Reset", json!({"type": "ImmediateAndResume"})).await);
    assert_eq!(reply["status"], "Rejected");
    assert_eq!(reply["statusInfo"]["reasonCode"], "UnsupportedParam");
    // The station neither rebooted nor closed the link.
    assert_eq!(csms.connections(), 1);
    assert_eq!(
        result_of(ask(&csms, "Reset", json!({"type": "OnIdle"})).await)["status"],
        "Accepted"
    );
    stop(&station, task).await;

    let csms = v201_csms().await;
    let (station, task) = booted(&csms, Protocol::V201).await;
    let refused = ask(&csms, "Reset", json!({"type": "ImmediateAndResume"})).await;
    let bogus = ask(&csms, "Reset", json!({"type": "NoSuchKind"})).await;
    assert_eq!(error_of(&refused), error_of(&bogus), "{refused:?}");
    stop(&station, task).await;
}

#[tokio::test]
async fn trigger_values_new_in_2_1_are_not_implemented_and_2_0_1_refuses_them() {
    let new_values = ["CustomTrigger", "SignV2G20Certificate"];
    let csms = v21_csms().await;
    let (station, task) = booted(&csms, Protocol::V21).await;
    for value in new_values {
        let reply =
            result_of(ask(&csms, "TriggerMessage", json!({"requestedMessage": value})).await);
        assert_eq!(reply["status"], "NotImplemented", "{value}");
    }
    stop(&station, task).await;

    let csms = v201_csms().await;
    let (station, task) = booted(&csms, Protocol::V201).await;
    let bogus = ask(
        &csms,
        "TriggerMessage",
        json!({"requestedMessage": "NoSuchMessage"}),
    )
    .await;
    for value in new_values {
        let refused = ask(&csms, "TriggerMessage", json!({"requestedMessage": value})).await;
        assert_eq!(error_of(&refused), error_of(&bogus), "{value}: {refused:?}");
    }
    stop(&station, task).await;
}

#[tokio::test]
async fn a_2_1_id_token_kind_and_length_reach_the_wire_and_2_0_1_refuses_them() {
    let long = "T".repeat(100);
    let csms = v21_csms().await;
    let (station, task) = booted(&csms, Protocol::V21).await;
    station
        .send(Command2::Authorize {
            id_token: long.clone(),
            token_type: IdTokenType::Other("Plate".to_owned()),
        })
        .await
        .expect("a 2.1 station sends the token");
    let sent = csms
        .wait_for_call_after(0, "Authorize", |_| true, STEP)
        .await
        .expect("the Authorize CALL");
    assert_eq!(sent["idToken"], json!({"idToken": long, "type": "Plate"}));
    stop(&station, task).await;

    let csms = v201_csms().await;
    let (station, task) = booted(&csms, Protocol::V201).await;
    for (id_token, token_type) in [
        (long, IdTokenType::Iso14443),
        ("T".to_owned(), IdTokenType::Other("Plate".to_owned())),
    ] {
        let outcome = station
            .send(Command2::Authorize {
                id_token,
                token_type,
            })
            .await;
        assert!(outcome.is_err(), "2.0.1 must refuse it: {outcome:?}");
    }
    assert!(
        csms.calls_since(0).iter().all(|c| c.action != "Authorize"),
        "nothing reached the wire"
    );
    stop(&station, task).await;
}

#[tokio::test]
async fn a_remote_start_with_a_2_1_token_carries_it_into_the_transaction() {
    let long = "R".repeat(100);
    let csms = v21_csms().await;
    let (station, task) = booted(&csms, Protocol::V21).await;
    station
        .send(Command2::PlugIn { evse: 1 })
        .await
        .expect("plug in");
    let reply = result_of(
        ask(
            &csms,
            "RequestStartTransaction",
            json!({
                "evseId": 1,
                "remoteStartId": 7,
                "idToken": {"idToken": long, "type": "Plate"},
            }),
        )
        .await,
    );
    assert_eq!(reply["status"], "Accepted");
    let started = csms
        .wait_for_call_after(0, "TransactionEvent", |p| p["eventType"] == "Started", STEP)
        .await
        .expect("the Started event");
    assert_eq!(
        started["idToken"],
        json!({"idToken": long, "type": "Plate"})
    );
    assert_calls_valid(&csms, Protocol::V21);
    stop(&station, task).await;

    let csms = v201_csms().await;
    let (station, task) = booted(&csms, Protocol::V201).await;
    let frame = ask(
        &csms,
        "RequestStartTransaction",
        json!({
            "evseId": 1,
            "remoteStartId": 7,
            "idToken": {"idToken": "R", "type": "Plate"},
        }),
    )
    .await;
    assert!(matches!(frame, Frame::CallError { .. }), "{frame:?}");
    stop(&station, task).await;
}
