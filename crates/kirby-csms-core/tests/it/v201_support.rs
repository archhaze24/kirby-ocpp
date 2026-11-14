//! A raw OCPP 2.0.1 station client for the engine tests.
//!
//! It speaks plain frames over a WebSocket, so the tests see exactly what a station sees.
//! Every wait has a deadline; nothing sleeps for a fixed time.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kirby_csms_core::{
    CsmsConfig, CsmsEvent, CsmsHandle, CsmsServer, Frame, StationState, SubprotocolPolicy,
};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const STEP: Duration = Duration::from_secs(10);

pub type Raw =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// An engine that serves OCPP 1.6 and 2.0.1 on one listener.
pub async fn start(tweak: impl FnOnce(&mut CsmsConfig)) -> CsmsHandle {
    let mut config = CsmsConfig::new("127.0.0.1:0".parse().unwrap());
    config.subprotocol =
        SubprotocolPolicy::RequireAny(vec!["ocpp1.6".to_owned(), "ocpp2.0.1".to_owned()]);
    tweak(&mut config);
    CsmsServer::start(config).await.expect("engine starts")
}

/// A timestamp `sec` seconds into a fixed day, as an RFC 3339 string.
pub fn at(sec: u32) -> String {
    format!("2026-01-01T00:{:02}:{:02}.000Z", sec / 60, sec % 60)
}

/// The handshake only: the open socket, or the HTTP status of the refusal.
pub async fn handshake(csms: &CsmsHandle, id: &str, offered: &str) -> Result<Raw, u16> {
    let mut request = format!("{}/ocpp/{id}", csms.url())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", offered.parse().unwrap());
    match tokio_tungstenite::connect_async(request).await {
        Ok((socket, _)) => Ok(socket),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            Err(response.status().as_u16())
        }
        Err(other) => panic!("unexpected handshake error: {other}"),
    }
}

/// A connected station.
pub struct Client {
    socket: Raw,
    id: String,
    events: broadcast::Receiver<CsmsEvent>,
    next: u32,
}

impl Client {
    /// Connects offering `subprotocol` and returns once the engine registered the session.
    pub async fn connect(csms: &CsmsHandle, id: &str, subprotocol: &str) -> Self {
        let mut events = csms.subscribe();
        let socket = handshake(csms, id, subprotocol)
            .await
            .unwrap_or_else(|status| panic!("{id} refused with HTTP {status}"));
        loop {
            match timeout(STEP, events.recv())
                .await
                .expect("connected in time")
            {
                Ok(CsmsEvent::Connected { station }) if station == id => break,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("events closed"),
            }
        }
        Self {
            socket,
            id: id.to_owned(),
            events,
            next: 0,
        }
    }

    pub async fn send_text(&mut self, text: String) {
        self.socket.send(Message::text(text)).await.unwrap();
    }

    /// The next frame from the engine.
    pub async fn recv(&mut self) -> Frame {
        loop {
            match timeout(STEP, self.socket.next())
                .await
                .expect("frame in time")
            {
                Some(Ok(Message::Text(text))) => return Frame::parse(text.as_str()).unwrap(),
                Some(Ok(_)) => {}
                other => panic!("socket ended: {other:?}"),
            }
        }
    }

    /// Sends a CALL with a fresh message id and returns its answer.
    pub async fn call(&mut self, action: &str, payload: Value) -> Frame {
        self.next += 1;
        let id = format!("m{}", self.next);
        self.send_text(Frame::call(id.as_str(), action, payload).to_json_string())
            .await;
        let answer = self.recv().await;
        assert_eq!(answer.id(), id, "answer to another message");
        answer
    }

    /// Waits for a CALL from the engine, answers it with `reply` and returns the action and
    /// the payload it carried.
    pub async fn serve_next_call(&mut self, reply: Value) -> (String, Value) {
        match self.recv().await {
            Frame::Call {
                id,
                action,
                payload,
            } => {
                self.send_text(Frame::call_result(id, reply).to_json_string())
                    .await;
                (action, payload)
            }
            other => panic!("expected a CALL, got {}", other.to_json_string()),
        }
    }

    /// Waits until the station's state satisfies `done`; checks the current state first and
    /// then after every `StateChanged` of this station.
    pub async fn wait_state(
        &mut self,
        csms: &CsmsHandle,
        done: impl Fn(&StationState) -> bool,
    ) -> StationState {
        timeout(STEP, async {
            loop {
                if let Some(state) = csms.state(&self.id)
                    && done(&state)
                {
                    return state;
                }
                match self.events.recv().await {
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => panic!("events closed"),
                }
            }
        })
        .await
        .expect("state in time")
    }
}

/// The result payload of an answer, or a panic that shows the frame.
pub fn result_of(frame: Frame) -> Value {
    match frame {
        Frame::CallResult { payload, .. } => payload,
        other => panic!("expected a CALLRESULT, got {}", other.to_json_string()),
    }
}

/// The error code spelling of an answer, or a panic that shows the frame.
pub fn error_code_of(frame: &Frame) -> String {
    match frame {
        Frame::CallError { code, .. } => code.as_str().to_owned(),
        other => panic!("expected a CALLERROR, got {}", other.to_json_string()),
    }
}

pub fn boot_payload() -> Value {
    json!({
        "reason": "PowerUp",
        "chargingStation": {"model": "M-1", "vendorName": "V-1", "serialNumber": "SN-1"}
    })
}

/// A `TransactionEvent` request; `patch` keys replace or add top-level fields.
pub fn transaction_event(kind: &str, seq: i32, sec: u32, id: &str, patch: Value) -> Value {
    let mut value = json!({
        "eventType": kind,
        "timestamp": at(sec),
        "triggerReason": "Authorized",
        "seqNo": seq,
        "transactionInfo": {"transactionId": id}
    });
    if let (Some(base), Value::Object(extra)) = (value.as_object_mut(), patch) {
        base.extend(extra);
    }
    value
}
