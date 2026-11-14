//! Engine behavior with OCPP 2.0.1 stations.

use kirby_csms_core::{CsmsEvent, Fault, Reply, Scope};
use kirby_ocpp_core::ocpp::v2::messages::{Reset, ResetRequest};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::time::timeout;

use crate::v201_support::{
    Client, STEP, at, boot_payload, error_code_of, handshake, result_of, start, transaction_event,
};

const V201: &str = "ocpp2.0.1";
const V16: &str = "ocpp1.6";

fn state_json(state: &kirby_csms_core::StationState) -> Value {
    serde_json::to_value(state).unwrap()
}

// ---- provisioning and status -------------------------------------------------------

#[tokio::test]
async fn boot_is_accepted_with_the_configured_interval_and_heartbeat_answers_the_time() {
    let csms = start(|c| c.heartbeat_interval = 77).await;
    let mut station = Client::connect(&csms, "CP-1", V201).await;

    let boot = result_of(station.call("BootNotification", boot_payload()).await);
    assert_eq!(boot["status"], "Accepted");
    assert_eq!(boot["interval"], 77);
    assert!(boot["currentTime"].as_str().is_some());

    let state = station
        .wait_state(&csms, |state| state.boot.is_some())
        .await;
    assert_eq!(state.vendor_model(), Some(("V-1", "M-1")));
    let info = state.boot.as_ref().unwrap();
    assert_eq!(info.charge_point_serial_number.as_deref(), Some("SN-1"));
    assert_eq!(info.interval, Some(77));
    assert_eq!(state_json(&state)["boot"]["registration"], "Accepted");
    assert_eq!(state_json(&state)["v2"]["protocol"], V201);

    assert!(state.last_heartbeat_at.is_none());
    let heartbeat = result_of(station.call("Heartbeat", json!({})).await);
    assert!(heartbeat["currentTime"].as_str().is_some());
    let state = station
        .wait_state(&csms, |state| state.last_heartbeat_at.is_some())
        .await;
    assert!(state.last_heartbeat_at.is_some());
    csms.shutdown().await;
}

#[tokio::test]
async fn status_notification_shows_in_the_evse_map() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V201).await;
    let answer = station
        .call(
            "StatusNotification",
            json!({
                "timestamp": at(1),
                "connectorStatus": "Occupied",
                "evseId": 1,
                "connectorId": 2
            }),
        )
        .await;
    assert_eq!(result_of(answer), json!({}));
    let state = station.wait_state(&csms, |state| state.v2.is_some()).await;
    let json = state_json(&state);
    assert_eq!(
        json["v2"]["evses"]["1"]["connectors"]["2"]["status"],
        "Occupied"
    );
    assert_eq!(
        state.connector_statuses().get("1/2").map(String::as_str),
        Some("Occupied")
    );
    csms.shutdown().await;
}

// ---- transactions ------------------------------------------------------------------

#[tokio::test]
async fn a_transaction_is_folded_from_started_to_ended() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V201).await;

    let started = station
        .call(
            "TransactionEvent",
            transaction_event(
                "Started",
                0,
                1,
                "tx-1",
                json!({
                    "idToken": {"idToken": "TAG-1", "type": "ISO14443"},
                    "evse": {"id": 1, "connectorId": 1}
                }),
            ),
        )
        .await;
    assert_eq!(
        result_of(started)["idTokenInfo"]["status"],
        "Accepted",
        "an idToken is answered with idTokenInfo"
    );
    let state = station
        .wait_state(&csms, |state| state.active_transaction_count() == 1)
        .await;
    let tx = state_json(&state)["v2"]["transactions"][0].clone();
    assert_eq!(tx["id"], "tx-1");
    assert_eq!(tx["id_token"], "TAG-1");
    assert_eq!(tx["id_token_status"], "Accepted");
    assert_eq!(tx["evse"], 1);

    // No evse and no idToken here: what is known stays.
    let updated = station
        .call(
            "TransactionEvent",
            transaction_event(
                "Updated",
                1,
                10,
                "tx-1",
                json!({
                    "triggerReason": "MeterValuePeriodic",
                    "meterValue": [{
                        "timestamp": at(10),
                        "sampledValue": [{
                            "value": 1500.0,
                            "measurand": "Energy.Active.Import.Register"
                        }]
                    }]
                }),
            ),
        )
        .await;
    assert_eq!(result_of(updated), json!({}), "no idToken, no idTokenInfo");
    let state = station
        .wait_state(&csms, |state| {
            state
                .v2
                .as_ref()
                .and_then(|v2| v2.transactions.first())
                .is_some_and(|tx| tx.energy_wh.is_some())
        })
        .await;
    let tx = &state.v2.as_ref().unwrap().transactions[0];
    assert_eq!(tx.evse, Some(1));
    assert_eq!(tx.connector, Some(1));
    assert_eq!(tx.id_token.as_deref(), Some("TAG-1"));
    assert_eq!(tx.energy_wh, Some(1500.0));

    let ended = station
        .call(
            "TransactionEvent",
            transaction_event(
                "Ended",
                2,
                20,
                "tx-1",
                json!({
                    "triggerReason": "EVCommunicationLost",
                    "transactionInfo": {
                        "transactionId": "tx-1",
                        "stoppedReason": "EVDisconnected"
                    }
                }),
            ),
        )
        .await;
    assert_eq!(result_of(ended), json!({}));
    let state = station
        .wait_state(&csms, |state| state.active_transaction_count() == 0)
        .await;
    assert_eq!(
        state_json(&state)["v2"]["transactions"][0]["status"],
        "stopped"
    );
    assert_stopped_transaction_row(&csms);
    csms.shutdown().await;
}

