//! `kirby-csms ctl`: a command-line client for the HTTP control API, for scripts and CI. It
//! sends the requests of `docs/kirby-csms-openapi.json` and prints the answers.
//!
//! Exit codes: 0 success, 1 the API answered with an error status, 2 usage error, 3 the API
//! could not be reached or its answer is unusable, 4 a call went through the API but the
//! station did not answer it (`"ok":false`).

mod client;
mod events;
mod out;
mod requests;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use serde_json::Value;

use self::client::{Client, Endpoint, Reply, Request, TlsFiles};
use self::events::EventsArgs;
use self::out::{BROKEN_PIPE, Printer, terminal_text};
use self::requests::{
    FaultInput, HistoryFilter, Kind, PayloadArgs, Spec, TransactionFilter, parse_millis,
};

/// The API answered with an error status.
const EXIT_API: u8 = 1;
/// Bad usage or configuration.
const EXIT_USAGE: u8 = 2;
/// The API could not be reached, or its answer could not be used.
const EXIT_TRANSPORT: u8 = 3;
/// A call reached the API but the station did not answer it.
const EXIT_NOT_OK: u8 = 4;

/// Default of `--url`: the example address of the documentation.
const DEFAULT_URL: &str = "http://127.0.0.1:9100";

/// Command-line client for the HTTP control API of a running `kirby-csms`.
///
/// Prints the JSON the API returns (indented on a terminal or with --pretty). The token is
/// read from KIRBY_CSMS_API_TOKEN or --token-file, never from a flag.
#[derive(Debug, Parser)]
#[command(
    name = "ctl",
    bin_name = "kirby-csms ctl",
    disable_help_subcommand = true,
    arg_required_else_help = true,
    after_help = "Examples:\n  \
  kirby-csms ctl stations\n  \
  kirby-csms ctl call CP-1 Reset --payload '{\"type\":\"Soft\"}'\n  \
  kirby-csms ctl bulk 'CP-*' GetConfiguration\n  \
  kirby-csms ctl export history CP-1 --as csv --output history.csv\n  \
  kirby-csms ctl events --station CP-1 --with-id\n\n\
Exit codes: 0 success, 1 the API answered with an error status (the error body goes to stderr), 2 usage error, 3 connection, TLS, timeout or protocol error, 4 a call was delivered but the station did not answer it (`ok` is false)."
)]
pub struct CtlCli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Args)]
struct Global {
    /// Base URL of the API (http:// or https://, no credentials)
    #[arg(
        long, global = true, env = "KIRBY_CSMS_API_URL", value_name = "URL",
        default_value = DEFAULT_URL
    )]
    url: String,

    /// File holding the API bearer token (a trailing newline is dropped). The environment
    /// variable KIRBY_CSMS_API_TOKEN is the alternative; both together are a usage error
    #[arg(
        long, global = true, env = "KIRBY_CSMS_API_TOKEN_FILE", value_name = "FILE",
        value_parser = non_empty_path
    )]
    token_file: Option<PathBuf>,

    /// PEM file with the CA certificate(s) that sign the API certificate (default: the
    /// system roots); ignored for http://
    #[arg(
        long, global = true, env = "KIRBY_CSMS_API_CA_FILE", value_name = "FILE",
        value_parser = non_empty_path
    )]
    ca_file: Option<PathBuf>,

    /// Client certificate chain (PEM) for an API that requires mutual TLS; needs --client-key
    #[arg(
        long, global = true, env = "KIRBY_CSMS_API_CLIENT_CERT", value_name = "FILE",
        value_parser = non_empty_path, requires = "client_key"
    )]
    client_cert: Option<PathBuf>,

    /// Private key (PEM) of the client certificate; needs --client-cert
    #[arg(
        long, global = true, env = "KIRBY_CSMS_API_CLIENT_KEY", value_name = "FILE",
        value_parser = non_empty_path, requires = "client_cert"
    )]
    client_key: Option<PathBuf>,

    /// Seconds a request may take, connect included (for `call` and `bulk` at least the
    /// call timeout plus 10 s); for `events` only the connect and the answer head
    #[arg(
        long, global = true, env = "KIRBY_CSMS_API_TIMEOUT", value_name = "SECONDS",
        default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=86_400)
    )]
    timeout: u64,

    /// Indent the JSON output (automatic on a terminal)
    #[arg(long, global = true)]
    pretty: bool,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Show the API health and the number of known stations
    Health,
    /// List the API routes and methods
    Index,
    /// List the known stations
    Stations,
    /// Show one station with its state
    Station {
        /// Station id
        id: String,
    },
    /// Forget a disconnected station
    Forget {
        /// Station id
        id: String,
    },
    /// Send one CALL to a station and print the outcome
    Call {
        /// Station id
        id: String,
        /// OCPP action, for example Reset
        action: String,
        #[command(flatten)]
        payload: PayloadArgs,
        /// How long the call may wait for the answer, milliseconds (default: the server's
        /// --call-timeout)
        #[arg(long, value_name = "MS", value_parser = parse_millis)]
        timeout_ms: Option<u32>,
    },
    /// Send one CALL to every known station that matches a glob (at most 64)
    Bulk {
        /// Station id pattern: `*` matches any characters, `?` one character
        pattern: String,
        /// OCPP action, for example Reset
        action: String,
        #[command(flatten)]
        payload: PayloadArgs,
        /// Only stations that negotiated this subprotocol, for example ocpp1.6
        #[arg(long, value_name = "NAME")]
        only_protocol: Option<String>,
        /// How long each call may wait for its answer, milliseconds
        #[arg(long, value_name = "MS", value_parser = parse_millis)]
        timeout_ms: Option<u32>,
    },
    /// Show the frames the server holds for a station, oldest first
    History {
        /// Station id
        id: String,
        #[command(flatten)]
        filter: HistoryFilter,
    },
    /// List the transactions the server tracks
    Transactions {
        #[command(flatten)]
        filter: TransactionFilter,
    },
    /// List, set and clear response rules
    #[command(subcommand)]
    Rules(RulesCmd),
    /// List, inject and remove faults
    #[command(subcommand)]
    Faults(FaultsCmd),
    /// Print the metrics (Prometheus text format)
    Metrics,
    /// Follow the event stream: one JSON line per event, with automatic resume
    Events(EventsArgs),
    /// Write a history or a transaction list as CSV or NDJSON
    #[command(subcommand)]
    Export(ExportCmd),
}

