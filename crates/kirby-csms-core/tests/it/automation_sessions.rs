//! Session-bound calls must never reach a replacement connection or another server.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kirby_csms_core::{CsmsConfig, CsmsError, CsmsEvent, CsmsHandle, CsmsServer, Frame};
use serde_json::json;
use tokio::sync::broadcast;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

const WAIT: Duration = Duration::from_secs(10);

async fn connected(events: &mut broadcast::Receiver<CsmsEvent>) {
    timeout(WAIT, async {
        loop {
            if matches!(events.recv().await.unwrap(), CsmsEvent::Connected { .. }) {
                return;
            }
        }
    })
    .await
    .unwrap();
}

async fn connect(
    handle: &CsmsHandle,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut request = format!("{}/CP", handle.url())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
    timeout(WAIT, tokio_tungstenite::connect_async(request))
        .await
        .unwrap()
        .unwrap()
        .0
}

#[tokio::test]
async fn replacement_and_foreign_tokens_cannot_enqueue_calls() {
    let first = CsmsServer::start(CsmsConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        ..CsmsConfig::default()
    })
    .await
    .unwrap();
    let second = CsmsServer::start(CsmsConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        ..CsmsConfig::default()
    })
    .await
    .unwrap();
    let mut events = first.subscribe();
    let old_socket = connect(&first).await;
    connected(&mut events).await;
    let old = first.connected_session("CP").unwrap();
    assert_eq!(old.station(), "CP");
    assert_eq!(old.protocol(), Some("ocpp1.6"));
    let mut socket = connect(&first).await;
    connected(&mut events).await;
    let current = first.connected_session("CP").unwrap();
    assert_ne!(old, current);
    assert_eq!(first.connected_sessions(), vec![current.clone()]);

    let mut second_events = second.subscribe();
    let foreign_socket = connect(&second).await;
    connected(&mut second_events).await;
    let foreign = second.connected_session("CP").unwrap();
    for token in [&old, &foreign] {
        let result = first
            .call_session_timeout(token, "Reset", json!({"type":"Soft"}), WAIT)
            .await;
        assert!(
            matches!(result, Err(CsmsError::NotConnected { .. })),
            "{result:?}"
        );
    }

    let caller = first.clone();
    let request = tokio::spawn(async move {
        caller
            .call_session_timeout(&current, "Reset", json!({"type":"Soft"}), WAIT)
            .await
    });
    let frame = timeout(WAIT, async {
        loop {
            if let Message::Text(text) = socket.next().await.unwrap().unwrap() {
                break Frame::parse(&text).unwrap();
            }
        }
    })
    .await
    .unwrap();
    assert!(matches!(&frame, Frame::Call { action, .. } if action == "Reset"));
    socket
        .send(Message::text(
            Frame::call_result(frame.id(), json!({"status":"Accepted"})).to_json_string(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        timeout(WAIT, request).await.unwrap().unwrap().unwrap(),
        Frame::CallResult { .. }
    ));
    drop((old_socket, foreign_socket, socket));
    first.shutdown().await;
    second.shutdown().await;
}
