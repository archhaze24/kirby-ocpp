//! Remote control of the OCPP 2.0.1 station: `RequestStartTransaction`, `RequestStopTransaction`,
//! `TriggerMessage`, `Reset`, `UnlockConnector`, `ClearCache`, `DataTransfer` and the default
//! for actions the station does not handle.
//!
//! Tests that need a transaction wait for the transaction engine through snapshots and CALLs;
//! the ones that do not (rejections, trigger answers, the reboot of an idle station) stand on
//! their own. Waits are bounded by `STEP`; nothing sleeps for a fixed time.

use std::sync::Arc;

use chrono::Utc;
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::ocpp::frame::Frame;
use kirby_ocpp_core::ocpp::v2::types::{Component, IdTokenType, StopReason, Variable};
use kirby_ocpp_core::station::{Direction, Event, FrameRecord};
use kirby_ocpp_core::{
    Command2, ManualClock, Outcome2, Snapshot2, Station2, Station2Handle, StationConfig,
    StationError,
};
use kirby_ocpp_testkit::{ErrorCode, MockCsms, Reply};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::station2_support::{STEP, v201_csms, wait_booted, wait_snapshot};

/// A boot answer; the long interval keeps periodic heartbeats out of the way.
pub(super) fn boot_reply(status: &str) -> Reply {
    Reply::Result(json!({
        "status": status,
        "currentTime": "2026-03-01T12:00:00Z",
        "interval": 3600,
    }))
}

/// A mock CSMS, a station connected to it and a clock the test controls.
pub(super) struct Rig {
    pub csms: MockCsms,
    pub station: Station2Handle,
    pub clock: ManualClock,
    task: JoinHandle<()>,
}

impl Rig {
    /// A booted station (boot `Accepted`) with its statuses delivered.
    pub async fn new() -> Self {
        Self::start("Accepted", |_| {}).await
    }

    /// A station whose boot is answered with `status`; waits for the boot to be answered.
    pub async fn start(status: &str, tweak: impl FnOnce(&mut StationConfig)) -> Self {
        let csms = v201_csms().await;
        let answer = status.to_owned();
        csms.respond("BootNotification", move |_| boot_reply(&answer));
        let clock = ManualClock::new(Utc::now());
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
        Protocol::V201
            .token()
            .clone_into(&mut config.ws_subprotocol);
        config.protocol = Protocol::V201;
        tweak(&mut config);
        let (station, task) = Station2::spawn(config, Arc::new(clock.clone()));
        station.connect().await.expect("connect");
        wait_snapshot(&station, "a boot answer", |s| s.registration.is_some()).await;
        let rig = Self {
            csms,
            station,
            clock,
            task,
        };
        if status == "Accepted" {
            wait_booted(&rig.station).await;
            rig.wait_calls("StatusNotification", 2).await;
        }
        rig
    }

    pub async fn stop(self) {
        let _ = self.station.shutdown().await;
        let _ = self.task.await;
        self.csms.shutdown().await;
    }

    pub async fn send(&self, command: Command2) -> Outcome2 {
        self.station.send(command).await.expect("command accepted")
    }

    pub async fn send_result(&self, command: Command2) -> Result<Outcome2, StationError> {
        self.station.send(command).await
    }

    /// Changes a device-model variable of the station.
    pub async fn set_variable(&self, component: &str, variable: &str, value: &str) {
        self.send(Command2::SetVariable {
            component: Component {
                evse: None,
                name: component.to_owned(),
                instance: None,
                custom_data: None,
            },
            variable: Variable {
                name: variable.to_owned(),
                instance: None,
                custom_data: None,
            },
            value: value.to_owned(),
        })
        .await;
    }

    /// A CSMS CALL; the CALLRESULT payload.
    pub async fn call(&self, action: &str, payload: Value) -> Value {
        match self
            .csms
            .send_call(action, payload)
            .await
            .expect("call answered")
        {
            Frame::CallResult { payload, .. } => payload,
            other => panic!("{action} was not answered with a CALLRESULT: {other:?}"),
        }
    }

