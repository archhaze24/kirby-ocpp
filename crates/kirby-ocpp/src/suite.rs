//! `kirby-ocpp suite`: several scenarios, each on a fresh station.
//!
//! Every scenario gets its own [`Station`] with persistence switched off (`--state-dir` and
//! `--no-persist` are ignored), so no state leaks from one scenario into the next and nothing
//! is written to disk. Scenarios run one after another, or `--jobs` at a time; a failed
//! scenario is run again up to `--retries` times, each attempt on a new station.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::scenario::{
    CancelToken, JunitRetries, JunitRun, JunitSuite, OCPP_VERSION, SCHEMA_VERSION, Scenario,
    ScenarioReport, StepStatus, TOOL_VERSION, Vars, builtin, junit_xml_retried, now_rfc3339,
};
use kirby_ocpp_core::{Station, StationConfig, SystemClock};
use serde_json::{Value, json};

use crate::cli::SuiteArgs;
use crate::headless::stdout_line;
use crate::suite_ci::{self, CiRow, Selection};
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
    /// RFC 3339 UTC start time of the first attempt; `None` for a scenario that never started.
    started_at: Option<String>,
    /// Duration of the final attempt.
    duration_ms: u64,
    /// The failed attempts that a retry followed, oldest first.
    earlier: Vec<Earlier>,
}

/// A failed attempt of a scenario that was run again.
struct Earlier {
    outcome: Outcome,
    duration_ms: u64,
}

fn outcome_status(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Done(report) if report.passed => "passed",
        Outcome::Done(_) => "failed",
        Outcome::Error(_) => "error",
        Outcome::Skipped => "skipped",
    }
}

/// Why an attempt failed, in one line; `None` for a pass or a scenario that did not run.
fn failure_reason(outcome: &Outcome) -> Option<String> {
    match outcome {
        Outcome::Done(report) if !report.passed => Some(
            report
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
                }),
        ),
        Outcome::Error(error) => Some(error.clone()),
        _ => None,
    }
}

impl Row {
    /// A row for a scenario that never started.
    fn skipped(name: &str, source: Option<String>) -> Self {
        Self {
            name: name.to_owned(),
            source,
            outcome: Outcome::Skipped,
            started_at: None,
            duration_ms: 0,
            earlier: Vec::new(),
        }
    }

    /// Attempts made: 0 for a scenario that never started.
    fn attempts(&self) -> u32 {
        if matches!(self.outcome, Outcome::Skipped) {
            0
        } else {
            u32::try_from(self.earlier.len())
                .unwrap_or(u32::MAX)
                .saturating_add(1)
        }
    }

    /// Passed after at least one failed attempt.
    fn flaky(&self) -> bool {
        self.status() == "passed" && !self.earlier.is_empty()
    }

    fn ci(&self) -> CiRow {
        let file = self
            .source
            .as_deref()
            .filter(|source| !source.starts_with("builtin:"))
            .map(str::to_owned);
        // A flaky pass explains its first failure; a failure explains itself.
        let message = failure_reason(&self.outcome).or_else(|| {
            self.earlier
                .first()
                .and_then(|earlier| failure_reason(&earlier.outcome))
        });
        CiRow {
            name: self.name.clone(),
            file,
            status: self.status(),
            duration_ms: self.duration_ms,
            attempts: self.attempts(),
            flaky: self.flaky(),
            message,
        }
    }

    fn failed(&self) -> bool {
        match &self.outcome {
            Outcome::Done(report) => !report.passed,
            Outcome::Error(_) => true,
            Outcome::Skipped => false,
        }
    }

    fn status(&self) -> &'static str {
        outcome_status(&self.outcome)
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

/// What every worker of a suite shares.
struct Ctx<'a> {
    config: &'a StationConfig,
    entries: &'a [Entry],
    names: &'a [String],
    overrides: &'a Vars,
    args: &'a SuiteArgs,
    json: bool,
    log_file: Option<&'a Path>,
    /// Cancelled by a signal (or a fatal error): running scenarios end as errors, the rest is skipped.
    abort: CancelToken,
    /// Index of the next scenario to start.
    next: AtomicUsize,
    /// A scenario has failed for good (for `--fail-fast`).
    failed: AtomicBool,
    /// An exit code to return at once (the log file cannot be opened).
    fatal: Mutex<Option<ExitCode>>,
    /// One slot per scenario, in selection order; `None` is a scenario that never started.
    rows: Mutex<Vec<Option<Row>>>,
}

