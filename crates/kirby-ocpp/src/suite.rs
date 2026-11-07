//! `kirby-ocpp suite`: several scenarios one after another, each on a fresh station.
//!
//! Every scenario gets its own [`Station`] with persistence switched off (`--state-dir` and
//! `--no-persist` are ignored), so no state leaks from one scenario into the next and nothing
//! is written to disk.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::scenario::{
    CancelToken, JunitRun, JunitSuite, OCPP_VERSION, SCHEMA_VERSION, Scenario, ScenarioReport,
    StepStatus, TOOL_VERSION, Vars, builtin, now_rfc3339,
};
use kirby_ocpp_core::{Station, StationConfig, SystemClock};
use serde_json::{Value, json};

use crate::cli::SuiteArgs;
use crate::headless::stdout_line;
use crate::{
    EXIT_INTERRUPTED, Shutdown, check_output_paths, connect_and_run, names_a_path, override_vars,
    print_report, spawn_sink, stop_station, unknown_scenario_message, usage_error, write_atomic,
    write_junit,
};

/// Built-ins that need a transaction left by an earlier run; they cannot pass on a fresh
/// station, so `--all-builtins` leaves them out (name one explicitly to run it anyway).
const STATEFUL_BUILTINS: &[&str] = &["finish-session"];

/// One scenario of the suite and where it came from.
pub(crate) struct Entry {
    pub(crate) scenario: Scenario,
    /// Built-in name or file path, used to tell apart equal scenario names.
    pub(crate) source: String,
}

/// How one scenario ended.
enum Outcome {
    /// The scenario ran to a report (which may be a failure or a timeout).
    Done(ScenarioReport),
    /// The station never connected or booted, or the run was cut short.
    Error(String),
    /// Not started (`--fail-fast` or a SIGTERM).
    Skipped,
}

struct Row {
    name: String,
    /// File path or `builtin:NAME` of the scenario.
    source: Option<String>,
    outcome: Outcome,
    /// RFC 3339 UTC start time; `None` for a scenario that never started.
    started_at: Option<String>,
    duration_ms: u64,
}

impl Row {
    fn failed(&self) -> bool {
        match &self.outcome {
            Outcome::Done(report) => !report.passed,
            Outcome::Error(_) => true,
            Outcome::Skipped => false,
        }
    }

    fn status(&self) -> &'static str {
        match &self.outcome {
            Outcome::Done(report) if report.passed => "passed",
            Outcome::Done(_) => "failed",
            Outcome::Error(_) => "error",
            Outcome::Skipped => "skipped",
        }
    }
}

/// Expands the SOURCE arguments into scenarios (files of a directory sorted by name).
pub(crate) fn load_entries(args: &SuiteArgs, connector: ConnectorId) -> Result<Vec<Entry>, String> {
    let mut entries: Vec<Entry> = Vec::new();
    let add_builtin = |scenario: Scenario, entries: &mut Vec<Entry>| {
        let source = format!("built-in {}", scenario.name);
        if entries.iter().all(|e| e.source != source) {
            let mut scenario = scenario;
            // Built-ins target connector 1; files say what they mean.
            scenario.retarget(Some(connector), None);
            entries.push(Entry { scenario, source });
        }
    };
    // Files already added, by canonical path: naming one twice (or a file and its directory)
    // runs it once.
    let mut seen_files: HashSet<PathBuf> = HashSet::new();
    for spec in &args.sources {
        // An existing path, or text that looks like one, beats a built-in of the same name.
        if !names_a_path(spec)
            && let Some(scenario) = builtin::get(spec)
        {
            add_builtin(scenario, &mut entries);
            continue;
        }
        let path = Path::new(spec);
        let files = match std::fs::metadata(path) {
            Ok(meta) if meta.is_dir() => {
                let files = toml_files(path)?;
                if files.is_empty() {
                    return Err(format!("no `*.toml` scenario files in `{spec}`"));
                }
                files
            }
            Ok(meta) if !meta.is_file() => {
                return Err(format!("`{spec}` is not a regular file or a directory"));
            }
            // Not a built-in, not path-like and not on disk: most likely a mistyped name.
            Err(_) if !names_a_path(spec) => return Err(unknown_scenario_message(spec)),
            // Any other missing file is reported by `Scenario::load`.
            _ => vec![path.to_path_buf()],
        };
        for file in files {
            let canonical = std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone());
            if !seen_files.insert(canonical) {
                continue;
            }
            let mut scenario = Scenario::load(&file).map_err(|e| e.to_string())?;
            scenario.retarget(None, None);
            entries.push(Entry {
                scenario,
                source: file.display().to_string(),
            });
        }
    }
    if args.all_builtins {
        for scenario in builtin::all() {
            if STATEFUL_BUILTINS.contains(&scenario.name.as_str()) {
                continue;
            }
            add_builtin(scenario, &mut entries);
        }
    }
    Ok(entries)
}

