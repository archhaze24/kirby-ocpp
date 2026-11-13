//! `--replay FILE`: sends the CALLs a CSMS recording holds to the stations that connect.
//!
//! Only the CALLs the CSMS originated are sent, in recorded order and with fresh message ids;
//! what the stations sent is never replayed. See `docs/csms.md` for the contract.

mod plan;
mod run;

use std::path::Path;
use std::time::Duration;

use kirby_csms_core::{CsmsHandle, Observer};
use kirby_ocpp_core::recording::Recording;
use tokio::time::Instant;

use plan::Plan;

/// Pending observations the runner may fall behind by before the run fails.
pub const OBSERVER_CAPACITY: usize = 4096;

/// A validated recording.
pub struct Replay {
    plan: Plan,
}

impl Replay {
    /// Reads and validates the whole recording; nothing binds or sends before this succeeds.
    ///
    /// # Errors
    /// A message for a file that cannot be read, is not a complete OCPP 1.6 CSMS recording,
    /// holds data that cannot be replayed safely, or spans more than `timeout_secs`.
    pub fn load(path: &Path, timeout_secs: u64) -> Result<Self, String> {
        let recording = Recording::read(path).map_err(|error| {
            format!(
                "cannot read the replay recording: {}",
                crate::headless::sanitize(&error.to_string())
            )
        })?;
        let plan = plan::plan(&recording)
            .map_err(|error| format!("cannot replay the recording: {error}"))?;
        if plan.span_ms / 1000 >= timeout_secs {
            return Err(format!(
                "the replayed CALLs span {} s of recorded time, which does not fit --timeout {timeout_secs}",
                plan.span_ms / 1000
            ));
        }
        Ok(Self { plan })
    }

    /// How many CALLs and stations the run covers.
    #[must_use]
    pub fn size(&self) -> (usize, usize) {
        (self.plan.steps.len(), self.plan.stations.len())
    }

    /// Sends the CALLs. `Ok` is the number of CALLs answered as recorded; `Err` carries the
    /// reason and how many were answered before it.
    ///
    /// # Errors
    /// The first CALL that fails or is answered differently, a lost connection, a missing
    /// prerequisite, an observer that fell behind, or the `deadline`.
    pub async fn run(
        &self,
        handle: &CsmsHandle,
        observer: &mut Observer,
        call_timeout: Duration,
        deadline: Instant,
    ) -> Result<usize, (usize, String)> {
        let mut sent = 0;
        let run = run::run(&self.plan, handle, observer, call_timeout, &mut sent);
        match tokio::time::timeout_at(deadline, run).await {
            Ok(Ok(())) => Ok(sent),
            Ok(Err(error)) => Err((sent, error)),
            Err(_) => Err((sent, "timed out (--timeout)".to_owned())),
        }
    }
}