/// Runs one scenario on a fresh station. `Err` is an exit code to return at once (the log
/// file cannot be opened).
async fn run_one(
    ctx: &Ctx<'_>,
    config: &StationConfig,
    entry: &Entry,
) -> Result<Outcome, ExitCode> {
    let (handle, join) = Station::spawn(config.clone(), Arc::new(SystemClock));
    // Events go to stdout only in JSON mode or with `-v` (like `run`); the human form is the summary.
    let stdout = crate::headless::events_on_stdout(ctx.json);
    let sink = if stdout || ctx.log_file.is_some() {
        match spawn_sink(&handle, ctx.json, ctx.log_file, stdout) {
            Ok(sink) => Some(sink),
            Err(code) => {
                stop_station(&handle, join).await;
                return Err(code);
            }
        }
    } else {
        None
    };
    let cancel = CancelToken::new();
    let limit = Duration::from_secs(ctx.args.timeout);
    let timer = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(limit).await;
            cancel.cancel();
        })
    };
    let result = tokio::select! {
        result = connect_and_run(&handle, &entry.scenario, &cancel, ctx.overrides, 1) => result,
        () = ctx.abort.cancelled() => {
            cancel.cancel();
            Err("interrupted".to_owned())
        }
    };
    timer.abort();
    stop_station(&handle, join).await;
    if let Some(sink) = sink {
        sink.finish().await;
    }
    Ok(match result {
        Ok(_) if ctx.abort.is_cancelled() => Outcome::Error("interrupted".to_owned()),
        Ok(report) => {
            if ctx.json {
                print_report(&report, true, None);
            }
            Outcome::Done(report)
        }
        Err(error) => Outcome::Error(error),
    })
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One job slot: takes scenarios off the shared queue until it is empty. Slot 0 uses the
/// station id as given, slot N (from 1) appends `-{N+1}`, so parallel scenarios never share
/// an id.
async fn worker(slot: u32, ctx: &Ctx<'_>) {
    let mut config = ctx.config.clone();
    if slot > 0 {
        config.charge_point_id = format!("{}-{}", config.charge_point_id, slot + 1);
    }
    let progress = !ctx.json && crate::headless::level() != crate::headless::Level::Quiet;
    loop {
        let index = ctx.next.fetch_add(1, Ordering::SeqCst);
        let (Some(entry), Some(name)) = (ctx.entries.get(index), ctx.names.get(index)) else {
            return;
        };
        if ctx.abort.is_cancelled() || (ctx.args.fail_fast && ctx.failed.load(Ordering::SeqCst)) {
            // Left empty: reported as skipped.
            continue;
        }
        let first_started_at = now_rfc3339();
        let mut earlier: Vec<Earlier> = Vec::new();
        let (outcome, duration_ms) = loop {
            let begun = Instant::now();
            let outcome = match run_one(ctx, &config, entry).await {
                Ok(outcome) => outcome,
                Err(code) => {
                    *lock(&ctx.fatal) = Some(code);
                    ctx.abort.cancel();
                    return;
                }
            };
            let duration_ms = u64::try_from(begun.elapsed().as_millis()).unwrap_or(u64::MAX);
            let failed = matches!(outcome_status(&outcome), "failed" | "error");
            let used = u32::try_from(earlier.len()).unwrap_or(u32::MAX);
            if failed && used < ctx.args.retries && !ctx.abort.is_cancelled() {
                if progress {
                    let reason = failure_reason(&outcome).unwrap_or_default();
                    stdout_line(
                        &format!(
                            "RETRY {name}: {reason} (attempt {} of {})",
                            used + 1,
                            ctx.args.retries + 1
                        ),
                        false,
                    );
                }
                earlier.push(Earlier {
                    outcome,
                    duration_ms,
                });
                continue;
            }
            break (outcome, duration_ms);
        };
        let row = Row {
            name: name.clone(),
            source: entry.scenario.source.clone(),
            outcome,
            started_at: Some(first_started_at),
            duration_ms,
            earlier,
        };
        if row.failed() {
            ctx.failed.store(true, Ordering::SeqCst);
        }
        if progress {
            stdout_line(&human_line(&row), false);
        }
        if let Some(slot) = lock(&ctx.rows).get_mut(index) {
            *slot = Some(row);
        }
    }
}