/// The `*.toml` files directly inside `dir`, sorted by file name.
pub(crate) fn toml_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    toml_files_noting(dir).map(|(files, _)| files)
}

/// The `*.toml` files of a directory, plus the `*.toml` entries that are not regular files
/// (dangling symlinks, directories) and were skipped.
pub(crate) fn toml_files_noting(dir: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let read =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    for item in read {
        let path = item
            .map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?
            .path();
        let is_toml = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
        if is_toml && path.is_file() {
            files.push(path);
        } else if is_toml {
            skipped.push(path);
        }
    }
    files.sort();
    skipped.sort();
    Ok((files, skipped))
}

/// Row names: the scenario name, with the source appended when two scenarios share a name.
fn unique_names(entries: &[Entry]) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for entry in entries {
        *counts.entry(entry.scenario.name.as_str()).or_default() += 1;
    }
    entries
        .iter()
        .map(|entry| {
            if counts
                .get(entry.scenario.name.as_str())
                .copied()
                .unwrap_or(0)
                > 1
            {
                format!("{} ({})", entry.scenario.name, entry.source)
            } else {
                entry.scenario.name.clone()
            }
        })
        .collect()
}

enum Signal {
    None,
    Int,
    Term,
}

/// Runs one scenario on a fresh station. `Err` is an exit code to return at once (the log
/// file cannot be opened).
async fn run_one(
    shutdown: &mut Shutdown,
    config: &StationConfig,
    entry: &Entry,
    overrides: &Vars,
    timeout: u64,
    json: bool,
    log_file: Option<&Path>,
) -> (Result<Outcome, ExitCode>, Signal) {
    let (handle, join) = Station::spawn(config.clone(), Arc::new(SystemClock));
    // Events go to stdout only in JSON mode or with `-v` (like `run`); the human form is the summary.
    let stdout = crate::headless::events_on_stdout(json);
    let sink = if stdout || log_file.is_some() {
        match spawn_sink(&handle, json, log_file, stdout) {
            Ok(sink) => Some(sink),
            Err(code) => {
                stop_station(&handle, join).await;
                return (Err(code), Signal::None);
            }
        }
    } else {
        None
    };
    let cancel = CancelToken::new();
    let limit = Duration::from_secs(timeout);
    let timer = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(limit).await;
            cancel.cancel();
        })
    };
    let mut signal = Signal::None;
    let result = tokio::select! {
        result = connect_and_run(&handle, &entry.scenario, &cancel, overrides, 1) => result,
        sigint = shutdown.recv() => {
            signal = if sigint { Signal::Int } else { Signal::Term };
            cancel.cancel();
            Err("interrupted".to_owned())
        }
    };
    timer.abort();
    stop_station(&handle, join).await;
    if let Some(sink) = sink {
        sink.finish().await;
    }
    let outcome = match result {
        Ok(_) if !matches!(signal, Signal::None) => Outcome::Error("interrupted".to_owned()),
        Ok(report) => {
            if json {
                print_report(&report, true, None);
            }
            Outcome::Done(report)
        }
        Err(error) => Outcome::Error(error),
    };
    (Ok(outcome), signal)
}

