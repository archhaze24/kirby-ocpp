//! Engine behavior with OCPP 2.1 stations.

use kirby_csms_core::{CsmsEvent, Protocol};
use kirby_ocpp_core::ocpp::SchemaRegistry;
use kirby_ocpp_core::ocpp::v2::messages::{
    RequestStartTransaction, RequestStartTransactionRequest,
};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::time::timeout;

use crate::v201_support::{
    Client, STEP, at, boot_payload, error_code_of, handshake, result_of, start, transaction_event,
};

const V21: &str = "ocpp2.1";
const V201: &str = "ocpp2.0.1";

fn state_json(state: &kirby_csms_core::StationState) -> Value {
    serde_json::to_value(state).unwrap()
}

#[tokio::test]
async fn boot_is_accepted_with_a_reply_valid_in_2_1_and_the_state_names_the_protocol() {
    let csms = start(|c| c.heartbeat_interval = 77).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;

    let boot = result_of(station.call("BootNotification", boot_payload()).await);
    SchemaRegistry::for_protocol(Protocol::V21)
        .unwrap()
        .validate_response("BootNotification", &boot)
        .expect("a reply valid against the 2.1 schema");
    assert_eq!(boot["status"], "Accepted");
    assert_eq!(boot["interval"], 77);

    let state = station
        .wait_state(&csms, |state| state.boot.is_some())
        .await;
    assert_eq!(state.vendor_model(), Some(("V-1", "M-1")));
    let json = state_json(&state);
    assert_eq!(json["v2"]["protocol"], V21);
    assert_eq!(json["boot"]["registration"], "Accepted");

    let info = csms.stations().into_iter().next().expect("one station");
    assert_eq!(info.subprotocol.as_deref(), Some(V21));
    let session = csms.connected_sessions().into_iter().next().unwrap();
    assert_eq!(session.protocol_kind(), Some(Protocol::V21));
    csms.shutdown().await;
}

#[tokio::test]
async fn payloads_are_checked_by_the_schemas_of_the_session_protocol() {
    let csms = start(|_| {}).await;
    let mut modern = Client::connect(&csms, "CP-NEW", V21).await;
    let mut older = Client::connect(&csms, "CP-OLD", V201).await;

    // 2.1 made the id token type an open string; 2.0.1 lists the values.
    let authorize = json!({ "idToken": { "idToken": "TAG-1", "type": "CustomType" } });
    let answer = modern.call("Authorize", authorize.clone()).await;
    assert_eq!(result_of(answer)["idTokenInfo"]["status"], "Accepted");
    let refused = older.call("Authorize", authorize).await;
    assert_eq!(error_code_of(&refused), "PropertyConstraintViolation");

    // A payload that breaks the schema of 2.1 is refused in the spelling of 2.x.
    let empty = modern.call("BootNotification", json!({})).await;
    assert_eq!(error_code_of(&empty), "OccurrenceConstraintViolation");
    csms.shutdown().await;
}

#[tokio::test]
async fn actions_new_in_2_1_get_their_default_reply() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;

    let settlement = station
        .call(
            "NotifySettlement",
            json!({
                "pspRef": "PSP-1",
                "status": "Settled",
                "settlementAmount": 12.5,
                "settlementTime": at(1),
            }),
        )
        .await;
    assert_eq!(result_of(settlement), json!({}));

    let vat = station
        .call(
            "VatNumberValidation",
            json!({ "vatNumber": "NL1", "evseId": 1 }),
        )
        .await;
    assert_eq!(
        result_of(vat),
        json!({ "vatNumber": "NL1", "status": "Accepted", "evseId": 1 })
    );

    let stream = station
        .call(
            "OpenPeriodicEventStream",
            json!({ "constantStreamData": {
                "id": 1,
                "variableMonitoringId": 1,
                "params": { "interval": 10 },
            } }),
        )
        .await;
    assert_eq!(result_of(stream), json!({ "status": "Accepted" }));

    // 2.0.1 does not define the action: it is not a default-reply action there.
    let mut older = Client::connect(&csms, "CP-OLD", V201).await;
    let unknown = older.call("NotifySettlement", json!({})).await;
    assert_eq!(error_code_of(&unknown), "NotImplemented");
    csms.shutdown().await;
}

