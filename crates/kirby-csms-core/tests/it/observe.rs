//! The observation feed: startup-atomic, ordered, bounded and independent of the history.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kirby_csms_core::{
    CsmsConfig, CsmsHandle, CsmsServer, Fault, Frame, Observation, ObserveError, Observer,
    StartOptions,
};
use serde_json::json;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const STEP: Duration = Duration::from_secs(8);

type Raw =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn start(capacity: usize, tweak: impl FnOnce(&mut CsmsConfig)) -> (CsmsHandle, Observer) {
    let mut config = CsmsConfig::new("127.0.0.1:0".parse().unwrap());
    tweak(&mut config);
    let (handle, observer) = CsmsServer::start_with(
        config,
        StartOptions {
            observe: Some(capacity),
            ..StartOptions::default()
        },
    )
    .await
    .expect("engine starts");
    (handle, observer.expect("observer requested"))
}

async fn connect(csms: &CsmsHandle, id: &str) -> Raw {
    let mut request = format!("{}/ocpp/{id}", csms.url())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
    tokio_tungstenite::connect_async(request).await.unwrap().0
}

/// Sends a CALL and returns the reply.
async fn exchange(socket: &mut Raw, text: &str) -> Frame {
    socket.send(Message::text(text)).await.unwrap();
    loop {
        match timeout(STEP, socket.next()).await.expect("reply in time") {
            Some(Ok(Message::Text(text))) => return Frame::parse(text.as_str()).unwrap(),
            Some(Ok(_)) => {}
            other => panic!("socket ended: {other:?}"),
        }
    }
}

async fn next(observer: &mut Observer) -> Observation {
    timeout(STEP, observer.recv())
        .await
        .expect("observation in time")
        .expect("feed intact")
}

const BOOT: &str = r#"[2,"b","BootNotification",{"chargePointVendor":"V","chargePointModel":"M"}]"#;
const START: &str = r#"[2,"s","StartTransaction",{"connectorId":1,"idTag":"${TAG}","meterStart":0,"timestamp":"2026-01-01T00:00:00Z"}]"#;

#[tokio::test]
async fn facts_before_the_consumer_polls_are_kept_in_order() {
    // The history is off: the feed must not depend on it.
    let (csms, mut observer) = start(64, |config| config.history_cap = 0).await;
    let mut socket = connect(&csms, "CP-1").await;
    let boot = exchange(&mut socket, BOOT).await;
    assert!(matches!(boot, Frame::CallResult { .. }));
    let started = exchange(&mut socket, START).await;
    let Frame::CallResult { payload, .. } = started else {
        panic!("start refused")
    };
    let live_id = i32::try_from(payload["transactionId"].as_i64().unwrap()).unwrap();
    let stopped = exchange(
        &mut socket,
        &format!(r#"[2,"x","StopTransaction",{{"transactionId":{live_id},"meterStop":1,"timestamp":"2026-01-01T00:01:00Z"}}]"#),
    )
    .await;
    assert!(matches!(stopped, Frame::CallResult { .. }));

    let Observation::Connected(token) = next(&mut observer).await else {
        panic!("connect first")
    };
    assert_eq!(token.station(), "CP-1");
    let session = token.id();
    assert_eq!(
        next(&mut observer).await,
        Observation::Boot {
            station: "CP-1".into(),
            session,
            accepted: true
        }
    );
    assert_eq!(
        next(&mut observer).await,
        Observation::Start {
            station: "CP-1".into(),
            session,
            connector: Some(1),
            id_tag: Some("${TAG}".into()),
            transaction_id: Some(live_id),
        }
    );
    assert_eq!(
        next(&mut observer).await,
        Observation::Stop {
            station: "CP-1".into(),
            session,
            transaction_id: Some(live_id)
        }
    );

    // A replacement ends the old session before the new one is announced.
    let mut second = connect(&csms, "CP-1").await;
    assert_eq!(
        next(&mut observer).await,
        Observation::Ended {
            station: "CP-1".into(),
            session
        }
    );
    let Observation::Connected(replacement) = next(&mut observer).await else {
        panic!("replacement connect")
    };
    assert_ne!(replacement.id(), session);
    assert_ne!(replacement, token);
    // The stale token cannot reach the replacement.
    let stale = csms
        .call_session_timeout(&token, "Reset", json!({"type": "Soft"}), STEP)
        .await;
    assert!(stale.is_err());

    assert!(csms.forget("CP-1"));
    assert_eq!(
        next(&mut observer).await,
        Observation::Ended {
            station: "CP-1".into(),
            session: replacement.id()
        }
    );
    drop(second.close(None).await);
    csms.shutdown().await;
}

#[tokio::test]
async fn refused_and_unanswered_starts_carry_no_transaction_id() {
    let (csms, mut observer) = start(64, |_| {}).await;
    csms.inject(Fault::call_error("StartTransaction"));
    let mut socket = connect(&csms, "CP-2").await;
    let refused = exchange(&mut socket, START).await;
    assert!(matches!(refused, Frame::CallError { .. }));
    let Observation::Connected(token) = next(&mut observer).await else {
        panic!("connect first")
    };
    assert_eq!(
        next(&mut observer).await,
        Observation::Start {
            station: "CP-2".into(),
            session: token.id(),
            connector: Some(1),
            id_tag: Some("${TAG}".into()),
            transaction_id: None,
        }
    );
    csms.shutdown().await;
}

#[tokio::test]
async fn overflow_is_reported_and_sticky() {
    let (csms, mut observer) = start(2, |_| {}).await;
    let mut socket = connect(&csms, "CP-3").await;
    for id in 0..4 {
        let heartbeat = exchange(
            &mut socket,
            BOOT.replace("\"b\"", &format!("\"b{id}\"")).as_str(),
        )
        .await;
        assert!(matches!(heartbeat, Frame::CallResult { .. }));
    }
    for _ in 0..2 {
        let result = timeout(STEP, observer.recv())
            .await
            .expect("answer in time");
        assert_eq!(result.unwrap_err(), ObserveError::Overflow);
    }
    csms.shutdown().await;
}
