//! Fleet mode (`--count N`): N stations in one process, with the terminal UI or headless.
//!
//! Output of a fleet is tagged with the station id: `[id] ` in front of every human
//! line, a `"station"` field in every JSON line (stdout and `--log-file` alike).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::join_all;
use kirby_ocpp_core::fleet::{Fleet, FleetSpec};
use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::scenario::{CancelToken, JunitSuite, ScenarioReport};
use kirby_ocpp_core::{Clock, StationConfig, SystemClock};
use kirby_ocpp_tui::{TuiError, TuiOptions};
use serde_json::{Value, json};

use crate::cli::RunArgs;
use crate::headless::{Sink, stdout_line};
use crate::{
    EXIT_INTERRUPTED, EXIT_USAGE, connect_and_run, load_scenario, override_vars, print_report,
    shutdown_signal, usage_error, write_junit, write_report,
};

fn clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

/// Connects every station and shows them in the terminal UI, or logs tagged events until
/// interrupted.
pub async fn interactive(
    config: StationConfig,
    spec: FleetSpec,
    tui: bool,
    json: bool,
    log_file: Option<&Path>,
) -> ExitCode {
    let mut fleet = match Fleet::spawn(&config, &spec, &clock()) {
        Ok(fleet) => fleet,
        Err(error) => return usage_error(&error.to_string()),
    };
    let mut sink = None;
    if tui {
        if log_file.is_some() {
            match Sink::spawn_fleet(fleet.subscribe(), json, log_file, false) {
                Ok(started) => sink = Some(started),
                Err(error) => {
                    fleet.shutdown().await;
                    eprintln!("error: cannot open the log file: {error}");
                    return ExitCode::from(EXIT_USAGE);
                }
            }
        }
        let handles: Vec<_> = fleet.stations().iter().map(|(_, h)| h.clone()).collect();
        let mut ui = std::pin::pin!(kirby_ocpp_tui::run_fleet(&handles, TuiOptions::default()));
        // Like the single station: connect at startup (staggered), while the UI is up.
        // Connect failures are printed after the UI ends (stderr would garble the screen).
        let mut connect_failures = Vec::new();
        let result = {
            let mut connecting = std::pin::pin!(fleet.connect_all());
            tokio::select! {
                result = &mut ui => Some(result),
                failed = &mut connecting => {
                    connect_failures = failed;
                    None
                }
            }
        };
        let result = match result {
            Some(result) => result,
            None => ui.await,
        };
        for (id, error) in connect_failures {
            eprintln!("error: [{id}] {error}");
        }
        match result {
            Ok(()) => {
                fleet.shutdown().await;
                if let Some(sink) = sink {
                    sink.finish().await;
                }
                return ExitCode::SUCCESS;
            }
            Err(error @ TuiError::Terminal(_)) => {
                eprintln!("note: {error}; running headless (pass --no-tui to hide this)");
                if let Some(sink) = &sink {
                    sink.enable_stdout();
                }
            }
            Err(error) => {
                fleet.shutdown().await;
                if let Some(sink) = sink {
                    sink.finish().await;
                }
                eprintln!("error: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    let sink = match sink {
        Some(sink) => sink,
        None => match Sink::spawn_fleet(fleet.subscribe(), json, log_file, true) {
            Ok(sink) => sink,
            Err(error) => {
                fleet.shutdown().await;
                eprintln!("error: cannot open the log file: {error}");
                return ExitCode::from(EXIT_USAGE);
            }
        },
    };
    let interrupted = tokio::select! {
        failed = fleet.connect_all() => {
            for (id, error) in failed {
                eprintln!("error: [{id}] {error}");
            }
            false
        }
        _ = shutdown_signal() => true,
    };
    if !interrupted {
        let _ = shutdown_signal().await;
    }
    fleet.shutdown().await;
    sink.finish().await;
    ExitCode::SUCCESS
}

/// Runs one scenario on every station at once; exit 0 only if all of them pass.
#[allow(clippy::too_many_lines)]
pub async fn run(
    config: StationConfig,
    spec: FleetSpec,
    args: &RunArgs,
    connector: ConnectorId,
    json: bool,
    log_file: Option<&Path>,
) -> ExitCode {
    let scenario = match load_scenario(&args.scenario, connector, args.id_tag.as_deref()) {
        Ok(scenario) => scenario,
        Err(error) => return usage_error(&error),
    };
    let mut fleet = match Fleet::spawn(&config, &spec, &clock()) {
        Ok(fleet) => fleet,
        Err(error) => return usage_error(&error.to_string()),
    };
    let sink = match Sink::spawn_fleet(fleet.subscribe(), json, log_file, true) {
        Ok(sink) => sink,
        Err(error) => {
            fleet.shutdown().await;
            eprintln!("error: cannot open the log file: {error}");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    let cancel = CancelToken::new();
    let limit = Duration::from_secs(args.timeout);
    let timer = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(limit).await;
            cancel.cancel();
        })
    };

    let stagger = fleet.stagger();
    let overrides = override_vars(args);
    let first_index = u64::from(spec.first_index);
    let runs = fleet
        .stations()
        .iter()
        .enumerate()
        .map(|(index, (_, handle))| {
            let (scenario, cancel, overrides) = (&scenario, &cancel, &overrides);
            async move {
                let delay = stagger.saturating_mul(u32::try_from(index).unwrap_or(u32::MAX));
                wait_or_cancel(delay, cancel).await;
                let index = first_index.saturating_add(index as u64);
                connect_and_run(handle, scenario, cancel, overrides, index).await
            }
        });
    let mut interrupted = false;
    let outcomes = tokio::select! {
        outcomes = join_all(runs) => Some(outcomes),
        sigint = shutdown_signal() => {
            interrupted = sigint;
            cancel.cancel();
            None
        }
    };
    timer.abort();
    let ids: Vec<String> = fleet.stations().iter().map(|(id, _)| id.clone()).collect();
    fleet.shutdown().await;
    sink.finish().await;

    let Some(outcomes) = outcomes else {
        eprintln!("error: interrupted");
        return if interrupted {
            ExitCode::from(EXIT_INTERRUPTED)
        } else {
            ExitCode::FAILURE
        };
    };

    let mut write_failed = false;
    let mut failed: Vec<&str> = Vec::new();
    let mut stations = Vec::with_capacity(ids.len());
    for (id, outcome) in ids.iter().zip(&outcomes) {
        match outcome {
            Ok(report) => {
                if let Some(path) = &args.report
                    && let Err(error) = write_report(&station_report_path(path, id), report)
                {
                    eprintln!("error: [{id}] cannot write the report: {error}");
                    write_failed = true;
                }
                if !report.passed {
                    failed.push(id);
                }
                stations.push(json!({"station": id, "passed": report.passed}));
            }
            Err(error) => {
                eprintln!("error: [{id}] {error}");
                failed.push(id);
                stations.push(json!({"station": id, "passed": false, "error": error}));
            }
        }
    }
    write_failed |= args
        .junit
        .as_ref()
        .is_some_and(|path| !write_fleet_junit(path, &scenario.name, &ids, &outcomes));
    // Files first: a closed stdout must not cost the machine-readable reports.
    for (id, outcome) in ids.iter().zip(&outcomes) {
        if let Ok(report) = outcome {
            print_report(report, json, Some(id));
        }
    }
    print_aggregate(ids.len(), &failed, &stations, json);

    if write_failed || !failed.is_empty() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Sleeps for `delay`, or until the run is cancelled.
async fn wait_or_cancel(delay: Duration, cancel: &CancelToken) {
    if !delay.is_zero() {
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = cancel.cancelled() => {}
        }
    }
}

/// Writes one JUnit file with a suite per station; `false` if it could not be written.
fn write_fleet_junit(
    path: &Path,
    scenario: &str,
    ids: &[String],
    outcomes: &[Result<ScenarioReport, String>],
) -> bool {
    let suites: Vec<JunitSuite<'_>> = ids
        .iter()
        .zip(outcomes)
        .map(|(id, outcome)| JunitSuite {
            station: Some(id),
            scenario,
            outcome: outcome.as_ref().map_err(String::as_str),
        })
        .collect();
    match write_junit(path, &suites) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("error: cannot write the JUnit report: {error}");
            false
        }
    }
}