#[derive(Debug, Subcommand)]
enum RulesCmd {
    /// List the rules: the global ones, or those of one station
    List {
        /// Show the rules of this station
        #[arg(long, value_name = "ID")]
        station: Option<String>,
    },
    /// Show one station rule
    Show {
        /// Action of the rule
        action: String,
        /// Station id
        #[arg(long, value_name = "ID")]
        station: String,
    },
    /// Set the fixed CALLRESULT payload for an action
    Set {
        /// Action of the rule, for example Heartbeat
        action: String,
        /// Apply the rule to this station only (default: every station)
        #[arg(long, value_name = "ID")]
        station: Option<String>,
        /// Apply the rule to stations that negotiated this subprotocol only
        #[arg(long, value_name = "NAME")]
        only_protocol: Option<String>,
        #[command(flatten)]
        payload: PayloadArgs,
    },
    /// Remove a rule; the default answer applies again
    Clear {
        /// Action of the rule
        action: String,
        /// Remove the rule of this station (default: the global rule)
        #[arg(long, value_name = "ID")]
        station: Option<String>,
        /// Remove the global rule limited to this subprotocol
        #[arg(long, value_name = "NAME", conflicts_with = "station")]
        only_protocol: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum FaultsCmd {
    /// List the stored faults
    List,
    /// Inject a fault: a station's next CALLs of an action get a wrong answer
    Add {
        /// Action to disturb
        action: String,
        /// What happens: call_error, silence, close or invalid_payload
        #[arg(long, value_name = "KIND", value_parser = ["call_error", "silence", "close", "invalid_payload"])]
        kind: String,
        /// Number of CALLs to disturb (default 1)
        #[arg(long, value_name = "COUNT", value_parser = clap::value_parser!(u32).range(1..=1_000_000))]
        times: Option<u32>,
        /// Limit the fault to this station (default: every station)
        #[arg(long, value_name = "ID")]
        station: Option<String>,
        /// Limit the fault to stations that negotiated this subprotocol
        #[arg(long, value_name = "NAME")]
        only_protocol: Option<String>,
        /// The CALLRESULT sent for kind invalid_payload
        #[command(flatten)]
        payload: PayloadArgs,
    },
    /// Remove one fault by its id
    Remove {
        /// Fault id, for example f1
        id: String,
    },
    /// Remove every fault
    Clear,
}

#[derive(Debug, Subcommand)]
enum ExportCmd {
    /// Export the history of a station
    History {
        /// Station id
        id: String,
        #[command(flatten)]
        filter: HistoryFilter,
        /// Output format
        #[arg(long = "as", value_name = "FORMAT", default_value = "ndjson", value_parser = ["ndjson", "csv"])]
        format: String,
        /// Write to this file instead of standard output (mode 0600 on unix)
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Export the transactions
    Transactions {
        #[command(flatten)]
        filter: TransactionFilter,
        /// Output format
        #[arg(long = "as", value_name = "FORMAT", default_value = "csv", value_parser = ["csv", "json"])]
        format: String,
        /// Write to this file instead of standard output (mode 0600 on unix)
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
}

fn non_empty_path(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        Err("must not be empty".to_owned())
    } else {
        Ok(PathBuf::from(value))
    }
}

/// The clap command, shared by the real parse and by the help and man pages of `kirby-csms`.
pub fn command() -> clap::Command {
    CtlCli::command()
}

/// Why a run did not succeed.
#[derive(Debug)]
pub enum Fail {
    Usage(String),
    Transport(String),
    /// The API answered with this status and body.
    Api(u16, Vec<u8>),
    /// The reader of the output went away.
    Pipe,
}

impl Fail {
    fn output(message: String) -> Self {
        if message == BROKEN_PIPE {
            Self::Pipe
        } else {
            Self::Transport(message)
        }
    }
}

/// Runs `kirby-csms ctl ...` (the arguments are the process arguments).
pub async fn run() -> ExitCode {
    let mut matches = command()
        .bin_name("kirby-csms ctl")
        .try_get_matches_from(std::env::args_os().skip(1))
        .unwrap_or_else(|error| error.exit());
    let cli = CtlCli::from_arg_matches_mut(&mut matches).unwrap_or_else(|error| error.exit());
    match execute(&cli).await {
        Ok(code) => ExitCode::from(code),
        Err(fail) => report(fail),
    }
}

fn report(fail: Fail) -> ExitCode {
    match fail {
        Fail::Pipe => ExitCode::SUCCESS,
        Fail::Usage(message) => {
            eprintln!("error: {}", terminal_text(&message));
            ExitCode::from(EXIT_USAGE)
        }
        Fail::Transport(message) => {
            eprintln!("error: {}", terminal_text(&message));
            ExitCode::from(EXIT_TRANSPORT)
        }
        Fail::Api(status, body) => {
            if let Ok(text) = out::json_text(&body, false) {
                eprintln!("{text}");
            } else {
                let text = String::from_utf8_lossy(&body);
                let text: String = text.chars().take(200).collect();
                let text = terminal_text(text.trim());
                if text.is_empty() {
                    eprintln!("error: the API answered HTTP {status}");
                } else {
                    eprintln!("error: the API answered HTTP {status}: {text}");
                }
            }
            ExitCode::from(EXIT_API)
        }
    }
}

/// The token: `KIRBY_CSMS_API_TOKEN` or the token file, visible ASCII without spaces.
fn resolve_token(global: &Global) -> Result<Option<String>, Fail> {
    let from_env = match std::env::var_os("KIRBY_CSMS_API_TOKEN") {
        None => None,
        Some(value) => Some(
            value
                .into_string()
                .map_err(|_| Fail::Usage("KIRBY_CSMS_API_TOKEN is not valid UTF-8".to_owned()))?,
        ),
    };
    let token = match (from_env, &global.token_file) {
        (Some(_), Some(_)) => {
            return Err(Fail::Usage(
                "KIRBY_CSMS_API_TOKEN and --token-file are both set; use one".to_owned(),
            ));
        }
        (Some(token), None) => {
            if token.is_empty() {
                return Err(Fail::Usage("KIRBY_CSMS_API_TOKEN is empty".to_owned()));
            }
            Some(token)
        }
        (None, Some(path)) => {
            Some(crate::cli::read_key_file(path, "--token-file").map_err(Fail::Usage)?)
        }
        (None, None) => None,
    };
    if let Some(token) = &token
        && !token.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(Fail::Usage(
            "the API token must be visible ASCII without spaces or control characters".to_owned(),
        ));
    }
    Ok(token)
}

/// The deadline of a call: the call's own timeout plus time to travel.
fn call_deadline(timeout: Duration, timeout_ms: Option<u32>) -> Duration {
    match timeout_ms {
        Some(ms) => timeout.max(Duration::from_millis(u64::from(ms)) + Duration::from_secs(10)),
        None => timeout,
    }
}

fn read_payload(payload: &PayloadArgs) -> Result<Option<Value>, Fail> {
    payload.read().map_err(Fail::Usage)
}

async fn fetch(client: &Client, spec: &Spec, deadline: Duration) -> Result<Reply, Fail> {
    let body = spec
        .body
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|error| Fail::Usage(error.to_string()))?;
    let request = Request {
        method: spec.method,
        path: &spec.path,
        body: body.as_deref(),
        headers: &[],
    };
    let reply = client
        .send(&request, deadline)
        .await
        .map_err(Fail::Transport)?;
    if (200..300).contains(&reply.head.status) {
        Ok(reply)
    } else {
        Err(Fail::Api(reply.head.status, reply.body))
    }
}

fn show(printer: Printer, kind: Kind, body: &[u8]) -> Result<(), Fail> {
    match kind {
        Kind::Json => printer.json(body).map_err(Fail::output),
        Kind::Text => printer.text(body).map_err(Fail::output),
    }
}

/// Fetches and prints a request; the exit code is 0.
async fn simple(client: &Client, printer: Printer, spec: Spec) -> Result<u8, Fail> {
    let reply = fetch(client, &spec, client.timeout).await?;
    show(printer, spec.kind, &reply.body)?;
    Ok(0)
}

/// Whether every answer of a call reached `ok: true`.
fn all_ok(body: &[u8], bulk: bool) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let ok = |value: &Value| value.get("ok") == Some(&Value::Bool(true));
    if bulk {
        value
            .get("results")
            .and_then(Value::as_array)
            .is_some_and(|results| results.iter().all(ok))
    } else {
        ok(&value)
    }
}

