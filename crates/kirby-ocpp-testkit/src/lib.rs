//! Dev-only scripted mock OCPP 1.6J central system (CSMS) for integration tests of the
//! charge point emulator.
//!
//! [`MockCsms`] is a WebSocket server on `127.0.0.1` (ephemeral port, `ws://` or `wss://`).
//! It answers the charge point's CALLs with the same canned responses as the TypeScript
//! smoke harness (`scripts/smoke/harness.mjs`), records every CALL for assertions, and lets a
//! test:
//!
//! * override the answer for one action with [`MockCsms::respond`],
//! * script failures for the next N occurrences of an action with [`Fault`]
//!   (CALLERROR, silence, socket close, invalid payload),
//! * send CSMS-originated CALLs to the charge point with [`MockCsms::send_call`],
//! * drop or count connections, and check subprotocol negotiation and mTLS.
//!
//! Waiting is event driven: every `wait_for_*` method takes a timeout and returns as soon as
//! the condition holds. Nothing sleeps for a fixed time.
//!
//! # Example
//!
//! ```no_run
//! use std::time::Duration;
//! use kirby_ocpp_testkit::{CsmsOptions, Fault, MockCsms, Reply, TestkitError};
//! use serde_json::json;
//!
//! # async fn run() -> Result<(), TestkitError> {
//! let csms = MockCsms::start(CsmsOptions::default()).await?;
//! let timeout = Duration::from_secs(5);
//!
//! // Hand `csms.url()` to the emulator as its central system URL.
//! csms.inject(Fault::call_error("StartTransaction").times(2));
//! csms.respond("DataTransfer", |_payload| {
//!     Reply::Result(json!({ "status": "Rejected" }))
//! });
//!
//! let boot = csms.wait_for_call("BootNotification", timeout).await?;
//! assert_eq!(boot["chargePointModel"], "TUI-16");
//!
//! // Only calls made after this mark count.
//! let mark = csms.mark();
//! csms.send_call("ChangeConfiguration", json!({ "key": "HeartbeatInterval", "value": "6" }))
//!     .await?;
//! csms.wait_for_call_after(mark, "Heartbeat", |_| true, timeout).await?;
//!
//! csms.shutdown().await;
//! # Ok(())
//! # }
//! ```
//!
//! # Threading and waits
//!
//! [`MockCsms`] is `Send + Sync` and all its methods take `&self`, so it can be shared between
//! tasks. Responder closures run outside the internal lock, so they may inspect the server, but
//! they should not block. A test that drives the charge point from another task should wait with
//! the `wait_for_*` methods, not with fixed sleeps.

mod defaults;
mod error;
mod fault;
mod options;
mod record;
mod server;
mod state;
mod tls;

pub use error::{TestkitError, TestkitResult};
pub use fault::{Fault, Reply};
pub use kirby_ocpp_core::ocpp::{ErrorCode, Frame};
pub use options::{CsmsOptions, SubprotocolPolicy, TransactionIds};
pub use record::{ConnectionInfo, Received};
pub use server::MockCsms;
pub use tls::{TlsMaterial, TlsOpts};