#[allow(clippy::too_many_lines)]
pub async fn run(
    mut config: StationConfig,
    args: &SuiteArgs,
    connector: ConnectorId,
    json: bool,
    log_file: Option<&Path>,
) -> ExitCode {
    if args.sources.is_empty() && !args.all_builtins {
        return usage_error(
            "`suite` needs at least one SOURCE (file, directory or built-in name) or --all-builtins",
        );
    }
    let entries = match load_entries(args, connector) {
        Ok(entries) => entries,
        Err(error) => return usage_error(&error),
    };
    let sources: Vec<&str> = entries.iter().map(|entry| entry.source.as_str()).collect();
    if let Err(error) = check_output_paths(args.report.as_deref(), args.junit.as_deref(), &sources)
    {
        return usage_error(&error);
    }
    let names = unique_names(&entries);
    config.persist = false;
    let overrides = override_vars(&args.vars);
    for entry in &entries {
        for (name, _, _) in &args.vars {
            if !entry.scenario.uses_variable(name) {
                eprintln!(
                    "warning: scenario `{}` does not use the variable `{name}` (--var ignored)",
                    entry.scenario.name
                );
            }
        }
    }
    let started = Instant::now();
    let started_at = now_rfc3339();
    // Created once: a signal between two scenarios is kept for the next check.
    let mut shutdown = Shutdown::new();

    let mut rows: Vec<Row> = Vec::new();
    let mut interrupted = false;
    let mut terminated = false;
    for (entry, name) in entries.iter().zip(&names) {
        if !terminated
            && !interrupted
            && let Some(sigint) = shutdown.recv().now_or_never()
        {
            if sigint {
                interrupted = true;
            } else {
                terminated = true;
            }
        }
        let stop = terminated || interrupted || (args.fail_fast && rows.iter().any(Row::failed));
        if stop {
            rows.push(Row {
                name: name.clone(),
                source: entry.scenario.source.clone(),
                outcome: Outcome::Skipped,
                started_at: None,
                duration_ms: 0,
            });
            continue;
        }
        let begun = Instant::now();
        let row_started_at = now_rfc3339();
        let (outcome, signal) = run_one(
            &mut shutdown,
            &config,
            entry,
            &overrides,
            args.timeout,
            json,
            log_file,
        )
        .await;
        match signal {
            Signal::None => {}
            Signal::Int => interrupted = true,
            Signal::Term => terminated = true,
        }
        let duration_ms = u64::try_from(begun.elapsed().as_millis()).unwrap_or(u64::MAX);
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(code) => return code,
        };
        if !json && crate::headless::level() != crate::headless::Level::Quiet {
            stdout_line(&human_line(name, &outcome, duration_ms), false);
        }
        rows.push(Row {
            name: name.clone(),
            source: entry.scenario.source.clone(),
            outcome,
            started_at: Some(row_started_at),
            duration_ms,
        });
    }
    let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let summary = summary_json(&rows, total_ms, &started_at);
    let mut write_failed = false;
    {
        // Files first: a closed stdout must not cost the machine-readable reports. They are
        // written after a signal too, for what finished (the interrupted scenario is an
        // error row, the unstarted ones are skipped).
        if let Some(path) = &args.report {
            let text = serde_json::to_string_pretty(&summary).unwrap_or_default() + "\n";
            if let Err(error) = write_atomic(path, text.as_bytes()) {
                eprintln!("error: cannot write the report: {error}");
                write_failed = true;
            }
        }
        if let Some(path) = &args.junit {
            let suites: Vec<JunitRun<'_>> = rows
                .iter()
                .filter_map(|row| {
                    let outcome = match &row.outcome {
                        Outcome::Done(report) => Ok(report),
                        Outcome::Error(error) => Err(error.as_str()),
                        Outcome::Skipped => return None,
                    };
                    Some(JunitRun {
                        suite: JunitSuite {
                            station: None,
                            scenario: &row.name,
                            outcome,
                        },
                        source: row.source.as_deref(),
                        duration_ms: row.duration_ms,
                    })
                })
                .collect();
            if let Err(error) = write_junit(path, &suites) {
                eprintln!("error: cannot write the JUnit report: {error}");
                write_failed = true;
            }
        }
    }
    if json {
        stdout_line(&summary.to_string(), true);
    } else {
        stdout_line(&summary_line(&rows), false);
    }
    if write_failed {
        ExitCode::FAILURE
    } else if interrupted {
        ExitCode::from(EXIT_INTERRUPTED)
    } else if summary["passed"] == true {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// A duration for the PASS line: milliseconds below one second, else seconds with one decimal.
fn seconds(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms} ms");
    }
    #[allow(clippy::cast_precision_loss, reason = "a display value")]
    let secs = ms as f64 / 1000.0;
    format!("{secs:.1} s")
}

fn human_line(name: &str, outcome: &Outcome, duration_ms: u64) -> String {
    match outcome {
        Outcome::Done(report) if report.passed => {
            format!("PASS {name} ({})", seconds(duration_ms))
        }
        Outcome::Done(report) => {
            let reason = report
                .steps
                .iter()
                .find_map(|step| match &step.status {
                    StepStatus::Failed(reason) => Some(format!("{} - {reason}", step.name)),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    if report.cancelled {
                        "timed out or cancelled".to_owned()
                    } else {
                        "failed".to_owned()
                    }
                });
            format!("FAIL {name}: {reason}")
        }
        Outcome::Error(error) => format!("FAIL {name}: {error}"),
        Outcome::Skipped => format!("SKIP {name}"),
    }
}

fn names_with(rows: &[Row], keep: impl Fn(&Row) -> bool) -> Vec<&str> {
    rows.iter()
        .filter(|row| keep(row))
        .map(|row| row.name.as_str())
        .collect()
}

fn summary_line(rows: &[Row]) -> String {
    let passed = rows.iter().filter(|r| r.status() == "passed").count();
    let mut line = format!("suite: {passed}/{} scenarios passed", rows.len());
    let failed = names_with(rows, Row::failed);
    if !failed.is_empty() {
        let _ = write!(line, ", FAILED: {}", failed.join(", "));
    }
    let skipped = names_with(rows, |r| matches!(r.outcome, Outcome::Skipped));
    if !skipped.is_empty() {
        let _ = write!(line, ", SKIPPED: {}", skipped.join(", "));
    }
    line
}

