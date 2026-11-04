//! Command-line interface.
//!
//! Mirrors the options of the TypeScript CLI (`src/cli.ts`, README) and fixes its
//! config bugs: precedence is **CLI flag > environment variable > default** (in the
//! TS CLI commander always supplied defaults, so the `OCPP_*` fallbacks were dead
//! code), and booleans are parsed strictly: only `true/false/yes/no/y/n/on/off/1/0`
//! are accepted (any other text is an error, whereas `z.coerce.boolean()` made every
//! non-empty string `true`).

use std::path::PathBuf;

use clap::builder::BoolishValueParser;
use clap::{ArgAction, Args, Parser, Subcommand};
use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::config::{ReconnectConfig, TlsConfig, defaults, validate_ws_url};
use kirby_ocpp_core::fleet::{FleetSpec, MAX_COUNT};
use kirby_ocpp_core::ids::ConnectorId;
use kirby_ocpp_core::station::ResponseRule;

/// Terminal OCPP 1.6J charge point emulator for CSMS testing.
///
/// Without a subcommand it opens an interactive terminal UI (or, when stdout is not a
/// terminal or with --no-tui, connects and logs events to stdout until interrupted).
/// Precedence: command-line flag, then environment variable, then default.
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
  kirby-ocpp scenarios list\n\n\
Exit codes: 0 success, 1 failure (scenario step failed, runtime error), 2 usage or configuration error, 130 `run` interrupted by SIGINT."
)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a CLI struct mirrors its flags one to one"
)]
pub struct Cli {
    /// Central System WebSocket URL, e.g. ws://localhost:9000/ocpp
    #[arg(global = true, short, long, env = "OCPP_URL", value_name = "URL", value_parser = parse_url)]
    pub url: Option<String>,

