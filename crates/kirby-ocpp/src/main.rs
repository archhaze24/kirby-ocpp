//! `kirby-ocpp`: terminal OCPP 1.6J charge point emulator.

/// Stderr lines are written whole and in one piece. The std macro issues several small
/// writes per line, which interleave with the stdout lines when both streams share a pipe
/// (`2>&1`) and tear one into the other. Declared before the modules so they all use it.
macro_rules! eprintln {
    ($($arg:tt)*) => {
        $crate::headless::stderr_line(&format!($($arg)*))
    };
}

mod check;
mod cli;
mod fleet;
mod headless;
mod record;
mod replay;
mod settings;
mod state;
mod suite;
mod suite_ci;

use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use kirby_ocpp_core::fleet::Fleet;
use kirby_ocpp_core::ids::{ConnectorId, IdTag};
use kirby_ocpp_core::recording::RecordSink;
use kirby_ocpp_core::scenario::{
    CancelToken, JunitRun, JunitSuite, Scenario, ScenarioReport, StepStatus, Vars, builtin,
    junit_xml_runs, run_scenario_subscribed,
};
use kirby_ocpp_core::{Station, StationConfig, StationHandle, SystemClock};
use kirby_ocpp_tui::TuiError;
use kirby_ocpp_tui::stdout_pump::print_lossy;
use tokio::task::JoinHandle;

use crate::cli::{Cli, Cmd, PrintFormat, RunArgs, ScenarioVar, ScenariosCmd, SuiteArgs};
use crate::headless::{Level, LinkWatch, Sink};
use crate::record::RecordTarget;

