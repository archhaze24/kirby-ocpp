//! Command-line interface.
//!
//! Mirrors the options of the TypeScript CLI (`src/cli.ts`, README) and fixes its
//! config bugs: precedence is **CLI flag > environment variable > default** (in the
//! TS CLI commander always supplied defaults, so the `OCPP_*` fallbacks were dead
//! code), and booleans are parsed strictly: only `true/false/t/f/yes/no/y/n/on/off/1/0` (any case)
//! are accepted (any other text is an error, whereas `z.coerce.boolean()` made every
//! non-empty string `true`).

use std::path::{Path, PathBuf};

use clap::builder::BoolishValueParser;
use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::config::{ReconnectConfig, TlsConfig, defaults};
use kirby_ocpp_core::fleet::{FleetSpec, MAX_COUNT};
use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::station::ResponseRule;

/// Terminal OCPP 1.6J charge point emulator for CSMS testing.
///
/// Without a subcommand it opens an interactive terminal UI (or, when stdout is not a
/// terminal or with --no-tui, connects and logs events to stdout until interrupted).
/// Precedence: command-line flag, then environment variable, then configuration file (--config), then default.
#[derive(Debug, Parser)]
#[command(
    name = "kirby-ocpp",
    version,
    about,
    after_help = "Examples:\n  \
  kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001\n  \
  kirby-ocpp --url wss://csms.example/ocpp --tls-ca ca.pem --connectors 2\n  \
  kirby-ocpp --url ws://localhost:9000/ocpp run --scenario charge-session\n  \
  kirby-ocpp --url ws://localhost:9000/ocpp --id 'CP-{n:03}' --count 10 --no-tui\n  \
  kirby-ocpp --url ws://localhost:9000/ocpp suite --all-builtins --junit junit.xml\n  \
  kirby-ocpp scenarios list\n\n\
Exit codes: 0 success, 1 failure (scenario step failed, runtime error), 2 usage or configuration error, 130 `run` or `suite` interrupted by SIGINT."
)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a CLI struct mirrors its flags one to one"
)]
pub struct Cli {
    /// Central System WebSocket URL, e.g. ws://localhost:9000/ocpp
    #[arg(
        help_heading = "Connection",
        global = true,
        short,
        long,
        env = "OCPP_URL",
        value_name = "URL",
        hide_env_values = true
    )]
    pub url: Option<String>,

    /// Charge point id
    #[arg(help_heading = "Identity", global = true,
        short, long, env = "OCPP_CHARGE_POINT_ID", value_name = "ID",
        default_value = defaults::CHARGE_POINT_ID, value_parser = non_empty
    )]
    pub id: String,

    /// Number of stations to run (fleet mode, 1-1000); the id needs an index placeholder such as CP-{n:03}
    #[arg(help_heading = "Fleet", global = true,
        long, env = "OCPP_COUNT", value_name = "COUNT", default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..=i64::from(MAX_COUNT))
    )]
    pub count: u32,

    /// Charge point id template, e.g. CP-{n:03} (default: the --id value)
    #[arg(help_heading = "Fleet", global = true, long, env = "OCPP_ID_TEMPLATE", value_name = "TEMPLATE", value_parser = non_empty)]
    pub id_template: Option<String>,

    /// Index of the first station of a fleet (the value its id placeholder gets)
    #[arg(
        help_heading = "Fleet",
        global = true,
        long,
        env = "OCPP_FIRST_INDEX",
        value_name = "INDEX",
        default_value_t = 1
    )]
    pub first_index: u32,

    /// Delay between starting two stations of a fleet in milliseconds
    #[arg(
        help_heading = "Fleet",
        global = true,
        long,
        env = "OCPP_STAGGER_MS",
        value_name = "MILLISECONDS",
        default_value_t = 0
    )]
    pub stagger_ms: u64,

    /// Charge point vendor
    #[arg(help_heading = "Identity", global = true,
        long, env = "OCPP_VENDOR", value_name = "VENDOR",
        default_value = defaults::VENDOR, value_parser = non_empty
    )]
    pub vendor: String,

    /// Charge point model
    #[arg(help_heading = "Identity", global = true,
        long, env = "OCPP_MODEL", value_name = "MODEL",
        default_value = defaults::MODEL, value_parser = non_empty
    )]
    pub model: String,

    /// Charge point serial number reported in BootNotification (at most 25 characters)
    #[arg(help_heading = "Identity", global = true, long, env = "OCPP_SERIAL", value_name = "SERIAL", value_parser = non_empty)]
    pub serial: Option<String>,

    /// Firmware version reported in BootNotification (at most 50 characters)
    #[arg(help_heading = "Identity", global = true,
        long = "firmware-version", env = "OCPP_FIRMWARE_VERSION", value_name = "VERSION",
        value_parser = non_empty
    )]
    pub firmware_version: Option<String>,

    /// Initial connector id
    #[arg(help_heading = "Station", global = true,
        short, long, value_name = "ID", default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub connector: u32,

    /// Number of station connectors (raised to --connector if smaller)
    #[arg(help_heading = "Station", global = true,
        long, value_name = "COUNT", default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub connectors: u32,

    /// Fallback heartbeat interval in seconds
    #[arg(help_heading = "Station", global = true,
        long, value_name = "SECONDS", default_value_t = defaults::HEARTBEAT_INTERVAL_SECS,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub heartbeat: u32,

    /// Nominal charging power of each connector in watts
    #[arg(help_heading = "Station", global = true,
        long, value_name = "WATTS", default_value_t = defaults::NOMINAL_POWER_W,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub nominal_power_w: u32,

    /// Supply voltage per phase in volts
    #[arg(help_heading = "Station", global = true,
        long, value_name = "VOLTS", default_value_t = defaults::VOLTAGE_V,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub voltage_v: u32,

    /// Number of charging phases (1-3)
    #[arg(help_heading = "Station", global = true,
        long, value_name = "COUNT", default_value_t = defaults::PHASES,
        value_parser = clap::value_parser!(u8).range(1..=3)
    )]
    pub phases: u8,

    /// Do not save station state such as local auth list and charging profiles
    #[arg(help_heading = "Station", global = true,
        long, env = "KIRBY_OCPP_NO_PERSIST", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub no_persist: bool,

    /// Directory for persisted station state
    #[arg(help_heading = "Station", global = true, long, env = "KIRBY_OCPP_STATE_DIR", value_name = "PATH", value_parser = non_empty_path)]
    pub state_dir: Option<PathBuf>,

    /// WebSocket subprotocol to request
    #[arg(help_heading = "Connection", global = true,
        long, env = "OCPP_WS_SUBPROTOCOL", value_name = "PROTOCOL",
        default_value = defaults::WS_SUBPROTOCOL, value_parser = non_empty
    )]
    pub ws_subprotocol: String,

    /// WebSocket ping interval in seconds, 0 disables pings
    #[arg(help_heading = "Connection", global = true,
        long, env = "OCPP_WS_PING_INTERVAL", value_name = "SECONDS",
        default_value_t = defaults::WS_PING_INTERVAL_SECS
    )]
    pub ws_ping: u32,

    /// Enable or disable automatic reconnect after an unexpected disconnect
    #[arg(help_heading = "Reconnect", global = true,
        long, env = "OCPP_RECONNECT", value_name = "BOOL", default_value_t = true,
        action = ArgAction::Set, value_parser = BoolishValueParser::new()
    )]
    pub reconnect: bool,

    /// Do not automatically reconnect (wins over --reconnect and OCPP_RECONNECT)
    #[arg(help_heading = "Reconnect", global = true, long)]
    pub no_reconnect: bool,

    /// Initial automatic reconnect delay in milliseconds
    #[arg(help_heading = "Reconnect", global = true,
        long, env = "OCPP_RECONNECT_INITIAL_MS", value_name = "MILLISECONDS",
        default_value_t = defaults::RECONNECT_INITIAL_MS
    )]
    pub reconnect_initial_ms: u64,

    /// Maximum automatic reconnect delay in milliseconds
    #[arg(help_heading = "Reconnect", global = true,
        long, env = "OCPP_RECONNECT_MAX_MS", value_name = "MILLISECONDS",
        default_value_t = defaults::RECONNECT_MAX_MS
    )]
    pub reconnect_max_ms: u64,

    /// Maximum automatic reconnect attempts, 0 means unlimited
    #[arg(
        help_heading = "Reconnect",
        global = true,
        long,
        env = "OCPP_RECONNECT_MAX_ATTEMPTS",
        value_name = "ATTEMPTS",
        default_value_t = 0
    )]
    pub reconnect_max_attempts: u32,

    /// OCPP CALL response timeout in milliseconds
    #[arg(help_heading = "Connection", global = true,
        long, env = "OCPP_CALL_TIMEOUT_MS", value_name = "MILLISECONDS",
        default_value_t = defaults::CALL_TIMEOUT_MS,
        value_parser = clap::value_parser!(u64).range(1..=86_400_000)
    )]
    pub call_timeout_ms: u64,

    /// CA certificate bundle for wss:// connections
    #[arg(help_heading = "TLS", global = true, long, env = "OCPP_TLS_CA_FILE", value_name = "PATH", value_parser = non_empty_path)]
    pub tls_ca: Option<PathBuf>,

    /// Client certificate for wss:// mutual TLS
    #[arg(help_heading = "TLS", global = true, long, env = "OCPP_TLS_CERT_FILE", value_name = "PATH", value_parser = non_empty_path)]
    pub tls_cert: Option<PathBuf>,

    /// Client private key for wss:// mutual TLS
    #[arg(help_heading = "TLS", global = true, long, env = "OCPP_TLS_KEY_FILE", value_name = "PATH", value_parser = non_empty_path)]
    pub tls_key: Option<PathBuf>,

    /// TLS server name override for wss:// connections
    #[arg(help_heading = "TLS", global = true, long, env = "OCPP_TLS_SERVER_NAME", value_name = "NAME", value_parser = non_empty)]
    pub tls_server_name: Option<String>,

    /// Disable TLS certificate verification for local/self-signed wss:// testing
    #[arg(help_heading = "TLS", global = true,
        long, env = "OCPP_TLS_SKIP_VERIFY", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub tls_skip_verify: bool,

    /// Show AuthorizationKey values in frame logs (by default they are replaced by ***)
    #[arg(help_heading = "Output", global = true,
        long, env = "OCPP_SHOW_SECRETS", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub show_secrets: bool,

    /// Set the read-only AdditionalRootCertificateCheck key: a new Central System root must be signed by the installed one
    #[arg(help_heading = "TLS", global = true,
        long, env = "OCPP_ADDITIONAL_ROOT_CERTIFICATE_CHECK", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub additional_root_certificate_check: bool,

    /// Go back to the previous SecurityProfile when the Central System cannot be reached with a newly set one
    #[arg(help_heading = "TLS", global = true,
        long, env = "OCPP_SECURITY_PROFILE_FALLBACK", value_name = "BOOL", default_value_t = true,
        action = ArgAction::Set, value_parser = BoolishValueParser::new()
    )]
    pub security_profile_fallback: bool,

    /// Misbehave on purpose when the CSMS sends a command: ACTION=BEHAVIOR[*COUNT]. Repeatable
    #[arg(
        help_heading = "Faults",
        global = true,
        long = "rule",
        env = "KIRBY_OCPP_RULES",
        value_name = "ACTION=BEHAVIOR",
        value_delimiter = ';',
        value_parser = parse_rule,
        long_help = "Overrides how the station answers one CSMS command, to test the CSMS against a \
misbehaving charge point. Form: ACTION=BEHAVIOR[*COUNT]. BEHAVIOR is one of: reject (the \
action's own rejection status, else CALLERROR NotSupported), delay:MS (handle normally, reply \
MS ms later), error:CODE[:TEXT] (CALLERROR with an OCPP-J error code), drop (close the \
connection without replying), ignore (never reply). *COUNT limits the rule to that many \
CALLs. Repeat the flag or separate several rules with `;` (also in KIRBY_OCPP_RULES); a rule \
must not contain `;`. Examples: --rule Reset=reject --rule GetConfiguration=delay:5000 \
--rule ChangeConfiguration=error:InternalError*3"
    )]
    pub rule: Vec<ResponseRule>,

    /// Read settings from this TOML file; flags and environment variables win over it
    #[arg(
        help_heading = "Configuration",
        global = true,
        long,
        env = "KIRBY_OCPP_CONFIG",
        value_name = "FILE",
        value_parser = non_empty_path
    )]
    pub config: Option<PathBuf>,

    /// Use the `[profile.NAME]` tables of the configuration file on top of its other values
    #[arg(
        help_heading = "Configuration",
        global = true,
        long,
        env = "KIRBY_OCPP_PROFILE",
        value_name = "NAME",
        value_parser = non_empty
    )]
    pub profile: Option<String>,

    /// Print the effective configuration and exit: JSON, or with `=toml` a configuration file for --config
    #[arg(
        help_heading = "Output",
        global = true,
        long,
        value_name = "FORMAT",
        value_enum,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "json"
    )]
    pub print_config: Option<PrintFormat>,

    /// Never start the terminal UI; log to stdout instead (automatic when stdout is not a terminal)
    #[arg(help_heading = "Output", global = true, long)]
    pub no_tui: bool,

    /// Machine-readable output: one JSON object per line (events, and the report of `run`); never opens the terminal UI
    #[arg(help_heading = "Output", global = true, long)]
    pub json: bool,

    /// Also append the event log to this file (with the terminal UI too)
    #[arg(help_heading = "Output", global = true, long, value_name = "PATH", value_parser = non_empty_path)]
    pub log_file: Option<PathBuf>,

    /// Show the event and frame lines of `run` and `suite` too (plain headless mode always prints them)
    #[arg(
        help_heading = "Output",
        global = true,
        short,
        long,
        conflicts_with = "quiet"
    )]
    pub verbose: bool,

    /// Print only errors and the final verdict line in `run` and `suite`
    #[arg(help_heading = "Output", global = true, short, long)]
    pub quiet: bool,

    /// Colorize PASS, FAIL and warnings in human output: auto (only on a terminal and without NO_COLOR), always or never
    #[arg(
        help_heading = "Output",
        global = true,
        long,
        value_name = "WHEN",
        value_enum,
        default_value = "auto"
    )]
    pub color: ColorChoice,

    /// Print the third-party license notices and exit
    #[arg(help_heading = "Output", long)]
    pub licenses: bool,

    #[command(subcommand)]
    pub command: Option<Cmd>,

    /// Whether the id came from `--id`, `OCPP_CHARGE_POINT_ID` or a configuration file
    /// rather than the default (`state` needs to know).
    #[arg(skip)]
    pub id_explicit: bool,
}

