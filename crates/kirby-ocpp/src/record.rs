//! `--record`: open the recording before the station starts, close it after the station ended,
//! and report what it holds.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kirby_ocpp_core::recording::{self, RecordError, RecordOptions, RecordSink, Role};

use crate::headless::{self, Level};
use crate::{same_file, usage_error};

/// Where `--record` writes.
#[derive(Debug, Clone)]
pub struct RecordTarget {
    pub path: PathBuf,
    pub overwrite: bool,
}

impl RecordTarget {
    /// Refuses a recording file that is also another file of the same command: a scenario,
    /// the log file, the recording being replayed. Each entry is a description and a path.
    pub fn check_against(&self, others: &[(&str, Option<&Path>)]) -> Result<(), String> {
        for (what, other) in others {
            if let Some(other) = other
                && same_file(&self.path, other)
            {
                return Err(format!("--record would overwrite {what}"));
            }
        }
        Ok(())
    }
}

/// Runs `body` with a recording of `station` when `target` is set. A recording that cannot
/// be created is a usage error (exit 2, nothing runs); one that lost data during the run makes
/// the exit code 1 and says why on stderr. A recording of a run that did nothing (not one
/// frame, not one command) is not kept.
pub async fn scope<F, Fut>(target: Option<&RecordTarget>, station: &str, body: F) -> ExitCode
where
    F: FnOnce(Option<RecordSink>) -> Fut,
    Fut: Future<Output = ExitCode>,
{
    let Some(target) = target else {
        return body(None).await;
    };
    let options = RecordOptions {
        overwrite: target.overwrite,
        ..RecordOptions::default()
    };
    let (sink, guard) =
        match recording::start(&target.path, "1.6", Role::Emulator, station, &options) {
            Ok(started) => started,
            Err(error @ RecordError::Exists { .. }) => {
                return usage_error(&format!(
                    "{}; use --record-overwrite to replace it",
                    kirby_ocpp_tui::text::sanitize(&error.to_string())
                ));
            }
            Err(error) => {
                return usage_error(&kirby_ocpp_tui::text::sanitize(&error.to_string()));
            }
        };
    // The body ends the station (and with it every producer) before it returns.
    let code = body(Some(sink)).await;
    // A run that recorded nothing leaves no file, and an existing one at the path is kept.
    let Ok(summary) = tokio::task::spawn_blocking(move || guard.finish_unless_empty()).await else {
        eprintln!("error: the recording could not be finished");
        return ExitCode::FAILURE;
    };
    let shown = kirby_ocpp_tui::text::sanitize(&summary.path.display().to_string());
    if summary.frames == 0 && summary.commands == 0 && summary.incomplete.is_none() {
        return code;
    }
    if let Some(reason) = &summary.incomplete {
        eprintln!("error: the recording {shown} is incomplete: {reason}");
        return ExitCode::FAILURE;
    }
    if headless::level() != Level::Quiet {
        eprintln!(
            "note: recorded {} frames and {} commands to {shown}",
            summary.frames, summary.commands
        );
    }
    code
}
