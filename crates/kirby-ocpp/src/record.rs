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
    /// Other files of the same command that the recording must never replace: the
    /// configuration file, the TLS files, the reports, the replayed recording.
    protected: Vec<(String, PathBuf)>,
}

impl RecordTarget {
    pub fn new(path: PathBuf, overwrite: bool) -> Self {
        Self {
            path,
            overwrite,
            protected: Vec::new(),
        }
    }

    /// Adds a file the recording must not replace; `None` is ignored.
    pub fn protect(&mut self, what: &str, path: Option<&Path>) {
        if let Some(path) = path {
            self.protected.push((what.to_owned(), path.to_path_buf()));
        }
    }

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
        self.check_protected(&self.path)
    }

    fn check_protected(&self, path: &Path) -> Result<(), String> {
        for (what, other) in &self.protected {
            if same_file(path, other) {
                return Err(format!("--record would overwrite {what}"));
            }
        }
        Ok(())
    }
}

/// An incomplete recording fails a run that succeeded; a code that already says why the run
/// failed (usage 2, interrupted 130, ...) is kept.
fn keep_failure(code: ExitCode) -> ExitCode {
    if code == ExitCode::SUCCESS {
        ExitCode::FAILURE
    } else {
        code
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
    if let Err(error) = target.check_protected(&target.path) {
        return usage_error(&error);
    }
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
    // An existing file is replaced through a temporary sibling: no other output may use it.
    if let Err(error) = target.check_protected(guard.writing_path()) {
        let _ = tokio::task::spawn_blocking(move || guard.finish_unless_empty()).await;
        return usage_error(&error);
    }
    // The body ends the station (and with it every producer) before it returns.
    let code = body(Some(sink)).await;
    // A run that recorded nothing leaves no file, and an existing one at the path is kept.
    let Ok(summary) = tokio::task::spawn_blocking(move || guard.finish_unless_empty()).await else {
        eprintln!("error: the recording could not be finished");
        return keep_failure(code);
    };
    let shown = kirby_ocpp_tui::text::sanitize(&summary.path.display().to_string());
    if summary.frames == 0 && summary.commands == 0 && summary.incomplete.is_none() {
        return code;
    }
    if let Some(reason) = &summary.incomplete {
        eprintln!("error: the recording {shown} is incomplete: {reason}");
        return keep_failure(code);
    }
    if headless::level() != Level::Quiet {
        eprintln!(
            "note: recorded {} frames and {} commands to {shown}",
            summary.frames, summary.commands
        );
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_incomplete_recording_keeps_a_failing_exit_code() {
        assert_eq!(keep_failure(ExitCode::SUCCESS), ExitCode::FAILURE);
        assert_eq!(keep_failure(ExitCode::from(130)), ExitCode::from(130));
        assert_eq!(keep_failure(ExitCode::from(2)), ExitCode::from(2));
    }
}
