//! Handshake, subprotocol, URL, authentication and keep-alive behaviour of
//! `ws::Connection` over plain `ws://` (ports `tests/ocpp-websocket.test.mjs`).

mod ws_support;

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures_util::SinkExt;
use kirby_ocpp_core::config::TlsConfig;
use kirby_ocpp_core::error::CallError;
use kirby_ocpp_core::ws::{ConnectError, ConnectOptions, Connection};
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use ws_support::{Negotiate, RawServer, STEP, collect_events, connect, next_closed, options};

#[tokio::test]
async fn negotiates_the_ocpp16_subprotocol() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (connection, _events) = connect(&server.url("/ocpp"), options()).await;
    assert!(connection.is_open());
    let _peer = server.next_peer().await;
}

#[tokio::test]
async fn closes_with_1002_when_the_subprotocol_is_omitted() {
    let mut server = RawServer::start(Negotiate::Omit).await;
    let result = Connection::connect(
        &server.url("/ocpp"),
        "CP-001",
        &TlsConfig::default(),
        "ocpp1.6",
        options(),
    )
    .await;
    assert!(
        matches!(&result, Err(ConnectError::SubprotocolMismatch { expected }) if expected == "ocpp1.6"),
        "{result:?}"
    );

    // The server saw our close frame carrying 1002.
    let mut peer = server.next_peer().await;
    match peer.recv().await {
        Some(Message::Close(Some(frame))) => {
            assert_eq!(u16::from(frame.code), 1002);
        }
        other => panic!("expected a 1002 close, got {other:?}"),
    }
}

#[tokio::test]
async fn closes_with_1002_when_a_different_subprotocol_is_selected() {
    let mut server = RawServer::start(Negotiate::Select("ocpp2.0.1".into())).await;
    let result = Connection::connect(
        &server.url("/ocpp"),
        "CP-001",
        &TlsConfig::default(),
        "ocpp1.6",
        options(),
    )
    .await;
    assert!(
        matches!(result, Err(ConnectError::SubprotocolMismatch { .. })),
        "{result:?}"
    );
    let mut peer = server.next_peer().await;
    assert!(
        matches!(peer.recv().await, Some(Message::Close(Some(frame))) if u16::from(frame.code) == 1002)
    );
}

#[tokio::test]
async fn refused_upgrade_is_a_handshake_error() {
    let server = RawServer::start(Negotiate::Reject).await;
    let result = Connection::connect(
        &server.url("/ocpp"),
        "CP-001",
        &TlsConfig::default(),
        "ocpp1.6",
        options(),
    )
    .await;
    assert!(
        matches!(result, Err(ConnectError::HttpStatus { .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn sends_websocket_pings_when_configured() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let options = ConnectOptions {
        ping_interval: Some(Duration::from_millis(10)),
        ..options()
    };
    let (_connection, _events) = connect(&server.url("/ocpp"), options).await;
    let mut peer = server.next_peer().await;
    loop {
        match peer.recv().await {
            Some(Message::Ping(_)) => break,
            Some(_) => {}
            None => panic!("socket ended before a ping arrived"),
        }
    }
}

#[tokio::test]
async fn answers_server_pings_with_pongs() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (_connection, _events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;
    let payload = b"keep-alive".to_vec();
    peer.ws
        .send(Message::Ping(payload.clone().into()))
        .await
        .expect("send ping");
    loop {
        match peer.recv().await {
            Some(Message::Pong(data)) => {
                assert_eq!(data.as_ref(), payload.as_slice());
                break;
            }
            Some(_) => {}
            None => panic!("socket ended before the pong"),
        }
    }
}

#[tokio::test]
async fn terminates_when_the_pong_is_missing() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let options = ConnectOptions {
        ping_interval: Some(Duration::from_millis(10)),
        ..options()
    };
    let (_connection, mut events) = connect(&server.url("/ocpp"), options).await;
    // Hold the peer without reading: tungstenite answers pings only when read,
    // so no pong is ever sent.
    let _silent_peer = server.next_peer().await;

    let (code, reason, by_us) = next_closed(&mut events).await;
    assert_eq!(code, Some(1006));
    assert!(reason.contains("pong"), "{reason}");
    assert!(by_us);
}

#[tokio::test]
async fn query_string_of_the_base_url_is_preserved() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (_connection, _events) = connect(&server.url("/ocpp?token=abc&x=1"), options()).await;
    let peer = server.next_peer().await;
    assert_eq!(peer.uri, "/ocpp/CP-001?token=abc&x=1");
}

#[tokio::test]
async fn charge_point_id_is_percent_encoded_in_the_path() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (connection, _events) = Connection::connect(
        &server.url("/ocpp"),
        "a b/c",
        &TlsConfig::default(),
        "ocpp1.6",
        options(),
    )
    .await
    .map(|(connection, events)| (connection, collect_events(events)))
    .expect("connect");
    let peer = server.next_peer().await;
    assert_eq!(peer.uri, "/ocpp/a%20b%2Fc");
    assert!(connection.is_open());
}

#[tokio::test]
async fn userinfo_becomes_basic_authorization_and_stays_out_of_host() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let url = format!("ws://user:p%40ss@{}/ocpp", server.addr);
    let (_connection, _events) = connect(&url, options()).await;
    let peer = server.next_peer().await;
    let expected = format!("Basic {}", STANDARD.encode("user:p@ss"));
    assert_eq!(peer.authorization.as_deref(), Some(expected.as_str()));
    assert_eq!(peer.host.as_deref(), Some(server.addr.to_string().as_str()));
    assert!(!peer.uri.contains('@'));
}