#[cfg(unix)]
fn private_options() -> std::fs::OpenOptions {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true).mode(0o600);
    options
}

#[cfg(not(unix))]
fn private_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    options
}

/// Prints an export, or writes it to a file once the whole answer has arrived.
async fn export(
    client: &Client,
    printer: Printer,
    spec: Spec,
    output: Option<&std::path::Path>,
) -> Result<u8, Fail> {
    let reply = fetch(client, &spec, client.timeout).await?;
    if reply.head.header("x-truncated") == Some("true") {
        eprintln!("warning: more transactions matched than --limit; the list is truncated");
    }
    match output {
        Some(path) => {
            use std::io::Write as _;
            private_options()
                .open(path)
                .and_then(|mut file| file.write_all(&reply.body))
                .map_err(|error| {
                    Fail::Transport(format!("cannot write {}: {error}", path.display()))
                })?;
        }
        None => show(printer, spec.kind, &reply.body)?,
    }
    Ok(0)
}

async fn execute(cli: &CtlCli) -> Result<u8, Fail> {
    let global = &cli.global;
    let token = resolve_token(global)?;
    let endpoint = Endpoint::parse(&global.url).map_err(Fail::Usage)?;
    if token.is_some() && !endpoint.is_tls() && !endpoint.is_loopback() {
        eprintln!(
            "warning: the token is sent in clear text to {}; use https:// or a tunnel",
            endpoint.display()
        );
    }
    let files = TlsFiles {
        ca: global.ca_file.clone(),
        cert: global.client_cert.clone(),
        key: global.client_key.clone(),
    };
    let client = Client::new(endpoint, token, &files, Duration::from_secs(global.timeout))
        .map_err(Fail::Usage)?;
    let terminal = std::io::stdout().is_terminal();
    let printer = Printer {
        pretty: global.pretty || terminal,
        terminal,
    };
    match &cli.command {
        Cmd::Health => simple(&client, printer, requests::health()).await,
        Cmd::Index => simple(&client, printer, requests::index()).await,
        Cmd::Stations => simple(&client, printer, requests::stations()).await,
        Cmd::Station { id } => simple(&client, printer, requests::station(id)).await,
        Cmd::Forget { id } => simple(&client, printer, requests::forget(id)).await,
        Cmd::Call {
            id,
            action,
            payload,
            timeout_ms,
        } => {
            let spec = requests::call(id, action, read_payload(payload)?, *timeout_ms);
            let deadline = call_deadline(client.timeout, *timeout_ms);
            let reply = fetch(&client, &spec, deadline).await?;
            show(printer, spec.kind, &reply.body)?;
            Ok(if all_ok(&reply.body, false) {
                0
            } else {
                EXIT_NOT_OK
            })
        }
        Cmd::Bulk {
            pattern,
            action,
            payload,
            only_protocol,
            timeout_ms,
        } => {
            let spec = requests::bulk(
                pattern,
                action,
                read_payload(payload)?,
                only_protocol.as_deref(),
                *timeout_ms,
            );
            let deadline = call_deadline(client.timeout, *timeout_ms);
            let reply = fetch(&client, &spec, deadline).await?;
            show(printer, spec.kind, &reply.body)?;
            Ok(if all_ok(&reply.body, true) {
                0
            } else {
                EXIT_NOT_OK
            })
        }
        Cmd::History { id, filter } => {
            simple(&client, printer, requests::history(id, filter, None)).await
        }
        Cmd::Transactions { filter } => {
            simple(&client, printer, requests::transactions(filter, None)).await
        }
        Cmd::Rules(rules) => rules_command(&client, printer, rules).await,
        Cmd::Faults(faults) => faults_command(&client, printer, faults).await,
        Cmd::Metrics => simple(&client, printer, requests::metrics()).await,
        Cmd::Events(args) => {
            let mut emit = |line: &str| out::write_stdout(format!("{line}\n").as_bytes());
            events::tail(&client, args, &mut emit).await.map(|()| 0)
        }
        Cmd::Export(ExportCmd::History {
            id,
            filter,
            format,
            output,
        }) => {
            let spec = requests::history(id, filter, Some(format));
            export(&client, printer, spec, output.as_deref()).await
        }
        Cmd::Export(ExportCmd::Transactions {
            filter,
            format,
            output,
        }) => {
            let spec = requests::transactions(filter, Some(format));
            export(&client, printer, spec, output.as_deref()).await
        }
    }
}

