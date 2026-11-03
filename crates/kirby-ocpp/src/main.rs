//! `kirby-ocpp`: terminal OCPP 1.6J charge point emulator.

mod cli;
mod fleet;
mod headless;

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use kirby_ocpp_core::fleet::Fleet;
use kirby_ocpp_core::ids::{ConnectorId, IdTag};
use kirby_ocpp_core::scenario::{
    CancelToken, JunitSuite, Scenario, ScenarioReport, StepStatus, builtin, junit_xml, run_scenario,
};
use kirby_ocpp_core::{Station, StationConfig, StationHandle, SystemClock};
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
const EXIT_USAGE: u8 = 2;
/// `run` stopped by SIGINT (128 + 2, the shell convention).
const EXIT_INTERRUPTED: u8 = 130;

#[tokio::main]
async fn main() -> ExitCode {
    let mut cli = Cli::parse();
    if cli.licenses {
        print!("{LICENSES}");
        return ExitCode::SUCCESS;
    }
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
                println!("{json}");
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
        Some(Cmd::Scenarios { .. }) => unreachable!("handled above"),
        None => {
            let tui = !no_tui && std::io::stdout().is_terminal();
            if is_fleet {
                return fleet::interactive(config, spec, tui, json, log_file.as_deref()).await;
            }
            interactive(config, tui, json, log_file.as_deref()).await
        }
    }
}

fn usage_error(error: &str) -> ExitCode {
    eprintln!("error: {error}");
    ExitCode::from(EXIT_USAGE)
}

fn scenarios(action: &ScenariosCmd) -> ExitCode {
    match action {
        ScenariosCmd::List => {
            for scenario in builtin::all() {
                println!("{:<16} {}", scenario.name, scenario.description);
            }
            ExitCode::SUCCESS
        }
        ScenariosCmd::Show { name } => match builtin::source(name) {
            Some(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            None => usage_error(&format!(
                "no built-in scenario `{name}` (try `kirby-ocpp scenarios list`)"
            )),
        },
    }
}

/// Resolves `--scenario`: a built-in name, else a file.
fn load_scenario(
    spec: &str,
    connector: ConnectorId,
    id_tag: Option<&str>,
) -> Result<Scenario, String> {
    let id_tag = id_tag
        .map(IdTag::new)
        .transpose()
        .map_err(|e| e.to_string())?;
    let (mut scenario, is_builtin) = match builtin::get(spec) {
        Some(scenario) => (scenario, true),
        None => (
            Scenario::load(Path::new(spec)).map_err(|e| e.to_string())?,
            false,
        ),
    };
    // Built-ins target connector 1; files say what they mean.
    scenario.retarget(is_builtin.then_some(connector), id_tag.as_ref());
    Ok(scenario)
}

/// Resolves on SIGINT or SIGTERM; `true` means SIGINT (Ctrl-C).
async fn shutdown_signal() -> bool {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => true,
                _ = term.recv() => false,
            }
        } else {
            let _ = tokio::signal::ctrl_c().await;
            true
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        true
    }
}

fn spawn_sink(
    handle: &StationHandle,
    json: bool,
    log_file: Option<&Path>,
) -> Result<Sink, ExitCode> {
    Sink::spawn(handle, json, log_file).map_err(|error| {
        eprintln!("error: cannot open the log file: {error}");
        ExitCode::from(EXIT_USAGE)
    })
}

/// Closes the connection (WebSocket Close) and waits for the station to end.
async fn stop_station(handle: &StationHandle, join: JoinHandle<()>) {
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
    if tui {
        // Like the TS CLI: connect at startup (the station does not do it by itself).
        if let Err(error) = handle.connect().await {
            eprintln!("error: {error}");
        }
        match kirby_ocpp_tui::run(handle.clone(), TuiOptions::default()).await {
            Ok(()) => {
                stop_station(&handle, join).await;
                return ExitCode::SUCCESS;
            }
            Err(error @ TuiError::Terminal(_)) => {
                // The terminal could not be set up (no usable TTY): keep going headless.
                eprintln!("note: {error}; running headless (pass --no-tui to hide this)");
            }
            Err(error) => {
                stop_station(&handle, join).await;
                eprintln!("error: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    let sink = match spawn_sink(&handle, json, log_file) {
        Ok(sink) => sink,
        Err(code) => {
            stop_station(&handle, join).await;
            return code;
        }
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
    let scenario = match load_scenario(&args.scenario, connector, args.id_tag.as_deref()) {
        Ok(scenario) => scenario,
        Err(error) => return usage_error(&error),
    };
    let (handle, join) = Station::spawn(config, Arc::new(SystemClock));
    let sink = match spawn_sink(&handle, json, log_file) {
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

    let mut interrupted = false;
    let outcome = tokio::select! {
        outcome = connect_and_run(&handle, &scenario, &cancel) => outcome,
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
    if let Some(report) = &report {
        print_report(report, json, None);
        if let Some(path) = &args.report
            && let Err(error) = write_report(path, report)
        {
            eprintln!("error: cannot write the report: {error}");
            return ExitCode::FAILURE;
        }
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
            return ExitCode::FAILURE;
        }
    }
    if interrupted {
        ExitCode::from(EXIT_INTERRUPTED)
    } else if report.is_some_and(|r| r.passed) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

async fn connect_and_run(
    handle: &StationHandle,
    scenario: &Scenario,
    cancel: &CancelToken,
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
    Ok(run_scenario(handle, scenario, cancel, None).await)
}

/// Prints a scenario report; `station` (fleet mode) tags every line.
fn print_report(report: &ScenarioReport, json: bool, station: Option<&str>) {
    if json {
        let mut value = serde_json::to_value(report).unwrap_or_default();
        if let Some(object) = value.as_object_mut() {
            object.insert("type".to_owned(), "report".into());
            if let Some(station) = station {
                object.insert("station".to_owned(), station.into());
            }
        }
        println!("{value}");
        return;
    }
    let say = |line: String| println!("{}", headless::tagged(&line, station));
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

fn write_junit(path: &Path, suites: &[JunitSuite<'_>]) -> std::io::Result<()> {
    std::fs::write(path, junit_xml(suites))
}

fn write_report(path: &Path, report: &ScenarioReport) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(report).map_err(std::io::Error::other)?;
    std::fs::write(path, text + "\n")
}
