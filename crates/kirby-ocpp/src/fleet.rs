//! Fleet mode (`--count N`): N stations in one process, with the terminal UI or headless.
//!
//! Output of a fleet is tagged with the station id: `[id] ` in front of every human
//! line, a `"station"` field in every JSON line (stdout and `--log-file` alike).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use kirby_ocpp_core::fleet::{Fleet, FleetSpec};
use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::scenario::{
    CancelToken, JunitSuite, OCPP_VERSION, SCHEMA_VERSION, ScenarioReport, TOOL_VERSION,
    now_rfc3339,
};
use kirby_ocpp_core::{Clock, StationConfig, SystemClock};
use kirby_ocpp_tui::text::sanitize;
use kirby_ocpp_tui::{TuiError, TuiOptions};
use serde_json::{Value, json};

use crate::cli::RunArgs;
use crate::headless::{self, LinkWatch, Sink, stdout_line};
use crate::{
    EXIT_INTERRUPTED, EXIT_USAGE, connect_and_run, load_scenario, override_vars, print_report,
    usage_error, write_junit, write_report,
};

fn clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

/// Connects every station and shows them in the terminal UI, or logs tagged events until
/// interrupted.
#[allow(clippy::too_many_lines)]
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
    // One watcher per station, subscribed before anything connects (also before the terminal UI
    // does). When every station has given up reconnecting the headless run ends with code 1
    // instead of idling.
    let mut watches: Vec<(String, LinkWatch)> = fleet
        .stations()
        .iter()
        .map(|(id, handle)| (id.clone(), LinkWatch::new(handle)))
        .collect();
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
            eprintln!("error: [{id}] {}", sanitize(&error.to_string()));
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
    let dead = async {
        let all_dead = futures_util::future::join_all(
            watches
                .iter_mut()
                .map(|(id, watch)| async move { (id.clone(), watch.dead().await) }),
        );
        all_dead.await
    };
    let mut dead = std::pin::pin!(dead);
    let mut shutdown = crate::Shutdown::new();
    let interrupted = tokio::select! {
        failed = fleet.connect_all() => {
            for (id, error) in failed {
                eprintln!("error: [{id}] {}", sanitize(&error.to_string()));
            }
            false
        }
        _ = shutdown.recv() => true,
    };
    let gave_up = if interrupted {
        None
    } else {
        tokio::select! {
            _ = shutdown.recv() => None,
            gave_up = &mut dead => Some(gave_up),
        }
    };
    fleet.shutdown().await;
    sink.finish().await;
    if let Some(gave_up) = gave_up {
        for (id, reason) in gave_up {
            eprintln!("error: [{id}] {}", sanitize(&reason));
        }
        return ExitCode::FAILURE;
    }
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
    let scenario = match load_scenario(
        &args.scenario,
        connector,
        args.id_tag.as_deref(),
        &args.vars,
    ) {
        Ok(scenario) => scenario,
        Err(error) => return usage_error(&error),
    };
    let mut fleet = match Fleet::spawn(&config, &spec, &clock()) {
        Ok(fleet) => fleet,
        Err(error) => return usage_error(&error.to_string()),
    };
    let stdout = headless::events_on_stdout(json);
    let sink = match Sink::spawn_fleet(fleet.subscribe(), json, log_file, stdout) {
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

    let started_at = now_rfc3339();
    let started = Instant::now();
    let stagger = fleet.stagger();
    let overrides = override_vars(&args.vars);
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
    let mut shutdown = crate::Shutdown::new();
    let outcomes = tokio::select! {
        outcomes = join_all(runs) => Some(outcomes),
        sigint = shutdown.recv() => {
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

    let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let identity = json!({
        "scenario": scenario.name,
        "source": scenario.source,
    });
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
                stations.push(json!({
                    "station": id,
                    "schema": SCHEMA_VERSION,
                    "ocpp_version": report.ocpp_version,
                    "tool_version": TOOL_VERSION,
                    "scenario": identity["scenario"],
                    "source": identity["source"],
                    "started_at": report.started_at,
                    "duration_ms": report.duration_ms,
                    "status": report.status.as_str(),
                    "passed": report.passed,
                }));
            }
            Err(error) => {
                // The text may come from the peer (handshake and close reasons).
                let error = sanitize(error);
                eprintln!("error: [{id}] {error}");
                failed.push(id);
                stations.push(json!({
                    "station": id,
                    "schema": SCHEMA_VERSION,
                    "ocpp_version": OCPP_VERSION,
                    "tool_version": TOOL_VERSION,
                    "scenario": identity["scenario"],
                    "source": identity["source"],
                    "started_at": started_at,
                    "duration_ms": total_ms,
                    "status": "error",
                    "passed": false,
                    "error": error,
                }));
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
    let head = FleetHead {
        scenario: &scenario.name,
        source: scenario.source.as_deref(),
        started_at: &started_at,
        duration_ms: total_ms,
    };
    print_aggregate(ids.len(), &failed, &stations, &head, json);

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

/// Run-level fields of the fleet report.
struct FleetHead<'a> {
    scenario: &'a str,
    source: Option<&'a str>,
    started_at: &'a str,
    duration_ms: u64,
}

fn names_with_status<'a>(stations: &'a [Value], status: &str) -> Vec<&'a str> {
    stations
        .iter()
        .filter(|s| s["status"] == status)
        .filter_map(|s| s["station"].as_str())
        .collect()
}

fn print_aggregate(
    total: usize,
    failed: &[&str],
    stations: &[Value],
    head: &FleetHead<'_>,
    json: bool,
) {
    let passed = total - failed.len();
    if json {
        let count = |status: &str| stations.iter().filter(|s| s["status"] == status).count();
        let ocpp_version = stations
            .iter()
            .find_map(|s| s["ocpp_version"].as_str())
            .unwrap_or(OCPP_VERSION);
        let value = json!({
            "type": "fleet_report",
            "schema": SCHEMA_VERSION,
            "ocpp_version": ocpp_version,
            "tool_version": TOOL_VERSION,
            "scenario": head.scenario,
            "source": head.source,
            "started_at": head.started_at,
            "duration_ms": head.duration_ms,
            "status": if failed.is_empty() { "passed" } else { "failed" },
            "counts": {
                "passed": count("passed"),
                "failed": count("failed"),
                "error": count("error"),
                "skipped": count("skipped"),
            },
            "passed": failed.is_empty(),
            "total": total,
            "passed_count": passed,
            "failed": failed,
            "failed_names": names_with_status(stations, "failed"),
            "error_names": names_with_status(stations, "error"),
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