/// `--print-config` formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PrintFormat {
    /// The station configuration as JSON.
    Json,
    /// A configuration file that `--config` accepts.
    Toml,
}

/// `--color` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    /// Only when stdout is a terminal and `NO_COLOR` is not set.
    Auto,
    /// Always, even in a pipe.
    Always,
    /// Never.
    Never,
}

/// Subcommands. Without one, the UI (or the headless logger) runs.
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Connect, run a scenario, print the report; exit code 1 if any step fails
    Run(RunArgs),
    /// Run several scenarios one after another, each on a fresh station; exit code 1 if any fails
    Suite(SuiteArgs),
    /// Check the way to the Central System step by step (URL, DNS, TCP, TLS, WebSocket upgrade,
    /// BootNotification); exit code 1 if a step fails
    Check(CheckArgs),
    /// Show, locate or delete the persisted state of a station
    State {
        #[command(subcommand)]
        action: StateCmd,
    },
    /// Built-in scenarios, and checking scenario files
    Scenarios {
        #[command(subcommand)]
        action: ScenariosCmd,
    },
    /// Print the version, git commit, build date, target and supported protocols (with --json as a JSON object)
    Version,
    /// Print a shell completion script to stdout
    Completions {
        /// Shell to generate the script for
        #[arg(value_enum, value_name = "SHELL")]
        shell: clap_complete::Shell,
    },
}

