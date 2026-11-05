//! Protocol-state tracking and typed commands, with real `kirby-ocpp-core` stations as
//! clients. Waits are event driven (CsmsEvent::StateChanged); no fixed sleeps.

#![allow(
    clippy::match_wild_err_arm,
    reason = "a timeout is the only error these waits can produce"
)]
#![allow(dead_code, reason = "helpers shared in shape with engine.rs")]

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kirby_csms_core::{
    CsmsConfig, CsmsError, CsmsEvent, CsmsHandle, CsmsServer, Direction, ErrorCode, Frame,
    StationState, TransactionIds, TransactionStatus,
};
use kirby_ocpp_core::config::ReconnectConfig;
use kirby_ocpp_core::ids::{ConnectorId, IdTag};
use kirby_ocpp_core::ocpp::messages::RemoteStartStopStatus;
use kirby_ocpp_core::ocpp::types::{
    ConfigurationStatus, ConnectorStatus, Measurand, RegistrationStatus, ResetType, StopReason,
};
use kirby_ocpp_core::station::{Command, TransactionState};
use kirby_ocpp_core::{Station, StationConfig, StationHandle, SystemClock};
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

async fn start(tweak: impl FnOnce(&mut CsmsConfig)) -> CsmsHandle {
    let mut config = CsmsConfig::new("127.0.0.1:0".parse().unwrap());
    config.transaction_ids = TransactionIds::Incrementing(500);
    tweak(&mut config);
    CsmsServer::start(config).await.expect("engine starts")
}

