//! One-shot replies to CSMS-originated CALLs.
//!
//! The TS station could answer one CALL twice (a handler replied `Accepted`,
//! a later step threw, the outer `catch` replied with a CALLERROR; see
//! `core-analysis.md`). Here the right to answer is a value:
//!
//! * [`ReplyHandle`] is the untyped right to send exactly one CALLRESULT or one
//!   CALLERROR for one message id. Every consuming method takes `self`, so the
//!   compiler rejects a second reply.
//! * [`Responder<A>`] is the typed wrapper handed to action handlers: it only
//!   accepts `A::Res`.
//! * **Drop rule:** a responder dropped without replying sends a CALLERROR
//!   `InternalError` so the CSMS is never left waiting (best effort; ignored if
//!   the connection is already gone). A reply that was consumed - successfully or
//!   with an error - is never followed by this fallback.

use std::marker::PhantomData;
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::ocpp::action::Action;
use crate::ocpp::frame::{ErrorCode, Frame};

/// Reply could not be handed to the connection.
#[derive(Debug, Error)]
pub enum ReplyError {
    /// The connection (its write queue) is gone; the CSMS will never see the reply.
    #[error("connection closed before the reply could be sent")]
    Closed,
    /// `A::Res` could not be turned into JSON.
    #[error("cannot serialize the response payload: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Untyped, consume-on-use right to reply to one incoming CALL.
#[derive(Debug)]
pub struct ReplyHandle {
    id: String,
    /// `None` once the reply has been consumed.
    sink: Option<mpsc::UnboundedSender<Frame>>,
}

impl ReplyHandle {
    /// Creates the handle for CALL `id`. `sink` is the connection's outbound frame queue;
    /// replies go through it so they stay ordered with other outbound traffic.
    pub fn new(id: impl Into<String>, sink: mpsc::UnboundedSender<Frame>) -> Self {
        Self {
            id: id.into(),
            sink: Some(sink),
        }
    }

    /// Id of the CALL being answered.
    pub fn message_id(&self) -> &str {
        &self.id
    }

    /// Sends a CALLRESULT with `payload` (already a JSON object).
    pub fn accept_value(mut self, payload: Value) -> Result<(), ReplyError> {
        let frame = Frame::call_result(self.id.clone(), payload);
        self.send(frame)
    }

    /// Sends a CALLERROR with empty details.
    pub fn reject(self, code: ErrorCode, description: impl Into<String>) -> Result<(), ReplyError> {
        self.reject_with_details(code, description, Value::Object(serde_json::Map::new()))
    }

    /// Sends a CALLERROR with the given details object.
    pub fn reject_with_details(
        mut self,
        code: ErrorCode,
        description: impl Into<String>,
        details: Value,
    ) -> Result<(), ReplyError> {
        let frame = Frame::CallError {
            id: self.id.clone(),
            code,
            description: description.into(),
            details,
        };
        self.send(frame)
    }

    /// Deliberately leaves the CALL unanswered: **no** reply and **not** the `InternalError`
    /// drop fallback. Only for response rules that test a CSMS call timeout (`ignore`) or
    /// that close the connection instead of answering (`drop`).
    pub fn abandon(mut self) {
        self.sink = None;
    }

    /// Returns a handle whose reply reaches the connection `delay` after this call (counted from
    /// now). Nothing blocks: a small task holds the real sink. Dropping the returned handle
    /// without replying still produces the `InternalError` fallback, delayed the same way.
    /// Must be called inside a tokio runtime.
    #[must_use]
    pub fn delayed(mut self, delay: Duration) -> Self {
        let Some(sink) = self.sink.take() else {
            return self;
        };
        let (proxy, mut frames) = mpsc::unbounded_channel();
        let due = tokio::time::Instant::now() + delay;
        tokio::spawn(async move {
            if let Some(frame) = frames.recv().await {
                tokio::time::sleep_until(due).await;
                // The connection may have closed meanwhile; nothing more can be done then.
                let _ = sink.send(frame);
            }
        });
        Self {
            id: self.id.clone(),
            sink: Some(proxy),
        }
    }

    fn send(&mut self, frame: Frame) -> Result<(), ReplyError> {
        let sink = self.sink.take().ok_or(ReplyError::Closed)?;
        sink.send(frame).map_err(|_| ReplyError::Closed)
    }
}

impl Drop for ReplyHandle {
    fn drop(&mut self) {
        if let Some(sink) = self.sink.take() {
            let frame = Frame::call_error(
                self.id.clone(),
                ErrorCode::InternalError,
                "request was dropped without a reply",
            );
            // The connection may already be gone; nothing more can be done then.
            let _ = sink.send(frame);
        }
    }
}

/// Typed one-shot reply for an incoming CALL of action `A`.
#[derive(Debug)]
pub struct Responder<A: Action> {
    handle: ReplyHandle,
    _action: PhantomData<fn() -> A>,
}

impl<A: Action> Responder<A> {
    pub fn new(handle: ReplyHandle) -> Self {
        Self {
            handle,
            _action: PhantomData,
        }
    }