/// Arguments of `check`; the connection flags are the global ones.
#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Give up after this many seconds in total (one connection step is also limited to 10)
    #[arg(
        help_heading = "Check",
        long, value_name = "SECONDS", default_value_t = 30,
        value_parser = clap::value_parser!(u64).range(1..=3_600)
    )]
    pub timeout: u64,
}

/// What `state` does. The station is the global `--id`; the directory is `--state-dir`.
#[derive(Debug, Subcommand)]
pub enum StateCmd {
    /// Print the state directory, or with --id the state file of that station; creates nothing
    Path,
    /// Print what the state of the station --id holds; secrets are not read or printed
    Show,
    /// Delete the persisted state of the station --id (state file, secrets file, backups)
    Reset {
        /// Delete without asking (required when stdin is not a terminal)
        #[arg(help_heading = "State", short = 'y', long)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Scenario: a built-in name (see `scenarios list`) or a TOML file
    #[arg(help_heading = "Scenarios", long, value_name = "FILE|NAME")]
    pub scenario: String,

    /// Write the report as JSON to this file
    #[arg(help_heading = "Scenarios", long, value_name = "PATH", value_parser = non_empty_path)]
    pub report: Option<PathBuf>,

    /// Write a JUnit XML report to this file (one file; one test suite per station in a fleet)
    #[arg(help_heading = "Scenarios", long, value_name = "PATH", value_parser = non_empty_path)]
    pub junit: Option<PathBuf>,

    /// Keep station state between runs: read and write the state file (a `run` is ephemeral by default)
    #[arg(help_heading = "Scenarios", long, conflicts_with = "no_persist")]
    pub persist: bool,

    /// Give up (and fail) after this many seconds, connecting included
    #[arg(
        help_heading = "Scenarios",
        long, value_name = "SECONDS", default_value_t = 60,
        value_parser = clap::value_parser!(u64).range(1..=86_400)
    )]
    pub timeout: u64,

    /// Replace the idTag of the scenario's authorize / start_tx / stop_tx steps
    #[arg(help_heading = "Scenarios", long, value_name = "IDTAG", value_parser = non_empty)]
    pub id_tag: Option<String>,

    /// Set a scenario variable (repeatable): `${NAME}` in the scenario becomes VALUE.
    /// Overrides `[vars]`; `true`/`false`/numbers keep their type. `station_id` and `index`
    /// are built in and cannot be set.
    #[arg(help_heading = "Scenarios", long = "var", value_name = "NAME=VALUE", value_parser = scenario_var)]
    pub vars: Vec<ScenarioVar>,
}

