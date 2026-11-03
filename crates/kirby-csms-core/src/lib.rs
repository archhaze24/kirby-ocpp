//! OCPP 1.6J central system (CSMS) engine: a multi-station WebSocket server with a station
//! registry, bounded per-station frame history, an event stream, CSMS-originated CALLs and
//! scripted replies. No UI; a terminal tool or a load test drives it through [`CsmsHandle`].
//!
//! * Stations connect to `ws(s)://host:port/<any prefix>/<station id>`; see [`CsmsServer`]
//!   for identity, replacement and outbound-call rules.
//! * Inbound CALLs get the canned answers of the test mock ([`default_reply`]), unless a
//!   [`Fault`] or a responder ([`CsmsHandle::respond`]) applies. Actions a charge point may
//!   not send get a `NotSupported` CALLERROR.
//! * [`CsmsHandle::subscribe`] streams [`CsmsEvent`]s; [`CsmsHandle::stations`] and
//!   [`CsmsHandle::history`] are the read model.
//!
//! # Example
//!
//! ```no_run
//! use kirby_csms_core::{CsmsConfig, CsmsError, CsmsEvent, CsmsServer};
//! use serde_json::json;
//!
//! # async fn run() -> Result<(), CsmsError> {
//! let csms = CsmsServer::start(CsmsConfig::new("0.0.0.0:9000".parse().unwrap())).await?;
//! let mut events = csms.subscribe();
//! while let Ok(event) = events.recv().await {
//!     if let CsmsEvent::Connected { station } = event {
//!         let reply = csms
//!             .call(&station, "ChangeConfiguration", json!({ "key": "HeartbeatInterval", "value": "60" }))
//!             .await?;
//!         println!("{station}: {}", reply.to_json_string());
//!     }
//! }
//! csms.shutdown().await;
//! # Ok(())
//! # }
//! ```

mod config;
mod defaults;
mod error;
mod event;
mod fault;
mod policy;
mod server;
mod tls;

pub use config::CsmsConfig;
pub use defaults::{CHARGE_POINT_ACTIONS, default_reply, is_charge_point_action};
pub use error::CsmsError;
pub use event::{CsmsEvent, Direction, DisconnectReason, FrameEntry, StationInfo};
pub use fault::{Fault, Reply, Responder};
pub use kirby_ocpp_core::ocpp::{ErrorCode, Frame};
pub use policy::{SubprotocolPolicy, TransactionIdAllocator, TransactionIds};
pub use server::{CsmsHandle, CsmsServer, station_id_from_path};
pub use tls::server_config_from_pem;
