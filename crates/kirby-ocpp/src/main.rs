//! `kirby-ocpp`: terminal OCPP 1.6J charge point emulator.

mod cli;
mod fleet;
mod headless;
mod suite;

use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::CommandFactory;
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

use crate::cli::{Cli, Cmd, RunArgs, ScenariosCmd, SuiteArgs};
use crate::headless::{Level, LinkWatch, Sink};

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

#[allow(clippy::too_many_lines)]
async fn real_main() -> ExitCode {
    let mut cli = Cli::parse_args();
    if cli.licenses {
        print_lossy(LICENSES);
        return ExitCode::SUCCESS;
    }
    headless::init_stdout(cli.json);
    let level = if cli.quiet {
        Level::Quiet
    } else if cli.verbose {
        Level::Verbose
    } else {
        Level::Normal
    };
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
    let color =
        !cli.json && headless::resolve_color(cli.color, std::io::stdout().is_terminal(), no_color);
    headless::init_style(level, color);
    let command = cli.command.take();
    match &command {
        Some(Cmd::Scenarios { action }) => return scenarios(action),
        Some(Cmd::Version) => return print_version(cli.json),
        Some(Cmd::Completions { shell }) => return print_completions(*shell),
        _ => {}
    }
    let (print_config, no_tui, json) = (cli.print_config, cli.no_tui, cli.json);
    let spec = cli.fleet_spec();
    let templated = cli.id_template.is_some();
    let log_file = cli.log_file.clone();
    let state_dir_given = cli.state_dir.is_some();
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
    // A scenario run is deterministic: no state file is read or written unless `--persist`.
    if let Some(Cmd::Run(args)) = &command
        && !args.persist
    {
        if state_dir_given && config.persist {
            eprintln!("note: --state-dir is ignored by `run` unless --persist is given");
        }
        config.persist = false;
    }
    config.normalize();
    if let Err(error) = config.validate() {
        return usage_error(&error.to_string());
    }
    headless::init_connect_context(&config.url, &config.ws_subprotocol);
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
        Some(Cmd::Scenarios { .. } | Cmd::Version | Cmd::Completions { .. }) => {
            unreachable!("handled above")
        }
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
        ScenariosCmd::Validate { paths } => validate_scenarios(paths),
    }
}

/// `scenarios validate`: parses every scenario without connecting.
fn validate_scenarios(paths: &[String]) -> ExitCode {
    let mut failed = false;
    let mut fail = |message: &str| {
        eprintln!("error: {}", kirby_ocpp_tui::text::sanitize(message));
        failed = true;
    };
    // Sources already validated (canonical path, or the name of a built-in) and the file that
    // first defined each scenario name.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for spec in paths {
        let mut sources = Vec::new();
        if names_a_path(spec) && Path::new(spec).is_dir() {
            match suite::toml_files_noting(Path::new(spec)) {
                Ok((files, skipped)) => {
                    for path in skipped {
                        eprintln!(
                            "note: skipped `{}`: not a regular file",
                            kirby_ocpp_tui::text::sanitize(&path.display().to_string())
                        );
                    }
                    if files.is_empty() {
                        fail(&format!("no `*.toml` scenario files in `{spec}`"));
                    }
                    sources.extend(files.iter().map(|file| file.display().to_string()));
                }
                Err(error) => fail(&error),
            }
        } else if !names_a_path(spec) && builtin::get(spec).is_none() {
            fail(&unknown_scenario_message(spec));
        } else {
            sources.push(spec.clone());
        }
        for source in sources {
            let key = std::fs::canonicalize(&source)
                .map_or_else(|_| source.clone(), |path| path.display().to_string());
            if !seen.insert(key) {
                continue;
            }
            let args = SuiteArgs::for_sources(vec![source.clone()]);
            match suite::load_entries(&args, ConnectorId::new(1)) {
                Ok(entries) => {
                    for entry in entries {
                        headless::stdout_line(
                            &format!(
                                "ok {} ({} steps)",
                                entry.source,
                                entry.scenario.expanded_len()
                            ),
                            false,
                        );
                        if let Some(first) =
                            names.insert(entry.scenario.name.clone(), entry.source.clone())
                        {
                            eprintln!(
                                "warning: scenario name `{}` is used by `{}` and `{}`",
                                kirby_ocpp_tui::text::sanitize(&entry.scenario.name),
                                kirby_ocpp_tui::text::sanitize(&first),
                                kirby_ocpp_tui::text::sanitize(&entry.source)
                            );
                        }
                    }
                }
                Err(error) if error.contains(&source) => fail(&error),
                Err(error) => fail(&format!("{source}: {error}")),
            }
        }
    }
    if failed {
        ExitCode::from(EXIT_USAGE)
    } else {
        ExitCode::SUCCESS
    }
}