    /// Charge point id
    #[arg(global = true,
        short, long, env = "OCPP_CHARGE_POINT_ID", value_name = "ID",
        default_value = defaults::CHARGE_POINT_ID, value_parser = non_empty
    )]
    pub id: String,

    /// Number of stations to run (fleet mode, 1-1000); needs a `{n}` placeholder in the id
    #[arg(global = true,
        long, env = "OCPP_COUNT", value_name = "COUNT", default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..=i64::from(MAX_COUNT))
    )]
    pub count: u32,

    /// Charge point id template, e.g. CP-{n:03} (default: the --id value)
    #[arg(global = true, long, env = "OCPP_ID_TEMPLATE", value_name = "TEMPLATE", value_parser = non_empty)]
    pub id_template: Option<String>,

    /// Index substituted for {n} in the first station's id
    #[arg(
        global = true,
        long,
        env = "OCPP_FIRST_INDEX",
        value_name = "INDEX",
        default_value_t = 1
    )]
    pub first_index: u32,

    /// Delay between starting two stations of a fleet in milliseconds
    #[arg(
        global = true,
        long,
        env = "OCPP_STAGGER_MS",
        value_name = "MILLISECONDS",
        default_value_t = 0
    )]
    pub stagger_ms: u64,

    /// Charge point vendor
    #[arg(global = true,
        long, env = "OCPP_VENDOR", value_name = "VENDOR",
        default_value = defaults::VENDOR, value_parser = non_empty
    )]
    pub vendor: String,

    /// Charge point model
    #[arg(global = true,
        long, env = "OCPP_MODEL", value_name = "MODEL",
        default_value = defaults::MODEL, value_parser = non_empty
    )]
    pub model: String,

    /// Charge point serial number reported in BootNotification (at most 25 characters)
    #[arg(global = true, long, env = "OCPP_SERIAL", value_name = "SERIAL", value_parser = non_empty)]
    pub serial: Option<String>,

    /// Firmware version reported in BootNotification (at most 50 characters)
    #[arg(global = true,
        long = "firmware-version", env = "OCPP_FIRMWARE_VERSION", value_name = "VERSION",
        value_parser = non_empty
    )]
    pub firmware_version: Option<String>,

    /// Initial connector id
    #[arg(global = true,
        short, long, value_name = "ID", default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub connector: u32,

    /// Number of station connectors (raised to --connector if smaller)
    #[arg(global = true,
        long, value_name = "COUNT", default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub connectors: u32,

    /// Fallback heartbeat interval in seconds
    #[arg(global = true,
        long, value_name = "SECONDS", default_value_t = defaults::HEARTBEAT_INTERVAL_SECS,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub heartbeat: u32,

    /// Nominal charging power of each connector in watts
    #[arg(global = true,
        long, value_name = "WATTS", default_value_t = defaults::NOMINAL_POWER_W,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub nominal_power_w: u32,

    /// Supply voltage per phase in volts
    #[arg(global = true,
        long, value_name = "VOLTS", default_value_t = defaults::VOLTAGE_V,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub voltage_v: u32,

    /// Number of charging phases (1-3)
    #[arg(global = true,
        long, value_name = "COUNT", default_value_t = defaults::PHASES,
        value_parser = clap::value_parser!(u8).range(1..=3)
    )]
    pub phases: u8,

    /// Do not save station state such as local auth list and charging profiles
    #[arg(global = true,
        long, env = "KIRBY_OCPP_NO_PERSIST", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub no_persist: bool,

    /// Directory for persisted station state
    #[arg(global = true, long, env = "KIRBY_OCPP_STATE_DIR", value_name = "PATH", value_parser = non_empty_path)]
    pub state_dir: Option<PathBuf>,

    /// WebSocket subprotocol to request
    #[arg(global = true,
        long, env = "OCPP_WS_SUBPROTOCOL", value_name = "PROTOCOL",
        default_value = defaults::WS_SUBPROTOCOL, value_parser = non_empty
    )]
    pub ws_subprotocol: String,

    /// WebSocket ping interval in seconds, 0 disables pings
    #[arg(global = true,
        long, env = "OCPP_WS_PING_INTERVAL", value_name = "SECONDS",
        default_value_t = defaults::WS_PING_INTERVAL_SECS
    )]
    pub ws_ping: u32,

    /// Enable or disable automatic reconnect after an unexpected disconnect
    #[arg(global = true,
        long, env = "OCPP_RECONNECT", value_name = "BOOL", default_value_t = true,
        action = ArgAction::Set, value_parser = BoolishValueParser::new()
    )]
    pub reconnect: bool,

    /// Do not automatically reconnect (wins over --reconnect and OCPP_RECONNECT)
    #[arg(global = true, long)]
    pub no_reconnect: bool,

    /// Initial automatic reconnect delay in milliseconds
    #[arg(global = true,
        long, env = "OCPP_RECONNECT_INITIAL_MS", value_name = "MILLISECONDS",
        default_value_t = defaults::RECONNECT_INITIAL_MS
    )]
    pub reconnect_initial_ms: u64,

    /// Maximum automatic reconnect delay in milliseconds
    #[arg(global = true,
        long, env = "OCPP_RECONNECT_MAX_MS", value_name = "MILLISECONDS",
        default_value_t = defaults::RECONNECT_MAX_MS
    )]
    pub reconnect_max_ms: u64,

    /// Maximum automatic reconnect attempts, 0 means unlimited
    #[arg(
        global = true,
        long,
        env = "OCPP_RECONNECT_MAX_ATTEMPTS",
        value_name = "ATTEMPTS",
        default_value_t = 0
    )]
    pub reconnect_max_attempts: u32,

    /// OCPP CALL response timeout in milliseconds
    #[arg(global = true,
        long, env = "OCPP_CALL_TIMEOUT_MS", value_name = "MILLISECONDS",
        default_value_t = defaults::CALL_TIMEOUT_MS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub call_timeout_ms: u64,

    /// CA certificate bundle for wss:// connections
    #[arg(global = true, long, env = "OCPP_TLS_CA_FILE", value_name = "PATH", value_parser = non_empty_path)]
    pub tls_ca: Option<PathBuf>,

    /// Client certificate for wss:// mutual TLS
    #[arg(global = true, long, env = "OCPP_TLS_CERT_FILE", value_name = "PATH", value_parser = non_empty_path)]
    pub tls_cert: Option<PathBuf>,

    /// Client private key for wss:// mutual TLS
    #[arg(global = true, long, env = "OCPP_TLS_KEY_FILE", value_name = "PATH", value_parser = non_empty_path)]
    pub tls_key: Option<PathBuf>,

    /// TLS server name override for wss:// connections
    #[arg(global = true, long, env = "OCPP_TLS_SERVER_NAME", value_name = "NAME", value_parser = non_empty)]
    pub tls_server_name: Option<String>,

    /// Disable TLS certificate verification for local/self-signed wss:// testing
    #[arg(global = true,
        long, env = "OCPP_TLS_SKIP_VERIFY", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub tls_skip_verify: bool,

    /// Show AuthorizationKey values in frame logs (by default they are replaced by ***)
    #[arg(global = true,
        long, env = "OCPP_SHOW_SECRETS", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub show_secrets: bool,

    /// Misbehave on purpose when the CSMS sends a command: ACTION=BEHAVIOR[*COUNT]. Repeatable
    #[arg(
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

    /// Print the effective configuration as JSON and exit
    #[arg(global = true, long)]
    pub print_config: bool,

    /// Never start the terminal UI; log to stdout instead (automatic when stdout is not a terminal)
    #[arg(global = true, long)]
    pub no_tui: bool,

    /// Machine-readable output: one JSON object per line (events, and the report of `run`)
    #[arg(global = true, long)]
    pub json: bool,

    /// Also append the headless event log to this file
    #[arg(global = true, long, value_name = "PATH", value_parser = non_empty_path)]
    pub log_file: Option<PathBuf>,

    /// Print the third-party licence notices and exit
    #[arg(long)]
    pub licenses: bool,

    #[command(subcommand)]
    pub command: Option<Cmd>,
}

/// Subcommands. Without one, the UI (or the headless logger) runs.
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Connect, run a scenario, print the report; exit code 1 if any step fails
    Run(RunArgs),
    /// Built-in scenarios
    Scenarios {
        #[command(subcommand)]
        action: ScenariosCmd,
    },
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Scenario: a built-in name (see `scenarios list`) or a TOML file
    #[arg(long, value_name = "FILE|NAME")]
    pub scenario: String,

    /// Write the report as JSON to this file
    #[arg(long, value_name = "PATH", value_parser = non_empty_path)]
    pub report: Option<PathBuf>,

    /// Write a JUnit XML report to this file (one file; one test suite per station in a fleet)
    #[arg(long, value_name = "PATH", value_parser = non_empty_path)]
    pub junit: Option<PathBuf>,

    /// Give up (and fail) after this many seconds, connecting included
    #[arg(
        long, value_name = "SECONDS", default_value_t = 60,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub timeout: u64,

    /// Replace the idTag of the scenario's authorize / start_tx / stop_tx steps
    #[arg(long, value_name = "IDTAG", value_parser = non_empty)]
    pub id_tag: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ScenariosCmd {
    /// List the built-in scenarios
    List,
    /// Print a built-in scenario as TOML
    Show { name: String },
}

impl Cli {
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
            "the Central System URL is required: use --url or set OCPP_URL".to_owned()
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
        })
    }
}

fn parse_url(value: &str) -> Result<String, String> {
    validate_ws_url(value)?;
    Ok(value.to_owned())
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
