//! Security Whitepaper, maintenance half: `GetLog` + `LogStatusNotification` and
//! `ExtendedTriggerMessage`.

mod station_support;

use kirby_ocpp_core::ocpp::messages::UploadLogStatus;
use kirby_ocpp_core::ocpp::{ErrorCode, Frame};
use kirby_ocpp_core::station::{Command, DiagnosticsOutcome, ResponseRule};
use kirby_ocpp_testkit::Reply;
use serde_json::{Value, json};
use station_support::{Rig, STEP};

async fn booted() -> Rig {
    let mut rig = Rig::start().await;
    for action in ["LogStatusNotification", "SecurityEventNotification"] {
        rig.csms.respond(action, |_| Reply::Result(json!({})));
    }
    rig.csms.respond("SignCertificate", |_| {
        Reply::Result(json!({"status": "Rejected"}))
    });
    rig.connect_booted().await;
    rig
}

fn get_log(id: i32, location: &str) -> Value {
    json!({"logType": "DiagnosticsLog", "requestId": id, "log": {"remoteLocation": location}})
}

fn log_calls(rig: &Rig, mark: usize) -> Vec<(String, Option<i64>)> {
    rig.calls("LogStatusNotification", mark)
        .iter()
        .map(|c| {
            (
                c["status"].as_str().unwrap_or_default().to_owned(),
                c["requestId"].as_i64(),
            )
        })
        .collect()
}