/// Waits until `pred` holds for the station's state. Subscribe before causing the change.
async fn wait_state(
    csms: &CsmsHandle,
    rx: &mut broadcast::Receiver<CsmsEvent>,
    id: &str,
    what: &str,
    pred: impl Fn(&StationState) -> bool,
) -> StationState {
    loop {
        if let Some(state) = csms.state(id)
            && pred(&state)
        {
            return state;
        }
        match timeout(STEP, rx.recv()).await {
            Ok(Ok(_) | Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(broadcast::error::RecvError::Closed)) => panic!("events closed for {what}"),
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }
}

fn tag(text: &str) -> IdTag {
    IdTag::new(text).unwrap()
}

const STEP: Duration = Duration::from_secs(8);

// ---- real station client -------------------------------------------------------------

struct Cp {
    handle: StationHandle,
    join: JoinHandle<()>,
    _dir: TempDir,
}

impl Cp {
    /// Spawns a station (no reconnect) for `<csms url><prefix>/<id>` and connects it.
    async fn connect(csms: &CsmsHandle, prefix: &str, id: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut config = StationConfig::new(format!("{}{prefix}", csms.url()))
            .with_charge_point_id(id)
            .with_connectors(2)
            .with_call_timeout_ms(2_000)
            .with_reconnect(ReconnectConfig {
                enabled: false,
                initial_delay_ms: 30,
                max_delay_ms: 100,
                max_attempts: 0,
            })
            .with_state_dir(dir.path());
        config.ws_ping_interval_secs = 0;
        let (handle, join) = Station::spawn(config, Arc::new(SystemClock));
        handle.connect().await.expect("connect");
        Self {
            handle,
            join,
            _dir: dir,
        }
    }

    async fn wait_accepted(&self) {
        let mut rx = self.handle.watch_snapshot();
        timeout(
            STEP,
            rx.wait_for(|s| s.registration == Some(RegistrationStatus::Accepted)),
        )
        .await
        .expect("boot accepted in time")
        .expect("station alive");
    }

    async fn stop(self) {
        self.handle.shutdown().await.expect("shutdown");
        self.join.await.expect("actor ends");
    }
}

async fn wait_event(
    rx: &mut broadcast::Receiver<CsmsEvent>,
    what: &str,
    mut pred: impl FnMut(&CsmsEvent) -> bool,
) -> CsmsEvent {
    loop {
        match timeout(STEP, rx.recv()).await {
            Ok(Ok(event)) if pred(&event) => return event,
            Ok(Ok(_) | Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(broadcast::error::RecvError::Closed)) => panic!("events closed for {what}"),
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }
}

fn connected(id: &str) -> impl FnMut(&CsmsEvent) -> bool + '_ {
    move |e| matches!(e, CsmsEvent::Connected { station } if station == id)
}

fn result_payload(frame: Frame) -> Value {
    match frame {
        Frame::CallResult { payload, .. } => payload,
        other => panic!("expected CALLRESULT, got {}", other.to_json_string()),
    }
}

// ---- raw client ----------------------------------------------------------------------

type Raw =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn raw_connect(csms: &CsmsHandle, path: &str) -> Raw {
    let mut request = format!("{}{path}", csms.url())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
    let (socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket
}

async fn raw_send(socket: &mut Raw, text: &str) {
    socket.send(Message::text(text)).await.unwrap();
}

async fn raw_recv(socket: &mut Raw) -> Frame {
    loop {
        match timeout(STEP, socket.next()).await.expect("reply in time") {
            Some(Ok(Message::Text(text))) => return Frame::parse(text.as_str()).unwrap(),
            Some(Ok(_)) => {}
            other => panic!("socket ended: {other:?}"),
        }
    }
}

/// Reads until the server closes the socket.
async fn raw_expect_closed(socket: &mut Raw) {
    loop {
        match timeout(STEP, socket.next()).await.expect("close in time") {
            Some(Ok(Message::Close(_)) | Err(_)) | None => return,
            Some(Ok(_)) => {}
        }
    }
}

// ---- tests ---------------------------------------------------------------------------

#[tokio::test]
async fn boot_info_and_connector_statuses_appear_in_state() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let cp = Cp::connect(&csms, "/ocpp", "CP-S1").await;
    let state = wait_state(&csms, &mut events, "CP-S1", "boot and 3 connectors", |s| {
        s.boot.is_some() && s.connectors.len() == 3
    })
    .await;
    let boot = state.boot.clone().unwrap();
    assert!(!boot.vendor.is_empty() && !boot.model.is_empty());
    assert_eq!(boot.registration, Some(RegistrationStatus::Accepted));
    assert_eq!(boot.interval, Some(300));
    assert_eq!(
        state.connectors.keys().copied().collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(state.connectors[&1].status, ConnectorStatus::Available);
    // Serializes for JSON output.
    let json = serde_json::to_value(&state).unwrap();
    assert_eq!(json["boot"]["registration"], "Accepted");
    assert_eq!(json["connectors"]["1"]["status"], "Available");
    assert_eq!(state.transactions.len(), 0);
    assert!(csms.state("nobody").is_none());
    cp.stop().await;
    csms.shutdown().await;
}

#[tokio::test]
async fn remote_start_and_stop_drive_a_real_transaction() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let cp = Cp::connect(&csms, "/ocpp", "CP-S2").await;
    wait_state(&csms, &mut events, "CP-S2", "boot", |s| {
        s.boot.is_some() && s.connectors.len() == 3
    })
    .await;

    let started = csms
        .remote_start_transaction("CP-S2", tag("TAG-1"), Some(ConnectorId::new(1)), None)
        .await
        .unwrap();
    assert_eq!(started.status, RemoteStartStopStatus::Accepted);
    let state = wait_state(&csms, &mut events, "CP-S2", "active tx", |s| {
        s.transactions
            .iter()
            .any(|t| t.status == TransactionStatus::Active)
    })
    .await;
    let tx = &state.transactions[0];
    assert_eq!((tx.id, tx.connector, tx.id_tag.as_str()), (500, 1, "TAG-1"));

    // The station learns the transaction id from the CONF we just sent.
    let mut snapshots = cp.handle.watch_snapshot();
    timeout(
        STEP,
        snapshots.wait_for(|snap| {
            snap.connectors
                .iter()
                .any(|c| matches!(c.transaction, TransactionState::Active(_)))
        }),
    )
    .await
    .expect("station tx active in time")
    .expect("station alive");
    cp.handle
        .send(Command::MeterValues {
            connector: ConnectorId::new(1),
            wh_delta: 1500,
            measurands: Some(vec![
                Measurand::EnergyActiveImportRegister,
                Measurand::PowerActiveImport,
            ]),
        })
        .await
        .unwrap();
    let state = wait_state(&csms, &mut events, "CP-S2", "meter reading", |s| {
        s.transactions.iter().any(|t| t.energy_wh.is_some())
    })
    .await;
    let tx = &state.transactions[0];
    assert!(tx.energy_wh.unwrap() >= 1500.0);
    assert!(tx.last_meter_at.is_some());

    let stopped = csms.remote_stop_transaction("CP-S2", 500).await.unwrap();
    assert_eq!(stopped.status, RemoteStartStopStatus::Accepted);
    let state = wait_state(&csms, &mut events, "CP-S2", "stopped tx", |s| {
        s.transactions
            .iter()
            .any(|t| t.status == TransactionStatus::Stopped)
    })
    .await;
    let tx = &state.transactions[0];
    assert_eq!(tx.stop_reason, Some(StopReason::Remote));
    assert!(tx.meter_stop.unwrap() >= tx.meter_start);
    assert!(tx.stopped_at.is_some());
    assert_eq!(state.unknown_stops, 0);
    cp.stop().await;
    csms.shutdown().await;
}

#[tokio::test]
async fn transaction_history_is_capped() {
    let csms = start(|c| c.transaction_history_cap = 2).await;
    let mut events = csms.subscribe();
    let cp = Cp::connect(&csms, "/ocpp", "CP-S3").await;
    wait_state(&csms, &mut events, "CP-S3", "boot", |s| s.boot.is_some()).await;
    for round in 0..4 {
        let id = 500 + round;
        cp.handle
            .start_transaction(ConnectorId::new(1), tag("T"))
            .await
            .unwrap();
        wait_state(&csms, &mut events, "CP-S3", "tx active", |s| {
            s.transactions
                .iter()
                .any(|t| t.id == id && t.status == TransactionStatus::Active)
        })
        .await;
        cp.handle
            .stop_transaction(ConnectorId::new(1), StopReason::Local)
            .await
            .unwrap();
        wait_state(&csms, &mut events, "CP-S3", "tx stopped", |s| {
            s.transactions
                .iter()
                .any(|t| t.id == id && t.status == TransactionStatus::Stopped)
        })
        .await;
        cp.handle
            .send(Command::Unplug {
                connector: ConnectorId::new(1),
            })
            .await
            .unwrap();
    }
    let state = csms.state("CP-S3").unwrap();
    let ids: Vec<i32> = state.transactions.iter().map(|t| t.id).collect();
    assert_eq!(ids, [502, 503]);
    cp.stop().await;
    csms.shutdown().await;
}

#[tokio::test]
async fn stop_and_meter_values_of_unknown_transactions_are_harmless() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let mut socket = raw_connect(&csms, "/CP-S4").await;
    wait_event(&mut events, "connect", connected("CP-S4")).await;
    raw_send(
        &mut socket,
        r#"[2,"m1","MeterValues",{"connectorId":1,"transactionId":999,"meterValue":[{"timestamp":"2026-01-01T00:00:00Z","sampledValue":[{"value":"5"}]}]}]"#,
    )
    .await;
    assert!(matches!(
        raw_recv(&mut socket).await,
        Frame::CallResult { .. }
    ));
    raw_send(
        &mut socket,
        r#"[2,"s1","StopTransaction",{"transactionId":999,"meterStop":5,"timestamp":"2026-01-01T00:00:00Z"}]"#,
    )
    .await;
    assert!(matches!(
        raw_recv(&mut socket).await,
        Frame::CallResult { .. }
    ));
    let state = wait_state(&csms, &mut events, "CP-S4", "unknown stop", |s| {
        s.unknown_stops == 1
    })
    .await;
    assert_eq!(state.transactions.len(), 0);
    // Garbage is refused by the schema check and does not touch the state.
    raw_send(&mut socket, r#"[2,"g1","StatusNotification",{"bogus":1}]"#).await;
    assert!(matches!(
        raw_recv(&mut socket).await,
        Frame::CallError { .. }
    ));
    assert_eq!(csms.state("CP-S4").unwrap().connectors.len(), 0);
    csms.shutdown().await;
}

#[tokio::test]
async fn state_survives_reconnect_and_forget_drops_it() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let cp = Cp::connect(&csms, "/ocpp", "CP-S5").await;
    let first = wait_state(&csms, &mut events, "CP-S5", "boot", |s| s.boot.is_some()).await;
    cp.stop().await;
    wait_event(
        &mut events,
        "disconnect",
        |e| matches!(e, CsmsEvent::Disconnected { station, .. } if station == "CP-S5"),
    )
    .await;
    assert!(!csms.station("CP-S5").unwrap().connected);
    assert_eq!(csms.state("CP-S5").unwrap().boot, first.boot);

    let cp = Cp::connect(&csms, "/ocpp", "CP-S5").await;
    let again = wait_state(&csms, &mut events, "CP-S5", "second boot", |s| {
        s.boot.as_ref().map(|b| b.booted_at) > first.boot.as_ref().map(|b| b.booted_at)
    })
    .await;
    assert!(again.connectors.len() >= first.connectors.len());

    assert!(csms.forget("CP-S5"));
    assert!(csms.state("CP-S5").is_none());
    cp.stop().await;
    csms.shutdown().await;
}

