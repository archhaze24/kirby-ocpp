//! Scenarios: sequences of station actions described as data, plus a runner.
//!
//! UI-free: the TUI, the headless CLI and tests all use [`run_scenario`] /
//! [`ScenarioRunner`]. Scenario files are TOML (see [`builtin`] for examples for the format).

pub mod builtin;
mod model;
mod runner;

use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::watch;

pub use self::model::{Action, Condition, DEFAULT_TIMEOUT_MS, Expect, Scenario, Step};
pub use self::runner::{
    ScenarioProgress, ScenarioReport, ScenarioRunner, StepReport, StepStatus, run_scenario,
    run_scenario_with_events,
};

/// Why a scenario could not be loaded or started.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ScenarioError {
    /// The file is not a valid scenario.
    #[error("invalid scenario: {0}")]
    Invalid(String),
    #[error("cannot read scenario file {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A scenario is already running on this station.
    #[error("a scenario is already running on this station")]
    AlreadyRunning,
}

impl ScenarioError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    /// `index` is 0-based; messages count steps from 1.
    pub(crate) fn step(index: usize, message: &str) -> Self {
        Self::Invalid(format!("step {}: {message}", index + 1))
    }
}

/// Cooperative cancellation for a running scenario. Cloneable; all clones share state.
#[derive(Debug, Clone)]
pub struct CancelToken {
    tx: Arc<watch::Sender<bool>>,
    rx: watch::Receiver<bool>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self {
            tx: Arc::new(tx),
            rx,
        }
    }

    pub fn cancel(&self) {
        self.tx.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolves once [`CancelToken::cancel`] was called.
    pub async fn cancelled(&self) {
        let mut rx = self.rx.clone();
        // The sender lives in `self`, so the channel cannot close.
        let _ = rx.wait_for(|cancelled| *cancelled).await;
    }
}