    /// The status of the answer to a CSMS CALL.
    pub async fn status_of(&self, action: &str, payload: Value) -> String {
        self.call(action, payload).await["status"]
            .as_str()
            .expect("a status")
            .to_owned()
    }

    /// The payloads of the CALLs of `action` received so far.
    pub fn calls(&self, action: &str) -> Vec<Value> {
        self.csms
            .received()
            .into_iter()
            .filter(|call| call.action == action)
            .map(|call| call.payload)
            .collect()
    }

    /// Waits until at least `count` CALLs of `action` arrived; returns all of them.
    pub async fn wait_calls(&self, action: &str, count: usize) -> Vec<Value> {
        self.csms
            .wait_for_call_count_after(0, action, count, |_| true, STEP)
            .await
            .expect("the calls arrive");
        self.calls(action)
    }

    /// Waits for a security event of `kind` and returns its payload. Events of other types
    /// are skipped: a reset right after the boot can make the station send the startup event
    /// again, because the link closed before its answer arrived.
    pub async fn wait_security_event(&self, kind: &str) -> Value {
        self.csms
            .wait_for_call_count_after(
                0,
                "SecurityEventNotification",
                1,
                |payload| payload["type"] == kind,
                STEP,
            )
            .await
            .expect("the security event arrives")
            .remove(0)
    }

    /// The `TransactionEvent` payloads with `eventType`.
    pub fn events_of(&self, kind: &str) -> Vec<Value> {
        self.calls("TransactionEvent")
            .into_iter()
            .filter(|payload| payload["eventType"] == kind)
            .collect()
    }

    pub async fn wait_event(&self, kind: &str) -> Value {
        self.wait_events(kind, 1).await.remove(0)
    }

    /// Waits until `count` events of `kind` arrived; returns the first `count`.
    pub async fn wait_events(&self, kind: &str, count: usize) -> Vec<Value> {
        self.csms
            .wait_for_call_count_after(
                0,
                "TransactionEvent",
                count,
                |payload| payload["eventType"] == kind,
                STEP,
            )
            .await
            .expect("the events arrive")
    }

    /// Starts a transaction on `evse` with a central token and waits for it.
    pub async fn start_transaction(&self, evse: u32) {
        self.send(Command2::PlugIn { evse }).await;
        self.send(Command2::PresentToken {
            evse: Some(evse),
            id_token: format!("TOKEN-{evse}"),
            token_type: IdTokenType::Central,
        })
        .await;
        wait_snapshot(&self.station, "a transaction", |s| {
            s.evse(evse).is_some_and(|e| e.transaction.is_some())
        })
        .await;
    }

    pub fn transaction_id(&self, evse: u32) -> String {
        self.station
            .snapshot()
            .evse(evse)
            .and_then(|e| e.transaction.as_ref())
            .map(|t| t.transaction_id.clone())
            .expect("a transaction runs")
    }
}

/// Frames the station published, for ordering checks.
pub(super) struct Frames(broadcast::Receiver<Event>);

impl Frames {
    pub fn new(station: &Station2Handle) -> Self {
        Self(station.subscribe())
    }

