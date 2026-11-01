//! Core of the kirby-ocpp OCPP 1.6J charge point emulator.
//!
//! This crate has **no UI dependencies**. A UI (the `kirby-ocpp-tui` crate, a
//! headless runner, a test) talks to the emulator exclusively through
//! [`station::StationHandle`]: it sends typed [`station::Command`]s, reads
//! [`station::Snapshot`]s and subscribes to [`station::Event`]s. See
//! `docs/rewrite/ARCHITECTURE.md` for the big picture.

pub mod clock;
pub mod config;
pub mod error;
pub mod ids;
pub mod ocpp;
pub mod station;
pub mod store;
pub mod ws;

pub use clock::{Clock, ManualClock, SystemClock};
pub use config::StationConfig;
pub use error::{CallError, StationError};
pub use station::{Station, StationHandle};

/// Version of this crate (equal to the workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
