//! `kirby-csms`: terminal OCPP 1.6J central system for charge point firmware development and
//! load tests. A terminal UI on an interactive terminal, headless otherwise.

mod api;
mod cli;
mod commands;
mod headless;
mod script;
mod stdin_cmd;

use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;
use kirby_csms_core::{
    CsmsConfig, CsmsEvent, CsmsHandle, CsmsServer, server_config_from_pem,
    server_config_with_client_ca,
};
use kirby_ocpp_core::scenario::{JunitSuite, ScenarioReport, StepStatus, junit_xml};
use tokio::sync::broadcast;

use crate::cli::Cli;
use crate::headless::{Output, Sink, sanitize};
use crate::script::Script;

/// Project license line and third-party notices (`--licenses`).
const LICENSES: &str = concat!(
    "kirby-csms is distributed under the MIT license (see LICENSE in the source repository).\n\n",
    include_str!("../THIRD_PARTY_NOTICES.md")
);

/// Bad usage or configuration.
const EXIT_USAGE: u8 = 2;
/// A script run stopped by SIGINT (128 + 2, the shell convention).
const EXIT_INTERRUPTED: u8 = 130;

#[tokio::main]
async fn main() -> ExitCode {
    let mut cli = Cli::parse();
    if cli.licenses {
        print!("{LICENSES}");
        return ExitCode::SUCCESS;
    }
    // Everything that can be wrong with the input is found before the server binds.
    let mut script = match cli.script.as_deref().map(Script::load).transpose() {
        Ok(script) => script,
        Err(error) => return usage_error(&error),
    };
    if let Some(script) = &mut script {
        for (name, value) in &cli.vars {
            if let Err(error) = script.set_var(name, value.clone()) {
                return usage_error(&format!("--var {name}: {error}"));
            }
        }
    }
    let config = match build_config(&mut cli) {
        Ok(config) => config,
        Err(error) => return usage_error(&error),
    };
    let api_plan = match prepare_api(&cli).await {
        Ok(plan) => plan,
        Err(code) => return code,
    };
    let out = match Output::open(cli.json, cli.log_file.as_deref()) {
        Ok(out) => out,
        Err(error) => return usage_error(&format!("cannot open the log file: {error}")),
    };

    let handle = match start_server(config).await {
        Ok(handle) => handle,
        Err(code) => return code,
    };
    // The terminal UI is for interactive runs only. The `listening` line goes to stderr before
    // the alternate screen opens, so it is still in the scrollback after the UI ends.
    let session = if cli.wants_tui(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    ) {
        eprintln!("listening on {}", handle.url());
        if let Some(plan) = &api_plan {
            eprintln!("api listening on http://{}", plan.addr);
        }
        startup_notes(&handle, cli.json, None);
        match kirby_csms_tui::Session::enter() {
            Ok(session) => Some(session),
            Err(error) => {
                eprintln!("note: terminal UI unavailable ({error}); running headless");
                None
            }
        }
    } else {
        None
    };
    if session.is_some() {
        // The screen belongs to the UI; `--log-file` still gets every event.
        out.set_stdout(false);
    }
    // Headless runs: register the signal handlers before `listening` is printed, so a signal
    // sent after that line is never lost to the default action.
    let mut shutdown = session.is_none().then(Shutdown::install);
    // Subscribe first, print `listening`, then start printing events: that line always comes
    // first and nothing is missed.
    let sink_events = handle.subscribe();
    let script_events = script.is_some().then(|| handle.subscribe());
    headless::announce(&out, &handle);
    if let Some(plan) = &api_plan {
        headless::announce_api(&out, plan.addr);
    }
    startup_notes(&handle, cli.json, Some(&out));
    let slots = commands::new_slots();
    let api =
        api_plan.map(|plan| api::spawn(plan.listener, handle.clone(), slots.clone(), plan.token));
    let sink = (session.is_none() || cli.log_file.is_some())
        .then(|| Sink::spawn(&handle, sink_events, out.clone()));

    // Stdin commands: on request, or by default when stdin is a pipe or a file and no script
    // is running. Never on a terminal by default (nobody types JSON lines unasked).
    let stdin = cli.stdin || (script.is_none() && !std::io::stdin().is_terminal());
    if stdin {
        stdin_cmd::spawn(handle.clone(), out.clone(), slots);
    }

    let code = if let Some(session) = session {
        match session
            .run(handle.clone(), kirby_csms_tui::TuiOptions::default())
            .await
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        }
    } else if let (Some(script), Some(events)) = (&script, script_events) {
        run_script(&cli, script, &handle, events, &out, shutdown.as_mut()).await
    } else {
        let _ = wait_shutdown(shutdown.as_mut()).await;
        ExitCode::SUCCESS
    };
    // The API stops last: event streams then still get the closing `disconnected` events.
    handle.shutdown().await;
    if let Some(api) = api {
        api.finish().await;
    }
    if let Some(sink) = sink {
        sink.finish().await;
    }
    out.finish();
    code
}

