//! OCPP 1.6J central system (CSMS) engine: a multi-station WebSocket server with a station
//! registry, bounded per-station frame history, an event stream, CSMS-originated CALLs and
//! scripted replies. No UI; a terminal tool or a load test drives it through [`CsmsHandle`].
//!
//! * Stations connect to `ws(s)://host:port/<any prefix>/<station id>`; see [`CsmsServer`]
//!   for identity, replacement and outbound-call rules.
//! * Inbound CALLs are checked against the OCPP 1.6J schemas ([`CsmsConfig::validate_inbound`]):
//!   an invalid payload gets a CALLERROR (a [`Fault`] on the same action still wins).
//! * Inbound CALLs get the canned answers of the test mock ([`default_reply`]), unless a
//!   [`Fault`] or a responder ([`CsmsHandle::respond`]) applies. Actions a charge point may
//!   not send get a `NotSupported` CALLERROR (known action names) or `NotImplemented` (unknown names).
//! * [`CsmsHandle::state`] is the per-station protocol read model ([`StationState`]: boot info,
//!   connector statuses, transactions with meter readings); [`CsmsEvent::StateChanged`] tells
//!   when it changed. [`CsmsHandle::call_typed`] and the command helpers (`reset`,
//!   `remote_start_transaction`, ...) send typed, schema-checked commands.
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

mod auth;
mod config;
mod defaults;
mod error;
mod event;
mod fault;
mod observe;
mod policy;
pub mod registry;
mod rules;
mod schemas;
mod server;
mod state;
mod tls;
mod typed;

pub use config::{CsmsConfig, SecurityPolicy, SecurityProfile};
pub use defaults::{
    ACTIONS_PROTOCOL, CHARGE_POINT_ACTIONS, default_reply, default_reply_for,
    is_charge_point_action, is_charge_point_action_for,
};
pub use error::CsmsError;
pub use event::{CsmsEvent, Direction, DisconnectReason, FrameEntry, StationInfo};
pub use fault::{Fault, Reply, Responder};
pub use kirby_ocpp_core::ocpp::{ErrorCode, Frame, Protocol};
pub use observe::{MAX_OBSERVED_ID_TAG, Observation, ObserveError, Observer, SessionId};
pub use policy::{SubprotocolPolicy, TransactionIdAllocator, TransactionIds};
pub use rules::{
    FaultInfo, MAX_FAULT_BYTES, MAX_FAULTS, MAX_FAULTS_PER_STATION, MAX_RULE_BYTES,
    MAX_RULES_PER_STATION, MAX_SCOPED_RULES, RuleInfo, Scope, reply_kind,
};
pub use server::{
    ConnectedSession, CsmsHandle, CsmsServer, HistoryPage, MAX_STATION_ID_LEN, StartOptions,
    event_frame, station_id_from_path,
};
pub use state::{
    BootInfo, CertificateRequest, ConnectorState, InstalledCertificates, LogReport,
    MAX_ACTIVE_TRANSACTIONS, MAX_CONNECTORS, MAX_INSTALLED_CERTIFICATES, MAX_SECURITY_EVENTS,
    Reported, SecurityEventRecord, SignedFirmwareReport, StationState, TransactionRecord,
    TransactionStatus,
};
pub use tls::{server_config_from_pem, server_config_with_client_ca};