async fn wait_log(rig: &Rig, mark: usize, status: &str, id: i64) {
    rig.csms
        .wait_for_call_after(
            mark,
            "LogStatusNotification",
            |p| p["status"] == status && p["requestId"] == id,
            STEP,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn get_log_reports_uploading_then_uploaded() {
    let rig = booted().await;
    let mark = rig.csms.mark();
    for (kind, id) in [("SecurityLog", 7), ("DiagnosticsLog", 8)] {
        let reply = rig
            .csms
            .expect_status(
                "GetLog",
                json!({"logType": kind, "requestId": id, "log": {"remoteLocation": "sftp://u@host/logs"}}),
                "Accepted",
            )
            .await
            .unwrap();
        assert!(reply["filename"].as_str().is_some_and(|f| !f.is_empty()));
        wait_log(&rig, mark, "Uploaded", i64::from(id)).await;
    }
    let seen = log_calls(&rig, mark);
    assert_eq!(
        seen,
        [
            ("Uploading".into(), Some(7)),
            ("Uploaded".into(), Some(7)),
            ("Uploading".into(), Some(8)),
            ("Uploaded".into(), Some(8)),
        ]
    );
    let snap = rig.station.snapshot();
    assert_eq!(snap.log_status, UploadLogStatus::Uploaded);
    assert_eq!(snap.log_request_id, Some(8));
    rig.shutdown().await;
}

#[tokio::test]
async fn get_log_honours_the_upload_failure_outcome() {
    let rig = booted().await;
    rig.send(Command::SetDiagnosticsOutcome {
        outcome: DiagnosticsOutcome::UploadFailure,
    })
    .await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status("GetLog", get_log(3, "ftp://host/x"), "Accepted")
        .await
        .unwrap();
    wait_log(&rig, mark, "UploadFailure", 3).await;
    rig.shutdown().await;
}

#[tokio::test]
async fn a_new_get_log_cancels_the_running_upload() {
    let rig = booted().await;
    rig.send(Command::SetDiagnosticsOutcome {
        outcome: DiagnosticsOutcome::UploadFailure,
    })
    .await;
    let mark = rig.csms.mark();
    let mut request = get_log(1, "https://host/up");
    request["retries"] = json!(5);
    request["retryInterval"] = json!(1);
    rig.csms
        .expect_status("GetLog", request, "Accepted")
        .await
        .unwrap();
    wait_log(&rig, mark, "Uploading", 1).await;
    rig.send(Command::SetDiagnosticsOutcome {
        outcome: DiagnosticsOutcome::Success,
    })
    .await;
    let second = rig
        .csms
        .expect_status("GetLog", get_log(2, "https://host/up"), "AcceptedCanceled")
        .await
        .unwrap();
    assert!(second["filename"].is_string());
    wait_log(&rig, mark, "Uploaded", 2).await;
    let seen = log_calls(&rig, mark);
    let first_new = seen.iter().position(|(_, id)| *id == Some(2)).unwrap();
    assert!(
        seen[first_new..].iter().all(|(_, id)| *id == Some(2)),
        "{seen:?}"
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn get_log_rejects_unsupported_locations() {
    let rig = booted().await;
    let mark = rig.csms.mark();
    for location in ["file:///etc/passwd", "not a url", "ws://host/x", "https://"] {
        let reply = rig
            .csms
            .expect_status("GetLog", get_log(1, location), "Rejected")
            .await
            .unwrap();
        assert!(reply.get("filename").is_none(), "{location}");
    }
    rig.assert_no_calls("LogStatusNotification", mark);
    rig.shutdown().await;
}

#[tokio::test]
async fn get_log_schema_violations_are_call_errors() {
    let rig = booted().await;
    rig.csms
        .expect_call_error(
            "GetLog",
            json!({"logType": "Nope", "requestId": 1, "log": {"remoteLocation": "ftp://h/x"}}),
            &ErrorCode::PropertyConstraintViolation,
        )
        .await
        .unwrap();
    rig.csms
        .expect_call_error(
            "GetLog",
            json!({"logType": "SecurityLog", "log": {"remoteLocation": "ftp://h/x"}}),
            &ErrorCode::OccurenceConstraintViolation,
        )
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn extended_trigger_sends_each_requested_message() {
    let rig = booted().await;
    let cases = [
        ("BootNotification", "BootNotification"),
        ("Heartbeat", "Heartbeat"),
        ("StatusNotification", "StatusNotification"),
        ("MeterValues", "MeterValues"),
        ("FirmwareStatusNotification", "FirmwareStatusNotification"),
        ("LogStatusNotification", "LogStatusNotification"),
        ("SignChargePointCertificate", "SignCertificate"),
    ];
    for (requested, sent) in cases {
        let mark = rig.csms.mark();
        rig.csms
            .expect_status(
                "ExtendedTriggerMessage",
                json!({"requestedMessage": requested}),
                "Accepted",
            )
            .await
            .unwrap();
        rig.csms
            .wait_for_call_after(mark, sent, |_| true, STEP)
            .await
            .unwrap_or_else(|e| panic!("{requested}: {e}"));
    }
    rig.shutdown().await;
}

#[tokio::test]
async fn extended_trigger_log_status_is_idle_without_an_upload() {
    let rig = booted().await;
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "ExtendedTriggerMessage",
            json!({"requestedMessage": "LogStatusNotification"}),
            "Accepted",
        )
        .await
        .unwrap();
    let call = rig
        .csms
        .wait_for_call_after(mark, "LogStatusNotification", |_| true, STEP)
        .await
        .unwrap();
    assert_eq!(call["status"], "Idle");
    assert!(call.get("requestId").is_none());
    rig.shutdown().await;
}

#[tokio::test]
async fn extended_trigger_log_status_reports_a_running_upload() {
    let rig = booted().await;
    rig.send(Command::SetDiagnosticsOutcome {
        outcome: DiagnosticsOutcome::UploadFailure,
    })
    .await;
    let mut request = get_log(9, "ftp://h/x");
    request["retries"] = json!(5);
    request["retryInterval"] = json!(1);
    rig.csms
        .expect_status("GetLog", request, "Accepted")
        .await
        .unwrap();
    let mark = rig.csms.mark();
    rig.csms
        .expect_status(
            "ExtendedTriggerMessage",
            json!({"requestedMessage": "LogStatusNotification"}),
            "Accepted",
        )
        .await
        .unwrap();
    // The triggered report is one of the (possibly several) `Uploading` reports of request 9.
    wait_log(&rig, mark, "Uploading", 9).await;
    rig.shutdown().await;
}

#[tokio::test]
async fn extended_trigger_validates_the_connector() {
    let rig = booted().await;
    for (requested, connector, status) in [
        ("StatusNotification", 99, "Rejected"),
        ("MeterValues", 99, "Rejected"),
        ("StatusNotification", 1, "Accepted"),
        ("MeterValues", 1, "Accepted"),
        ("Heartbeat", 99, "Accepted"),
    ] {
        rig.csms
            .expect_status(
                "ExtendedTriggerMessage",
                json!({"requestedMessage": requested, "connectorId": connector}),
                status,
            )
            .await
            .unwrap_or_else(|e| panic!("{requested} {connector}: {e}"));
    }
    rig.shutdown().await;
}

#[tokio::test]
async fn extended_trigger_schema_violations_are_call_errors() {
    let rig = booted().await;
    rig.csms
        .expect_call_error(
            "ExtendedTriggerMessage",
            json!({"requestedMessage": "Nope"}),
            &ErrorCode::PropertyConstraintViolation,
        )
        .await
        .unwrap();
    rig.shutdown().await;
}

#[tokio::test]
async fn response_rules_reject_get_log_and_extended_trigger() {
    let rig = booted().await;
    for text in ["GetLog=reject*1", "ExtendedTriggerMessage=reject*1"] {
        let rule: ResponseRule = text.parse().unwrap();
        rig.send(Command::SetResponseRule { rule }).await;
    }
    let mark = rig.csms.mark();
    rig.csms
        .expect_status("GetLog", get_log(1, "ftp://h/x"), "Rejected")
        .await
        .unwrap();
    rig.csms
        .expect_status(
            "ExtendedTriggerMessage",
            json!({"requestedMessage": "Heartbeat"}),
            "Rejected",
        )
        .await
        .unwrap();
    rig.assert_no_calls("LogStatusNotification", mark);
    rig.assert_no_calls("Heartbeat", mark);
    // The rules were one-shot: the next requests work.
    rig.csms
        .expect_status("GetLog", get_log(2, "ftp://h/x"), "Accepted")
        .await
        .unwrap();
    let frame: Frame = rig
        .csms
        .send_call(
            "ExtendedTriggerMessage",
            json!({"requestedMessage": "Heartbeat"}),
        )
        .await
        .unwrap();
    assert!(matches!(frame, Frame::CallResult { .. }));
    rig.shutdown().await;
}