/// Applies the security flags and builds the engine configuration.
fn build_config(cli: &mut Cli) -> Result<CsmsConfig, String> {
    cli.apply_env_auth_keys(std::env::var("KIRBY_CSMS_AUTH_KEY").ok().as_deref())?;
    cli.validate_security()?;
    let mut config = cli.csms_config();
    if let Some(path) = &cli.auth_keys_file {
        merge_auth_keys_file(&mut config, path)?;
    }
    cli.require_keys(&config)?;
    if let (Some(cert), Some(key)) = (&cli.tls_cert, &cli.tls_key) {
        load_tls(&mut config, cert, key, cli.tls_client_ca.as_deref())?;
    }
    Ok(config)
}

/// The bound API listener with its address and token.
struct ApiPlan {
    listener: tokio::net::TcpListener,
    addr: std::net::SocketAddr,
    token: Option<String>,
}

/// Validates the API flags and binds its listener (before the CSMS starts, so a busy port
/// fails early). `None` when `--api-listen` is not given.
async fn prepare_api(cli: &Cli) -> Result<Option<ApiPlan>, ExitCode> {
    let token = cli.api_token().map_err(|error| usage_error(&error))?;
    let Some(addr) = cli.api_listen else {
        return Ok(None);
    };
    let listener = api::bind(addr).await.map_err(|error| {
        eprintln!("error: cannot bind the API listener {addr}: {error}");
        ExitCode::FAILURE
    })?;
    let addr = listener.local_addr().unwrap_or(addr);
    Ok(Some(ApiPlan {
        listener,
        addr,
        token,
    }))
}