/// The one finished transaction of the test above, as `CsmsHandle::transactions` lists it.
fn assert_stopped_transaction_row(csms: &kirby_csms_core::CsmsHandle) {
    let (rows, more) = csms.transactions(Some("CP-1"), None, None, 10);
    assert!(!more);
    assert_eq!(rows.len(), 1);
    let (station_id, row) = &rows[0];
    assert_eq!(station_id, "CP-1");
    assert_eq!(row.id.to_string(), "tx-1");
    assert_eq!(row.evse, Some(1));
    assert_eq!(row.connector, Some(1));
    assert_eq!(row.id_tag.as_deref(), Some("TAG-1"));
    assert_eq!(row.stop_reason.as_deref(), Some("EVDisconnected"));
    assert_eq!(row.energy_wh, Some(1500.0));
    assert_eq!(row.started_at.to_rfc3339(), "2026-01-01T00:00:01+00:00");
    // The `since` filter uses the start time.
    let after = row.started_at + chrono::Duration::seconds(1);
    assert_eq!(csms.transactions(None, None, Some(after), 10).0.len(), 0);
    assert_eq!(csms.transactions(None, Some(false), None, 10).0.len(), 1);
    assert_eq!(csms.transactions(None, Some(true), None, 10).0.len(), 0);
}

#[tokio::test]
async fn a_duplicate_event_is_answered_twice_but_folded_once() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V201).await;
    let started = transaction_event(
        "Started",
        0,
        1,
        "tx-1",
        json!({
            "idToken": {"idToken": "TAG-1", "type": "ISO14443"},
            "evse": {"id": 1}
        }),
    );

    let first = result_of(station.call("TransactionEvent", started.clone()).await);
    let after_first = station
        .wait_state(&csms, |state| state.active_transaction_count() == 1)
        .await;
    let second = result_of(station.call("TransactionEvent", started).await);
    assert_eq!(first, second, "a retry gets the same answer");

    // The duplicate changes nothing but its counters.
    let after_second = station
        .wait_state(&csms, |state| {
            state
                .v2
                .as_ref()
                .is_some_and(|v2| v2.counters.duplicate_events == 1)
        })
        .await;
    let mut expected = state_json(&after_first);
    expected["v2"]["counters"]["duplicate_events"] = json!(1);
    expected["v2"]["transactions"][0]["seq"]["duplicates"] = json!(1);
    assert_eq!(state_json(&after_second), expected);
    assert_eq!(after_second.active_transaction_count(), 1);
    assert_eq!(csms.transactions(None, None, None, 10).0.len(), 1);

    // An Ended event stops it; an Updated one after it does not revive it.
    let ended = transaction_event(
        "Ended",
        1,
        20,
        "tx-1",
        json!({"transactionInfo": {"transactionId": "tx-1", "stoppedReason": "Local"}}),
    );
    result_of(station.call("TransactionEvent", ended).await);
    let stopped = station
        .wait_state(&csms, |state| state.active_transaction_count() == 0)
        .await;
    let late = transaction_event(
        "Updated",
        2,
        30,
        "tx-1",
        json!({"triggerReason": "MeterValuePeriodic"}),
    );
    result_of(station.call("TransactionEvent", late).await);
    let after_late = station
        .wait_state(&csms, |state| {
            state
                .v2
                .as_ref()
                .is_some_and(|v2| v2.counters.transaction_events == 3)
        })
        .await;
    assert_eq!(after_late.active_transaction_count(), 0);
    let before = &stopped.v2.as_ref().unwrap().transactions[0];
    let after = &after_late.v2.as_ref().unwrap().transactions[0];
    assert_eq!(after.status, before.status);
    assert_eq!(after.stopped_at, before.stopped_at);
    assert_eq!(after.stopped_reason, before.stopped_reason);
    csms.shutdown().await;
}