#[tokio::test]
async fn actions_a_station_may_not_send_get_the_ocpp_j_codes() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;
    // Defined actions that only a CSMS sends: one shared with 2.0.1, one new in 2.1.
    let start_tx = station
        .call(
            "RequestStartTransaction",
            json!({ "remoteStartId": 1, "idToken": { "idToken": "T", "type": "Local" } }),
        )
        .await;
    assert_eq!(error_code_of(&start_tx), "NotSupported");
    let tariffs = station.call("GetTariffs", json!({ "evseId": 1 })).await;
    assert_eq!(error_code_of(&tariffs), "NotSupported");
    // The SEND message of 2.1 is no CALL a station may make.
    let stream = station
        .call(
            "NotifyPeriodicEventStream",
            json!({ "id": 1, "pending": 0, "basetime": at(1), "data": [{ "t": 0, "v": "1" }] }),
        )
        .await;
    assert_eq!(error_code_of(&stream), "NotSupported");
    // A name nobody defines, and a 1.6 action.
    let foo = station.call("Foo", json!({})).await;
    assert_eq!(error_code_of(&foo), "NotImplemented");
    let legacy = station.call("StartTransaction", json!({})).await;
    assert_eq!(error_code_of(&legacy), "NotImplemented");
    csms.shutdown().await;
}

#[tokio::test]
async fn a_transaction_is_folded_into_the_2_x_state() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;

    let started = station
        .call(
            "TransactionEvent",
            transaction_event(
                "Started",
                0,
                1,
                "tx-1",
                json!({
                    "idToken": { "idToken": "TAG-1", "type": "ISO14443" },
                    "evse": { "id": 1, "connectorId": 1 },
                }),
            ),
        )
        .await;
    assert_eq!(result_of(started)["idTokenInfo"]["status"], "Accepted");
    let state = station
        .wait_state(&csms, |state| state.active_transaction_count() == 1)
        .await;
    let json = state_json(&state);
    assert_eq!(json["v2"]["protocol"], V21);
    assert_eq!(json["v2"]["transactions"][0]["id"], "tx-1");
    assert_eq!(json["v2"]["transactions"][0]["evse"], 1);

    let ended = station
        .call(
            "TransactionEvent",
            transaction_event(
                "Ended",
                1,
                20,
                "tx-1",
                json!({ "triggerReason": "EVDeparted" }),
            ),
        )
        .await;
    assert_eq!(result_of(ended), json!({}));
    station
        .wait_state(&csms, |state| state.active_transaction_count() == 0)
        .await;
    csms.shutdown().await;
}

#[tokio::test]
async fn typed_and_raw_calls_reach_a_2_1_station() {
    let csms = start(|_| {}).await;
    let mut station = Client::connect(&csms, "CP-1", V21).await;

    let request: RequestStartTransactionRequest = serde_json::from_value(json!({
        "idToken": { "idToken": "TAG-1", "type": "ISO14443" },
        "remoteStartId": 5,
    }))
    .unwrap();
    let (answer, (action, received)) = tokio::join!(
        csms.call_typed::<RequestStartTransaction>("CP-1", request),
        station.serve_next_call(json!({ "status": "Accepted" }))
    );
    assert_eq!(action, "RequestStartTransaction");
    assert_eq!(received["remoteStartId"], 5);
    assert_eq!(
        serde_json::to_value(answer.unwrap()).unwrap()["status"],
        "Accepted"
    );

    // An action new in 2.1 goes out as a raw payload, checked by the 2.1 schema.
    let payload = json!({ "evseId": 1 });
    let (answer, (action, received)) = tokio::join!(
        csms.call("CP-1", "GetTariffs", payload.clone()),
        station.serve_next_call(json!({ "status": "NoTariff" }))
    );
    assert_eq!(action, "GetTariffs");
    assert_eq!(received, payload);
    assert_eq!(result_of(answer.unwrap()), json!({ "status": "NoTariff" }));
    csms.shutdown().await;
}

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
async fn a_reconnect_on_the_same_2_x_protocol_keeps_the_state_and_another_resets_it() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let mut first = Client::connect(&csms, "CP-1", V21).await;
    result_of(first.call("BootNotification", boot_payload()).await);
    first.wait_state(&csms, |state| state.boot.is_some()).await;
    drop(first);

    let second = Client::connect(&csms, "CP-1", V21).await;
    let state = csms.state("CP-1").unwrap();
    assert!(state.boot.is_some());
    assert_eq!(state_json(&state)["v2"]["protocol"], V21);
    drop(second);

    // 2.0.1 and 2.1 states are separate: the fold of one is not the other's.
    let _older = Client::connect(&csms, "CP-1", V201).await;
    wait_state_changed(&mut events, "CP-1").await;
    let state = csms.state("CP-1").unwrap();
    assert!(state.boot.is_none());
    assert!(state.v2.is_none());
    csms.shutdown().await;
}

#[tokio::test]
async fn ocpp_2_1_enforces_the_2_x_station_id_rule() {
    let csms = start(|_| {}).await;
    let long = "x".repeat(49);
    for id in [long.as_str(), "CP:1", "CP+é"] {
        assert_eq!(handshake(&csms, id, V21).await.err(), Some(400), "{id}");
    }
    assert_eq!(csms.stations().len(), 0);
    for id in ["x".repeat(48).as_str(), "a.b_c=d+e|f@g*h-9"] {
        assert!(handshake(&csms, id, V21).await.is_ok(), "{id}");
    }
    csms.shutdown().await;
}
