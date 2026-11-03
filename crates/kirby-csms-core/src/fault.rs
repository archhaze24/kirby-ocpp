//! Reply and fault script types shared by the engine and the test mock CSMS.

use std::sync::Arc;

use kirby_ocpp_core::ocpp::ErrorCode;
use serde_json::Value;

/// What the central system does with one charge-point-originated CALL.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// Send a CALLRESULT with this payload. The payload is not validated, so it
    /// may deliberately violate the OCPP schema.
    Result(Value),
    /// Send a CALLERROR with this code and description (empty details).
    Error(ErrorCode, String),
    /// Send nothing; the charge point's call times out.
    Silence,
    /// Close the WebSocket with code 1011 without replying.
    Close,
}

/// A scripted one-off failure for the next N occurrences of one action.
///
/// Faults take precedence over responders and over the default answers. Build one,
/// then pass it to the server's `inject`:
///
/// ```
/// use kirby_csms_core::Fault;
///
/// // The next two StartTransaction calls get a CALLERROR, then answers resume.
/// let fault = Fault::call_error("StartTransaction").times(2);
/// # let _ = fault;
/// ```
#[derive(Debug, Clone, PartialEq)]
#[must_use = "a Fault does nothing until it is injected into a server"]
pub struct Fault {
    /// Action the fault applies to.
    pub action: String,
    /// What to do instead of the normal answer.
    pub reply: Reply,
    /// Occurrences left; the fault is dropped when this reaches zero.
    pub remaining: usize,
}

impl Fault {
    /// Replies with `reply` to the next occurrence of `action` (use [`Fault::times`] for more).
    pub fn reply(action: impl Into<String>, reply: Reply) -> Self {
        Self {
            action: action.into(),
            reply,
            remaining: 1,
        }
    }

    /// Replies with a CALLERROR `InternalError` ("injected CALLERROR").
    pub fn call_error(action: impl Into<String>) -> Self {
        Self::reply(
            action,
            Reply::Error(ErrorCode::InternalError, "injected CALLERROR".to_owned()),
        )
    }

    /// Stays silent, so the charge point's call times out.
    pub fn silence(action: impl Into<String>) -> Self {
        Self::reply(action, Reply::Silence)
    }

    /// Closes the socket without replying.
    pub fn close(action: impl Into<String>) -> Self {
        Self::reply(action, Reply::Close)
    }

    /// Sends a CALLRESULT with `payload` as given (for example a schema-invalid one).
    pub fn invalid_payload(action: impl Into<String>, payload: Value) -> Self {
        Self::reply(action, Reply::Result(payload))
    }

    /// Applies the fault to the next `count` occurrences instead of one. `0` is a no-op.
    pub fn times(mut self, count: usize) -> Self {
        self.remaining = count;
        self
    }
}

/// A per-action responder: payload of the CALL in, [`Reply`] out.
pub type Responder = Arc<dyn Fn(&Value) -> Reply + Send + Sync>;
