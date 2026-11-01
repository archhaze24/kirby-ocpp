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
use clap::{ArgAction, Parser};
use kirby_ocpp_core::StationConfig;
use kirby_ocpp_core::config::{ReconnectConfig, TlsConfig, defaults, validate_ws_url};
use kirby_ocpp_core::ids::ConnectorId;

/// Terminal OCPP charge point emulator.
#[derive(Debug, Parser)]
#[command(name = "kirby-ocpp", version, about)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a CLI struct mirrors its flags one to one"
)]
pub struct Cli {
    /// Central System WebSocket URL, e.g. ws://localhost:9000/ocpp
    #[arg(short, long, env = "OCPP_URL", value_name = "URL", value_parser = parse_url)]
    pub url: Option<String>,

    /// Charge point id
    #[arg(
        short, long, env = "OCPP_CHARGE_POINT_ID", value_name = "ID",
        default_value = defaults::CHARGE_POINT_ID, value_parser = non_empty
    )]
    pub id: String,

    /// Charge point vendor
    #[arg(
        long, env = "OCPP_VENDOR", value_name = "VENDOR",
        default_value = defaults::VENDOR, value_parser = non_empty
    )]
    pub vendor: String,

    /// Charge point model
    #[arg(
        long, env = "OCPP_MODEL", value_name = "MODEL",
        default_value = defaults::MODEL, value_parser = non_empty
    )]
    pub model: String,

    /// Initial connector id
    #[arg(
        short, long, value_name = "ID", default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub connector: u32,

    /// Number of station connectors (raised to --connector if smaller)
    #[arg(
        long, value_name = "COUNT", default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub connectors: u32,

    /// Fallback heartbeat interval in seconds
    #[arg(
        long, value_name = "SECONDS", default_value_t = defaults::HEARTBEAT_INTERVAL_SECS,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub heartbeat: u32,

    /// Do not save station state such as local auth list and charging profiles
    #[arg(
        long, env = "KIRBY_OCPP_NO_PERSIST", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub no_persist: bool,

    /// Directory for persisted station state
    #[arg(long, env = "KIRBY_OCPP_STATE_DIR", value_name = "PATH", value_parser = non_empty_path)]
    pub state_dir: Option<PathBuf>,

    /// WebSocket subprotocol to request
    #[arg(
        long, env = "OCPP_WS_SUBPROTOCOL", value_name = "PROTOCOL",
        default_value = defaults::WS_SUBPROTOCOL, value_parser = non_empty
    )]
    pub ws_subprotocol: String,

    /// WebSocket ping interval in seconds, 0 disables pings
    #[arg(
        long, env = "OCPP_WS_PING_INTERVAL", value_name = "SECONDS",
        default_value_t = defaults::WS_PING_INTERVAL_SECS
    )]
    pub ws_ping: u32,

    /// Enable or disable automatic reconnect after an unexpected disconnect
    #[arg(
        long, env = "OCPP_RECONNECT", value_name = "BOOL", default_value_t = true,
        action = ArgAction::Set, value_parser = BoolishValueParser::new()
    )]
    pub reconnect: bool,

    /// Do not automatically reconnect (wins over --reconnect and OCPP_RECONNECT)
    #[arg(long)]
    pub no_reconnect: bool,

    /// Initial automatic reconnect delay in milliseconds
    #[arg(
        long, env = "OCPP_RECONNECT_INITIAL_MS", value_name = "MILLISECONDS",
        default_value_t = defaults::RECONNECT_INITIAL_MS
    )]
    pub reconnect_initial_ms: u64,

    /// Maximum automatic reconnect delay in milliseconds
    #[arg(
        long, env = "OCPP_RECONNECT_MAX_MS", value_name = "MILLISECONDS",
        default_value_t = defaults::RECONNECT_MAX_MS
    )]
    pub reconnect_max_ms: u64,

    /// Maximum automatic reconnect attempts, 0 means unlimited
    #[arg(
        long,
        env = "OCPP_RECONNECT_MAX_ATTEMPTS",
        value_name = "ATTEMPTS",
        default_value_t = 0
    )]
    pub reconnect_max_attempts: u32,

    /// OCPP CALL response timeout in milliseconds
    #[arg(
        long, env = "OCPP_CALL_TIMEOUT_MS", value_name = "MILLISECONDS",
        default_value_t = defaults::CALL_TIMEOUT_MS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub call_timeout_ms: u64,

    /// CA certificate bundle for wss:// connections
    #[arg(long, env = "OCPP_TLS_CA_FILE", value_name = "PATH", value_parser = non_empty_path)]
    pub tls_ca: Option<PathBuf>,

    /// Client certificate for wss:// mutual TLS
    #[arg(long, env = "OCPP_TLS_CERT_FILE", value_name = "PATH", value_parser = non_empty_path)]
    pub tls_cert: Option<PathBuf>,

    /// Client private key for wss:// mutual TLS
    #[arg(long, env = "OCPP_TLS_KEY_FILE", value_name = "PATH", value_parser = non_empty_path)]
    pub tls_key: Option<PathBuf>,

    /// TLS server name override for wss:// connections
    #[arg(long, env = "OCPP_TLS_SERVER_NAME", value_name = "NAME", value_parser = non_empty)]
    pub tls_server_name: Option<String>,

    /// Disable TLS certificate verification for local/self-signed wss:// testing
    #[arg(
        long, env = "OCPP_TLS_SKIP_VERIFY", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub tls_skip_verify: bool,

    /// Print the effective configuration as JSON and exit
    #[arg(long)]
    pub print_config: bool,
}

impl Cli {
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
            serial_number: None,
            firmware_version: None,
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
            },
            persist: !self.no_persist,
            state_dir: self.state_dir,
        })
    }
}

fn parse_url(value: &str) -> Result<String, String> {
    validate_ws_url(value)?;
    Ok(value.to_owned())
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