/// A parsed `--var`: name, typed value and the value as written.
pub type ScenarioVar = (String, serde_json::Value, String);

#[derive(Debug, Args)]
pub struct SuiteArgs {
    /// Scenario sources: a TOML file, a directory (every `*.toml` directly inside, sorted by
    /// name) or a built-in name (see `scenarios list`)
    #[arg(value_name = "SOURCE")]
    pub sources: Vec<String>,

    /// Also run every built-in scenario except `finish-session`, which needs a transaction left by an earlier run
    #[arg(help_heading = "Scenarios", long)]
    pub all_builtins: bool,

    /// Write the suite summary as JSON to this file
    #[arg(help_heading = "Scenarios", long, value_name = "PATH", value_parser = non_empty_path)]
    pub report: Option<PathBuf>,

    /// Write one JUnit XML report to this file (one test suite per scenario)
    #[arg(help_heading = "Scenarios", long, value_name = "PATH", value_parser = non_empty_path)]
    pub junit: Option<PathBuf>,

    /// Give up on a scenario (and fail it) after this many seconds, connecting included
    #[arg(
        help_heading = "Scenarios",
        long, value_name = "SECONDS", default_value_t = 60,
        value_parser = clap::value_parser!(u64).range(1..=86_400)
    )]
    pub timeout: u64,

    /// Stop after the first scenario that fails; the rest are reported as SKIP
    #[arg(help_heading = "Scenarios", long)]
    pub fail_fast: bool,

    /// Set a scenario variable for every scenario (repeatable), see `run --var`
    #[arg(help_heading = "Scenarios", long = "var", value_name = "NAME=VALUE", value_parser = scenario_var)]
    pub vars: Vec<ScenarioVar>,

    /// Run only scenarios whose name matches this glob (`*` and `?`; repeatable, any match selects)
    #[arg(help_heading = "Selection", long, value_name = "GLOB", value_parser = non_empty)]
    pub filter: Vec<String>,

    /// Leave out scenarios whose name matches this glob (repeatable), applied after `--filter`
    #[arg(help_heading = "Selection", long, value_name = "GLOB", value_parser = non_empty)]
    pub skip: Vec<String>,

    /// Run only scenarios that carry this tag (repeatable: a scenario needs every given tag)
    #[arg(help_heading = "Selection", long, value_name = "TAG", value_parser = non_empty)]
    pub tag: Vec<String>,

    /// Leave out scenarios that carry this tag (repeatable: any given tag leaves a scenario out)
    #[arg(help_heading = "Selection", long, value_name = "TAG", value_parser = non_empty)]
    pub skip_tag: Vec<String>,

    /// Run a failed scenario again up to this many times on a fresh station; a scenario that passes
    /// after a failure is reported as flaky and counts as passed
    #[arg(
        help_heading = "Scenarios",
        long, value_name = "N", default_value_t = 0,
        value_parser = clap::value_parser!(u32).range(0..=10)
    )]
    pub retries: u32,

    /// Run up to this many scenarios at the same time; slot 2 and up use the station id with
    /// `-2`, `-3`, ... appended. Reports keep the order of the selection
    #[arg(
        help_heading = "Scenarios",
        long, value_name = "N", default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..=16)
    )]
    pub jobs: u32,

    /// Print a GitHub Actions `::error` or `::warning` workflow command for every failed or
    /// flaky scenario (not with `--json`)
    #[arg(help_heading = "CI output", long)]
    pub github_annotations: bool,

    /// Write a Markdown summary table to this file; the summary is also appended to the file
    /// named by `$GITHUB_STEP_SUMMARY` when that variable is set
    #[arg(help_heading = "CI output", long, value_name = "PATH", value_parser = non_empty_path)]
    pub summary_md: Option<PathBuf>,
}