/// Adds the keys of an `--auth-keys-file`; entries given on the command line win.
fn merge_auth_keys_file(config: &mut CsmsConfig, path: &Path) -> Result<(), String> {
    use std::io::Read as _;
    cli::warn_if_shared(path);
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(cli::AUTH_KEYS_FILE_MAX + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if bytes.len() as u64 > cli::AUTH_KEYS_FILE_MAX {
        return Err(format!(
            "{} is larger than {} bytes",
            path.display(),
            cli::AUTH_KEYS_FILE_MAX
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("cannot read {}: not valid UTF-8", path.display()))?;
    let keys = cli::parse_auth_keys_text(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    for (id, key) in keys {
        config.security.keys.entry(id).or_insert(key);
    }
    Ok(())
}

/// Extra startup lines on stderr, after the `listening` line: a warning for an open profile 0
/// listener on a non-loopback address, and how stations connect (omitted with `--json`).
/// Printed once, whichever path reaches it first (the TUI path prints before the screen opens).
fn startup_notes(handle: &kirby_csms_core::CsmsHandle, json: bool, out: Option<&Output>) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| print_startup_notes(handle, json, out));
}

/// Headless runs print the notes through `out` (the same ordered queue as the `listening`
/// line, so the lines are whole and in order on one stream); the terminal UI path prints them
/// on stderr before the screen opens. Each line goes out in one write.
fn print_startup_notes(handle: &kirby_csms_core::CsmsHandle, json: bool, out: Option<&Output>) {
    let addr = handle.local_addr();
    if handle.security_profile().number() == 0 && !addr.ip().is_loopback() {
        let text = "security profile 0 on a non-loopback address: any peer can connect and take over a station id; see docs/security.md";
        match out {
            Some(out) => out.event(
                &serde_json::json!({"type": "warning", "text": text}),
                &format!("warning: {text}"),
            ),
            None => stderr_line(&format!("warning: {text}")),
        }
    }
    if !json {
        let line = format!(
            "stations connect to {}",
            kirby_csms_tui::ui::station_url_example(&handle.url(), true)
        );
        match out {
            Some(out) => out.event(&serde_json::json!({"type": "note", "text": line}), &line),
            None => stderr_line(&line),
        }
    }
}

/// Writes one line to stderr in a single write, so it cannot be split by another writer.
fn stderr_line(line: &str) {
    use std::io::Write as _;
    let _ = std::io::stderr().write_all(format!("{line}\n").as_bytes());
}

/// Starts the server; a bind error names the address that could not be bound.
async fn start_server(
    config: kirby_csms_core::CsmsConfig,
) -> Result<kirby_csms_core::CsmsHandle, ExitCode> {
    let listen = config.listen;
    match CsmsServer::start(config).await {
        Ok(handle) => Ok(handle),
        Err(kirby_csms_core::CsmsError::Bind(error)) => {
            eprintln!("error: failed to bind the CSMS listener {listen}: {error}");
            Err(ExitCode::FAILURE)
        }
        Err(error) => {
            eprintln!("error: {error}");
            Err(ExitCode::FAILURE)
        }
    }
}

fn usage_error(error: &str) -> ExitCode {
    eprintln!("error: {error}");
    ExitCode::from(EXIT_USAGE)
}

fn load_tls(
    config: &mut CsmsConfig,
    cert: &Path,
    key: &Path,
    client_ca: Option<&Path>,
) -> Result<(), String> {
    let read = |path: &Path| {
        std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
    };
    let (cert, key) = (read(cert)?, read(key)?);
    let tls = match client_ca {
        Some(ca) => server_config_with_client_ca(&cert, &key, &read(ca)?),
        None => server_config_from_pem(&cert, &key),
    };
    config.tls = Some(tls.map_err(|e| e.to_string())?);
    Ok(())
}

/// SIGINT and SIGTERM, registered up front so no signal is lost between startup and the wait.
struct Shutdown {
    #[cfg(unix)]
    term: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    int: Option<tokio::signal::unix::Signal>,
}

impl Shutdown {
    fn install() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Self {
                term: signal(SignalKind::terminate()).ok(),
                int: signal(SignalKind::interrupt()).ok(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    /// Resolves on SIGINT or SIGTERM; `true` means SIGINT (Ctrl-C).
    async fn wait(&mut self) -> bool {
        #[cfg(unix)]
        {
            async fn recv(signal: &mut Option<tokio::signal::unix::Signal>) {
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
                    () = recv(&mut self.term) => false,
                };
            }
            tokio::select! {
                () = recv(&mut self.int) => true,
                () = recv(&mut self.term) => false,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            true
        }
    }
}

/// Waits for a signal; never resolves without handlers (the terminal UI owns the signals then).
async fn wait_shutdown(shutdown: Option<&mut Shutdown>) -> bool {
    match shutdown {
        Some(shutdown) => shutdown.wait().await,
        None => std::future::pending().await,
    }
}

/// Runs the script against the live server, prints and writes the reports.
async fn run_script(
    cli: &Cli,
    script: &Script,
    handle: &CsmsHandle,
    events: broadcast::Receiver<CsmsEvent>,
    out: &Output,
    shutdown: Option<&mut Shutdown>,
) -> ExitCode {
    // `--timeout` is range-checked by clap; `checked_add` keeps a future change from panicking.
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(cli.timeout))
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400));
    let mut interrupted = false;
    let report = tokio::select! {
        report = script::run(script, handle, events, deadline) => Some(report),
        sigint = wait_shutdown(shutdown) => {
            interrupted = sigint;
            None
        }
    };
    let Some(report) = report else {
        eprintln!("error: interrupted");
        // Like `kirby-ocpp run`: SIGTERM (a CI cancel) still leaves a JUnit file saying why;
        // Ctrl-C is a deliberate abort and leaves nothing.
        if !interrupted && let Some(path) = &cli.junit {
            let suite = JunitSuite {
                station: None,
                scenario: &script.name,
                outcome: Err("interrupted"),
            };
            if let Err(error) = std::fs::write(path, junit_xml(&[suite])) {
                eprintln!("error: cannot write the JUnit report: {error}");
            }
        }
        return ExitCode::from(if interrupted { EXIT_INTERRUPTED } else { 1 });
    };
    print_report(&report, out);
    let mut write_failed = false;
    if let Some(path) = &cli.report
        && let Err(error) = write_report(path, &report)
    {
        eprintln!("error: cannot write the report: {error}");
        write_failed = true;
    }
    if let Some(path) = &cli.junit {
        let suite = JunitSuite {
            station: None,
            scenario: &report.scenario,
            outcome: Ok(&report),
        };
        if let Err(error) = std::fs::write(path, junit_xml(&[suite])) {
            eprintln!("error: cannot write the JUnit report: {error}");
            write_failed = true;
        }
    }
    if write_failed || !report.passed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Prints a script report like `kirby-ocpp run` does.
fn print_report(report: &ScenarioReport, out: &Output) {
    let mut value = serde_json::to_value(report).unwrap_or_default();
    if let Some(object) = value.as_object_mut() {
        object.insert("type".to_owned(), "report".into());
    }
    // Texts from stations (CALLERROR descriptions) end up in step names and reasons: one line
    // each, so a station cannot forge `PASS` lines.
    let mut human = format!("script {}", sanitize(&report.scenario));
    for step in &report.steps {
        let (mark, detail) = match &step.status {
            StepStatus::Passed => ("PASS", String::new()),
            StepStatus::Failed(reason) => ("FAIL", format!(": {}", sanitize(reason))),
            StepStatus::Skipped => ("SKIP", String::new()),
        };
        let _ = write!(
            human,
            "\n  {mark} {:>2} {} ({} ms){detail}",
            step.index + 1,
            sanitize(&step.name),
            step.duration_ms
        );
    }
    let _ = write!(
        human,
        "\n{} in {} ms",
        if report.passed { "PASSED" } else { "FAILED" },
        report.duration_ms
    );
    out.event(&value, &human);
}

fn write_report(path: &Path, report: &ScenarioReport) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(report).map_err(std::io::Error::other)?;
    std::fs::write(path, text + "\n")
}
