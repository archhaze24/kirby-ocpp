//! Fleet mode (`--count N`): N stations in one process, headless only for now.
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
use kirby_ocpp_core::scenario::CancelToken;
use kirby_ocpp_core::{Clock, StationConfig, SystemClock};
use serde_json::{Value, json};

use crate::cli::RunArgs;
use crate::headless::Sink;
use crate::{
    EXIT_INTERRUPTED, EXIT_USAGE, connect_and_run, load_scenario, print_report, shutdown_signal,
    usage_error, write_report,
};

fn clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

/// Connects every station and logs tagged events until interrupted.
pub async fn interactive(
    config: StationConfig,
    spec: FleetSpec,
    tui: bool,
    json: bool,
    log_file: Option<&Path>,
) -> ExitCode {
    // FLEET-GUARD: remove in step B
    if tui {
        return usage_error(
            "fleet mode (--count > 1) does not support the terminal UI yet; pass --no-tui",
        );
    }
    let mut fleet = match Fleet::spawn(&config, &spec, &clock()) {
        Ok(fleet) => fleet,
        Err(error) => return usage_error(&error.to_string()),
    };
    let sink = match Sink::spawn_fleet(fleet.subscribe(), json, log_file) {
        Ok(sink) => sink,
        Err(error) => {
            fleet.shutdown().await;
            eprintln!("error: cannot open the log file: {error}");
            return ExitCode::from(EXIT_USAGE);
        }
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
    let sink = match Sink::spawn_fleet(fleet.subscribe(), json, log_file) {
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
    let runs = fleet
        .stations()
        .iter()
        .enumerate()
        .map(|(index, (_, handle))| {
            let (scenario, cancel) = (&scenario, &cancel);
            async move {
                let delay = stagger.saturating_mul(u32::try_from(index).unwrap_or(u32::MAX));
                if !delay.is_zero() {
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        () = cancel.cancelled() => {}
                    }
                }
                connect_and_run(handle, scenario, cancel).await
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
                print_report(report, json, Some(id));
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
    print_aggregate(ids.len(), &failed, &stations, json);

    if write_failed || !failed.is_empty() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn print_aggregate(total: usize, failed: &[&str], stations: &[Value], json: bool) {
    let passed = total - failed.len();
    if json {
        println!(
            "{}",
            json!({
                "type": "fleet_report",
                "passed": failed.is_empty(),
                "total": total,
                "passed_count": passed,
                "failed": failed,
                "stations": stations,
            })
        );
    } else if failed.is_empty() {
        println!("fleet: {passed}/{total} stations PASSED");
    } else {
        println!(
            "fleet: {passed}/{total} stations passed, FAILED: {}",
            failed.join(", ")
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