impl SuiteArgs {
    /// The arguments of a suite over `sources` with every option at its default.
    pub fn for_sources(sources: Vec<String>) -> Self {
        Self {
            sources,
            all_builtins: false,
            report: None,
            junit: None,
            timeout: 60,
            fail_fast: false,
            vars: Vec::new(),
            filter: Vec::new(),
            skip: Vec::new(),
            tag: Vec::new(),
            skip_tag: Vec::new(),
            retries: 0,
            jobs: 1,
            github_annotations: false,
            summary_md: None,
        }
    }
}

/// Parses a `--var` argument.
fn scenario_var(text: &str) -> Result<ScenarioVar, String> {
    let (name, value, written) = kirby_ocpp_core::scenario::vars::parse_assignment_text(text)?;
    if kirby_ocpp_core::scenario::vars::is_builtin(&name) {
        return Err(format!("`{name}` is a built-in variable and cannot be set"));
    }
    Ok((name, value, written))
}

#[derive(Debug, Subcommand)]
pub enum ScenariosCmd {
    /// List the built-in scenarios
    List,
    /// Print a built-in scenario as TOML
    Show { name: String },
    /// Check scenario files, directories or built-in names without connecting; exit 2 if any is invalid
    Validate {
        /// Scenario file, directory (`*.toml` directly inside) or built-in name
        #[arg(required = true, value_name = "FILE|DIR|NAME", value_hint = clap::ValueHint::AnyPath)]
        paths: Vec<String>,
    },
}