#[tokio::test]
async fn closing_during_a_pending_handshake_is_clean() {
    // Accepts TCP and never answers the upgrade.
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let url = format!("ws://{addr}/ocpp");

    let attempt = tokio::spawn(async move {
        Connection::connect(&url, "CP-001", &TlsConfig::default(), "ocpp1.6", options()).await
    });
    let (mut silent, _) = listener.accept().await.expect("accept");
    attempt.abort();
    assert!(attempt.await.unwrap_err().is_cancelled());

    // The client side is gone: the silent server reads end of stream.
    let mut buf = [0_u8; 1024];
    let read = tokio::time::timeout(STEP, async {
        loop {
            match silent.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    })
    .await;
    assert!(
        read.is_ok(),
        "the aborted attempt must close its TCP stream"
    );
}

#[tokio::test]
async fn connection_attempt_is_bounded_by_the_call_timeout() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let _accepted = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((tcp, _)) = listener.accept().await {
            held.push(tcp);
        }
    });
    let options = ConnectOptions {
        call_timeout: Duration::from_millis(100),
        ..options()
    };
    let result = Connection::connect(
        &format!("ws://{addr}/ocpp"),
        "CP-001",
        &TlsConfig::default(),
        "ocpp1.6",
        options,
    )
    .await;
    assert!(matches!(result, Err(ConnectError::Timeout)), "{result:?}");
}

#[tokio::test]
async fn unreachable_server_is_a_network_error() {
    // Bind and drop: the port is free, so connecting is refused.
    let addr = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        listener.local_addr().expect("addr")
    };
    let result = Connection::connect(
        &format!("ws://{addr}/ocpp"),
        "CP-001",
        &TlsConfig::default(),
        "ocpp1.6",
        options(),
    )
    .await;
    assert!(
        matches!(result, Err(ConnectError::Network(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn graceful_close_sends_a_close_frame_and_reports_it() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (connection, mut events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;

    connection.close();
    assert!(!connection.is_open());
    connection.close(); // idempotent

    match peer.recv().await {
        Some(Message::Close(Some(frame))) => assert_eq!(u16::from(frame.code), 1000),
        other => panic!("expected a normal close, got {other:?}"),
    }
    // tungstenite queued the close reply when it read our Close; flushing writes it,
    // so the client's close handshake completes at once.
    peer.ws.flush().await.expect("flush the close reply");

    let (code, _reason, by_us) = next_closed(&mut events).await;
    assert_eq!(code, Some(1000));
    assert!(by_us);
}

#[tokio::test]
async fn server_close_is_reported_with_its_code() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (connection, mut events) = connect(&server.url("/ocpp"), options()).await;
    let mut peer = server.next_peer().await;
    peer.close_with(1001).await;

    let (code, reason, by_us) = next_closed(&mut events).await;
    assert_eq!(code, Some(1001));
    assert_eq!(reason, "test");
    assert!(!by_us);
    assert!(!connection.is_open());
    assert!(matches!(
        connection.call("Heartbeat", json!({}), None).await,
        Err(CallError::NotConnected)
    ));
}

#[tokio::test]
async fn dropped_socket_is_reported_as_abnormal_closure() {
    let mut server = RawServer::start(Negotiate::Select("ocpp1.6".into())).await;
    let (_connection, mut events) = connect(&server.url("/ocpp"), options()).await;
    let peer = server.next_peer().await;
    drop(peer);

    let (code, _reason, by_us) = next_closed(&mut events).await;
    assert_eq!(code, Some(1006));
    assert!(!by_us);
}