fn summary_json(rows: &[Row], duration_ms: u64, started_at: &str) -> Value {
    let passed_count = rows.iter().filter(|r| r.status() == "passed").count();
    let failed = names_with(rows, Row::failed);
    let failed_names = names_with(rows, |r| r.status() == "failed");
    let error_names = names_with(rows, |r| r.status() == "error");
    let skipped = names_with(rows, |r| matches!(r.outcome, Outcome::Skipped));
    let scenarios: Vec<Value> = rows
        .iter()
        .map(|row| {
            let ocpp_version = match &row.outcome {
                Outcome::Done(report) => report.ocpp_version.as_str(),
                _ => OCPP_VERSION,
            };
            let mut value = json!({
                "name": row.name,
                "scenario": row.name,
                "status": row.status(),
                "ocpp_version": ocpp_version,
                "duration_ms": row.duration_ms,
            });
            let started_at = match &row.outcome {
                Outcome::Done(report) => Some(&report.started_at),
                _ => row.started_at.as_ref(),
            };
            if let Some(started_at) = started_at {
                value["started_at"] = json!(started_at);
            }
            if let Some(source) = &row.source {
                value["source"] = json!(source);
            }
            match &row.outcome {
                Outcome::Done(report) => {
                    value["station"] = json!(report.station);
                    value["report"] = serde_json::to_value(report).unwrap_or_default();
                }
                Outcome::Error(error) => value["error"] = json!(error),
                Outcome::Skipped => {}
            }
            value
        })
        .collect();
    let count = |status: &str| rows.iter().filter(|r| r.status() == status).count();
    let passed = failed.is_empty() && skipped.is_empty();
    let status = if passed {
        "passed"
    } else if failed.is_empty() {
        "skipped"
    } else {
        "failed"
    };
    json!({
        "type": "suite_report",
        "schema": SCHEMA_VERSION,
        "ocpp_version": OCPP_VERSION,
        "tool_version": TOOL_VERSION,
        "started_at": started_at,
        "status": status,
        "counts": {
            "passed": count("passed"),
            "failed": count("failed"),
            "error": count("error"),
            "skipped": count("skipped"),
        },
        "passed": passed,
        "total": rows.len(),
        "passed_count": passed_count,
        "failed": failed,
        "failed_names": failed_names,
        "error_names": error_names,
        "skipped": skipped,
        "scenarios": scenarios,
        "duration_ms": duration_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_show_milliseconds_below_one_second() {
        assert_eq!(seconds(0), "0 ms");
        assert_eq!(seconds(34), "34 ms");
        assert_eq!(seconds(999), "999 ms");
        assert_eq!(seconds(1000), "1.0 s");
        assert_eq!(seconds(1234), "1.2 s");
    }

    fn row(name: &str, outcome: Outcome) -> Row {
        let started_at = (!matches!(outcome, Outcome::Skipped)).then(now_rfc3339);
        Row {
            name: name.to_owned(),
            source: None,
            outcome,
            started_at,
            duration_ms: 1,
        }
    }

    #[test]
    fn summary_arrays_agree_with_the_counts() {
        let passed = ScenarioReport::new("p".to_owned(), true, false, Vec::new(), 1);
        let failed = ScenarioReport::new("f".to_owned(), false, false, Vec::new(), 1);
        let rows = vec![
            row("p", Outcome::Done(passed)),
            row("f", Outcome::Done(failed)),
            row("e", Outcome::Error("no boot".to_owned())),
            row("s", Outcome::Skipped),
        ];
        let summary = summary_json(&rows, 5, "2026-01-01T00:00:00Z");
        let names = |key: &str| -> Vec<String> {
            summary[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(names("failed"), ["f", "e"]);
        assert_eq!(names("failed_names"), ["f"]);
        assert_eq!(names("error_names"), ["e"]);
        assert_eq!(names("skipped"), ["s"]);
        let counts = &summary["counts"];
        assert_eq!(counts["failed"], 1);
        assert_eq!(counts["error"], 1);
        assert_eq!(counts["skipped"], 1);
        assert_eq!(
            summary["failed"].as_array().unwrap().len() as u64,
            counts["failed"].as_u64().unwrap() + counts["error"].as_u64().unwrap()
        );
        for entry in summary["scenarios"].as_array().unwrap() {
            assert_eq!(entry["scenario"], entry["name"]);
            assert_eq!(
                entry["started_at"].is_string(),
                entry["status"] != "skipped"
            );
        }
    }
}
