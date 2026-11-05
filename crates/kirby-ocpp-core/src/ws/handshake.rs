//! The HTTP upgrade to WebSocket, written out by hand.
//!
//! tungstenite fails the handshake (and drops the stream) when the server does not
//! select the offered subprotocol, so a close with 1002 could never be sent. Here
//! the response head is read and checked first: a `101` answer without the requested
//! subprotocol is closed with 1002 and reported as
//! [`ConnectError::SubprotocolMismatch`]. Framing is then tungstenite's.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use percent_encoding::percent_decode_str;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use url::Url;
use uuid::Uuid;

use super::ConnectError;

/// Largest HTTP response head accepted from the server.
const MAX_HEAD: usize = 16 * 1024;
/// Bounds the 1002 close sent to a server that did not select the subprotocol.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Performs the upgrade on `stream` (plain TCP or TLS) and returns the WebSocket.
pub(super) async fn upgrade<S>(
    mut stream: S,
    url: &Url,
    subprotocol: &str,
) -> Result<WebSocketStream<S>, ConnectError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let key = STANDARD.encode(Uuid::new_v4().as_bytes());
    let request = request_head(url, subprotocol, &key);
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| handshake_error(format!("cannot send the upgrade request: {e}")))?;

    let head = parse_head(&read_head(&mut stream).await?)?;
    if head.status != 101 {
        return Err(ConnectError::HttpStatus {
            status: head.status,
            line: head.status_line,
        });
    }
    let upgrades = head
        .header("upgrade")
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let connection_upgrades = head.header("connection").is_some_and(|value| {
        value
            .split(',')
            .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
    });
    let accepted =
        head.header("sec-websocket-accept") == Some(derive_accept_key(key.as_bytes()).as_str());
    if !(upgrades && connection_upgrades && accepted) {
        return Err(handshake_error(
            "the upgrade response is not a valid WebSocket handshake",
        ));
    }

    let mut ws =
        WebSocketStream::from_raw_socket(stream, Role::Client, Some(websocket_config())).await;
    if head.header("sec-websocket-protocol") != Some(subprotocol) {
        let close = CloseFrame {
            code: CloseCode::Protocol,
            reason: "OCPP subprotocol required".into(),
        };
        let _ = timeout(CLOSE_TIMEOUT, ws.close(Some(close))).await;
        return Err(ConnectError::SubprotocolMismatch {
            expected: subprotocol.to_owned(),
        });
    }
    Ok(ws)
}

/// Largest WebSocket message and frame accepted from the CSMS, in bytes. A peer that sends
/// more is disconnected (tungstenite's defaults would take 64 MiB messages).
pub const MAX_INBOUND_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_INBOUND_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_INBOUND_MESSAGE_BYTES))
}

fn handshake_error(message: impl Into<String>) -> ConnectError {
    ConnectError::Handshake(message.into())
}

/// The request head: target is the URL path and query exactly as written in the URL
/// (the charge point id is already percent-encoded in the path); userinfo becomes a
/// Basic `Authorization` header and is never part of the target or `Host`.
fn request_head(url: &Url, subprotocol: &str, key: &str) -> String {
    let host = url.host_str().unwrap_or_default();
    let host = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    let mut target = url.path().to_owned();
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    let mut head = format!(
        "GET {target} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Protocol: {subprotocol}\r\n"
    );
    if let Some(authorization) = authorization(url) {
        head.push_str("Authorization: ");
        head.push_str(&authorization);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    head
}

/// `Basic base64(user:password)` from the URL userinfo, percent-decoded.
fn authorization(url: &Url) -> Option<String> {
    if url.username().is_empty() && url.password().is_none() {
        return None;
    }
    let user = percent_decode_str(url.username()).decode_utf8_lossy();
    let password = percent_decode_str(url.password().unwrap_or_default()).decode_utf8_lossy();
    Some(format!(
        "Basic {}",
        STANDARD.encode(format!("{user}:{password}"))
    ))
}

/// Reads the response head byte by byte, so no WebSocket frame that follows it
/// in the stream is consumed.
async fn read_head<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>, ConnectError> {
    let mut head = Vec::with_capacity(256);
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD {
            return Err(handshake_error("the upgrade response head is too large"));
        }
        let read = stream
            .read(&mut byte)
            .await
            .map_err(|e| handshake_error(format!("cannot read the upgrade response: {e}")))?;
        if read == 0 {
            return Err(handshake_error(
                "the connection closed before the upgrade response",
            ));
        }
        head.push(byte[0]);
    }
    Ok(head)
}

struct Head {
    status_line: String,
    status: u16,
    /// Header names lower-cased, values trimmed.
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, lowercase_name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(name, _)| name == lowercase_name)
            .map(|(_, value)| value.as_str())
    }
}

fn parse_head(bytes: &[u8]) -> Result<Head, ConnectError> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or_default().to_owned();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| handshake_error(format!("malformed HTTP status line `{status_line}`")))?;
    let headers = lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Ok(Head {
        status_line,
        status,
        headers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_head_keeps_query_and_moves_userinfo_into_authorization() {
        let url = Url::parse("ws://user:p%40ss@h:9000/ocpp/CP-1?token=x").unwrap();
        let head = request_head(&url, "ocpp1.6", "key");
        assert!(head.starts_with("GET /ocpp/CP-1?token=x HTTP/1.1\r\nHost: h:9000\r\n"));
        assert!(
            !head.contains("user:p"),
            "userinfo must not appear in the head"
        );
        let expected = format!("Authorization: Basic {}", STANDARD.encode("user:p@ss"));
        assert!(head.contains(&expected), "{head}");
        assert!(head.ends_with("\r\n\r\n"));
    }

    #[test]
    fn request_head_without_userinfo_has_no_authorization() {
        let url = Url::parse("ws://h/ocpp/CP-1").unwrap();
        let head = request_head(&url, "ocpp1.6", "key");
        assert!(!head.contains("Authorization"));
        assert!(head.contains("Host: h\r\n"));
        assert!(head.contains("Sec-WebSocket-Protocol: ocpp1.6\r\n"));
    }

    #[test]
    fn parses_status_and_headers_case_insensitively() {
        let head = parse_head(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nSEC-WebSocket-Protocol:  ocpp1.6 \r\n\r\n",
        )
        .unwrap();
        assert_eq!(head.status, 101);
        assert_eq!(head.header("sec-websocket-protocol"), Some("ocpp1.6"));
        assert_eq!(head.header("upgrade"), Some("websocket"));
        assert_eq!(head.header("connection"), None);
    }

    #[test]
    fn malformed_status_line_is_a_handshake_error() {
        assert!(matches!(
            parse_head(b"garbage\r\n\r\n"),
            Err(ConnectError::Handshake(_))
        ));
    }
}
