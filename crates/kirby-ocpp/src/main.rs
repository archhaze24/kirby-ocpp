//! `kirby-ocpp`: terminal OCPP 1.6J charge point emulator.

mod cli;
mod fleet;
mod headless;
mod suite;

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use kirby_ocpp_core::fleet::Fleet;
use kirby_ocpp_core::ids::{ConnectorId, IdTag};
use kirby_ocpp_core::scenario::{
    CancelToken, JunitSuite, Scenario, ScenarioReport, StepStatus, Vars, builtin, junit_xml,
    run_scenario_with_vars,
};
use kirby_ocpp_core::{Station, StationConfig, StationHandle, SystemClock};
use kirby_ocpp_tui::stdout_pump::print_lossy;
use kirby_ocpp_tui::{TuiError, TuiOptions};
use tokio::task::JoinHandle;

use crate::cli::{Cli, Cmd, RunArgs, ScenariosCmd};
use crate::headless::Sink;

/// Project licence line and third-party notices (`--licenses`).
const LICENSES: &str = concat!(
    "kirby-ocpp is distributed under the MIT licence (see LICENSE in the source repository).\n\n",
    include_str!("../THIRD_PARTY_NOTICES.md")
);

/// Bad usage or configuration.
pub(crate) const EXIT_USAGE: u8 = 2;
/// `run` stopped by SIGINT (128 + 2, the shell convention).
pub(crate) const EXIT_INTERRUPTED: u8 = 130;

#[tokio::main]
async fn main() -> ExitCode {
    let code = real_main().await;
    // Queued stdout lines (events, the report) are written by a helper thread.
    headless::flush_stdout();
    code
}

