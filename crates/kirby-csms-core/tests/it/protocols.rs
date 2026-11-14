//! Subprotocol negotiation and per-session protocol handling, with a raw WebSocket client.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kirby_csms_core::{CsmsConfig, CsmsHandle, CsmsServer, ErrorCode, Frame, Protocol};
use kirby_csms_core::{CsmsEvent, SubprotocolPolicy};
use tokio::sync::broadcast;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const STEP: Duration = Duration::from_secs(8);

type Raw =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn start(policy: SubprotocolPolicy) -> CsmsHandle {
    let mut config = CsmsConfig::new("127.0.0.1:0".parse().unwrap());
    config.subprotocol = policy;
    CsmsServer::start(config).await.expect("engine starts")
}

fn any_of(names: &[&str]) -> SubprotocolPolicy {
    SubprotocolPolicy::RequireAny(names.iter().map(|name| (*name).to_owned()).collect())
}

/// Connects offering `offered` (one header line, comma separated) and returns the socket and the
/// selected subprotocol, or the HTTP status of a refusal.
async fn connect(csms: &CsmsHandle, id: &str, offered: &str) -> Result<(Raw, Option<String>), u16> {
    let mut request = format!("{}/ocpp/{id}", csms.url())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", offered.parse().unwrap());
    match tokio_tungstenite::connect_async(request).await {
        Ok((socket, response)) => {
            let selected = response
                .headers()
                .get("Sec-WebSocket-Protocol")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            Ok((socket, selected))
        }
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            Err(response.status().as_u16())
        }
        Err(other) => panic!("unexpected handshake error: {other}"),
    }
}

async fn recv(socket: &mut Raw) -> Frame {
    loop {
        match timeout(STEP, socket.next()).await.expect("reply in time") {
            Some(Ok(Message::Text(text))) => return Frame::parse(text.as_str()).unwrap(),
            Some(Ok(_)) => {}
            other => panic!("socket ended: {other:?}"),
        }
    }
}

async fn call_error_code(socket: &mut Raw, text: &str) -> ErrorCode {
    socket.send(Message::text(text)).await.unwrap();
    match recv(socket).await {
        Frame::CallError { code, .. } => code,
        other => panic!("expected CALLERROR, got {}", other.to_json_string()),
    }
}

async fn wait_connected(events: &mut broadcast::Receiver<CsmsEvent>, id: &str) {
    loop {
        match timeout(STEP, events.recv()).await.expect("event in time") {
            Ok(CsmsEvent::Connected { station }) if station == id => return,
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => panic!("events closed"),
        }
    }
}

#[tokio::test]
async fn require_any_selects_the_first_listed_offer_in_the_client_order() {
    let csms = start(any_of(&["ocpp1.6", "ocpp2.0.1"])).await;
    let (_a, selected) = connect(&csms, "CP-A", "ocpp2.1, ocpp2.0.1, ocpp1.6")
        .await
        .expect("accepted");
    assert_eq!(selected.as_deref(), Some("ocpp2.0.1"));
    let (_b, selected) = connect(&csms, "CP-B", "ocpp1.6, ocpp2.0.1")
        .await
        .expect("accepted");
    assert_eq!(selected.as_deref(), Some("ocpp1.6"));
    let (_c, selected) = connect(&csms, "CP-C", "ocpp1.6").await.expect("accepted");
    assert_eq!(selected.as_deref(), Some("ocpp1.6"));
    csms.shutdown().await;
}

#[tokio::test]
async fn require_any_refuses_a_client_that_offers_none_of_the_names() {
    let csms = start(any_of(&["ocpp1.6", "ocpp2.0.1"])).await;
    assert_eq!(
        connect(&csms, "CP-X", "ocpp2.1, mqtt").await.err(),
        Some(400)
    );
    // Nothing offered at all.
    let request = format!("{}/ocpp/CP-NONE", csms.url())
        .into_client_request()
        .unwrap();
    assert!(matches!(
        tokio_tungstenite::connect_async(request).await,
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status() == 400
    ));
    assert_eq!(csms.stations().len(), 0);
    csms.shutdown().await;
}

#[tokio::test]
async fn sessions_report_the_protocol_they_negotiated() {
    let csms = start(any_of(&["ocpp1.6", "ocpp2.0.1"])).await;
    let mut events = csms.subscribe();
    let (_old, _) = connect(&csms, "CP-OLD", "ocpp1.6").await.expect("accepted");
    wait_connected(&mut events, "CP-OLD").await;
    let (_new, _) = connect(&csms, "CP-NEW", "ocpp2.0.1")
        .await
        .expect("accepted");
    wait_connected(&mut events, "CP-NEW").await;
    for session in csms.connected_sessions() {
        match session.station() {
            "CP-OLD" => {
                assert_eq!(session.protocol(), Some("ocpp1.6"));
                assert_eq!(session.protocol_kind(), Some(Protocol::V16));
            }
            "CP-NEW" => {
                assert_eq!(session.protocol(), Some("ocpp2.0.1"));
                assert_eq!(session.protocol_kind(), Some(Protocol::V201));
            }
            other => panic!("unexpected session {other}"),
        }
    }
    csms.shutdown().await;
}

#[tokio::test]
async fn each_session_is_validated_against_its_own_schemas_with_the_protocol_error_spelling() {
    let csms = start(any_of(&["ocpp1.6", "ocpp2.0.1"])).await;
    let (mut old, _) = connect(&csms, "CP-OLD", "ocpp1.6").await.expect("accepted");
    let (mut new, _) = connect(&csms, "CP-NEW", "ocpp2.0.1")
        .await
        .expect("accepted");

    // An empty BootNotification breaks the schema of both versions; each session is
    // answered in its own spelling.
    let boot = r#"[2,"b1","BootNotification",{}]"#;
    assert_eq!(
        call_error_code(&mut old, boot).await,
        ErrorCode::OccurenceConstraintViolation
    );
    assert_eq!(
        call_error_code(&mut new, boot).await.as_str(),
        "OccurrenceConstraintViolation"
    );

    // A CALL without a payload is a formation error in 1.6 and a format error in 2.x.
    let broken = r#"[2,"b2","Heartbeat"]"#;
    assert_eq!(
        call_error_code(&mut old, broken).await.as_str(),
        "FormationViolation"
    );
    assert_eq!(
        call_error_code(&mut new, broken).await.as_str(),
        "FormatViolation"
    );
    csms.shutdown().await;
}
