//! The OCPP 2.0.1 scenario target: runs scenario steps against a
//! [`Station2Handle`](crate::station2::Station2Handle).
//!
//! The step mapping is not written yet: every `wait_for` and command step fails with a clear
//! message; `sleep` and `expect_call` already work because the runner handles them itself.

use std::future::ready;

use serde_json::Value;
use tokio::sync::broadcast;

use super::model::Step;
use super::runner::{Evidence, ScenarioTarget, WaitFailure, sealed};
use crate::ocpp::Protocol;
use crate::station::Event;
use crate::station2::Station2Handle;

/// Failure reason of a step the 2.0.1 target cannot run yet.
fn not_implemented(step: &Step) -> String {
    format!(
        "`{}` is not implemented for OCPP 2.0.1 yet",
        step.action.name()
    )
}

impl sealed::Sealed for Station2Handle {}

impl ScenarioTarget for Station2Handle {
    fn protocol(&self) -> Protocol {
        Protocol::V201
    }

    fn station_id(&self) -> String {
        self.snapshot().charge_point_id
    }

    fn subscribe(&self) -> broadcast::Receiver<Event> {
        Station2Handle::subscribe(self)
    }

    fn wait_for(&self, step: &Step) -> impl Future<Output = Result<(), WaitFailure>> + Send {
        ready(Err(WaitFailure {
            reason: not_implemented(step),
            expected: None,
        }))
    }

    fn run_command_step(
        &self,
        step: &Step,
        _tag: u64,
        _evidence: &mut Evidence,
    ) -> impl Future<Output = Result<Option<Value>, String>> + Send {
        ready(Err(not_implemented(step)))
    }

    fn clear_replies(&self) -> impl Future<Output = Option<Value>> + Send {
        ready(None)
    }
}