// ---- unhandled actions ---------------------------------------------------------------

#[tokio::test]
async fn actions_a_station_may_not_send_get_the_ocpp_j_codes() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V201).await;
    // A defined 2.0.1 action that only a CSMS sends.
    let reset = station.call("Reset", json!({"type": "Immediate"})).await;
    assert_eq!(error_code_of(&reset), "NotSupported");
    // A name nobody defines.
    let foo = station.call("Foo", json!({})).await;
    assert_eq!(error_code_of(&foo), "NotImplemented");
    // A 1.6 action that 2.0.1 does not define.
    let start_tx = station
        .call(
            "StartTransaction",
            json!({"connectorId": 1, "idTag": "T", "meterStart": 0, "timestamp": at(1)}),
        )
        .await;
    assert_eq!(error_code_of(&start_tx), "NotImplemented");
    // None of them left a trace in the state.
    assert!(
        csms.state("CP-1")
            .is_none_or(|state| state.active_transaction_count() == 0)
    );
    csms.shutdown().await;
}

// ---- faults and rules --------------------------------------------------------------

#[tokio::test]
async fn faults_and_rules_apply_to_a_2_0_1_session() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V201).await;

    // A fault wins over everything and is used up.
    csms.inject(Fault::call_error("Heartbeat"));
    assert_eq!(
        error_code_of(&station.call("Heartbeat", json!({})).await),
        "InternalError"
    );
    assert!(
        result_of(station.call("Heartbeat", json!({})).await)["currentTime"]
            .as_str()
            .is_some()
    );

    // A fault limited to the protocol.
    csms.inject_scoped(
        Fault::reply("Heartbeat", Reply::Result(json!({"currentTime": "x"}))),
        Scope::global().protocol(V201),
    );
    assert_eq!(
        result_of(station.call("Heartbeat", json!({})).await),
        json!({"currentTime": "x"})
    );

    // A rule of the station and a rule of the protocol.
    let blocked = json!({"idTokenInfo": {"status": "Blocked"}});
    let authorize = json!({"idToken": {"idToken": "TAG", "type": "ISO14443"}});
    csms.respond_scoped(
        &Scope::global().protocol(V201),
        "Authorize",
        blocked.clone(),
    )
    .unwrap();
    assert_eq!(
        result_of(station.call("Authorize", authorize.clone()).await),
        blocked
    );
    let expired = json!({"idTokenInfo": {"status": "Expired"}});
    csms.respond_scoped(
        &Scope::station("CP-1").protocol(V201),
        "Authorize",
        expired.clone(),
    )
    .unwrap();
    assert_eq!(
        result_of(station.call("Authorize", authorize).await),
        expired,
        "the station's rule wins over the protocol's"
    );
    csms.shutdown().await;
}

#[tokio::test]
async fn a_rule_scoped_to_one_protocol_leaves_the_other_alone() {
    let csms = start(|_| {}).await;
    let mut modern = Client::connect(&csms, "CP-NEW", V201).await;
    let mut legacy = Client::connect(&csms, "CP-OLD", V16).await;
    csms.respond_scoped(
        &Scope::global().protocol(V201),
        "Authorize",
        json!({"idTokenInfo": {"status": "Blocked"}}),
    )
    .unwrap();
    let authorize = json!({"idToken": {"idToken": "TAG", "type": "ISO14443"}});
    assert_eq!(
        result_of(modern.call("Authorize", authorize).await)["idTokenInfo"]["status"],
        "Blocked"
    );
    let answer = result_of(legacy.call("Authorize", json!({"idTag": "TAG"})).await);
    assert_eq!(answer["idTagInfo"]["status"], "Accepted");
    csms.shutdown().await;
}

/// A global responder has no protocol: it answers by action name for every session.
#[tokio::test]
async fn a_global_responder_without_a_protocol_answers_every_protocol() {
    let csms = start(|_| {}).await;
    let mut modern = Client::connect(&csms, "CP-NEW", V201).await;
    let mut legacy = Client::connect(&csms, "CP-OLD", V16).await;
    csms.respond("DataTransfer", |_| {
        Reply::Result(json!({"status": "Rejected"}))
    });
    let modern_answer = modern.call("DataTransfer", json!({"vendorId": "V"})).await;
    let legacy_answer = legacy.call("DataTransfer", json!({"vendorId": "V"})).await;
    assert_eq!(result_of(modern_answer), json!({"status": "Rejected"}));
    assert_eq!(result_of(legacy_answer), json!({"status": "Rejected"}));
    csms.shutdown().await;
}

// ---- commands to the station -----------------------------------------------------------