async fn real_main() -> ExitCode {
    let mut cli = Cli::parse();
    if cli.licenses {
        print_lossy(LICENSES);
        return ExitCode::SUCCESS;
    }
    headless::init_stdout(cli.json);
    let command = cli.command.take();
    if let Some(Cmd::Scenarios { action }) = &command {
        return scenarios(action);
    }
    let (print_config, no_tui, json) = (cli.print_config, cli.no_tui, cli.json);
    let spec = cli.fleet_spec();
    let templated = cli.id_template.is_some();
    let log_file = cli.log_file.clone();
    let connector = ConnectorId::new(cli.connector);
    let mut config = match cli.into_config() {
        Ok(config) => config,
        Err(error) => return usage_error(&error),
    };
    // A fleet has more than one station. A single station keeps `--id` verbatim unless a
    // template is given explicitly.
    let is_fleet = spec.count > 1;
    if is_fleet || templated {
        match spec.ids() {
            Ok(ids) => config.charge_point_id.clone_from(&ids[0]),
            Err(error) => return usage_error(&error.to_string()),
        }
    }
    config.normalize();
    if let Err(error) = config.validate() {
        return usage_error(&error.to_string());
    }
    if is_fleet && let Err(error) = Fleet::configs(&config, &spec) {
        return usage_error(&error.to_string());
    }

    if print_config {
        // The URL may carry `user:password@`; never print credentials.
        let mut shown = config.clone();
        shown.url = kirby_ocpp_tui::text::redact_url(&shown.url);
        let printed = if is_fleet {
            serde_json::to_string_pretty(&serde_json::json!({
                "fleet": {
                    "id_template": spec.id_template,
                    "first_index": spec.first_index,
                    "count": spec.count,
                    "stagger_ms": spec.stagger_ms,
                },
                "config": shown,
            }))
        } else {
            serde_json::to_string_pretty(&shown)
        };
        return match printed {
            Ok(json) => {
                print_lossy(&format!("{json}\n"));
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        };
    }

    match command {
        Some(Cmd::Run(args)) if is_fleet => {
            fleet::run(config, spec, &args, connector, json, log_file.as_deref()).await
        }
        Some(Cmd::Run(args)) => run(config, &args, connector, json, log_file.as_deref()).await,
        Some(Cmd::Suite(_)) if is_fleet => usage_error(
            "`suite` runs one station per scenario; fleet mode (--count > 1) is not supported",
        ),
        Some(Cmd::Suite(args)) => {
            suite::run(config, &args, connector, json, log_file.as_deref()).await
        }
        Some(Cmd::Scenarios { .. }) => unreachable!("handled above"),
        None => {
            // `--json` is for machines: it never opens the terminal UI.
            let tui = !no_tui && !json && std::io::stdout().is_terminal();
            if is_fleet {
                return fleet::interactive(config, spec, tui, json, log_file.as_deref()).await;
            }
            interactive(config, tui, json, log_file.as_deref()).await
        }
    }
}

pub(crate) fn usage_error(error: &str) -> ExitCode {
    eprintln!("error: {error}");
    ExitCode::from(EXIT_USAGE)
}

fn scenarios(action: &ScenariosCmd) -> ExitCode {
    match action {
        ScenariosCmd::List => {
            for scenario in builtin::all() {
                print_lossy(&format!("{:<16} {}\n", scenario.name, scenario.description));
            }
            ExitCode::SUCCESS
        }
        ScenariosCmd::Show { name } => match builtin::source(name) {
            Some(text) => {
                print_lossy(text);
                ExitCode::SUCCESS
            }
            None => usage_error(&format!(
                "no built-in scenario `{name}` (try `kirby-ocpp scenarios list`)"
            )),
        },
    }
}

/// Does a scenario SOURCE argument name a file or directory rather than a built-in: it has a
/// path separator, ends in `.toml`, or exists on disk.
pub(crate) fn names_a_path(spec: &str) -> bool {
    spec.contains(['/', '\\'])
        || Path::new(spec)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        || Path::new(spec).exists()
}

/// Resolves `--scenario`: an existing path (or something that looks like one), else a
/// built-in name.
fn load_scenario(
    spec: &str,
    connector: ConnectorId,
    id_tag: Option<&str>,
    vars: &[(String, serde_json::Value)],
) -> Result<Scenario, String> {
    let id_tag = id_tag
        .map(IdTag::new)
        .transpose()
        .map_err(|e| e.to_string())?;
    let builtin = if names_a_path(spec) {
        None
    } else {
        builtin::get(spec)
    };
    let (mut scenario, is_builtin) = match builtin {
        Some(scenario) => (scenario, true),
        None => (
            Scenario::load(Path::new(spec)).map_err(|e| e.to_string())?,
            false,
        ),
    };
    // Built-ins target connector 1; files say what they mean.
    scenario.retarget(is_builtin.then_some(connector), id_tag.as_ref());
    if let Some((name, _)) = vars.iter().find(|(name, _)| !scenario.uses_variable(name)) {
        return Err(format!(
            "unknown variable `{name}`: scenario `{}` neither declares it in `[vars]` nor uses `${{{name}}}`",
            scenario.name
        ));
    }
    Ok(scenario)
}

/// Resolves on SIGINT or SIGTERM; `true` means SIGINT (Ctrl-C). Builds its streams per call,
/// so use [`Shutdown`] where a signal must not be lost between two waits.
pub(crate) async fn shutdown_signal() -> bool {
    Shutdown::new().recv().await
}

/// SIGINT/SIGTERM streams created once: a signal that arrives while nobody awaits is kept
/// and delivered by the next [`Shutdown::recv`].
pub(crate) struct Shutdown {
    #[cfg(unix)]
    int: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    term: Option<tokio::signal::unix::Signal>,
}

impl Shutdown {
    pub(crate) fn new() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Self {
                int: signal(SignalKind::interrupt()).ok(),
                term: signal(SignalKind::terminate()).ok(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    /// Resolves on the next signal; `true` means SIGINT (Ctrl-C).
    #[cfg_attr(not(unix), allow(clippy::unused_self))]
    pub(crate) async fn recv(&mut self) -> bool {
        #[cfg(unix)]
        {
            match (self.int.as_mut(), self.term.as_mut()) {
                (Some(int), Some(term)) => {
                    tokio::select! {
                        _ = int.recv() => true,
                        _ = term.recv() => false,
                    }
                }
                (Some(int), None) => {
                    int.recv().await;
                    true
                }
                (None, Some(term)) => {
                    term.recv().await;
                    false
                }
                (None, None) => {
                    let _ = tokio::signal::ctrl_c().await;
                    true
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            true
        }
    }
}

pub(crate) fn spawn_sink(
    handle: &StationHandle,
    json: bool,
    log_file: Option<&Path>,
    stdout: bool,
) -> Result<Sink, ExitCode> {
    Sink::spawn(handle, json, log_file, stdout).map_err(|error| {
        eprintln!("error: cannot open the log file: {error}");
        ExitCode::from(EXIT_USAGE)
    })
}

/// Closes the connection (WebSocket Close) and waits for the station to end.
pub(crate) async fn stop_station(handle: &StationHandle, join: JoinHandle<()>) {
    // The station may already be gone; either way we are leaving.
    let _ = handle.shutdown().await;
    let _ = join.await;
}

async fn interactive(
    config: StationConfig,
    tui: bool,
    json: bool,
    log_file: Option<&Path>,
) -> ExitCode {
    let (handle, join) = Station::spawn(config, Arc::new(SystemClock));
    // Under the terminal UI the screen is the UI's: `--log-file` still gets the events, stdout
    // does not.
    let mut sink = None;
    if tui {
        if log_file.is_some() {
            match spawn_sink(&handle, json, log_file, false) {
                Ok(started) => sink = Some(started),
                Err(code) => {
                    stop_station(&handle, join).await;
                    return code;
                }
            }
        }
        // Like the TS CLI: connect at startup (the station does not do it by itself).
        if let Err(error) = handle.connect().await {
            eprintln!("error: {error}");
        }
        match kirby_ocpp_tui::run(handle.clone(), TuiOptions::default()).await {
            Ok(()) => {
                stop_station(&handle, join).await;
                if let Some(sink) = sink {
                    sink.finish().await;
                }
                return ExitCode::SUCCESS;
            }
            Err(error @ TuiError::Terminal(_)) => {
                // The terminal could not be set up (no usable TTY): keep going headless.
                eprintln!("note: {error}; running headless (pass --no-tui to hide this)");
                if let Some(sink) = &sink {
                    sink.enable_stdout();
                }
            }
            Err(error) => {
                stop_station(&handle, join).await;
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
        None => match spawn_sink(&handle, json, log_file, true) {
            Ok(sink) => sink,
            Err(code) => {
                stop_station(&handle, join).await;
                return code;
            }
        },
    };
    if !tui && let Err(error) = handle.connect().await {
        eprintln!("error: {error}");
    }
    let _ = shutdown_signal().await;
    stop_station(&handle, join).await;
    sink.finish().await;
    ExitCode::SUCCESS
}

async fn run(
    config: StationConfig,
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
    let (handle, join) = Station::spawn(config, Arc::new(SystemClock));
    let sink = match spawn_sink(&handle, json, log_file, true) {
        Ok(sink) => sink,
        Err(code) => {
            stop_station(&handle, join).await;
            return code;
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

    let overrides = override_vars(&args.vars);
    let mut interrupted = false;
    let outcome = tokio::select! {
        outcome = connect_and_run(&handle, &scenario, &cancel, &overrides, 1) => outcome,
        sigint = shutdown_signal() => {
            interrupted = sigint;
            cancel.cancel();
            Err("interrupted".to_owned())
        }
    };
    timer.abort();
    stop_station(&handle, join).await;
    sink.finish().await;

    let (report, setup_error) = match outcome {
        Ok(report) => (Some(report), None),
        Err(error) => (None, Some(error)),
    };
    if let Some(error) = &setup_error {
        eprintln!("error: {error}");
    }
    let mut write_failed = false;
    // Files first: a closed stdout must not cost the machine-readable reports.
    if let Some(report) = &report
        && let Some(path) = &args.report
        && let Err(error) = write_report(path, report)
    {
        eprintln!("error: cannot write the report: {error}");
        write_failed = true;
    }
    if !interrupted && let Some(path) = &args.junit {
        let suite = JunitSuite {
            station: None,
            scenario: &scenario.name,
            outcome: report
                .as_ref()
                .ok_or_else(|| setup_error.as_deref().unwrap_or("no report")),
        };
        if let Err(error) = write_junit(path, &[suite]) {
            eprintln!("error: cannot write the JUnit report: {error}");
            write_failed = true;
        }
    }
    if let Some(report) = &report {
        print_report(report, json, None);
    }
    if write_failed {
        ExitCode::FAILURE
    } else if interrupted {
        ExitCode::from(EXIT_INTERRUPTED)
    } else if report.is_some_and(|r| r.passed) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

pub(crate) async fn connect_and_run(
    handle: &StationHandle,
    scenario: &Scenario,
    cancel: &CancelToken,
    overrides: &Vars,
    index: u64,
) -> Result<ScenarioReport, String> {
    handle.connect().await.map_err(|e| e.to_string())?;
    let mut snapshots = handle.watch_snapshot();
    tokio::select! {
        registered = snapshots.wait_for(kirby_ocpp_core::station::Snapshot::booted) => {
            registered.map_err(|_| "the station stopped".to_owned())?;
        }
        () = cancel.cancelled() => {
            return Err("timed out waiting for the central system to accept the BootNotification".to_owned());
        }
    }
    Ok(run_scenario_with_vars(handle, scenario, cancel, None, overrides, index).await)
}

/// The `--var` overrides of a `run` or `suite` command.
pub(crate) fn override_vars(vars: &[(String, serde_json::Value)]) -> Vars {
    let mut out = Vars::new();
    for (name, value) in vars {
        // Names were validated by the argument parser.
        let _ = out.insert(name, value.clone());
    }
    out
}

/// Prints a scenario report; `station` (fleet mode) tags every line.
pub(crate) fn print_report(report: &ScenarioReport, json: bool, station: Option<&str>) {
    if json {
        let mut value = serde_json::to_value(report).unwrap_or_default();
        if let Some(object) = value.as_object_mut() {
            object.insert("type".to_owned(), "report".into());
            if let Some(station) = station {
                object.insert("station".to_owned(), station.into());
            }
        }
        headless::stdout_line(&value.to_string(), true);
        return;
    }
    let say = |line: String| headless::stdout_line(&headless::tagged(&line, station), false);
    say(format!("scenario {}", report.scenario));
    for step in &report.steps {
        let (mark, detail) = match &step.status {
            StepStatus::Passed => ("PASS", String::new()),
            StepStatus::Failed(reason) => ("FAIL", format!(": {reason}")),
            StepStatus::Skipped => ("SKIP", String::new()),
        };
        say(format!(
            "  {mark} {:>2} {} ({} ms){detail}",
            step.index + 1,
            step.name,
            step.duration_ms
        ));
    }
    say(format!(
        "{} in {} ms",
        if report.passed { "PASSED" } else { "FAILED" },
        report.duration_ms
    ));
}

pub(crate) fn write_junit(path: &Path, suites: &[JunitSuite<'_>]) -> std::io::Result<()> {
    std::fs::write(path, junit_xml(suites))
}

fn write_report(path: &Path, report: &ScenarioReport) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(report).map_err(std::io::Error::other)?;
    std::fs::write(path, text + "\n")
}
