//! Terminal UI for kirby-ocpp (ratatui + crossterm).
//!
//! Placeholder: the real UI arrives in stage 5. The contract is already fixed:
//! the UI receives a [`StationHandle`] and talks to the station only through
//! `send(Command)`, `snapshot()` and `subscribe()`; it owns the terminal and
//! must restore it on every exit path (RAII guard + panic hook).

use std::time::Duration;

use kirby_ocpp_core::StationHandle;
use thiserror::Error;

/// UI tuning knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiOptions {
    /// Minimum time between redraws (drawing only happens when something changed).
    pub tick_rate: Duration,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            tick_rate: Duration::from_millis(33),
        }
    }
}

/// Why the UI ended abnormally.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TuiError {
    /// The skeleton has no UI yet.
    #[error("the terminal UI is not implemented yet (station {charge_point_id})")]
    NotImplemented { charge_point_id: String },
}

/// Runs the UI until the user quits. Does not shut the station down; the caller does.
#[allow(
    clippy::unused_async,
    reason = "the real UI awaits terminal and station events; signature is final"
)]
pub async fn run(handle: StationHandle, options: TuiOptions) -> Result<(), TuiError> {
    let _ = options;
    Err(TuiError::NotImplemented {
        charge_point_id: handle.snapshot().charge_point_id,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kirby_ocpp_core::{Station, StationConfig, SystemClock};

    use super::*;

    #[tokio::test]
    async fn placeholder_reports_which_station_it_was_given() {
        let config = StationConfig::new("ws://localhost:9000/ocpp").with_charge_point_id("CP-TUI");
        let (handle, join) = Station::spawn(config, Arc::new(SystemClock));
        let err = run(handle.clone(), TuiOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(
            &err,
            TuiError::NotImplemented { charge_point_id } if charge_point_id == "CP-TUI"
        ));
        handle.shutdown().await.unwrap();
        join.await.unwrap();
    }

    #[test]
    fn default_tick_rate_is_about_30_fps() {
        assert_eq!(TuiOptions::default().tick_rate, Duration::from_millis(33));
    }
}