fn print_aggregate(total: usize, failed: &[&str], stations: &[Value], json: bool) {
    let passed = total - failed.len();
    if json {
        let value = json!({
            "type": "fleet_report",
            "passed": failed.is_empty(),
            "total": total,
            "passed_count": passed,
            "failed": failed,
            "stations": stations,
        });
        stdout_line(&value.to_string(), true);
    } else if failed.is_empty() {
        stdout_line(&format!("fleet: {passed}/{total} stations PASSED"), false);
    } else {
        stdout_line(
            &format!(
                "fleet: {passed}/{total} stations passed, FAILED: {}",
                failed.join(", ")
            ),
            false,
        );
    }
}

/// `--report out.json` for station `CP-1` becomes `out.CP-1.json` (id sanitised for file names).
fn station_report_path(path: &Path, station: &str) -> PathBuf {
    let safe: String = station
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match path.extension() {
        Some(ext) => format!("{stem}.{safe}.{}", ext.to_string_lossy()),
        None => format!("{stem}.{safe}"),
    };
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_path_gets_the_station_id() {
        assert_eq!(
            station_report_path(Path::new("out/report.json"), "CP-001"),
            Path::new("out/report.CP-001.json")
        );
        assert_eq!(
            station_report_path(Path::new("report"), "a/b c"),
            Path::new("report.a_b_c")
        );
    }
}
