//! A small OCPP-J charge point client for driving the mock CSMS in tests.
#![allow(dead_code)] // Each test binary uses a different subset of these helpers.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kirby_ocpp_testkit::Frame;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::{Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, client_async, connect_async};

/// Upper bound for any single step. Only reached when something is broken.
pub const STEP: Duration = Duration::from_secs(5);

pub type PlainSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub type TlsSocket = WebSocketStream<TlsStream<TcpStream>>;

/// A WebSocket request for `url` offering `protocols` (comma-joined in one header).
pub fn request(url: &str, protocols: &[&str]) -> Request {
    let mut request = url.into_client_request().expect("valid URL");
    if !protocols.is_empty() {
        let value = HeaderValue::from_str(&protocols.join(", ")).expect("valid protocol header");
        request
            .headers_mut()
            .insert("sec-websocket-protocol", value);
    }
    request
}

/// Connects over plain `ws://` and returns the socket and the handshake response.
pub async fn connect_plain(
    url: &str,
    protocols: &[&str],
) -> Result<(PlainSocket, Response), WsError> {
    connect_async(request(url, protocols)).await
}

/// Connects over `wss://` to `addr`, verifying the server against `config`.
/// Any failure (TCP, TLS or the WebSocket upgrade) is returned as an error.
pub async fn connect_tls(
    addr: SocketAddr,
    path: &str,
    config: Arc<ClientConfig>,
) -> Result<TlsSocket, WsError> {
    let tcp = TcpStream::connect(addr).await.map_err(WsError::Io)?;
    let name = ServerName::from(addr.ip());
    let tls = TlsConnector::from(config)
        .connect(name, tcp)
        .await
        .map_err(WsError::Io)?;
    let (socket, _) =
        client_async(request(&format!("wss://{addr}{path}"), &["ocpp1.6"]), tls).await?;
    Ok(socket)
}

/// A client TLS configuration trusting `ca_pem`, optionally presenting a client identity.
pub fn tls_client_config(ca_pem: &str, identity: Option<(&str, &str)>) -> Arc<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        roots.add(cert.expect("CA PEM")).expect("CA root");
    }
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_root_certificates(roots);
    let config = match identity {
        Some((cert_pem, key_pem)) => {
            let chain: Vec<CertificateDer<'static>> =
                CertificateDer::pem_slice_iter(cert_pem.as_bytes())
                    .collect::<Result<_, _>>()
                    .expect("client certificate PEM");
            let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).expect("client key PEM");
            builder
                .with_client_auth_cert(chain, key)
                .expect("client identity")
        }
        None => builder.with_no_client_auth(),
    };
    Arc::new(config)
}

pub async fn send_frame<S>(socket: &mut WebSocketStream<S>, frame: &Frame)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    socket
        .send(Message::text(frame.to_json_string()))
        .await
        .expect("send frame");
}

/// The next text message, or a panic if the socket closes or nothing arrives in time.
pub async fn recv_text<S>(socket: &mut WebSocketStream<S>) -> String
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let next = tokio::time::timeout(STEP, socket.next())
        .await
        .expect("timed out waiting for a message");
    match next {
        Some(Ok(Message::Text(text))) => text.as_str().to_owned(),
        other => panic!("expected a text message, got {other:?}"),
    }
}

pub async fn recv_frame<S>(socket: &mut WebSocketStream<S>) -> Frame
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    Frame::parse(&recv_text(socket).await).expect("well-formed OCPP-J frame")
}

/// Sends a CALL and returns the reply, which must carry the same message id.
pub async fn cp_call<S>(
    socket: &mut WebSocketStream<S>,
    id: &str,
    action: &str,
    payload: Value,
) -> Frame
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    send_frame(socket, &Frame::call(id, action, payload)).await;
    let reply = recv_frame(socket).await;
    assert_eq!(reply.id(), id, "reply to a different call: {reply:?}");
    reply
}

/// The CALLRESULT payload of a reply, or a panic.
pub fn result_payload(frame: Frame) -> Value {
    match frame {
        Frame::CallResult { payload, .. } => payload,
        other => panic!("expected CALLRESULT, got {other:?}"),
    }
}

/// Reads until the socket closes (a Close frame or end of stream). Returns `true` if it closed
/// within [`STEP`]; `false` if it stayed open.
pub async fn wait_closed<S>(socket: &mut WebSocketStream<S>) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let outcome = tokio::time::timeout(STEP, async {
        loop {
            match socket.next().await {
                None | Some(Err(_) | Ok(Message::Close(_))) => return true,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    outcome.unwrap_or(false)
}

/// `Heartbeat` payload shorthand.
pub fn heartbeat() -> Value {
    json!({})
}