/// Shown at the end of `run --help` and `suite --help`.
const STATION_FLAGS_NOTE: &str =
    "Connection, station, TLS and output flags are listed in `kirby-ocpp --help`.";

/// Subcommands that never connect: their help hides the global station flags.
/// The flags still parse there, so existing command lines keep working.
/// Each entry lists the global flags that stay visible.
const OFFLINE_SUBCOMMANDS: &[(&str, &[&str])] = &[
    ("scenarios", &[]),
    ("completions", &[]),
    ("version", &["json"]),
    ("state", &["id", "state_dir", "json"]),
];

/// Redefines the global arguments `hidden` on `command` and its nested subcommands as
/// hidden copies. (Clap propagates globals only while parsing, so `mut_arg` cannot reach
/// them; a copy defined up front wins over the propagated one and parses identically.)
fn hide_global_args(mut command: clap::Command, hidden: &[clap::Arg]) -> clap::Command {
    for arg in hidden {
        command = command.arg(arg.clone().hide(true));
    }
    let nested: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect();
    for name in nested {
        command = command.mut_subcommand(name, |sub| hide_global_args(sub, hidden));
    }
    command
}

/// The clap command with per-subcommand help tuned: `run` and `suite` point at the
/// top-level help for the station flags; offline subcommands do not list them.
pub fn command() -> clap::Command {
    use clap::CommandFactory;
    let mut command = Cli::command();
    let globals: Vec<clap::Arg> = command
        .get_arguments()
        .filter(|arg| arg.is_global_set())
        .cloned()
        .collect();
    for name in ["run", "suite", "check"] {
        command = command.mut_subcommand(name, |sub| sub.after_help(STATION_FLAGS_NOTE));
    }
    for (name, keep) in OFFLINE_SUBCOMMANDS {
        let hidden: Vec<clap::Arg> = globals
            .iter()
            .filter(|arg| !keep.contains(&arg.get_id().as_str()))
            .cloned()
            .collect();
        command = command.mut_subcommand(*name, |sub| hide_global_args(sub, &hidden));
    }
    // `version` keeps `--json` with a help text of its own (the global one talks about
    // events and reports).
    if let Some(json) = globals.iter().find(|arg| arg.get_id() == "json") {
        let json = json
            .clone()
            .help("Print the version details as one JSON object")
            .long_help("Print the version details as one JSON object");
        command = command.mut_subcommand("version", |sub| sub.arg(json));
    }
    // The same for `state`: its flags `--id` and `--json` mean something else there.
    let on_state = [(
        "Charge point id of the station (needed by show and reset)",
        "Print the result as one JSON object",
    )];
    for (id_help, json_help) in on_state {
        let id = globals.iter().find(|arg| arg.get_id() == "id").cloned();
        let json = globals.iter().find(|arg| arg.get_id() == "json").cloned();
        if let (Some(id), Some(json)) = (id, json) {
            let id = id.help(id_help).long_help(id_help).hide_default_value(true);
            let json = json.help(json_help).long_help(json_help);
            command = command.mut_subcommand("state", |state| state.arg(id).arg(json));
        }
    }
    for (name, id_help, json_help) in STATE_HELP {
        let id = globals.iter().find(|arg| arg.get_id() == "id").cloned();
        let json = globals.iter().find(|arg| arg.get_id() == "json").cloned();
        if let (Some(id), Some(json)) = (id, json) {
            let id = id
                .help(*id_help)
                .long_help(*id_help)
                .hide_default_value(true);
            let json = json.help(*json_help).long_help(*json_help);
            command = command.mut_subcommand("state", |state| {
                state.mut_subcommand(*name, |sub| sub.arg(id).arg(json))
            });
        }
    }
    command
}

/// Help texts of `--id` and `--json` under `state path`, `state show` and `state reset`.
const STATE_HELP: &[(&str, &str, &str)] = &[
    (
        "path",
        "Charge point id: print the state file of this station instead of the directory",
        "Print the paths as one JSON object",
    ),
    (
        "show",
        "Charge point id of the station (required)",
        "Print the state as one JSON object",
    ),
    (
        "reset",
        "Charge point id of the station (required)",
        "Print the outcome as one JSON object",
    ),
];

