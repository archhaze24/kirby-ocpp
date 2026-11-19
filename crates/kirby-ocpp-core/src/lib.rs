//! Core of the kirby-ocpp OCPP 1.6J and 2.0.1 charge point emulator.
//!
//! This crate has **no UI dependencies**. A UI (the `kirby-ocpp-tui` crate, a
//! headless runner, a test) talks to the emulator exclusively through
//! [`station::StationHandle`]: it sends typed [`station::Command`]s, reads
//! [`station::Snapshot`]s and subscribes to [`station::Event`]s. See the
//! crate modules for the big picture.

pub mod clock;
pub mod config;
pub mod config_file;
pub mod error;
pub mod fleet;
pub mod fs_util;
pub mod ids;
pub mod link_view;
pub mod ocpp;
pub mod recording;
pub mod scenario;
pub mod station;
pub mod station2;
pub mod store;
pub mod text;
pub mod timefmt;
pub mod timers;
pub mod ws;

pub use clock::{Clock, ManualClock, SystemClock};
pub use config::{AuthorizationKeyEncoding, StationConfig};
pub use error::{CallError, StationError};
pub use station::{Station, StationHandle};
pub use station2::{Command2, Outcome2, Snapshot2, Station2, Station2Handle};

/// Version of this crate (equal to the workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