/// Project license line and third-party notices (`--licenses`).
const LICENSES: &str = concat!(
    "kirby-ocpp is distributed under the MIT license (see LICENSE in the source repository).\n\n",
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
    // The terminal UI is a terminal by definition: only the choice and `NO_COLOR` decide.
    headless::init_display(
        cli.time.mode(),
        !headless::resolve_color(cli.color, true, no_color),
    );
    let command = cli.command.take();
    match &command {
        Some(Cmd::Scenarios { action }) => return scenarios(action),
        Some(Cmd::Version) => return print_version(cli.json),
        Some(Cmd::Completions { shell }) => return print_completions(*shell),
        Some(Cmd::State { action }) => {
            let options = state::StateOptions {
                id: cli.id.clone(),
                id_given: cli.id_explicit,
                dir: cli.state_dir.clone(),
                json: cli.json,
            };
            return state::run(action, &options);
        }
        _ => {}
    }
    let (print_config, no_tui, json) = (cli.print_config, cli.no_tui, cli.json);
    let config_toml = (print_config == Some(PrintFormat::Toml)).then(|| cli.config_toml());
    let spec = cli.fleet_spec();
    let templated = cli.id_template.is_some();
    let log_file = cli.log_file.clone();
    let mut record = cli
        .record
        .clone()
        .map(|path| RecordTarget::new(path, cli.record_overwrite));
    if let Some(target) = &mut record {
        target.protect("the configuration file", cli.config.as_deref());
    }
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
    // A replay is ephemeral too: it must not depend on, or change, a state file.
    if matches!(command, Some(Cmd::Replay(_))) {
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

    if let Some(text) = config_toml {
        print_lossy(&text);
        return ExitCode::SUCCESS;
    }
    if print_config.is_some() {
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
    if let Some(target) = &mut record {
        target.protect("the TLS CA file", config.tls.ca_file.as_deref());
        target.protect("the TLS certificate", config.tls.cert_file.as_deref());
        target.protect("the TLS key", config.tls.key_file.as_deref());
        target.protect("the log file", log_file.as_deref());
        match &command {
            Some(Cmd::Run(args)) => {
                target.protect("the report", args.report.as_deref());
                target.protect("the JUnit report", args.junit.as_deref());
            }
            Some(Cmd::Replay(args)) => {
                target.protect("the report", args.report.as_deref());
                target.protect("the JUnit report", args.junit.as_deref());
                target.protect("the recording that is replayed", Some(&args.recording));
            }
            _ => {}
        }
    }
    // Only a command that connects needs the TLS files (`--print-config` does not).
    if let Err(error) = config.check_tls_files() {
        return usage_error(&error.to_string());
    }

    if record.is_some() && (is_fleet || matches!(command, Some(Cmd::Suite(_) | Cmd::Check(_)))) {
        return usage_error(
            "--record records one station: it is not supported with `suite`, `check` or a fleet (--count > 1)",
        );
    }

    match command {
        Some(Cmd::Run(args)) if is_fleet => {
            fleet::run(config, spec, &args, connector, json, log_file.as_deref()).await
        }
        Some(Cmd::Run(args)) => {
            run(
                config,
                &args,
                connector,
                json,
                log_file.as_deref(),
                record.as_ref(),
            )
            .await
        }
        Some(Cmd::Replay(_)) if is_fleet => {
            usage_error("`replay` uses one station; fleet mode (--count > 1) is not supported")
        }
        Some(Cmd::Replay(args)) => {
            replay::run(config, &args, json, log_file.as_deref(), record.as_ref()).await
        }
        Some(Cmd::Suite(_)) if is_fleet => usage_error(
            "`suite` runs one station per scenario; fleet mode (--count > 1) is not supported",
        ),
        Some(Cmd::Suite(args)) => {
            suite::run(config, &args, connector, json, log_file.as_deref()).await
        }
        Some(Cmd::Check(_)) if is_fleet => {
            usage_error("`check` tests one station; fleet mode (--count > 1) is not supported")
        }
        Some(Cmd::Check(args)) => check::run(&config, &args, json).await,
        Some(
            Cmd::Scenarios { .. } | Cmd::Version | Cmd::Completions { .. } | Cmd::State { .. },
        ) => {
            unreachable!("handled above")
        }
        None => {
            // `--json` is for machines: it never opens the terminal UI.
            let tui = !no_tui && !json && std::io::stdout().is_terminal();
            if is_fleet {
                return fleet::interactive(config, spec, tui, json, log_file.as_deref()).await;
            }
            interactive(config, tui, json, log_file.as_deref(), record.as_ref()).await
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
        ScenariosCmd::FromRecording {
            recording,
            output,
            overwrite,
        } => replay::from_recording(recording, output.as_deref(), *overwrite),
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
    let protocol_tokens = ["ocpp1.6"];
    if json {
        let value = serde_json::json!({
            "name": "kirby-ocpp",
            "version": version,
            "commit": commit,
            "build_date": date,
            "target": target,
            "protocols": protocols,
            "protocol_tokens": protocol_tokens,
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
    clap_complete::generate(
        shell,
        &mut cli::completion_command(),
        "kirby-ocpp",
        &mut script,
    );
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
    vars: &[ScenarioVar],
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
    if let Some((name, _, _)) = vars
        .iter()
        .find(|(name, _, _)| !scenario.uses_variable(name))
    {
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
    record: Option<&RecordTarget>,
) -> ExitCode {
    if let Some(target) = record
        && let Err(error) = target.check_against(&[("the --log-file", log_file)])
    {
        return usage_error(&error);
    }
    let station = config.charge_point_id.clone();
    record::scope(record, &station, |sink| {
        interactive_recorded(config, tui, json, log_file, sink)
    })
    .await
}

async fn interactive_recorded(
    config: StationConfig,
    tui: bool,
    json: bool,
    log_file: Option<&Path>,
    sink: Option<RecordSink>,
) -> ExitCode {
    let (handle, join) = Station::spawn_recorded(config, Arc::new(SystemClock), sink);
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
        match kirby_ocpp_tui::run(handle.clone(), headless::tui_options()).await {
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
    record: Option<&RecordTarget>,
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
    let outputs = RunOutputs {
        report: args.report.as_deref(),
        junit: args.junit.as_deref(),
        timeout: args.timeout,
        vars: &args.vars,
    };
    execute_scenario(config, scenario, &outputs, json, log_file, record).await
}

/// The per-run outputs and limits of a scenario run (`run` and `replay` share them).
pub(crate) struct RunOutputs<'a> {
    pub(crate) report: Option<&'a Path>,
    pub(crate) junit: Option<&'a Path>,
    pub(crate) timeout: u64,
    pub(crate) vars: &'a [ScenarioVar],
}

/// Runs a loaded scenario on a fresh station and writes its report; the exit code of `run`.
pub(crate) async fn execute_scenario(
    config: StationConfig,
    scenario: Scenario,
    outputs: &RunOutputs<'_>,
    json: bool,
    log_file: Option<&Path>,
    record: Option<&RecordTarget>,
) -> ExitCode {
    if let Some(target) = record
        && let Err(error) = target.check_against(&[
            (
                "the scenario or recording that is run",
                scenario.source.as_deref().map(Path::new),
            ),
            ("the --log-file", log_file),
        ])
    {
        return usage_error(&error);
    }
    let mut protected: Vec<&str> = scenario.source.as_deref().into_iter().collect();
    let record_path = record.map(|target| target.path.to_string_lossy().into_owned());
    protected.extend(record_path.as_deref());
    if let Err(error) = check_output_paths(outputs.report, outputs.junit, &protected) {
        return usage_error(&error);
    }
    let station = config.charge_point_id.clone();
    record::scope(record, &station, |sink| {
        execute_recorded(config, scenario, outputs, json, log_file, sink)
    })
    .await
}

async fn execute_recorded(
    config: StationConfig,
    scenario: Scenario,
    outputs: &RunOutputs<'_>,
    json: bool,
    log_file: Option<&Path>,
    sink: Option<RecordSink>,
) -> ExitCode {
    let (handle, join) = Station::spawn_recorded(config, Arc::new(SystemClock), sink);
    let sink = match spawn_sink(&handle, json, log_file, headless::events_on_stdout(json)) {
        Ok(sink) => sink,
        Err(code) => {
            stop_station(&handle, join).await;
            return code;
        }
    };

    let cancel = CancelToken::new();
    let limit = Duration::from_secs(outputs.timeout);
    let timer = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(limit).await;
            cancel.cancel();
        })
    };

    let overrides = override_vars(outputs.vars);
    let mut interrupted = false;
    let mut shutdown = Shutdown::new();
    let run_started = std::time::Instant::now();
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
        && let Some(path) = outputs.report
        && let Err(error) = write_report(path, report)
    {
        eprintln!("error: cannot write the report: {error}");
        write_failed = true;
    }
    if !interrupted && let Some(path) = outputs.junit {
        let suite = JunitRun {
            suite: JunitSuite {
                station: None,
                scenario: &scenario.name,
                outcome: report
                    .as_ref()
                    .ok_or_else(|| setup_error.as_deref().unwrap_or("no report")),
            },
            source: scenario.source.as_deref(),
            duration_ms: u64::try_from(run_started.elapsed().as_millis()).unwrap_or(u64::MAX),
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
    // The scenario's own subscription, also taken before connecting: a CSMS CALL that
    // arrives right after the boot must still be seen by `expect_call direction = "in"`.
    let events = handle.subscribe();
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
        report = run_scenario_subscribed(handle, scenario, cancel, None, overrides, index, events) => {
            Ok(report)
        }
        dead = link.dead() => Err(format!("connection lost: {dead}")),
    }
}

/// The `--var` overrides of a `run` or `suite` command.
pub(crate) fn override_vars(vars: &[ScenarioVar]) -> Vars {
    let mut out = Vars::new();
    for (name, value, written) in vars {
        // Names were validated by the argument parser. A number keeps the text it was
        // written with for interpolation into longer text.
        let _ = if value.is_number() {
            out.insert_with_text(name, value.clone(), written.clone())
        } else {
            out.insert(name, value.clone())
        };
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

pub(crate) fn write_junit(path: &Path, runs: &[JunitRun<'_>]) -> std::io::Result<()> {
    write_atomic(path, junit_xml_runs(runs).as_bytes())
}

pub(crate) fn write_report(path: &Path, report: &ScenarioReport) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(report).map_err(std::io::Error::other)?;
    write_atomic(path, (text + "\n").as_bytes())
}

/// Writes `bytes` to a temporary file next to `path` and renames it over `path`, so a reader
/// never sees half a report. An existing file keeps its permissions; a new one gets the
/// default mode, as with a plain write.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("the path has no file name"))?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".{}.tmp", std::process::id()));
    let temp = path.with_file_name(temp_name);
    let written = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        if let Ok(existing) = std::fs::metadata(path) {
            file.set_permissions(existing.permissions())?;
        }
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// Checks the `--report` and `--junit` paths before anything runs: the directory exists and
/// accepts a new file, the two paths differ, and neither is one of the scenario files
/// (`sources`). An unwritable path then fails with exit 2 up front instead of costing the
/// result of a finished run.
///
/// # Errors
/// A one-line message naming the flag.
pub(crate) fn check_output_paths(
    report: Option<&Path>,
    junit: Option<&Path>,
    sources: &[&str],
) -> Result<(), String> {
    for (flag, path) in [("--report", report), ("--junit", junit)] {
        let Some(path) = path else { continue };
        if path.is_dir() {
            return Err(format!("{flag} {}: is a directory", path.display()));
        }
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if !parent.is_dir() {
            return Err(format!(
                "{flag} {}: the directory {} does not exist",
                path.display(),
                parent.display()
            ));
        }
        let probe = parent.join(format!(".kirby-ocpp-probe-{}", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
        {
            Ok(file) => {
                drop(file);
                let _ = std::fs::remove_file(&probe);
            }
            Err(error) => {
                return Err(format!(
                    "{flag} {}: cannot write to the directory {}: {error}",
                    path.display(),
                    parent.display()
                ));
            }
        }
        if let Some(source) = sources
            .iter()
            .find(|source| Path::new(source).is_file() && same_file(path, Path::new(source)))
        {
            return Err(format!(
                "{flag} {}: is the scenario file {source}; it would be overwritten",
                path.display()
            ));
        }
    }
    if let (Some(report), Some(junit)) = (report, junit)
        && same_file(report, junit)
    {
        return Err(format!(
            "--report and --junit name the same file ({})",
            report.display()
        ));
    }
    Ok(())
}

/// Whether two paths name the same file, also when neither exists yet.
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    fn resolve(path: &Path) -> std::path::PathBuf {
        if let Ok(real) = std::fs::canonicalize(path) {
            return real;
        }
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        match (std::fs::canonicalize(parent), path.file_name()) {
            (Ok(parent), Some(name)) => parent.join(name),
            _ => path.to_path_buf(),
        }
    }
    // Hard links and other aliases of an existing file share the inode.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b))
            && a.dev() == b.dev()
            && a.ino() == b.ino()
        {
            return true;
        }
    }
    resolve(a) == resolve(b)
}