async fn rules_command(client: &Client, printer: Printer, rules: &RulesCmd) -> Result<u8, Fail> {
    let spec = match rules {
        RulesCmd::List { station } => requests::rules_list(station.as_deref()),
        RulesCmd::Show { action, station } => requests::rule_show(station, action),
        RulesCmd::Set {
            action,
            station,
            only_protocol,
            payload,
        } => {
            let payload = read_payload(payload)?.ok_or_else(|| {
                Fail::Usage("rules set needs --payload or --payload-file".to_owned())
            })?;
            requests::rule_set(
                action,
                station.as_deref(),
                only_protocol.as_deref(),
                payload,
            )
        }
        RulesCmd::Clear {
            action,
            station,
            only_protocol,
        } => requests::rule_clear(action, station.as_deref(), only_protocol.as_deref()),
    };
    simple(client, printer, spec).await
}

async fn faults_command(client: &Client, printer: Printer, faults: &FaultsCmd) -> Result<u8, Fail> {
    let spec = match faults {
        FaultsCmd::List => requests::faults_list(),
        FaultsCmd::Add {
            action,
            kind,
            times,
            station,
            only_protocol,
            payload,
        } => {
            let payload = read_payload(payload)?;
            if kind == "invalid_payload" && payload.is_none() {
                return Err(Fail::Usage(
                    "--kind invalid_payload needs --payload or --payload-file".to_owned(),
                ));
            }
            requests::fault_add(FaultInput {
                action,
                kind,
                payload,
                times: *times,
                station: station.as_deref(),
                protocol: only_protocol.as_deref(),
            })
        }
        FaultsCmd::Remove { id } => requests::fault_remove(id),
        FaultsCmd::Clear => requests::faults_clear(),
    };
    simple(client, printer, spec).await
}