/// The command shell completion is generated from. `clap_complete` offers hidden flags and
/// every global flag under every subcommand, so here the station flags are plain (non-global)
/// arguments of the root, `run` and `suite` only; `version` keeps `--json`. Never used to parse.
pub fn completion_command() -> clap::Command {
    use clap::CommandFactory;
    let mut command = Cli::command();
    let globals: Vec<clap::Arg> = command
        .get_arguments()
        .filter(|arg| arg.is_global_set())
        .cloned()
        .collect();
    for arg in &globals {
        command = command.mut_arg(arg.get_id().clone(), |arg| arg.global(false));
    }
    for name in ["run", "suite", "check"] {
        command = command.mut_subcommand(name, |sub| {
            globals
                .iter()
                .fold(sub, |sub, arg| sub.arg(arg.clone().global(false)))
        });
    }
    for (name, _, _) in STATE_HELP {
        command = command.mut_subcommand("state", |state| {
            state.mut_subcommand(*name, |sub| {
                globals
                    .iter()
                    .filter(|arg| ["id", "state_dir", "json"].contains(&arg.get_id().as_str()))
                    .fold(sub, |sub, arg| sub.arg(arg.clone().global(false)))
            })
        });
    }
    if let Some(json) = globals.iter().find(|arg| arg.get_id() == "json") {
        command = command.mut_subcommand("version", |sub| sub.arg(json.clone().global(false)));
    }
    command
}

impl Cli {
    /// Parses the process arguments with the tuned command; exits on `--help`,
    /// `--version` and usage errors like `Parser::parse`.
    pub fn parse_args() -> Self {
        use clap::FromArgMatches;
        let mut matches = crate::settings::parse_matches(std::env::args_os().collect());
        let id_explicit = matches!(
            matches.value_source("id"),
            Some(clap::parser::ValueSource::CommandLine | clap::parser::ValueSource::EnvVariable)
        );
        let mut cli = Self::from_arg_matches_mut(&mut matches).unwrap_or_else(|error| error.exit());
        cli.id_explicit = id_explicit;
        cli
    }

    /// The effective settings as a configuration file (`--print-config=toml`): every
    /// setting that has a value, paths absolute, the URL without its password, no secrets.
    pub fn config_toml(&self) -> String {
        use crate::settings::{SCHEMA, absolute_text};
        use kirby_ocpp_core::config_file::{Value, render, strip_url_password};
        let text = |value: &str| Value::Text(value.to_owned());
        let int = |value: u64| Value::Integer(i64::try_from(value).unwrap_or(i64::MAX));
        let path = |value: &Path| Value::Text(absolute_text(value));
        let mut comments =
            vec!["Effective kirby-ocpp configuration; valid input for --config.".to_owned()];
        let mut values: Vec<(&str, Value)> = Vec::new();
        if let Some(url) = &self.url {
            let shown = strip_url_password(url);
            if shown != *url {
                comments.push(
                    "The URL password is not printed; pass the URL through --url or OCPP_URL."
                        .to_owned(),
                );
            }
            values.push(("url", Value::Text(shown)));
        }
        values.push(("ws_subprotocol", text(&self.ws_subprotocol)));
        values.push(("ws_ping", int(u64::from(self.ws_ping))));
        values.push(("call_timeout_ms", int(self.call_timeout_ms)));
        values.push(("id", text(&self.id)));
        values.push(("vendor", text(&self.vendor)));
        values.push(("model", text(&self.model)));
        if let Some(serial) = &self.serial {
            values.push(("serial", text(serial)));
        }
        if let Some(version) = &self.firmware_version {
            values.push(("firmware_version", text(version)));
        }
        values.push(("connector", int(u64::from(self.connector))));
        values.push(("connectors", int(u64::from(self.connectors))));
        values.push(("heartbeat", int(u64::from(self.heartbeat))));
        values.push(("nominal_power_w", int(u64::from(self.nominal_power_w))));
        values.push(("voltage_v", int(u64::from(self.voltage_v))));
        values.push(("phases", int(u64::from(self.phases))));
        values.push(("no_persist", Value::Bool(self.no_persist)));
        if let Some(dir) = &self.state_dir {
            values.push(("state_dir", path(dir)));
        }
        for (key, file) in [
            ("tls_ca", &self.tls_ca),
            ("tls_cert", &self.tls_cert),
            ("tls_key", &self.tls_key),
        ] {
            if let Some(file) = file {
                values.push((key, path(file)));
            }
        }
        if let Some(name) = &self.tls_server_name {
            values.push(("tls_server_name", text(name)));
        }
        values.push(("tls_skip_verify", Value::Bool(self.tls_skip_verify)));
        values.push((
            "additional_root_certificate_check",
            Value::Bool(self.additional_root_certificate_check),
        ));
        values.push((
            "security_profile_fallback",
            Value::Bool(self.security_profile_fallback),
        ));
        values.push(("reconnect", Value::Bool(self.reconnect)));
        values.push(("no_reconnect", Value::Bool(self.no_reconnect)));
        values.push(("reconnect_initial_ms", int(self.reconnect_initial_ms)));
        values.push(("reconnect_max_ms", int(self.reconnect_max_ms)));
        values.push((
            "reconnect_max_attempts",
            int(u64::from(self.reconnect_max_attempts)),
        ));
        values.push(("count", int(u64::from(self.count))));
        if let Some(template) = &self.id_template {
            values.push(("id_template", text(template)));
        }
        values.push(("first_index", int(u64::from(self.first_index))));
        values.push(("stagger_ms", int(self.stagger_ms)));
        values.push((
            "rule",
            Value::List(self.rule.iter().map(ToString::to_string).collect()),
        ));
        values.push(("show_secrets", Value::Bool(self.show_secrets)));
        values.push(("no_tui", Value::Bool(self.no_tui)));
        values.push(("json", Value::Bool(self.json)));
        if let Some(file) = &self.log_file {
            values.push(("log_file", path(file)));
        }
        values.push(("verbose", Value::Bool(self.verbose)));
        values.push(("quiet", Value::Bool(self.quiet)));
        let color = self
            .color
            .to_possible_value()
            .map_or_else(|| "auto".to_owned(), |value| value.get_name().to_owned());
        values.push(("color", text(&color)));
        render(&SCHEMA, &values, &comments)
    }