#[tokio::test]
async fn configuration_commands_round_trip_typed() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let cp = Cp::connect(&csms, "/ocpp", "CP-S6").await;
    wait_state(&csms, &mut events, "CP-S6", "boot", |s| s.boot.is_some()).await;

    let changed = csms
        .change_configuration("CP-S6", "MeterValueSampleInterval", "45")
        .await
        .unwrap();
    assert_eq!(changed.status, ConfigurationStatus::Accepted);
    let got = csms
        .get_configuration(
            "CP-S6",
            vec!["MeterValueSampleInterval".into(), "NoSuchKey".into()],
        )
        .await
        .unwrap();
    let keys = got.configuration_key.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].value.as_deref(), Some("45"));
    assert_eq!(got.unknown_key.unwrap(), ["NoSuchKey"]);
    let unknown = csms
        .change_configuration("CP-S6", "NoSuchKey", "1")
        .await
        .unwrap();
    assert_eq!(unknown.status, ConfigurationStatus::NotSupported);
    // Whole configuration with an empty key list.
    let all = csms.get_configuration("CP-S6", Vec::new()).await.unwrap();
    assert!(all.configuration_key.unwrap().len() > 5);

    // Stop of a transaction that does not exist is a Rejected status, not an error.
    let rejected = csms.remote_stop_transaction("CP-S6", 4242).await.unwrap();
    assert_eq!(rejected.status, RemoteStartStopStatus::Rejected);
    cp.stop().await;
    csms.shutdown().await;
}