/// `version`: the tool version and how it was built.
fn print_version(json: bool) -> ExitCode {
    let version = env!("CARGO_PKG_VERSION");
    let commit = env!("KIRBY_OCPP_GIT_COMMIT");
    let date = env!("KIRBY_OCPP_BUILD_DATE");
    let target = env!("KIRBY_OCPP_TARGET");
    let protocols = ["OCPP 1.6J"];
    if json {
        let value = serde_json::json!({
            "name": "kirby-ocpp",
            "version": version,
            "commit": commit,
            "build_date": date,
            "target": target,
            "protocols": protocols,
        });
        headless::stdout_line(&value.to_string(), true);
    } else {
        print_lossy(&format!(
            "kirby-ocpp {version}\ncommit: {commit}\nbuilt: {date}\ntarget: {target}\nprotocols: {}\n",
            protocols.join(", ")
        ));
    }
    ExitCode::SUCCESS
}

/// `completions SHELL`: the completion script on stdout.
fn print_completions(shell: clap_complete::Shell) -> ExitCode {
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), "kirby-ocpp", &mut script);
    print_lossy(&String::from_utf8_lossy(&script));
    ExitCode::SUCCESS
}

/// The error for a scenario name that is neither a built-in nor an existing file, with the
/// closest built-in name when one is near.
pub(crate) fn unknown_scenario_message(name: &str) -> String {
    let mut message =
        format!("no built-in scenario `{name}` and no such file; see `kirby-ocpp scenarios list`");
    let wanted = name.to_ascii_lowercase().replace('_', "-");
    let closest = builtin::all()
        .into_iter()
        .map(|scenario| {
            let distance = edit_distance(&wanted, &scenario.name.to_ascii_lowercase());
            (distance, scenario.name)
        })
        .min();
    if let Some((distance, candidate)) = closest
        && distance <= 2.max(wanted.len() / 4)
    {
        let _ = write!(message, " (did you mean `{candidate}`?)");
    }
    message
}

/// Levenshtein distance of two short strings.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(above + 1);
            diagonal = above;
        }
    }
    row[b.len()]
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
    if builtin.is_none() && !names_a_path(spec) {
        return Err(unknown_scenario_message(spec));
    }
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
    // Subscribed before anything connects (also before the terminal UI does), so a give-up that
    // happens early is not missed when the run falls back to headless.
    let mut link = LinkWatch::new(&handle);
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
            eprintln!(
                "error: {}",
                kirby_ocpp_tui::text::sanitize(&error.to_string())
            );
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
    // Signal streams exist before the connection starts, so an early signal is not lost.
    let mut shutdown = Shutdown::new();
    if !tui && let Err(error) = handle.connect().await {
        eprintln!(
            "error: {}",
            kirby_ocpp_tui::text::sanitize(&error.to_string())
        );
    }
    // A station that gave up reconnecting ends the process.
    let dead = tokio::select! {
        _ = shutdown.recv() => None,
        dead = link.dead() => Some(dead),
    };
    stop_station(&handle, join).await;
    sink.finish().await;
    if let Some(dead) = dead {
        eprintln!("error: {}", kirby_ocpp_tui::text::sanitize(&dead));
        return ExitCode::FAILURE;
    }
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
    let sink = match spawn_sink(&handle, json, log_file, headless::events_on_stdout(json)) {
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
    let mut shutdown = Shutdown::new();
    let outcome = tokio::select! {
        outcome = connect_and_run(&handle, &scenario, &cancel, &overrides, 1) => outcome,
        sigint = shutdown.recv() => {
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
        eprintln!("error: {}", kirby_ocpp_tui::text::sanitize(error));
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
    // Subscribed before connecting: a connection that fails at once must not be missed.
    let mut link = LinkWatch::new(handle);
    handle.connect().await.map_err(|e| e.to_string())?;
    let mut snapshots = handle.watch_snapshot();
    tokio::select! {
        registered = snapshots.wait_for(kirby_ocpp_core::station::Snapshot::booted) => {
            registered.map_err(|_| "the station stopped".to_owned())?;
        }
        // No reconnect left (`--no-reconnect`, attempts used up): fail now, not at the timeout.
        dead = link.dead() => return Err(dead),
        () = cancel.cancelled() => {
            let mut message =
                "timed out waiting for the central system to accept the BootNotification".to_owned();
            if let Some(last) = link.last_error() {
                message.push_str("; last error: ");
                message.push_str(&last);
            }
            return Err(message);
        }
    }
    // The link can also go away while the scenario runs: end the run with that reason.
    tokio::select! {
        report = run_scenario_with_vars(handle, scenario, cancel, None, overrides, index) => {
            Ok(report)
        }
        dead = link.dead() => Err(format!("connection lost: {dead}")),
    }
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
    let verdict = format!(
        "{} in {} ms",
        if report.passed { "PASSED" } else { "FAILED" },
        report.duration_ms
    );
    if headless::level() == Level::Quiet {
        // Only the final line; a fleet prints its aggregate line instead.
        if station.is_none() {
            say(verdict);
        }
        return;
    }
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
    say(verdict);
}

pub(crate) fn write_junit(path: &Path, suites: &[JunitSuite<'_>]) -> std::io::Result<()> {
    std::fs::write(path, junit_xml(suites))
}

fn write_report(path: &Path, report: &ScenarioReport) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(report).map_err(std::io::Error::other)?;
    std::fs::write(path, text + "\n")
}