    /// The next frame matching `pred`, with everything before it that did not.
    pub async fn until(
        &mut self,
        what: &str,
        pred: impl Fn(&FrameRecord) -> bool,
    ) -> Vec<FrameRecord> {
        let mut seen = Vec::new();
        let result = timeout(STEP, async {
            loop {
                match self.0.recv().await {
                    Ok(Event::Frame(record)) => {
                        let hit = pred(&record);
                        seen.push(record);
                        if hit {
                            return;
                        }
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => panic!("station stopped"),
                }
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "timed out waiting for {what}; saw {seen:#?}"
        );
        seen
    }

    /// Waits for a log line containing `text`.
    pub async fn log_containing(&mut self, text: &str) -> String {
        let result = timeout(STEP, async {
            loop {
                match self.0.recv().await {
                    Ok(Event::Log(record)) if record.text.contains(text) => return record.text,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => panic!("station stopped"),
                }
            }
        })
        .await;
        result.unwrap_or_else(|_| panic!("timed out waiting for the log line {text:?}"))
    }
}

fn is_call(record: &FrameRecord, direction: Direction, action: &str) -> bool {
    record.direction == direction
        && record.frame[0] == 2
        && record.action.as_deref() == Some(action)
}

fn is_result(record: &FrameRecord, direction: Direction, action: &str) -> bool {
    record.direction == direction
        && record.frame[0] == 3
        && record.action.as_deref() == Some(action)
}

/// A `RequestStartTransaction` body.
fn start_body(evse: Option<u32>, token: &str, kind: &str, remote_start_id: i32) -> Value {
    let mut body = json!({
        "idToken": {"idToken": token, "type": kind},
        "remoteStartId": remote_start_id,
    });
    if let Some(evse) = evse {
        body["evseId"] = json!(evse);
    }
    body
}

// ---- answers that do not need a transaction --------------------------------------------

#[tokio::test]
async fn clear_cache_is_accepted_and_rejected_when_the_cache_is_off() {
    let rig = Rig::new().await;
    assert_eq!(rig.status_of("ClearCache", json!({})).await, "Accepted");
    rig.set_variable("AuthCacheCtrlr", "Enabled", "false").await;
    assert_eq!(rig.status_of("ClearCache", json!({})).await, "Rejected");
    rig.stop().await;
}

#[tokio::test]
async fn clear_cache_forgets_tokens() {
    let rig = Rig::new().await;
    let authorize = || Command2::Authorize {
        id_token: "CARD".to_owned(),
        token_type: IdTokenType::Iso14443,
    };
    rig.send(authorize()).await;
    // A cached token is not sent to the CSMS before a transaction.
    let present = |evse| Command2::PresentToken {
        evse: Some(evse),
        id_token: "CARD".to_owned(),
        token_type: IdTokenType::Iso14443,
    };
    let _ = rig.send_result(present(1)).await;
    assert_eq!(rig.calls("Authorize").len(), 1);
    assert_eq!(rig.status_of("ClearCache", json!({})).await, "Accepted");
    // Another EVSE, so the token does not just end the authorization of the first.
    let _ = rig.send_result(present(2)).await;
    assert_eq!(rig.calls("Authorize").len(), 2);
    rig.stop().await;
}

#[tokio::test]
async fn unlock_connector_statuses() {
    let rig = Rig::new().await;
    let unlock = |evse: u32, connector: u32| json!({"evseId": evse, "connectorId": connector});
    assert_eq!(
        rig.status_of("UnlockConnector", unlock(1, 1)).await,
        "Unlocked"
    );
    assert_eq!(
        rig.status_of("UnlockConnector", unlock(9, 1)).await,
        "UnknownConnector"
    );
    assert_eq!(
        rig.status_of("UnlockConnector", unlock(1, 2)).await,
        "UnknownConnector"
    );
    rig.stop().await;
}

#[tokio::test]
async fn unlock_connector_refuses_during_an_authorized_transaction() {
    let rig = Rig::new().await;
    rig.start_transaction(1).await;
    assert_eq!(
        rig.status_of("UnlockConnector", json!({"evseId": 1, "connectorId": 1}))
            .await,
        "OngoingAuthorizedTransaction"
    );
    assert_eq!(
        rig.status_of("UnlockConnector", json!({"evseId": 2, "connectorId": 1}))
            .await,
        "Unlocked"
    );
    rig.stop().await;
}

#[tokio::test]
async fn inbound_data_transfer_follows_the_vendor_id() {
    let rig = Rig::new().await;
    let vendor = rig.station.config().vendor.clone();
    let accepted = rig
        .call(
            "DataTransfer",
            json!({"vendorId": vendor, "messageId": "m", "data": {"a": [1, 2]}}),
        )
        .await;
    assert_eq!(accepted["status"], "Accepted");
    assert_eq!(accepted["data"], json!({"a": [1, 2]}));
    let unknown = rig
        .call("DataTransfer", json!({"vendorId": "someone-else"}))
        .await;
    assert_eq!(unknown["status"], "UnknownVendorId");
    assert!(unknown.get("data").is_none());
    rig.stop().await;
}

#[tokio::test]
async fn outbound_data_transfer_returns_the_answer() {
    let rig = Rig::new().await;
    let outcome = rig
        .send(Command2::DataTransfer {
            vendor_id: "acme".to_owned(),
            message_id: Some("ping".to_owned()),
            data: Some(json!({"n": 1})),
        })
        .await;
    let Outcome2::Response { payload } = outcome else {
        panic!("expected a response, got {outcome:?}");
    };
    assert_eq!(payload["status"], "Accepted");
    let sent = rig.calls("DataTransfer");
    assert_eq!(
        sent,
        [json!({"vendorId": "acme", "messageId": "ping", "data": {"n": 1}})]
    );
    rig.stop().await;
}

#[tokio::test]
async fn an_action_without_a_handler_is_not_supported() {
    let rig = Rig::new().await;
    rig.csms
        .expect_call_error("GetLocalListVersion", json!({}), &ErrorCode::NotSupported)
        .await
        .expect("NotSupported");
    rig.stop().await;
}

// ---- TriggerMessage ---------------------------------------------------------------------

#[tokio::test]
async fn trigger_heartbeat_sends_one_heartbeat() {
    let rig = Rig::new().await;
    assert_eq!(
        rig.status_of("TriggerMessage", json!({"requestedMessage": "Heartbeat"}))
            .await,
        "Accepted"
    );
    rig.wait_calls("Heartbeat", 1).await;
    rig.stop().await;
}

#[tokio::test]
async fn trigger_boot_is_rejected_after_an_accepted_boot() {
    let rig = Rig::new().await;
    assert_eq!(
        rig.status_of(
            "TriggerMessage",
            json!({"requestedMessage": "BootNotification"})
        )
        .await,
        "Rejected"
    );
    assert_eq!(rig.calls("BootNotification").len(), 1);
    rig.stop().await;
}

#[tokio::test]
async fn trigger_boot_is_accepted_while_the_boot_is_pending() {
    let rig = Rig::start("Pending", |_| {}).await;
    assert_eq!(
        rig.status_of(
            "TriggerMessage",
            json!({"requestedMessage": "BootNotification"})
        )
        .await,
        "Accepted"
    );
    let boots = rig.wait_calls("BootNotification", 2).await;
    assert_eq!(boots[1]["reason"], "Triggered");
    rig.stop().await;
}

#[tokio::test]
async fn trigger_status_notification_needs_a_connector() {
    let rig = Rig::new().await;
    let trigger = |evse: Value| json!({"requestedMessage": "StatusNotification", "evse": evse});
    assert_eq!(
        rig.status_of(
            "TriggerMessage",
            json!({"requestedMessage": "StatusNotification"})
        )
        .await,
        "Rejected"
    );
    assert_eq!(
        rig.status_of("TriggerMessage", trigger(json!({"id": 1})))
            .await,
        "Rejected"
    );
    assert_eq!(
        rig.status_of(
            "TriggerMessage",
            trigger(json!({"id": 9, "connectorId": 1}))
        )
        .await,
        "Rejected"
    );
    let before = rig.calls("StatusNotification").len();
    assert_eq!(
        rig.status_of(
            "TriggerMessage",
            trigger(json!({"id": 2, "connectorId": 1}))
        )
        .await,
        "Accepted"
    );
    let notes = rig.wait_calls("StatusNotification", before + 1).await;
    let last = notes.last().expect("a notification");
    assert_eq!(
        (last["evseId"].as_i64(), last["connectorId"].as_i64()),
        (Some(2), Some(1))
    );
    assert_eq!(notes.len(), before + 1, "only the triggered one was sent");
    rig.stop().await;
}

#[tokio::test]
async fn trigger_values_the_station_does_not_send_are_not_implemented() {
    let rig = Rig::new().await;
    for message in [
        "SignChargingStationCertificate",
        "SignV2GCertificate",
        "SignCombinedCertificate",
        "PublishFirmwareStatusNotification",
    ] {
        assert_eq!(
            rig.status_of("TriggerMessage", json!({"requestedMessage": message}))
                .await,
            "NotImplemented",
            "{message}"
        );
    }
    rig.stop().await;
}

#[tokio::test]
async fn trigger_log_and_firmware_status_report_idle() {
    let rig = Rig::new().await;
    for (message, action) in [
        ("LogStatusNotification", "LogStatusNotification"),
        ("FirmwareStatusNotification", "FirmwareStatusNotification"),
    ] {
        assert_eq!(
            rig.status_of("TriggerMessage", json!({"requestedMessage": message}))
                .await,
            "Accepted"
        );
        let calls = rig.wait_calls(action, 1).await;
        assert_eq!(calls[0], json!({"status": "Idle"}));
    }
    rig.stop().await;
}

#[tokio::test]
async fn trigger_meter_values_and_transaction_event_need_known_targets() {
    let rig = Rig::new().await;
    assert_eq!(
        rig.status_of(
            "TriggerMessage",
            json!({"requestedMessage": "MeterValues", "evse": {"id": 9}})
        )
        .await,
        "Rejected"
    );
    assert_eq!(
        rig.status_of(
            "TriggerMessage",
            json!({"requestedMessage": "TransactionEvent"})
        )
        .await,
        "Rejected"
    );
    assert_eq!(
        rig.status_of(
            "TriggerMessage",
            json!({"requestedMessage": "TransactionEvent", "evse": {"id": 1}})
        )
        .await,
        "Rejected"
    );
    rig.stop().await;
}

#[tokio::test]
async fn trigger_response_goes_out_before_the_triggered_call() {
    let rig = Rig::new().await;
    let mut frames = Frames::new(&rig.station);
    let status = rig
        .status_of(
            "TriggerMessage",
            json!({
                "requestedMessage": "StatusNotification",
                "evse": {"id": 1, "connectorId": 1}
            }),
        )
        .await;
    assert_eq!(status, "Accepted");
    let seen = frames
        .until("the triggered StatusNotification", |r| {
            is_call(r, Direction::Out, "StatusNotification")
        })
        .await;
    let result = seen
        .iter()
        .position(|r| is_result(r, Direction::Out, "TriggerMessage"))
        .expect("the response was sent");
    // The answer to the security event of the startup may still arrive in between.
    assert!(
        result < seen.len() - 1,
        "the CALL follows the response: {seen:#?}"
    );
    rig.stop().await;
}

#[tokio::test]
async fn trigger_transaction_event_sends_an_updated_event() {
    let rig = Rig::new().await;
    rig.start_transaction(1).await;
    let id = rig.transaction_id(1);
    let before = rig.events_of("Updated").len();
    assert_eq!(
        rig.status_of(
            "TriggerMessage",
            json!({"requestedMessage": "TransactionEvent", "evse": {"id": 1}})
        )
        .await,
        "Accepted"
    );
    let event = rig
        .csms
        .wait_for_call_after(
            0,
            "TransactionEvent",
            |p| p["eventType"] == "Updated" && p["triggerReason"] == "Trigger",
            STEP,
        )
        .await
        .expect("the triggered event arrives");
    assert_eq!(event["transactionInfo"]["transactionId"], id);
    assert!(event["transactionInfo"]["chargingState"].is_string());
    assert!(rig.events_of("Updated").len() > before);
    rig.stop().await;
}

// ---- Reset ------------------------------------------------------------------------------

#[tokio::test]
async fn reset_with_an_evse_is_rejected() {
    let rig = Rig::new().await;
    assert_eq!(
        rig.status_of("Reset", json!({"type": "Immediate", "evseId": 1}))
            .await,
        "Rejected"
    );
    assert_eq!(
        rig.status_of("Reset", json!({"type": "OnIdle", "evseId": 2}))
            .await,
        "Rejected"
    );
    assert_eq!(rig.csms.connections(), 1, "no reboot happened");
    rig.stop().await;
}

#[tokio::test]
async fn an_idle_station_reboots_for_both_kinds_of_reset() {
    for kind in ["Immediate", "OnIdle"] {
        let rig = Rig::new().await;
        assert_eq!(
            rig.status_of("Reset", json!({"type": kind})).await,
            "Accepted"
        );
        let boots = rig.wait_calls("BootNotification", 2).await;
        assert_eq!(boots[0]["reason"], "PowerUp");
        assert_eq!(boots[1]["reason"], "RemoteReset", "{kind}");
        assert_eq!(rig.csms.connections(), 2, "{kind}: one new connection");
        // A04.FR.01: the reboot is a critical security event.
        let event = rig.wait_security_event("ResetOrReboot").await;
        assert!(event["timestamp"].is_string(), "{kind}");
        rig.stop().await;
    }
}

#[tokio::test]
async fn a_later_connect_boots_as_power_up_again() {
    let rig = Rig::new().await;
    let mut frames = Frames::new(&rig.station);
    assert_eq!(
        rig.status_of("Reset", json!({"type": "Immediate"})).await,
        "Accepted"
    );
    rig.wait_calls("BootNotification", 2).await;
    wait_snapshot(&rig.station, "the new boot answered", Snapshot2::booted).await;
    // The reset is over once the boot of the reboot was answered.
    frames.log_containing("reset finished").await;
    rig.send(Command2::Disconnect).await;
    rig.send(Command2::Connect).await;
    let boots = rig.wait_calls("BootNotification", 3).await;
    assert_eq!(boots[2]["reason"], "PowerUp");
    rig.stop().await;
}

#[tokio::test]
async fn an_immediate_reset_ends_the_transaction_before_the_reboot() {
    let rig = Rig::new().await;
    rig.start_transaction(1).await;
    assert_eq!(
        rig.status_of("Reset", json!({"type": "Immediate"})).await,
        "Accepted"
    );
    let ended = rig.wait_event("Ended").await;
    assert_eq!(ended["triggerReason"], "ResetCommand");
    assert_eq!(ended["transactionInfo"]["stoppedReason"], "ImmediateReset");
    let boots = rig.wait_calls("BootNotification", 2).await;
    assert_eq!(boots[1]["reason"], "RemoteReset");
    rig.wait_security_event("ResetOrReboot").await;
    let order: Vec<String> = rig
        .csms
        .received()
        .into_iter()
        .filter(|c| {
            c.action == "BootNotification"
                || (c.action == "TransactionEvent" && c.payload["eventType"] == "Ended")
        })
        .map(|c| c.action)
        .collect();
    assert_eq!(
        order,
        ["BootNotification", "TransactionEvent", "BootNotification"],
        "the Ended event precedes the new boot"
    );
    rig.stop().await;
}

#[tokio::test]
async fn an_on_idle_reset_waits_for_the_transaction() {
    let rig = Rig::new().await;
    rig.start_transaction(1).await;
    assert_eq!(
        rig.status_of("Reset", json!({"type": "OnIdle"})).await,
        "Scheduled"
    );
    // The station handles commands in order; once this one is done, a reset that was going
    // to start by itself would have.
    rig.send(Command2::Heartbeat).await;
    assert_eq!(rig.csms.connections(), 1, "still running");
    assert_eq!(rig.calls("BootNotification").len(), 1);
    rig.send(Command2::StopTransaction {
        evse: 1,
        reason: StopReason::Local,
    })
    .await;
    let ended = rig.wait_event("Ended").await;
    assert_ne!(ended["transactionInfo"]["stoppedReason"], "ImmediateReset");
    // TC_B_21_CS: a reset that was answered `Scheduled` boots as `ScheduledReset`.
    let boots = rig.wait_calls("BootNotification", 2).await;
    assert_eq!(boots[1]["reason"], "ScheduledReset");
    rig.wait_security_event("ResetOrReboot").await;
    rig.stop().await;
}

// ---- RequestStartTransaction -------------------------------------------------------------

#[tokio::test]
async fn remote_start_is_rejected_for_unusable_evses() {
    let rig = Rig::new().await;
    assert_eq!(
        rig.status_of(
            "RequestStartTransaction",
            start_body(Some(9), "T", "Central", 1)
        )
        .await,
        "Rejected"
    );
    let reply = rig
        .call(
            "ChangeAvailability",
            json!({"operationalStatus": "Inoperative", "evse": {"id": 1}}),
        )
        .await;
    assert_eq!(reply["status"], "Accepted");
    assert_eq!(
        rig.status_of(
            "RequestStartTransaction",
            start_body(Some(1), "T", "Central", 2)
        )
        .await,
        "Rejected"
    );
    assert_eq!(rig.calls("TransactionEvent").len(), 0);
    rig.stop().await;
}

#[tokio::test]
async fn remote_start_and_stop_are_rejected_while_the_boot_is_pending() {
    let rig = Rig::start("Pending", |_| {}).await;
    assert_eq!(
        rig.status_of(
            "RequestStartTransaction",
            start_body(Some(1), "T", "Central", 1)
        )
        .await,
        "Rejected"
    );
    assert_eq!(
        rig.status_of(
            "RequestStopTransaction",
            json!({"transactionId": "anything"})
        )
        .await,
        "Rejected"
    );
    rig.stop().await;
}

#[tokio::test]
async fn a_transaction_id_from_the_csms_is_sanitized_in_the_log() {
    let rig = Rig::new().await;
    let mut frames = Frames::new(&rig.station);
    assert_eq!(
        rig.status_of(
            "RequestStopTransaction",
            json!({"transactionId": "unknown\u{1b}[31m"})
        )
        .await,
        "Rejected"
    );
    let line = frames.log_containing("no transaction unknown").await;
    assert!(!line.contains('\u{1b}'), "control character in {line:?}");
    rig.stop().await;
}

#[tokio::test]
async fn remote_start_with_the_ev_connected_starts_a_transaction() {
    let rig = Rig::new().await;
    rig.send(Command2::PlugIn { evse: 1 }).await;
    let reply = rig
        .call(
            "RequestStartTransaction",
            start_body(Some(1), "REMOTE", "Central", 42),
        )
        .await;
    assert_eq!(reply["status"], "Accepted");
    let started = rig.wait_event("Started").await;
    assert_eq!(started["transactionInfo"]["remoteStartId"], 42);
    assert_eq!(started["idToken"]["idToken"], "REMOTE");
    assert_eq!(started["triggerReason"], "RemoteStart");
    // A central token is not authorized by the station.
    assert_eq!(rig.calls("Authorize").len(), 0);
    rig.stop().await;
}

#[tokio::test]
async fn remote_start_without_an_evse_takes_the_first_free_one() {
    let rig = Rig::new().await;
    rig.start_transaction(1).await;
    rig.send(Command2::PlugIn { evse: 2 }).await;
    let reply = rig
        .call(
            "RequestStartTransaction",
            start_body(None, "REMOTE", "Central", 5),
        )
        .await;
    assert_eq!(reply["status"], "Accepted");
    wait_snapshot(&rig.station, "a transaction on EVSE 2", |s| {
        s.evse(2).is_some_and(|e| e.transaction.is_some())
    })
    .await;
    assert_eq!(
        rig.station
            .snapshot()
            .evse(2)
            .and_then(|e| e.transaction.as_ref())
            .and_then(|t| t.remote_start_id),
        Some(5)
    );
    rig.stop().await;
}

#[tokio::test]
async fn remote_start_on_a_busy_evse_is_rejected() {
    let rig = Rig::new().await;
    rig.start_transaction(1).await;
    assert_eq!(
        rig.status_of(
            "RequestStartTransaction",
            start_body(Some(1), "OTHER", "Central", 9)
        )
        .await,
        "Rejected"
    );
    rig.stop().await;
}

#[tokio::test]
async fn remote_start_authorizes_a_local_token_first() {
    let rig = Rig::new().await;
    rig.send(Command2::PlugIn { evse: 1 }).await;
    let reply = rig
        .call(
            "RequestStartTransaction",
            start_body(Some(1), "CARD", "ISO14443", 3),
        )
        .await;
    assert_eq!(reply["status"], "Accepted");
    rig.wait_event("Started").await;
    let authorize = rig.calls("Authorize");
    assert_eq!(authorize.len(), 1);
    assert_eq!(authorize[0]["idToken"]["idToken"], "CARD");
    // F01.FR.01: the Authorize CALL precedes the TransactionEvent.
    let actions: Vec<String> = rig
        .csms
        .received()
        .into_iter()
        .map(|c| c.action)
        .filter(|a| a == "Authorize" || a == "TransactionEvent")
        .collect();
    assert_eq!(actions[0], "Authorize");
    rig.stop().await;
}

#[tokio::test]
async fn remote_start_with_a_blocked_token_starts_nothing() {
    let rig = Rig::new().await;
    rig.send(Command2::PlugIn { evse: 1 }).await;
    let reply = rig
        .call(
            "RequestStartTransaction",
            start_body(Some(1), "BADTAG", "ISO14443", 3),
        )
        .await;
    assert_eq!(reply["status"], "Accepted");
    rig.wait_calls("Authorize", 1).await;
    // Everything the station does for the request is done once it handled a later command.
    rig.send(Command2::Heartbeat).await;
    assert_eq!(rig.calls("TransactionEvent").len(), 0);
    assert!(
        rig.station
            .snapshot()
            .evse(1)
            .expect("EVSE")
            .transaction
            .is_none()
    );
    rig.stop().await;
}

#[tokio::test]
async fn remote_start_skips_authorize_when_the_station_is_told_to() {
    let rig = Rig::new().await;
    rig.set_variable("AuthCtrlr", "AuthorizeRemoteStart", "false")
        .await;
    rig.send(Command2::PlugIn { evse: 1 }).await;
    let reply = rig
        .call(
            "RequestStartTransaction",
            start_body(Some(1), "CARD", "ISO14443", 3),
        )
        .await;
    assert_eq!(reply["status"], "Accepted");
    let started = rig.wait_event("Started").await;
    // The CSMS checks the token in the event (F01.FR.02/03).
    assert_eq!(started["idToken"]["idToken"], "CARD");
    assert_eq!(rig.calls("Authorize").len(), 0);
    rig.stop().await;
}

#[tokio::test]
async fn remote_start_without_an_ev_times_out() {
    let rig = Rig::new().await;
    rig.set_variable("TxCtrlr", "EVConnectionTimeOut", "1")
        .await;
    rig.set_variable("TxCtrlr", "TxStartPoint", "Authorized")
        .await;
    let reply = rig
        .call(
            "RequestStartTransaction",
            start_body(Some(1), "REMOTE", "Central", 8),
        )
        .await;
    assert_eq!(reply["status"], "Accepted");
    let ended = rig.wait_event("Ended").await;
    assert_eq!(ended["triggerReason"], "EVConnectTimeout");
    assert_eq!(ended["transactionInfo"]["stoppedReason"], "Timeout");
    rig.stop().await;
}

// ---- RequestStopTransaction --------------------------------------------------------------

#[tokio::test]
async fn remote_stop_without_a_match_is_rejected() {
    let rig = Rig::new().await;
    assert_eq!(
        rig.status_of("RequestStopTransaction", json!({"transactionId": "nope"}))
            .await,
        "Rejected"
    );
    rig.start_transaction(1).await;
    assert_eq!(
        rig.status_of("RequestStopTransaction", json!({"transactionId": "nope"}))
            .await,
        "Rejected"
    );
    assert!(
        rig.station
            .snapshot()
            .evse(1)
            .expect("EVSE")
            .transaction
            .is_some()
    );
    rig.stop().await;
}

#[tokio::test]
async fn remote_stop_ends_the_matching_transaction() {
    let rig = Rig::new().await;
    rig.start_transaction(1).await;
    let id = rig.transaction_id(1);
    assert_eq!(
        rig.status_of("RequestStopTransaction", json!({"transactionId": id}))
            .await,
        "Accepted"
    );
    let ended = rig.wait_event("Ended").await;
    assert_eq!(ended["triggerReason"], "RemoteStop");
    assert_eq!(ended["transactionInfo"]["stoppedReason"], "Remote");
    assert_eq!(ended["transactionInfo"]["transactionId"], id);
    let snapshot: Snapshot2 = wait_snapshot(&rig.station, "the transaction ended", |s| {
        s.evse(1).is_some_and(|e| e.transaction.is_none())
    })
    .await;
    assert!(snapshot.evse(2).expect("EVSE").transaction.is_none());
    rig.stop().await;
}