#[tokio::test]
async fn calls_errors_surface_as_typed_errors() {
    let csms = start(|_| {}).await;
    let mut events = csms.subscribe();
    let cp = Cp::connect(&csms, "/ocpp", "CP-S7").await;
    wait_state(&csms, &mut events, "CP-S7", "boot", |s| s.boot.is_some()).await;

    // The station answers the next Reset with a CALLERROR.
    cp.handle
        .send(Command::SetResponseRule {
            rule: "Reset=error:InternalError:boom*1".parse().unwrap(),
        })
        .await
        .unwrap();
    match csms.reset("CP-S7", ResetType::Soft).await {
        Err(CsmsError::CallError {
            station,
            action,
            code,
            description,
        }) => {
            assert_eq!((station.as_str(), action.as_str()), ("CP-S7", "Reset"));
            assert_eq!(code, ErrorCode::InternalError);
            assert!(description.contains("boom"), "{description}");
        }
        other => panic!("expected a typed CALLERROR, got {other:?}"),
    }
    assert_eq!(csms.state("CP-S7").unwrap().failed_calls, 1);

    // A request that violates the schema is refused locally: nothing goes out.
    let out_before = csms
        .history("CP-S7")
        .unwrap()
        .iter()
        .filter(|e| e.direction == Direction::Out)
        .count();
    let long_key = "k".repeat(60);
    match csms.change_configuration("CP-S7", long_key, "1").await {
        Err(CsmsError::InvalidRequest { action, .. }) => assert_eq!(action, "ChangeConfiguration"),
        other => panic!("expected InvalidRequest, got {other:?}"),
    }
    let out_after = csms
        .history("CP-S7")
        .unwrap()
        .iter()
        .filter(|e| e.direction == Direction::Out)
        .count();
    assert_eq!(out_before, out_after);

    assert!(matches!(
        csms.clear_cache("nobody").await,
        Err(CsmsError::UnknownStation { .. })
    ));
    cp.stop().await;
    csms.shutdown().await;
}
