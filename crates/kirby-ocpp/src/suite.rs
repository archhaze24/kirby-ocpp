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
    CancelToken, JunitSuite, Scenario, ScenarioReport, StepStatus, Vars, builtin,
};
use kirby_ocpp_core::{Station, StationConfig, SystemClock};
use serde_json::{Value, json};

use crate::cli::SuiteArgs;
use crate::headless::stdout_line;
use crate::{
    EXIT_INTERRUPTED, Shutdown, connect_and_run, names_a_path, override_vars, print_report,
    spawn_sink, stop_station, usage_error, write_junit,
};

/// Built-ins that need a transaction left by an earlier run; they cannot pass on a fresh
/// station, so `--all-builtins` leaves them out (name one explicitly to run it anyway).
const STATEFUL_BUILTINS: &[&str] = &["finish-session"];

/// One scenario of the suite and where it came from.
struct Entry {
    scenario: Scenario,
    /// Built-in name or file path, used to tell apart equal scenario names.
    source: String,
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
    outcome: Outcome,
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
fn load_entries(args: &SuiteArgs, connector: ConnectorId) -> Result<Vec<Entry>, String> {
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
            // A missing file is reported by `Scenario::load`.
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
fn toml_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let read =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    let mut files = Vec::new();
    for item in read {
        let path = item
            .map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?
            .path();
        let is_toml = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
        if is_toml && path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
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
    // Events go to stdout only in JSON mode (like `run`); the human form is the summary.
    let sink = if json || log_file.is_some() {
        match spawn_sink(&handle, json, log_file, json) {
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
    let names = unique_names(&entries);
    config.persist = false;
    let overrides = override_vars(&args.vars);
    for entry in &entries {
        for (name, _) in &args.vars {
            if !entry.scenario.uses_variable(name) {
                eprintln!(
                    "warning: scenario `{}` does not use the variable `{name}` (--var ignored)",
                    entry.scenario.name
                );
            }
        }
    }
    let started = Instant::now();
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
                outcome: Outcome::Skipped,
                duration_ms: 0,
            });
            continue;
        }
        let begun = Instant::now();
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
        if !json {
            stdout_line(&human_line(name, &outcome, duration_ms), false);
        }
        rows.push(Row {
            name: name.clone(),
            outcome,
            duration_ms,
        });
    }
    let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let summary = summary_json(&rows, total_ms);
    let mut write_failed = false;
    {
        // Files first: a closed stdout must not cost the machine-readable reports. They are
        // written after a signal too, for what finished (the interrupted scenario is an
        // error row, the unstarted ones are skipped).
        if let Some(path) = &args.report {
            let text = serde_json::to_string_pretty(&summary).unwrap_or_default() + "\n";
            if let Err(error) = std::fs::write(path, text) {
                eprintln!("error: cannot write the report: {error}");
                write_failed = true;
            }
        }
        if let Some(path) = &args.junit {
            let suites: Vec<JunitSuite<'_>> = rows
                .iter()
                .filter_map(|row| match &row.outcome {
                    Outcome::Done(report) => Some(JunitSuite {
                        station: None,
                        scenario: &row.name,
                        outcome: Ok(report),
                    }),
                    Outcome::Error(error) => Some(JunitSuite {
                        station: None,
                        scenario: &row.name,
                        outcome: Err(error),
                    }),
                    Outcome::Skipped => None,
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

fn seconds(ms: u64) -> String {
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

fn summary_json(rows: &[Row], duration_ms: u64) -> Value {
    let passed_count = rows.iter().filter(|r| r.status() == "passed").count();
    let failed = names_with(rows, Row::failed);
    let skipped = names_with(rows, |r| matches!(r.outcome, Outcome::Skipped));
    let scenarios: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut value = json!({
                "name": row.name,
                "status": row.status(),
                "duration_ms": row.duration_ms,
            });
            match &row.outcome {
                Outcome::Done(report) => {
                    value["report"] = serde_json::to_value(report).unwrap_or_default();
                }
                Outcome::Error(error) => value["error"] = json!(error),
                Outcome::Skipped => {}
            }
            value
        })
        .collect();
    json!({
        "type": "suite_report",
        "passed": failed.is_empty() && skipped.is_empty(),
        "total": rows.len(),
        "passed_count": passed_count,
        "failed": failed,
        "skipped": skipped,
        "scenarios": scenarios,
        "duration_ms": duration_ms,
    })
}