    /// The fleet this command line describes: the id template (`--id-template`, else
    /// `--id`), the first index, the count and the stagger.
    pub fn fleet_spec(&self) -> FleetSpec {
        FleetSpec {
            id_template: self.id_template.clone().unwrap_or_else(|| self.id.clone()),
            first_index: self.first_index,
            count: self.count,
            stagger_ms: self.stagger_ms,
        }
    }

    /// Maps the parsed options onto the core configuration (not yet validated).
    ///
    /// # Errors
    /// The URL is the only option without a default; its absence is reported with
    /// both ways to set it, which clap's generic "required argument" error does not do.
    pub fn into_config(self) -> Result<StationConfig, String> {
        let url = self.url.ok_or_else(|| {
            "the Central System URL is required: use --url or set OCPP_URL, for example \
             `kirby-ocpp --url ws://localhost:9000/ocpp`; for a local peer to connect to, run \
             `kirby-csms --listen 127.0.0.1:9000`"
                .to_owned()
        })?;
        Ok(StationConfig {
            url,
            charge_point_id: self.id,
            vendor: self.vendor,
            model: self.model,
            serial_number: self.serial,
            firmware_version: self.firmware_version,
            initial_connector: ConnectorId::new(self.connector),
            connector_count: self.connectors,
            heartbeat_interval_secs: self.heartbeat,
            ws_subprotocol: self.ws_subprotocol,
            ws_ping_interval_secs: self.ws_ping,
            reconnect: ReconnectConfig {
                enabled: self.reconnect && !self.no_reconnect,
                initial_delay_ms: self.reconnect_initial_ms,
                max_delay_ms: self.reconnect_max_ms,
                max_attempts: self.reconnect_max_attempts,
            },
            call_timeout_ms: self.call_timeout_ms,
            tls: TlsConfig {
                ca_file: self.tls_ca,
                cert_file: self.tls_cert,
                key_file: self.tls_key,
                server_name: self.tls_server_name,
                skip_verify: self.tls_skip_verify,
                ..TlsConfig::default()
            },
            persist: !self.no_persist,
            state_dir: self.state_dir,
            nominal_power_w: self.nominal_power_w,
            voltage_v: self.voltage_v,
            phases: self.phases,
            response_rules: self.rule,
            show_secrets: self.show_secrets,
            additional_root_certificate_check: self.additional_root_certificate_check,
            security_profile_fallback: self.security_profile_fallback,
        })
    }
}

fn parse_rule(value: &str) -> Result<ResponseRule, String> {
    value
        .parse()
        .map_err(|error: kirby_ocpp_core::station::response_rules::RuleError| error.to_string())
}

fn non_empty(value: &str) -> Result<String, String> {
    if value.is_empty() {
        Err("must not be empty".to_owned())
    } else {
        Ok(value.to_owned())
    }
}

fn non_empty_path(value: &str) -> Result<PathBuf, String> {
    non_empty(value).map(PathBuf::from)
}