#[tokio::test]
async fn raw_and_typed_calls_reach_a_2_0_1_station() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V201).await;

    let raw_payload = json!({"requestId": 7, "reportBase": "FullInventory"});
    let (answer, (action, received)) = tokio::join!(
        csms.call("CP-1", "GetBaseReport", raw_payload.clone()),
        station.serve_next_call(json!({"status": "Accepted"}))
    );
    assert_eq!(action, "GetBaseReport");
    assert_eq!(received, raw_payload);
    assert_eq!(result_of(answer.unwrap()), json!({"status": "Accepted"}));

    let request: ResetRequest = serde_json::from_value(json!({"type": "OnIdle"})).unwrap();
    let (answer, (action, received)) = tokio::join!(
        csms.call_typed::<Reset>("CP-1", request),
        station.serve_next_call(json!({"status": "Scheduled"}))
    );
    assert_eq!(action, "Reset");
    assert_eq!(received, json!({"type": "OnIdle"}));
    assert_eq!(
        serde_json::to_value(answer.unwrap()).unwrap(),
        json!({"status": "Scheduled"})
    );
    csms.shutdown().await;
}

// ---- reconnects --------------------------------------------------------------------

async fn wait_state_changed(events: &mut broadcast::Receiver<CsmsEvent>, id: &str) {
    timeout(STEP, async {
        loop {
            match events.recv().await {
                Ok(CsmsEvent::StateChanged { station }) if station == id => return,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("events closed"),
            }
        }
    })
    .await
    .expect("state change in time");
}

#[tokio::test]
async fn a_reconnect_on_the_same_protocol_keeps_the_state() {
    let csms = start(|_| {}).await;
    let mut first = Client::connect(&csms, "CP-1", V201).await;
    result_of(first.call("BootNotification", boot_payload()).await);
    first.wait_state(&csms, |state| state.boot.is_some()).await;
    drop(first);

    let mut second = Client::connect(&csms, "CP-1", V201).await;
    let state = second.wait_state(&csms, |_| true).await;
    assert_eq!(state.vendor_model(), Some(("V-1", "M-1")));
    assert!(state.v2.is_some());
    csms.shutdown().await;
}

#[tokio::test]
async fn a_reconnect_on_another_protocol_starts_the_state_afresh() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let mut modern = Client::connect(&csms, "CP-1", V201).await;
    result_of(modern.call("BootNotification", boot_payload()).await);
    modern.wait_state(&csms, |state| state.boot.is_some()).await;
    drop(modern);

    let _legacy = Client::connect(&csms, "CP-1", V16).await;
    wait_state_changed(&mut events, "CP-1").await;
    let state = csms.state("CP-1").unwrap();
    assert!(state.boot.is_none());
    assert!(state.v2.is_none());
    assert!(state.connectors.is_empty());
    csms.shutdown().await;
}

// ---- station ids ---------------------------------------------------------------------

#[tokio::test]
async fn ocpp_2_0_1_refuses_ids_it_does_not_allow_and_ocpp_1_6_does_not() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let long = "x".repeat(49);
    let mut sockets = Vec::new();
    for id in [long.as_str(), "CP:1", "CP+é"] {
        assert_eq!(handshake(&csms, id, V201).await.err(), Some(400), "{id}");
        // The client's order decides, so 2.0.1 is chosen here too.
        assert_eq!(
            handshake(&csms, id, "ocpp2.0.1, ocpp1.6").await.err(),
            Some(400),
            "{id}"
        );
        // The same id is fine for 1.6.
        sockets.push(handshake(&csms, id, V16).await.expect(id));
    }
    // The refusal names no id (it is unusable and attacker-controlled); the 1.6 sessions
    // register once their socket is up, so wait for their events.
    let (mut refusals, mut connected) = (0, 0);
    while refusals < 6 || connected < 3 {
        match timeout(STEP, events.recv()).await.expect("event in time") {
            Ok(CsmsEvent::AuthFailed {
                station, reason, ..
            }) => {
                assert_eq!(station, None);
                assert_eq!(
                    reason,
                    "refused: station id is not valid for ocpp2.0.1 (1 to 48 characters from a-z A-Z 0-9 * - _ = + | @ .)"
                );
                refusals += 1;
            }
            Ok(CsmsEvent::Connected { .. }) => connected += 1,
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => panic!("events closed"),
        }
    }
    assert_eq!(
        csms.stations().len(),
        3,
        "only the 1.6 sessions are registered"
    );
    // The longest valid id and every allowed character connect.
    for id in ["x".repeat(48).as_str(), "a.b_c=d+e|f@g*h-9"] {
        assert!(handshake(&csms, id, V201).await.is_ok(), "{id}");
    }
    drop(sockets);
    csms.shutdown().await;
}