/// Why a suite finished without running anything, or `None`.
fn selection_error(selected: usize, loaded: usize) -> Option<String> {
    (selected == 0).then(|| {
        format!(
            "the selection (--filter, --skip, --tag, --skip-tag) matches none of the {loaded} scenarios"
        )
    })
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
    if json && args.github_annotations {
        return usage_error(
            "--github-annotations prints workflow commands to stdout and cannot be combined with --json",
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
    if let Some(error) = summary_path_error(args, &sources) {
        return usage_error(&error);
    }
    let loaded = entries.len();
    let selection = Selection {
        filter: &args.filter,
        skip: &args.skip,
        tag: &args.tag,
        skip_tag: &args.skip_tag,
    };
    let entries: Vec<Entry> = if selection.is_active() {
        entries
            .into_iter()
            .filter(|entry| selection.selects(&entry.scenario.name, &entry.scenario.tags))
            .collect()
    } else {
        entries
    };
    if let Some(error) = selection_error(entries.len(), loaded) {
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

    let ctx = Ctx {
        config: &config,
        entries: &entries,
        names: &names,
        overrides: &overrides,
        args,
        json,
        log_file,
        abort: CancelToken::new(),
        next: AtomicUsize::new(0),
        failed: AtomicBool::new(false),
        fatal: Mutex::new(None),
        rows: Mutex::new((0..entries.len()).map(|_| None).collect()),
    };
    let jobs = u32::try_from(entries.len())
        .unwrap_or(u32::MAX)
        .min(args.jobs.max(1));
    let mut interrupted = false;
    {
        let workers = futures_util::future::join_all((0..jobs).map(|slot| worker(slot, &ctx)));
        tokio::pin!(workers);
        loop {
            tokio::select! {
                _ = &mut workers => break,
                sigint = shutdown.recv() => {
                    // SIGTERM only stops the suite; SIGINT also changes the exit code.
                    if sigint {
                        interrupted = true;
                    }
                    ctx.abort.cancel();
                }
            }
        }
    }
    if let Some(code) = *lock(&ctx.fatal) {
        return code;
    }
    let slots = std::mem::take(&mut *lock(&ctx.rows));
    let rows: Vec<Row> = slots
        .into_iter()
        .zip(entries.iter().zip(&names))
        .map(|(slot, (entry, name))| {
            slot.unwrap_or_else(|| Row::skipped(name, entry.scenario.source.clone()))
        })
        .collect();
    let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let summary = summary_json(&rows, total_ms, &started_at, args.retries);
    let ci_rows: Vec<CiRow> = rows.iter().map(Row::ci).collect();
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
        if let Some(path) = &args.junit
            && let Err(error) = write_suite_junit(path, &rows, args.retries)
        {
            eprintln!("error: cannot write the JUnit report: {error}");
            write_failed = true;
        }
        write_failed |= write_summary_md(args, &suite_ci::summary_markdown(&ci_rows, total_ms));
    }
    if json {
        stdout_line(&summary.to_string(), true);
    } else {
        if args.github_annotations {
            for line in suite_ci::annotations(&ci_rows) {
                stdout_line(&line, false);
            }
        }
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

/// An error when `--summary-md` names a file the suite also reads or writes.
fn summary_path_error(args: &SuiteArgs, sources: &[&str]) -> Option<String> {
    let path = args.summary_md.as_deref()?;
    if path.is_dir() {
        return Some(format!("--summary-md {}: is a directory", path.display()));
    }
    let real = |p: &Path| std::fs::canonicalize(p).ok();
    let mine = real(path)?;
    let clash = sources
        .iter()
        .any(|source| real(Path::new(source)).is_some_and(|other| other == mine))
        || [args.report.as_deref(), args.junit.as_deref()]
            .into_iter()
            .flatten()
            .any(|other| real(other).is_some_and(|other| other == mine));
    clash.then(|| {
        format!(
            "--summary-md {}: is another file of this suite; it would be overwritten",
            path.display()
        )
    })
}

/// Writes the Markdown summary to `--summary-md` and appends it to `$GITHUB_STEP_SUMMARY`.
/// Returns whether the explicitly requested file could not be written.
fn write_summary_md(args: &SuiteArgs, markdown: &str) -> bool {
    use std::io::Write as _;
    let mut failed = false;
    if let Some(path) = &args.summary_md
        && let Err(error) = write_atomic(path, markdown.as_bytes())
    {
        eprintln!("error: cannot write the summary: {error}");
        failed = true;
    }
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY").filter(|path| !path.is_empty()) {
        let appended = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .and_then(|mut file| file.write_all(format!("{markdown}\n").as_bytes()));
        if let Err(error) = appended {
            eprintln!("warning: cannot append to $GITHUB_STEP_SUMMARY: {error}");
        }
    }
    failed
}

/// Writes the JUnit report: with `--retries` the retried suites carry their earlier attempts.
fn write_suite_junit(path: &Path, rows: &[Row], retries: u32) -> std::io::Result<()> {
    fn suite_of(row: &Row) -> Option<JunitRun<'_>> {
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
    }
    if retries == 0 {
        let suites: Vec<JunitRun<'_>> = rows.iter().filter_map(suite_of).collect();
        return write_junit(path, &suites);
    }
    let earlier: Vec<Vec<Result<&ScenarioReport, &str>>> = rows
        .iter()
        .map(|row| {
            row.earlier
                .iter()
                .filter_map(|attempt| match &attempt.outcome {
                    Outcome::Done(report) => Some(Ok(report)),
                    Outcome::Error(error) => Some(Err(error.as_str())),
                    Outcome::Skipped => None,
                })
                .collect()
        })
        .collect();
    let runs: Vec<(JunitRun<'_>, JunitRetries<'_>)> = rows
        .iter()
        .zip(&earlier)
        .filter_map(|(row, earlier)| {
            Some((
                suite_of(row)?,
                JunitRetries {
                    attempts: row.attempts(),
                    earlier,
                },
            ))
        })
        .collect();
    write_atomic(path, junit_xml_retried(&runs).as_bytes())
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

fn human_line(row: &Row) -> String {
    let name = &row.name;
    let attempts = row.attempts();
    match &row.outcome {
        Outcome::Done(report) if report.passed => {
            if row.flaky() {
                format!(
                    "PASS {name} ({}, flaky: passed on attempt {attempts})",
                    seconds(row.duration_ms)
                )
            } else {
                format!("PASS {name} ({})", seconds(row.duration_ms))
            }
        }
        Outcome::Skipped => format!("SKIP {name}"),
        outcome => {
            let reason = failure_reason(outcome).unwrap_or_default();
            if attempts > 1 {
                format!("FAIL {name}: {reason} (after {attempts} attempts)")
            } else {
                format!("FAIL {name}: {reason}")
            }
        }
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
    let flaky = names_with(rows, Row::flaky);
    if !flaky.is_empty() {
        let _ = write!(line, ", FLAKY: {}", flaky.join(", "));
    }
    let skipped = names_with(rows, |r| matches!(r.outcome, Outcome::Skipped));
    if !skipped.is_empty() {
        let _ = write!(line, ", SKIPPED: {}", skipped.join(", "));
    }
    line
}

/// The `attempt_results` of a scenario: every attempt, oldest first.
fn attempt_results(row: &Row) -> Vec<Value> {
    let last =
        (!matches!(row.outcome, Outcome::Skipped)).then_some((&row.outcome, row.duration_ms));
    row.earlier
        .iter()
        .map(|attempt| (&attempt.outcome, attempt.duration_ms))
        .chain(last)
        .enumerate()
        .map(|(position, (outcome, duration_ms))| {
            let mut value = json!({
                "attempt": position + 1,
                "status": outcome_status(outcome),
                "duration_ms": duration_ms,
            });
            if let Some(message) = failure_reason(outcome) {
                value["message"] = json!(message);
            }
            value
        })
        .collect()
}

fn summary_json(rows: &[Row], duration_ms: u64, started_at: &str, retries: u32) -> Value {
    let passed_count = rows.iter().filter(|r| r.status() == "passed").count();
    let failed = names_with(rows, Row::failed);
    let failed_names = names_with(rows, |r| r.status() == "failed");
    let error_names = names_with(rows, |r| r.status() == "error");
    let skipped = names_with(rows, |r| matches!(r.outcome, Outcome::Skipped));
    let flaky_names = names_with(rows, Row::flaky);
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
                "attempts": row.attempts(),
                "flaky": row.flaky(),
                "attempt_results": attempt_results(row),
            });
            let started_at = match &row.outcome {
                Outcome::Done(report) if row.earlier.is_empty() => Some(&report.started_at),
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
        "flaky_names": flaky_names,
        "retries": retries,
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
            earlier: Vec::new(),
        }
    }

    fn done(name: &str, passed: bool) -> Outcome {
        Outcome::Done(ScenarioReport::new(
            name.to_owned(),
            passed,
            false,
            Vec::new(),
            1,
        ))
    }

    #[test]
    fn summary_arrays_agree_with_the_counts() {
        let rows = vec![
            row("p", done("p", true)),
            row("f", done("f", false)),
            row("e", Outcome::Error("no boot".to_owned())),
            row("s", Outcome::Skipped),
        ];
        let summary = summary_json(&rows, 5, "2026-01-01T00:00:00Z", 0);
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
        assert_eq!(summary["flaky_names"], json!([]));
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

    #[test]
    fn retry_bookkeeping_marks_a_pass_after_a_failure_as_flaky() {
        let mut flaky = row("w", done("w", true));
        flaky.earlier = vec![
            Earlier {
                outcome: Outcome::Error("no boot".to_owned()),
                duration_ms: 7,
            },
            Earlier {
                outcome: done("w", false),
                duration_ms: 9,
            },
        ];
        let mut stubborn = row("s", done("s", false));
        stubborn.earlier = vec![Earlier {
            outcome: done("s", false),
            duration_ms: 3,
        }];
        let plain = row("p", done("p", true));
        let skipped = row("k", Outcome::Skipped);
        assert_eq!(flaky.attempts(), 3);
        assert!(flaky.flaky() && !flaky.failed());
        assert_eq!(flaky.status(), "passed", "status is the last attempt's");
        assert!(!stubborn.flaky() && stubborn.failed());
        assert_eq!(stubborn.attempts(), 2);
        assert!(!plain.flaky());
        assert_eq!((plain.attempts(), skipped.attempts()), (1, 0));
        assert_eq!(flaky.ci().message.as_deref(), Some("no boot"));

        let rows = vec![flaky, stubborn, plain, skipped];
        let summary = summary_json(&rows, 5, "2026-01-01T00:00:00Z", 2);
        assert_eq!(summary["flaky_names"], json!(["w"]));
        assert_eq!(summary["retries"], 2);
        assert_eq!(
            summary["counts"]["passed"], 2,
            "a flaky pass counts as passed"
        );
        let scenarios = summary["scenarios"].as_array().unwrap();
        assert_eq!(scenarios[0]["attempts"], 3);
        assert_eq!(scenarios[0]["flaky"], true);
        let results = scenarios[0]["attempt_results"].as_array().unwrap();
        let statuses: Vec<&str> = results
            .iter()
            .map(|r| r["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, ["error", "failed", "passed"]);
        assert_eq!(results[0]["message"], "no boot");
        assert_eq!(results[2].get("message"), None);
        assert_eq!(scenarios[1]["flaky"], false);
        assert_eq!(scenarios[3]["attempt_results"], json!([]));
        let line = summary_line(&rows);
        assert!(line.contains(", FAILED: s, FLAKY: w, SKIPPED: k"), "{line}");
        assert_eq!(
            human_line(&rows[0]),
            "PASS w (1 ms, flaky: passed on attempt 3)"
        );
        assert_eq!(human_line(&rows[1]), "FAIL s: failed (after 2 attempts)");
    }

    #[test]
    fn an_empty_selection_is_an_error() {
        assert!(
            selection_error(0, 3)
                .unwrap()
                .contains("matches none of the 3")
        );
        assert!(selection_error(1, 3).is_none());
    }
}
