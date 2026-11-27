//! `kirby-ocpp`: terminal OCPP 1.6J, 2.0.1 and 2.1 charge point emulator.

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
use kirby_ocpp_core::link_view::{LinkSnapshot, StationLike};
use kirby_ocpp_core::ocpp::Protocol;
use kirby_ocpp_core::recording::RecordSink;
use kirby_ocpp_core::scenario::{
    CancelToken, JunitRun, JunitSuite, Scenario, ScenarioReport, ScenarioTarget, StepStatus, Vars,
    builtin, junit_xml_runs_for, run_scenario_subscribed,
};
use kirby_ocpp_core::{
    Station, Station2, Station2Handle, StationConfig, StationHandle, SystemClock,
};
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

/// Stack of the thread that runs the async main: the Windows default of 1 MiB is too small for debug builds.
const MAIN_STACK: usize = 8 * 1024 * 1024;

fn main() -> ExitCode {
    let thread = std::thread::Builder::new()
        .name("main".into())
        .stack_size(MAIN_STACK)
        .spawn(|| {
            match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(async_main()),
                Err(error) => {
                    eprintln!("error: cannot start the async runtime: {error}");
                    ExitCode::FAILURE
                }
            }
        });
    let thread = match thread {
        Ok(thread) => thread,
        Err(error) => {
            eprintln!("error: cannot start the main thread: {error}");
            return ExitCode::FAILURE;
        }
    };
    match thread.join() {
        Ok(code) => code,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

async fn async_main() -> ExitCode {
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
        Some(Cmd::Version) => return print_version(cli.json),
        Some(Cmd::Completions { shell }) => return print_completions(*shell),
        _ => {}
    }
    let protocol = cli.protocol.protocol();
    if !protocol.is_station_implemented() {
        return usage_error(&format!(
            "OCPP {} is not supported by this build",
            protocol.ocpp_version()
        ));
    }
    if let Some(Cmd::Scenarios { action }) = &command {
        return scenarios(action, protocol);
    }
    if let Some(Cmd::State { action }) = &command {
        let options = state::StateOptions {
            id: cli.id.clone(),
            id_given: cli.id_explicit,
            dir: cli.state_dir.clone(),
            json: cli.json,
            protocol,
        };
        return state::run(action, &options);
    }
    if protocol != Protocol::V16 && !cli.v16_only_given.is_empty() {
        return usage_error(&format!(
            "{} only exist{} for ocpp1.6 and cannot be used with {}",
            cli.v16_only_given.join(", "),
            if cli.v16_only_given.len() == 1 {
                "s"
            } else {
                ""
            },
            protocol.token()
        ));
    }
    if let Some(offered) = &cli.ws_subprotocol
        && offered != protocol.token()
    {
        eprintln!(
            "note: offering {} while behaving as {}",
            kirby_ocpp_tui::text::sanitize(offered),
            protocol.token()
        );
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
    if let Some(note) = station_id_note(&config) {
        eprintln!("note: {note}");
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

    // `--json` is for machines: it never opens the terminal UI.
    let tui = !no_tui && !json && std::io::stdout().is_terminal();
    if let Some(reason) = mode_gate(command.as_ref(), config.protocol) {
        return usage_error(&reason);
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

/// The reason a mode cannot run with `protocol`, or `None` when it is available.
fn mode_gate(command: Option<&Cmd>, protocol: Protocol) -> Option<String> {
    if protocol == Protocol::V16 {
        return None;
    }
    match command {
        Some(Cmd::Replay(_)) => Some(format!(
            "`replay` is not available for {} yet",
            protocol.token()
        )),
        _ => None,
    }
}

/// A note about a station id that OCPP 2.x does not allow in the URL path: at most 48
/// characters of `identifierString` and no colon (Part 4 section 3.1.1). The id is used as
/// given.
fn station_id_note(config: &StationConfig) -> Option<String> {
    if config.protocol == Protocol::V16 {
        return None;
    }
    let id = &config.charge_point_id;
    let mut problems = Vec::new();
    if id.chars().count() > 48 {
        problems.push("is longer than 48 characters");
    }
    if id.contains(':') {
        problems.push("contains a colon");
    }
    if id
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || "*-_=:+|@.".contains(c)))
    {
        problems.push("has characters outside identifierString");
    }
    if problems.is_empty() {
        return None;
    }
    let shown: String = id.chars().take(64).collect();
    let ellipsis = if id.chars().count() > 64 { "..." } else { "" };
    Some(format!(
        "station id `{}{ellipsis}` {} (OCPP {} allows at most 48 identifierString characters and no colon); it is used as given",
        kirby_ocpp_tui::text::sanitize(&shown),
        problems.join(" and "),
        config.protocol.ocpp_version()
    ))
}

/// `scenarios`: the built-ins of `protocol` (`list`, `show`), or files of any version
/// (`validate`).
fn scenarios(action: &ScenariosCmd, protocol: Protocol) -> ExitCode {
    match action {
        ScenariosCmd::List => {
            for scenario in builtin::all_for(protocol) {
                print_lossy(&format!("{:<16} {}\n", scenario.name, scenario.description));
            }
            ExitCode::SUCCESS
        }
        ScenariosCmd::Show { name } => match builtin::source_text(name, protocol) {
            Some(text) => {
                print_lossy(&text);
                ExitCode::SUCCESS
            }
            None => usage_error(&format!(
                "no built-in scenario `{name}` (try `kirby-ocpp scenarios list{}`)",
                if protocol == Protocol::V16 {
                    String::new()
                } else {
                    format!(" --protocol {}", protocol.token())
                }
            )),
        },
        ScenariosCmd::Validate { paths } => validate_scenarios(paths, protocol),
        ScenariosCmd::FromRecording { .. } if protocol != Protocol::V16 => usage_error(&format!(
            "`scenarios from-recording` is not available for {} yet",
            protocol.token()
        )),
        ScenariosCmd::FromRecording {
            recording,
            output,
            overwrite,
        } => replay::from_recording(recording, output.as_deref(), *overwrite),
    }
}

/// `scenarios validate`: parses every scenario without connecting. Files of any OCPP version
/// are accepted; built-in names resolve for `protocol`.
fn validate_scenarios(paths: &[String], protocol: Protocol) -> ExitCode {
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
        } else if !names_a_path(spec) && builtin::get_for(spec, protocol).is_none() {
            fail(&unknown_scenario_message(spec, protocol));
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
            match suite::load_entries(&args, ConnectorId::new(1), protocol, false) {
                Ok(entries) => {
                    for entry in entries {
                        // The version is named only when it is not the default one.
                        let version = match entry.scenario.protocol() {
                            Protocol::V16 => String::new(),
                            other => format!(", OCPP {}", other.ocpp_version()),
                        };
                        headless::stdout_line(
                            &format!(
                                "ok {} ({} steps{version})",
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
    let protocols = ["OCPP 1.6J", "OCPP 2.0.1", "OCPP 2.1"];
    let protocol_tokens = ["ocpp1.6", "ocpp2.0.1", "ocpp2.1"];
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
pub(crate) fn unknown_scenario_message(name: &str, protocol: Protocol) -> String {
    let mut message =
        format!("no built-in scenario `{name}` and no such file; see `kirby-ocpp scenarios list`");
    if protocol != Protocol::V16 {
        let _ = write!(message, " --protocol {}", protocol.token());
    }
    let wanted = name.to_ascii_lowercase().replace('_', "-");
    let closest = builtin::all_for(protocol)
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
    protocol: Protocol,
    connector: ConnectorId,
    id_tag: Option<&str>,
    vars: &[ScenarioVar],
) -> Result<Scenario, String> {
    let builtin = if names_a_path(spec) {
        None
    } else {
        builtin::get_for(spec, protocol)
    };
    if builtin.is_none() && !names_a_path(spec) {
        return Err(unknown_scenario_message(spec, protocol));
    }
    let (mut scenario, is_builtin) = match builtin {
        Some(scenario) => (scenario, true),
        None => (
            Scenario::load(Path::new(spec)).map_err(|e| e.to_string())?,
            false,
        ),
    };
    if let Some(error) = protocol_mismatch(&scenario, protocol) {
        return Err(error);
    }
    // Built-ins target connector 1; files say what they mean.
    retarget_for(&mut scenario, is_builtin.then_some(connector), id_tag)?;
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

/// The error for a scenario written for another OCPP version than the station speaks.
pub(crate) fn protocol_mismatch(scenario: &Scenario, station: Protocol) -> Option<String> {
    (scenario.protocol() != station).then(|| {
        format!(
            "scenario {} is for OCPP {}; this station speaks {}",
            kirby_ocpp_tui::text::sanitize(&scenario.name),
            scenario.protocol().ocpp_version(),
            station.token()
        )
    })
}

/// Points a scenario at `connector` (the EVSE for OCPP 2.0.1) and `id_tag` (the `id_token`
/// for OCPP 2.0.1), as far as they are given.
pub(crate) fn retarget_for(
    scenario: &mut Scenario,
    connector: Option<ConnectorId>,
    id_tag: Option<&str>,
) -> Result<(), String> {
    if scenario.protocol() == Protocol::V16 {
        let id_tag = id_tag
            .map(IdTag::new)
            .transpose()
            .map_err(|e| e.to_string())?;
        scenario.retarget(connector, id_tag.as_ref());
    } else {
        if let Some(token) = id_tag {
            let len = token.chars().count();
            // OCPP 2.1 raised the maximum of `idToken` from 36 to 255 characters.
            let max = if scenario.protocol() == Protocol::V21 {
                255
            } else {
                36
            };
            if !(1..=max).contains(&len) {
                return Err(format!(
                    "idToken is {len} characters long, it must be 1 to {max}"
                ));
            }
        }
        scenario.retarget_v201(connector.map(ConnectorId::get), id_tag);
    }
    Ok(())
}

/// SIGINT/SIGTERM/SIGHUP streams created once: a signal that arrives while nobody awaits is
/// kept and delivered by the next [`Shutdown::recv`]. On Windows, Ctrl-C and closing the
/// console window.
pub(crate) struct Shutdown {
    #[cfg(unix)]
    int: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    term: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    hup: Option<tokio::signal::unix::Signal>,
    #[cfg(windows)]
    close: Option<tokio::signal::windows::CtrlClose>,
}

impl Shutdown {
    pub(crate) fn new() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Self {
                int: signal(SignalKind::interrupt()).ok(),
                term: signal(SignalKind::terminate()).ok(),
                hup: signal(SignalKind::hangup()).ok(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                #[cfg(windows)]
                close: tokio::signal::windows::ctrl_close().ok(),
            }
        }
    }

    /// Resolves on the next signal; `true` means SIGINT (Ctrl-C). SIGHUP and a closed console
    /// window end the run like SIGTERM.
    #[cfg_attr(not(unix), allow(clippy::unused_self))]
    pub(crate) async fn recv(&mut self) -> bool {
        #[cfg(unix)]
        {
            async fn next(signal: &mut Option<tokio::signal::unix::Signal>) {
                match signal {
                    Some(signal) => {
                        signal.recv().await;
                    }
                    None => std::future::pending().await,
                }
            }
            if self.int.is_none() {
                // No SIGINT stream: fall back to the lazy Ctrl-C future.
                return tokio::select! {
                    _ = tokio::signal::ctrl_c() => true,
                    () = next(&mut self.term) => false,
                    () = next(&mut self.hup) => false,
                };
            }
            tokio::select! {
                () = next(&mut self.int) => true,
                () = next(&mut self.term) => false,
                () = next(&mut self.hup) => false,
            }
        }
        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            {
                async fn closed(close: &mut Option<tokio::signal::windows::CtrlClose>) {
                    match close {
                        Some(close) => {
                            close.recv().await;
                        }
                        None => std::future::pending().await,
                    }
                }
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => true,
                    () = closed(&mut self.close) => false,
                }
            }
            #[cfg(not(windows))]
            {
                let _ = tokio::signal::ctrl_c().await;
                true
            }
        }
    }
}

pub(crate) fn spawn_sink<H: StationLike>(
    handle: &H,
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
pub(crate) async fn stop_station<H: StationLike>(handle: &H, join: JoinHandle<()>) {
    // The station may already be gone; either way we are leaving.
    let _ = handle.shutdown().await;
    let _ = join.await;
}

/// A running station of either OCPP version. The 1.6 path stays a single variant.
pub(crate) enum AnyStation {
    V16(StationHandle, JoinHandle<()>),
    V2(Station2Handle, JoinHandle<()>),
}

impl AnyStation {
    /// Spawns the station of `config.protocol`, recording to `sink` when given.
    pub(crate) fn spawn(config: StationConfig, sink: Option<RecordSink>) -> Self {
        let clock = Arc::new(SystemClock);
        if config.protocol == Protocol::V16 {
            let (handle, join) = Station::spawn_recorded(config, clock, sink);
            Self::V16(handle, join)
        } else {
            let (handle, join) = Station2::spawn_recorded(config, clock, sink);
            Self::V2(handle, join)
        }
    }

    pub(crate) fn spawn_sink(
        &self,
        json: bool,
        log_file: Option<&Path>,
        stdout: bool,
    ) -> Result<Sink, ExitCode> {
        match self {
            Self::V16(handle, _) => spawn_sink(handle, json, log_file, stdout),
            Self::V2(handle, _) => spawn_sink(handle, json, log_file, stdout),
        }
    }

    /// Connects, waits for the boot and runs `scenario`.
    pub(crate) async fn run_scenario(
        &self,
        scenario: &Scenario,
        cancel: &CancelToken,
        overrides: &Vars,
    ) -> Result<ScenarioReport, String> {
        match self {
            Self::V16(handle, _) => connect_and_run(handle, scenario, cancel, overrides, 1).await,
            Self::V2(handle, _) => connect_and_run(handle, scenario, cancel, overrides, 1).await,
        }
    }

    /// Closes the connection and waits for the station to end.
    pub(crate) async fn stop(self) {
        match self {
            Self::V16(handle, join) => stop_station(&handle, join).await,
            Self::V2(handle, join) => stop_station(&handle, join).await,
        }
    }
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
    let protocol = config.protocol;
    record::scope(record, &station, protocol, |sink| {
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
    match AnyStation::spawn(config, sink) {
        AnyStation::V16(handle, join) => {
            interactive_station(handle, join, tui, json, log_file, |handle| {
                kirby_ocpp_tui::run(handle, headless::tui_options())
            })
            .await
        }
        AnyStation::V2(handle, join) => {
            interactive_station(handle, join, tui, json, log_file, |handle| {
                kirby_ocpp_tui::v2::run2(handle, headless::tui_options())
            })
            .await
        }
    }
}

/// The station of an interactive run: terminal UI (`run_ui`) first, headless when it is off or
/// cannot start.
async fn interactive_station<H, F, Fut>(
    handle: H,
    join: JoinHandle<()>,
    tui: bool,
    json: bool,
    log_file: Option<&Path>,
    run_ui: F,
) -> ExitCode
where
    H: StationLike,
    F: FnOnce(H) -> Fut,
    Fut: Future<Output = Result<(), TuiError>>,
{
    // Subscribed before anything connects (also before the terminal UI does), so a give-up that
    // happens early is not missed when the run falls back to headless.
    let link = LinkWatch::new(&handle);
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
        match run_ui(handle.clone()).await {
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
    headless_loop(&handle, join, link, sink, tui, json, log_file).await
}

/// Runs a station without the terminal UI until a signal arrives or the reconnect loop ends.
/// `sink` is an existing event sink (the fallback from the UI), `connected` says that the
/// station was connected already.
async fn headless_loop<H: StationLike>(
    handle: &H,
    join: JoinHandle<()>,
    mut link: LinkWatch<H::Snap>,
    sink: Option<Sink>,
    connected: bool,
    json: bool,
    log_file: Option<&Path>,
) -> ExitCode {
    let sink = match sink {
        Some(sink) => sink,
        None => match spawn_sink(handle, json, log_file, true) {
            Ok(sink) => sink,
            Err(code) => {
                stop_station(handle, join).await;
                return code;
            }
        },
    };
    // Signal streams exist before the connection starts, so an early signal is not lost.
    let mut shutdown = Shutdown::new();
    if !connected && let Err(error) = handle.connect().await {
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
    stop_station(handle, join).await;
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
        config.protocol,
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
    let protocol = config.protocol;
    record::scope(record, &station, protocol, |sink| {
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
    let protocol = config.protocol;
    let station = AnyStation::spawn(config, sink);
    let sink = match station.spawn_sink(json, log_file, headless::events_on_stdout(json)) {
        Ok(sink) => sink,
        Err(code) => {
            station.stop().await;
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
        outcome = station.run_scenario(&scenario, &cancel, &overrides) => outcome,
        sigint = shutdown.recv() => {
            interrupted = sigint;
            cancel.cancel();
            Err("interrupted".to_owned())
        }
    };
    timer.abort();
    station.stop().await;
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
        if let Err(error) = write_junit(path, protocol.ocpp_version(), &[suite]) {
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

/// Connects the station and waits until the central system accepted its boot. `link` was
/// created before the connection, so a failure that happens at once is not missed.
///
/// # Errors
/// The connection could not start, the station ended, the reconnect loop gave up, or `cancel`
/// fired first (the message then carries the last connection error).
async fn connect_and_wait_boot<H: StationLike>(
    handle: &H,
    link: &mut LinkWatch<H::Snap>,
    cancel: &CancelToken,
) -> Result<(), String> {
    handle.connect().await.map_err(|e| e.to_string())?;
    let mut snapshots = handle.watch_snapshot();
    tokio::select! {
        registered = snapshots.wait_for(H::Snap::booted) => {
            registered.map(|_| ()).map_err(|_| "the station stopped".to_owned())
        }
        // No reconnect left (`--no-reconnect`, attempts used up): fail now, not at the timeout.
        dead = link.dead() => Err(dead),
        () = cancel.cancelled() => {
            let mut message =
                "timed out waiting for the central system to accept the BootNotification".to_owned();
            if let Some(last) = link.last_error() {
                message.push_str("; last error: ");
                message.push_str(&last);
            }
            Err(message)
        }
    }
}

pub(crate) async fn connect_and_run<H: StationLike + ScenarioTarget>(
    handle: &H,
    scenario: &Scenario,
    cancel: &CancelToken,
    overrides: &Vars,
    index: u64,
) -> Result<ScenarioReport, String> {
    let report = connect_and_run_with(handle, cancel, |events| {
        run_scenario_subscribed(handle, scenario, cancel, None, overrides, index, events)
    })
    .await?;
    drain_transaction_events(handle, cancel).await;
    Ok(report)
}

/// After a 2.x scenario: waits until the station delivered its queued transaction events, so
/// the CSMS sees the end of the session before the station disconnects. Bounded by `cancel`
/// (the `--timeout` limit); a queue that did not drain only prints a warning.
async fn drain_transaction_events<H: StationLike>(handle: &H, cancel: &CancelToken) {
    let mut snapshots = handle.watch_snapshot();
    tokio::select! {
        _ = snapshots.wait_for(|s| s.queued_events() == 0 || s.reconnect_stopped().is_some()) => {}
        () = cancel.cancelled() => {}
    }
    let left = snapshots.borrow().queued_events();
    if left > 0 {
        eprintln!("warning: {left} queued transaction event(s) were not delivered");
    }
}

/// Connects any station, waits for the boot and runs the scenario that `run` starts on the
/// event subscription it is given. The subscription is taken before connecting, so a CSMS
/// CALL that arrives right after the boot is still seen by `expect_call direction = "in"`.
pub(crate) async fn connect_and_run_with<H, F, Fut>(
    handle: &H,
    cancel: &CancelToken,
    run: F,
) -> Result<ScenarioReport, String>
where
    H: StationLike,
    F: FnOnce(tokio::sync::broadcast::Receiver<kirby_ocpp_core::station::Event>) -> Fut,
    Fut: std::future::Future<Output = ScenarioReport>,
{
    // Subscribed before connecting: a connection that fails at once must not be missed.
    let mut link = LinkWatch::new(handle);
    let events = StationLike::subscribe(handle);
    connect_and_wait_boot(handle, &mut link, cancel).await?;
    // The link can also go away while the scenario runs: end the run with that reason.
    tokio::select! {
        report = run(events) => Ok(report),
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

/// Writes the JUnit report of `runs`; `ocpp_version` is the station's, for a run that never
/// produced a report.
pub(crate) fn write_junit(
    path: &Path,
    ocpp_version: &str,
    runs: &[JunitRun<'_>],
) -> std::io::Result<()> {
    write_atomic(path, junit_xml_runs_for(ocpp_version, runs).as_bytes())
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kirby_ocpp_core::config::ReconnectConfig;
    use kirby_ocpp_core::station::Event;
    use kirby_ocpp_testkit::{CsmsOptions, MockCsms, SubprotocolPolicy};

    use super::*;

    const LIMIT: Duration = Duration::from_secs(20);

    fn config_for(url: &str, protocol: Protocol, reconnect: bool) -> StationConfig {
        let mut config = StationConfig::new(url)
            .with_reconnect(ReconnectConfig {
                enabled: reconnect,
                initial_delay_ms: 20,
                max_delay_ms: 50,
                max_attempts: 0,
            })
            .with_call_timeout_ms(2_000)
            .without_persistence();
        config.ws_ping_interval_secs = 0;
        config.protocol = protocol;
        config.ws_subprotocol = protocol.token().to_owned();
        config
    }

    fn unused_url() -> String {
        kirby_ocpp_testkit::refused_url()
    }

    #[test]
    fn mode_gate_allows_2_x_fleets_in_every_mode_but_replay() {
        let suite = Some(Cmd::Suite(SuiteArgs::for_sources(Vec::new())));
        let run = Cmd::Run(RunArgs {
            scenario: "smoke".to_owned(),
            report: None,
            junit: None,
            persist: false,
            timeout: 60,
            id_tag: None,
            vars: Vec::new(),
        });
        let replay = <Cli as clap::Parser>::try_parse_from(["kirby-ocpp", "replay", "rec.jsonl"])
            .unwrap()
            .command;
        assert!(matches!(replay, Some(Cmd::Replay(_))));
        assert_eq!(mode_gate(None, Protocol::V16), None);
        assert_eq!(mode_gate(replay.as_ref(), Protocol::V16), None);
        for protocol in [Protocol::V201, Protocol::V21] {
            assert_eq!(mode_gate(suite.as_ref(), protocol), None);
            assert_eq!(mode_gate(None, protocol), None);
            assert_eq!(mode_gate(Some(&run), protocol), None);
            assert_eq!(
                mode_gate(replay.as_ref(), protocol).as_deref(),
                Some(format!("`replay` is not available for {} yet", protocol.token()).as_str())
            );
        }
    }

    #[test]
    fn the_station_id_note_covers_length_characters_and_colons() {
        let note = |id: &str, protocol: Protocol| {
            let mut config = StationConfig::new("ws://h/ocpp").with_charge_point_id(id);
            config.protocol = protocol;
            station_id_note(&config)
        };
        assert_eq!(note("CP-001_a.b*=+|@", Protocol::V201), None);
        assert_eq!(note(&"A".repeat(48), Protocol::V201), None);
        assert!(
            note(&"A".repeat(49), Protocol::V201)
                .unwrap()
                .contains("longer than 48")
        );
        assert!(note("CP:1", Protocol::V201).unwrap().contains("a colon"));
        assert!(
            note("CP 1", Protocol::V201)
                .unwrap()
                .contains("outside identifierString")
        );
        // The shown id is bounded and free of control characters.
        let shown = note(&format!("{}\u{1b}[2J", "A".repeat(100)), Protocol::V201).unwrap();
        assert!(!shown.contains('\u{1b}') && shown.len() < 400, "{shown}");
        assert_eq!(note("CP:1", Protocol::V16), None);
    }

    // The boot wait is generic: these run it with a 2.0.1 station (needs a 2.0.1 station).

    #[tokio::test(flavor = "multi_thread")]
    async fn the_boot_wait_returns_once_a_2_0_1_station_is_accepted() {
        let csms = MockCsms::start(CsmsOptions {
            subprotocol: SubprotocolPolicy::Require("ocpp2.0.1".to_owned()),
            ..CsmsOptions::default()
        })
        .await
        .unwrap();
        let (handle, join) = Station2::spawn(
            config_for(csms.url(), Protocol::V201, true),
            Arc::new(SystemClock),
        );
        let mut link = LinkWatch::new(&handle);
        let cancel = CancelToken::new();
        let result =
            tokio::time::timeout(LIMIT, connect_and_wait_boot(&handle, &mut link, &cancel))
                .await
                .expect("the station boots in time");
        assert_eq!(result, Ok(()));
        stop_station(&handle, join).await;
        csms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_boot_wait_fails_at_once_when_no_reconnect_is_left() {
        let url = unused_url();
        let (handle, join) = Station2::spawn(
            config_for(&url, Protocol::V201, false),
            Arc::new(SystemClock),
        );
        let mut link = LinkWatch::new(&handle);
        let cancel = CancelToken::new();
        let result =
            tokio::time::timeout(LIMIT, connect_and_wait_boot(&handle, &mut link, &cancel))
                .await
                .expect("the wait ends without the cancel token");
        let message = result.unwrap_err();
        assert!(message.starts_with("reconnect stopped"), "{message}");
        assert!(message.contains("last error"), "{message}");
        stop_station(&handle, join).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_boot_wait_timeout_names_the_last_connection_error() {
        let url = unused_url();
        let (handle, join) = Station2::spawn(
            config_for(&url, Protocol::V201, true),
            Arc::new(SystemClock),
        );
        let mut link = LinkWatch::new(&handle);
        let cancel = CancelToken::new();
        // The deadline fires after the first failed attempt was logged.
        let mut events = handle.subscribe();
        let trigger = cancel.clone();
        let watcher = tokio::spawn(async move {
            while let Ok(event) = events.recv().await {
                if let Event::Log(record) = event
                    && record.text.starts_with("connection failed: ")
                {
                    trigger.cancel();
                    return;
                }
            }
        });
        let result =
            tokio::time::timeout(LIMIT, connect_and_wait_boot(&handle, &mut link, &cancel))
                .await
                .expect("the wait ends at the deadline");
        let message = result.unwrap_err();
        assert!(
            message.starts_with(
                "timed out waiting for the central system to accept the BootNotification"
            ),
            "{message}"
        );
        assert!(message.contains("; last error: "), "{message}");
        watcher.abort();
        stop_station(&handle, join).await;
    }
}