    /// Id of the CALL being answered.
    pub fn message_id(&self) -> &str {
        self.handle.message_id()
    }

    /// Replies with a CALLRESULT. Consumes the responder: a second reply does not compile.
    ///
    /// If `payload` cannot be serialized the responder is still consumed and the
    /// drop rule answers with `InternalError`.
    pub fn accept(self, payload: A::Res) -> Result<(), ReplyError> {
        let value = serde_json::to_value(payload)?;
        self.handle.accept_value(value)
    }

    /// Replies with a CALLERROR. Consumes the responder.
    pub fn reject_error(
        self,
        code: ErrorCode,
        description: impl Into<String>,
    ) -> Result<(), ReplyError> {
        self.handle.reject(code, description)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;
    use crate::ocpp::messages::{Heartbeat, HeartbeatResponse};

    fn drain(rx: &mut mpsc::UnboundedReceiver<Frame>) -> Vec<Frame> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    #[test]
    fn accept_sends_exactly_one_call_result() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let responder = Responder::<Heartbeat>::new(ReplyHandle::new("m1", tx));
        assert_eq!(responder.message_id(), "m1");
        responder
            .accept(HeartbeatResponse {
                current_time: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            })
            .unwrap();
        assert_eq!(
            drain(&mut rx),
            vec![Frame::call_result(
                "m1",
                json!({"currentTime": "2026-01-01T00:00:00Z"})
            )]
        );
    }

    #[test]
    fn reject_sends_exactly_one_call_error() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        ReplyHandle::new("m2", tx)
            .reject(ErrorCode::NotSupported, "nope")
            .unwrap();
        assert_eq!(
            drain(&mut rx),
            vec![Frame::call_error("m2", ErrorCode::NotSupported, "nope")]
        );
    }

    #[test]
    fn typed_reject_sends_exactly_one_call_error() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        Responder::<Heartbeat>::new(ReplyHandle::new("m3", tx))
            .reject_error(ErrorCode::GenericError, "bad")
            .unwrap();
        assert_eq!(drain(&mut rx).len(), 1);
    }

    #[test]
    fn dropping_without_reply_sends_one_internal_error() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        drop(Responder::<Heartbeat>::new(ReplyHandle::new("m4", tx)));
        let frames = drain(&mut rx);
        assert_eq!(frames.len(), 1);
        let Frame::CallError { id, code, .. } = &frames[0] else {
            panic!("expected CALLERROR, got {:?}", frames[0]);
        };
        assert_eq!(id, "m4");
        assert_eq!(*code, ErrorCode::InternalError);
    }

    #[test]
    fn consumed_handle_does_not_send_the_drop_fallback() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        ReplyHandle::new("m5", tx).accept_value(json!({})).unwrap();
        assert_eq!(drain(&mut rx).len(), 1, "only the CALLRESULT, no fallback");
    }

    #[test]
    fn closed_connection_is_an_error_not_a_panic() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        let err = ReplyHandle::new("m6", tx)
            .accept_value(json!({}))
            .unwrap_err();
        assert!(matches!(err, ReplyError::Closed));
    }

    #[test]
    fn abandoned_handle_sends_nothing_not_even_the_drop_fallback() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        ReplyHandle::new("m7", tx).abandon();
        assert_eq!(drain(&mut rx).len(), 0);
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
    }

    #[tokio::test]
    async fn delayed_reply_arrives_after_the_delay_and_only_once() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let started = tokio::time::Instant::now();
        ReplyHandle::new("m8", tx)
            .delayed(Duration::from_millis(80))
            .accept_value(json!({}))
            .unwrap();
        assert!(rx.try_recv().is_err(), "nothing yet");
        let frame = rx.recv().await.expect("the delayed reply");
        assert_eq!(frame, Frame::call_result("m8", json!({})));
        assert!(started.elapsed() >= Duration::from_millis(80));
        assert!(rx.recv().await.is_none(), "no second frame");
    }

    #[tokio::test]
    async fn delayed_handle_dropped_without_reply_sends_the_fallback_late() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        drop(ReplyHandle::new("m9", tx).delayed(Duration::from_millis(30)));
        let Some(Frame::CallError { code, .. }) = rx.recv().await else {
            panic!("expected the InternalError fallback");
        };
        assert_eq!(code, ErrorCode::InternalError);
    }

    #[tokio::test]
    async fn abandoned_delayed_handle_sends_nothing() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        ReplyHandle::new("m10", tx)
            .delayed(Duration::from_millis(10))
            .abandon();
        assert!(rx.recv().await.is_none());
    }
}
