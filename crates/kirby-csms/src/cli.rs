//! Command-line interface. Precedence is CLI flag > environment variable (`KIRBY_CSMS_*`) >
//! default; booleans accept only `true/false/t/f/yes/no/y/n/on/off/1/0` (any case), like `kirby-ocpp`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::builder::BoolishValueParser;
use clap::{ArgAction, Parser};
use kirby_csms_core::{CsmsConfig, SecurityProfile, SubprotocolPolicy, TransactionIds};

/// Terminal OCPP 1.6J central system (CSMS) for charge point firmware development and load tests.
///
/// Accepts any number of charge points on `ws(s)://HOST:PORT/<any prefix>/<station id>`, prints
/// what happens and takes commands as JSON lines on stdin. With --script it runs a TOML script
/// against the stations that connect and exits with its verdict.
/// Precedence: command-line flag, then environment variable, then the configuration file
/// (--config), then default.
#[derive(Debug, Parser)]
#[command(
    name = "kirby-csms",
    version,
    about,
    group(clap::ArgGroup::new("run_input").args(["script", "replay"])),
    after_help = "Examples:\n  \
  kirby-csms --listen 0.0.0.0:9000\n  \
  kirby-csms --listen 127.0.0.1:0 --json --subprotocol omit\n  \
  kirby-csms --tls-cert cert.pem --tls-key key.pem\n  \
  kirby-csms --script suite.toml --junit junit.xml --timeout 120\n  \
  kirby-csms --replay session.jsonl --timeout 300\n  \
  echo '{\"station\":\"CP-1\",\"action\":\"Reset\",\"payload\":{\"type\":\"Soft\"}}' | kirby-csms\n  \
  kirby-csms ctl stations   (control a running server through its HTTP API)\n\n\
Exit codes: 0 success or clean shutdown, 1 script or replay failed or timed out (or runtime error), 2 usage or configuration error, 130 script or replay interrupted by SIGINT."
)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a CLI struct mirrors its flags one to one"
)]
pub struct Cli {
    /// Address to listen on; port 0 picks a free port (the real address is printed first)
    #[arg(
        long,
        env = "KIRBY_CSMS_LISTEN",
        value_name = "ADDR",
        default_value = "0.0.0.0:9000"
    )]
    pub listen: SocketAddr,

    /// Address of the local HTTP control API (off by default); port 0 picks a free port. A
    /// non-loopback address needs a token (--api-token or --api-token-file) and --api-tls-cert
    #[arg(long, env = "KIRBY_CSMS_API_LISTEN", value_name = "ADDR")]
    pub api_listen: Option<SocketAddr>,

    /// Bearer token the API requires (visible in `ps`: prefer the environment variable or
    /// --api-token-file); needs --api-listen
    #[arg(
        long, env = "KIRBY_CSMS_API_TOKEN", value_name = "TOKEN", hide_env_values = true,
        value_parser = non_empty_secret, requires = "api_listen", conflicts_with = "api_token_file"
    )]
    pub api_token: Option<String>,

    /// File holding the API bearer token (read once; a trailing newline is dropped); needs
    /// --api-listen
    #[arg(
        long, env = "KIRBY_CSMS_API_TOKEN_FILE", value_name = "FILE", value_parser = non_empty_path,
        requires = "api_listen"
    )]
    pub api_token_file: Option<PathBuf>,

    /// Server certificate chain (PEM) that makes the API serve HTTPS; needs --api-tls-key and
    /// --api-listen. A non-loopback --api-listen needs it
    #[arg(
        long, env = "KIRBY_CSMS_API_TLS_CERT", value_name = "FILE", value_parser = non_empty_path,
        requires = "api_tls_key", requires = "api_listen"
    )]
    pub api_tls_cert: Option<PathBuf>,

    /// Server private key (PEM) of the API certificate; needs --api-tls-cert
    #[arg(
        long, env = "KIRBY_CSMS_API_TLS_KEY", value_name = "FILE", value_parser = non_empty_path,
        requires = "api_tls_cert"
    )]
    pub api_tls_key: Option<PathBuf>,

    /// Trusted CA certificate(s) (PEM) for API clients; every client must present a certificate
    /// signed by them (mutual TLS); needs --api-tls-cert
    #[arg(
        long, env = "KIRBY_CSMS_API_TLS_CLIENT_CA", value_name = "FILE",
        value_parser = non_empty_path, requires = "api_tls_cert"
    )]
    pub api_tls_client_ca: Option<PathBuf>,

    /// Server certificate chain (PEM) for wss://; needs --tls-key
    #[arg(long, env = "KIRBY_CSMS_TLS_CERT", value_name = "FILE", value_parser = non_empty_path, requires = "tls_key")]
    pub tls_cert: Option<PathBuf>,

    /// Server private key (PEM) for wss://; needs --tls-cert
    #[arg(long, env = "KIRBY_CSMS_TLS_KEY", value_name = "FILE", value_parser = non_empty_path, requires = "tls_cert")]
    pub tls_key: Option<PathBuf>,

    /// Trusted CA certificate(s) (PEM) for client certificates; with --tls-cert, every client must
    /// present a certificate signed by them (mutual TLS)
    #[arg(long, env = "KIRBY_CSMS_TLS_CLIENT_CA", value_name = "FILE", value_parser = non_empty_path, requires = "tls_cert")]
    pub tls_client_ca: Option<PathBuf>,

    /// Security Whitepaper profile: 0 none, 1 Basic Auth, 2 Basic Auth + TLS, 3 TLS with client
    /// certificates (the certificate's CN must equal the station id)
    #[arg(
        long, env = "KIRBY_CSMS_SECURITY_PROFILE", value_name = "0..3",
        default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=3)
    )]
    pub security_profile: u8,

    /// Basic Auth key of one station as ID=KEY (repeatable). The station sends the key as the
    /// password and its id as the user name. Environment KIRBY_CSMS_AUTH_KEY holds several
    /// entries separated by `;` (used only when no --auth-key is given)
    // Parsed by `apply_env_auth_keys` rather than by clap, so that a rejected value is never
    // echoed in an error message.
    #[arg(long = "auth-key", value_name = "ID=KEY", hide_env_values = true)]
    auth_key_raw: Vec<String>,

    /// The parsed `--auth-key` (or `KIRBY_CSMS_AUTH_KEY`) entries.
    #[arg(skip)]
    pub auth_keys: Vec<(String, String)>,

    /// File with Basic Auth keys: lines `ID=KEY` (blank lines and `#` comments are skipped). Keep
    /// it private: a warning is printed when it is readable by group or others (Unix)
    #[arg(long, env = "KIRBY_CSMS_AUTH_KEYS_FILE", value_name = "FILE", value_parser = non_empty_path)]
    pub auth_keys_file: Option<PathBuf>,

    /// Basic Auth key for stations without an ID=KEY entry
    #[arg(
        long, env = "KIRBY_CSMS_DEFAULT_AUTH_KEY", value_name = "KEY",
        hide_env_values = true, value_parser = non_empty_secret,
        conflicts_with = "default_auth_key_file"
    )]
    pub default_auth_key: Option<String>,

    /// File holding the default Basic Auth key (read once; a trailing newline is dropped). Keep
    /// it private: a warning is printed when it is readable by group or others (Unix)
    #[arg(long, env = "KIRBY_CSMS_DEFAULT_AUTH_KEY_FILE", value_name = "FILE", value_parser = non_empty_path)]
    pub default_auth_key_file: Option<PathBuf>,

    /// WebSocket subprotocol policy: require:NAME (refuse clients that do not offer it), require:A,B
    /// (select the first offered name that is listed, refuse if none), select:NAME (always select
    /// it), omit (select nothing), reject (refuse every upgrade). The default serves OCPP 1.6 and
    /// 2.0.1; a client that offers both gets the one it lists first
    #[arg(
        long, env = "KIRBY_CSMS_SUBPROTOCOL", value_name = "POLICY",
        default_value = "require:ocpp1.6,ocpp2.0.1", value_parser = parse_policy
    )]
    pub subprotocol: SubprotocolPolicy,

    /// StartTransaction ids: fixed:ID (every transaction gets ID) or incrementing:FIRST
    #[arg(
        long, env = "KIRBY_CSMS_TRANSACTION_IDS", value_name = "POLICY",
        default_value = "fixed:123", value_parser = parse_transaction_ids
    )]
    pub transaction_ids: TransactionIds,

    /// `interval` in the BootNotification answer, in seconds
    #[arg(
        long, env = "KIRBY_CSMS_HEARTBEAT_INTERVAL", value_name = "SECONDS",
        default_value_t = 300, value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub heartbeat_interval: u32,

    /// Frames kept per station (0 keeps none)
    #[arg(
        long,
        env = "KIRBY_CSMS_HISTORY_CAP",
        value_name = "COUNT",
        default_value_t = 1000
    )]
    pub history_cap: usize,

    /// Show AuthorizationKey values in frame logs and history (by default they are replaced by ***)
    #[arg(
        long, env = "KIRBY_CSMS_SHOW_SECRETS", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub show_secrets: bool,

    /// Validate inbound CALL payloads against the OCPP 1.6J schemas and answer invalid ones with a
    /// CALLERROR (true/false)
    #[arg(
        long, env = "KIRBY_CSMS_VALIDATE_INBOUND", value_name = "BOOL", default_value_t = true,
        action = ArgAction::Set, value_parser = BoolishValueParser::new()
    )]
    pub validate_inbound: bool,

    /// Send a WebSocket ping to every station this often, in seconds (0 disables the watchdog)
    #[arg(
        long,
        env = "KIRBY_CSMS_PING_INTERVAL",
        value_name = "SECONDS",
        default_value_t = 30
    )]
    pub ping_interval: u32,

    /// Drop a station that sends nothing this long after a ping, in seconds
    #[arg(
        long, env = "KIRBY_CSMS_PONG_TIMEOUT", value_name = "SECONDS",
        default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub pong_timeout: u32,

    /// Stopped transactions kept per station (0 keeps none)
    #[arg(
        long,
        env = "KIRBY_CSMS_TRANSACTION_HISTORY_CAP",
        value_name = "COUNT",
        default_value_t = 100
    )]
    pub transaction_history_cap: usize,

    /// How long a command to a station may wait for its answer, in seconds
    #[arg(
        long, env = "KIRBY_CSMS_CALL_TIMEOUT", value_name = "SECONDS",
        default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=86_400)
    )]
    pub call_timeout: u64,

    /// Machine-readable output: one JSON object per line
    #[arg(
        long, env = "KIRBY_CSMS_JSON", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub json: bool,

    /// Also append every event as a JSON line to this file
    #[arg(long, env = "KIRBY_CSMS_LOG_FILE", value_name = "PATH", value_parser = non_empty_path)]
    pub log_file: Option<PathBuf>,

    /// Record station frames to a JSONL file
    #[arg(long, value_name = "PATH", value_parser = non_empty_path)]
    pub record: Option<PathBuf>,

    /// Replace an existing recording
    #[arg(long, requires = "record")]
    pub record_overwrite: bool,

    /// Read JSON-line commands from stdin even on a terminal (automatic when stdin is not a
    /// terminal and there is no --script or --replay)
    #[arg(
        long, env = "KIRBY_CSMS_STDIN", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub stdin: bool,

    /// Never start the terminal UI; print events to stdout instead (automatic when stdout or
    /// stdin is not a terminal, and with --json, --script, --replay or --stdin)
    #[arg(
        long, env = "KIRBY_CSMS_NO_TUI", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub no_tui: bool,

    /// Color of the terminal UI: auto (color unless NO_COLOR is set), always or never; the UI then marks states with text instead
    #[arg(
        long,
        env = "KIRBY_CSMS_COLOR",
        value_name = "WHEN",
        value_enum,
        default_value = "auto"
    )]
    pub color: ColorChoice,

    /// Clock in front of event lines of the terminal UI and of human output: utc, local (this machine's time zone) or none; --json keeps its own timestamps
    #[arg(
        long,
        env = "KIRBY_CSMS_TIME",
        value_name = "ZONE",
        value_enum,
        default_value = "utc"
    )]
    pub time: TimeChoice,

    /// Run this TOML script against the connecting stations, print the report and exit
    #[arg(long, env = "KIRBY_CSMS_SCRIPT", value_name = "FILE", value_parser = non_empty_path)]
    pub script: Option<PathBuf>,

    /// Send the CSMS CALLs of this recording (written by --record) to the connecting stations
    /// in recorded order, then exit; excludes --script
    #[arg(long, value_name = "FILE", value_parser = non_empty_path)]
    pub replay: Option<PathBuf>,

    /// Write the script report as JSON to this file
    #[arg(long, value_name = "PATH", value_parser = non_empty_path, requires = "script")]
    pub report: Option<PathBuf>,

    /// Write a JUnit XML report of the script to this file
    #[arg(long, value_name = "PATH", value_parser = non_empty_path, requires = "script")]
    pub junit: Option<PathBuf>,

    /// Set a script variable (repeatable): `${NAME}` in the script becomes VALUE. Overrides
    /// `[vars]`; `true`/`false`/numbers keep their type
    #[arg(long = "var", value_name = "NAME=VALUE", value_parser = script_var, requires = "script")]
    pub vars: Vec<(String, serde_json::Value)>,

    /// Give up (and fail) the script or replay after this many seconds
    #[arg(
        long, value_name = "SECONDS", default_value_t = 60,
        value_parser = clap::value_parser!(u64).range(1..=86_400), requires = "run_input"
    )]
    pub timeout: u64,

    /// Print the third-party license notices and exit
    #[arg(long)]
    pub licenses: bool,

    /// Read settings from this TOML configuration file; flags and environment variables win
    #[arg(
        long, env = "KIRBY_CSMS_CONFIG", value_name = "FILE", value_parser = non_empty_path
    )]
    pub config: Option<PathBuf>,

    /// Use the `[profile.NAME]` tables of the configuration file on top of its other values
    #[arg(long, env = "KIRBY_CSMS_PROFILE", value_name = "NAME", value_parser = non_empty_text)]
    pub profile: Option<String>,

    /// Print the effective configuration and exit: JSON, or with `=toml` a configuration file for --config
    #[arg(
        long, value_name = "FORMAT", value_enum, num_args = 0..=1, require_equals = true,
        default_missing_value = "json", conflicts_with = "check_config"
    )]
    pub print_config: Option<PrintFormat>,

    /// Validate the configuration file, the profile and every flag, print the verdict and exit
    /// (0 valid, 2 invalid); never binds a port
    #[arg(long)]
    pub check_config: bool,

    /// Station and rule blocks of the configuration file (not a flag).
    #[arg(skip)]
    pub blocks: crate::settings::Blocks,
}

/// `--print-config` formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PrintFormat {
    /// The effective settings as JSON.
    Json,
    /// A configuration file that `--config` accepts.
    Toml,
}

/// `--color` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    /// Color unless `NO_COLOR` is set and non-empty.
    Auto,
    /// Always.
    Always,
    /// Never.
    Never,
}

impl ColorChoice {
    /// Whether the terminal UI draws without color.
    #[must_use]
    pub fn monochrome(self, no_color: bool) -> bool {
        match self {
            Self::Always => false,
            Self::Never => true,
            Self::Auto => no_color,
        }
    }

    /// The name used on the command line and in the configuration file.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

/// `--time` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TimeChoice {
    /// UTC (the default).
    Utc,
    /// The time zone of this machine.
    Local,
    /// No timestamp.
    None,
}

impl TimeChoice {
    /// The mode the output layers use.
    #[must_use]
    pub fn mode(self) -> kirby_ocpp_core::timefmt::TimeMode {
        use kirby_ocpp_core::timefmt::TimeMode;
        match self {
            Self::Utc => TimeMode::Utc,
            Self::Local => TimeMode::Local,
            Self::None => TimeMode::None,
        }
    }
}

/// The command with the `--help` headings that name the tables of the configuration file.
pub fn command() -> clap::Command {
    use clap::CommandFactory;
    // `ctl` is parsed on its own (see `main`); it is registered here for `--help` and the man
    // pages only.
    let mut command = Cli::command()
        .disable_help_subcommand(true)
        .subcommand(crate::ctl::command());
    for (heading, ids) in HEADINGS {
        for id in *ids {
            command = command.mut_arg(*id, |arg| arg.help_heading(*heading));
        }
    }
    command
}

/// `--help` headings: the heading of a group of flags and the ids of its arguments.
const HEADINGS: &[(&str, &[&str])] = &[
    ("Listener", &["listen", "subprotocol"]),
    (
        "API",
        &[
            "api_listen",
            "api_token",
            "api_token_file",
            "api_tls_cert",
            "api_tls_key",
            "api_tls_client_ca",
        ],
    ),
    ("TLS", &["tls_cert", "tls_key", "tls_client_ca"]),
    (
        "Security",
        &[
            "security_profile",
            "auth_key_raw",
            "auth_keys_file",
            "default_auth_key",
            "default_auth_key_file",
            "show_secrets",
        ],
    ),
    (
        "Behavior",
        &[
            "transaction_ids",
            "heartbeat_interval",
            "validate_inbound",
            "ping_interval",
            "pong_timeout",
            "call_timeout",
            "history_cap",
            "transaction_history_cap",
        ],
    ),
    (
        "Output",
        &[
            "json",
            "log_file",
            "record",
            "record_overwrite",
            "stdin",
            "no_tui",
            "color",
            "time",
            "licenses",
        ],
    ),
    (
        "Script",
        &["script", "replay", "report", "junit", "vars", "timeout"],
    ),
    (
        "Configuration",
        &["config", "profile", "print_config", "check_config"],
    ),
];

impl Cli {
    /// Parses the process arguments, with the configuration file applied when `--config` is
    /// given; exits on `--help`, `--version` and usage errors.
    pub fn parse_args() -> Self {
        use clap::FromArgMatches;
        let (mut matches, blocks) = crate::settings::parse_matches(std::env::args_os().collect());
        if let Some(name) = matches.subcommand_name() {
            eprintln!("error: `{name}` must be the first argument: kirby-csms {name} [OPTIONS]");
            std::process::exit(2);
        }
        let mut cli = Self::from_arg_matches_mut(&mut matches).unwrap_or_else(|e| e.exit());
        cli.blocks = blocks;
        cli
    }

    /// Whether any source names an authorization key: the flags, the keys file, the default
    /// key (or its file) or a `[[station]]` block of the configuration file.
    fn has_auth_key_source(&self) -> bool {
        !self.auth_keys.is_empty()
            || !self.auth_key_raw.is_empty()
            || self.auth_keys_file.is_some()
            || self.default_auth_key.is_some()
            || self.default_auth_key_file.is_some()
            || self
                .blocks
                .stations
                .iter()
                .any(|station| station.auth_key_file.is_some())
    }

    /// Replaces `--default-auth-key-file` by the key it holds. The error names the file,
    /// never the key.
    pub fn load_default_key_file(&mut self) -> Result<(), String> {
        if let Some(path) = &self.default_auth_key_file {
            let key = read_key_file(
                path,
                &self.source_name("security.default_auth_key_file", "--default-auth-key-file"),
            )?;
            self.default_auth_key = Some(key);
        }
        Ok(())
    }
}

impl Cli {
    /// How an error names a setting: the configuration file key when the value came from the
    /// file, the flag otherwise.
    fn source_name(&self, key_path: &str, flag: &str) -> String {
        if self.blocks.file_keys.iter().any(|key| key == key_path) {
            format!("`{key_path}`")
        } else {
            flag.to_owned()
        }
    }
}

/// Reads a file that holds one key: a trailing line end is dropped, an empty key is an error.
pub fn read_key_file(path: &std::path::Path, what: &str) -> Result<String, String> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(KEY_FILE_MAX + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("{what}: cannot read {}: {error}", path.display()))?;
    if bytes.len() as u64 > KEY_FILE_MAX {
        return Err(format!(
            "{what}: {} is larger than {KEY_FILE_MAX} bytes",
            path.display()
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("{what}: {} is not valid UTF-8", path.display()))?;
    let key = text.trim_end_matches(['\n', '\r']);
    if key.is_empty() {
        return Err(format!("{what}: {} is empty", path.display()));
    }
    warn_if_shared(path);
    Ok(key.to_owned())
}

/// Largest key file that is read, in bytes.
const KEY_FILE_MAX: u64 = 64 * 1024;

fn non_empty_text(value: &str) -> Result<String, String> {
    if value.is_empty() {
        Err("must not be empty".to_owned())
    } else {
        Ok(value.to_owned())
    }
}

/// Prints a warning when the token file can be read by the group or by others.
#[cfg(unix)]
pub fn warn_if_shared(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path)
        && meta.permissions().mode() & 0o044 != 0
    {
        eprintln!(
            "warning: {} is readable by group or others; chmod 600 it",
            path.display()
        );
    }
}

#[cfg(not(unix))]
pub fn warn_if_shared(_path: &std::path::Path) {}

/// Parses a `--var` argument.
fn script_var(text: &str) -> Result<(String, serde_json::Value), String> {
    kirby_ocpp_core::scenario::vars::parse_assignment(text)
}

impl Cli {
    /// The API token (`--api-token`, or the content of `--api-token-file`), checked against
    /// the listen address: a non-loopback address needs a token and TLS.
    pub fn api_token(&self) -> Result<Option<String>, String> {
        let token = if let Some(path) = &self.api_token_file {
            Some(read_key_file(
                path,
                &self.source_name("api.api_token_file", "--api-token-file"),
            )?)
        } else {
            self.api_token.clone()
        };
        if let Some(token) = &token
            && !token.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(
                "the API token must be visible ASCII without spaces or control characters"
                    .to_owned(),
            );
        }
        if let Some(addr) = self.api_listen
            && !addr.ip().is_loopback()
        {
            if token.is_none() {
                return Err(format!(
                    "--api-listen {addr} is not a loopback address: set --api-token or --api-token-file"
                ));
            }
            if self.api_tls_cert.is_none() {
                return Err(format!(
                    "--api-listen {addr} is not a loopback address: the token would travel in \
                     clear text; set --api-tls-cert and --api-tls-key, or listen on a loopback \
                     address"
                ));
            }
        }
        Ok(token)
    }

    /// Whether the terminal UI should be tried: only for an interactive run.
    pub fn wants_tui(&self, stdin_is_terminal: bool, stdout_is_terminal: bool) -> bool {
        !self.no_tui
            && !self.json
            && !self.stdin
            && self.script.is_none()
            && self.replay.is_none()
            && stdin_is_terminal
            && stdout_is_terminal
    }

    /// The engine configuration (TLS is added by the caller).
    pub fn csms_config(&self) -> CsmsConfig {
        let mut config = CsmsConfig::new(self.listen);
        config.subprotocol = self.subprotocol.clone();
        config.transaction_ids = self.transaction_ids;
        config.heartbeat_interval = self.heartbeat_interval;
        config.history_cap = self.history_cap;
        config.show_secrets = self.show_secrets;
        config.validate_inbound = self.validate_inbound;
        config.ping_interval = Duration::from_secs(u64::from(self.ping_interval));
        config.pong_timeout = Duration::from_secs(u64::from(self.pong_timeout));
        config.transaction_history_cap = self.transaction_history_cap;
        config.call_timeout = Duration::from_secs(self.call_timeout);
        config.security.profile =
            SecurityProfile::from_number(self.security_profile).unwrap_or_default();
        config.security.keys.extend(self.auth_keys.iter().cloned());
        config
            .security
            .default_key
            .clone_from(&self.default_auth_key);
        config
    }

    /// Takes the keys from `KIRBY_CSMS_AUTH_KEY` (`ID=KEY;ID=KEY`) unless `--auth-key` was given.
    ///
    /// Also parses the raw `--auth-key` values. Errors name the flag or variable, never the value.
    pub fn apply_env_auth_keys(&mut self, env: Option<&str>) -> Result<(), String> {
        for raw in std::mem::take(&mut self.auth_key_raw) {
            let pair = parse_auth_key(&raw).map_err(|e| format!("--auth-key: {e}"))?;
            self.auth_keys.push(pair);
        }
        if !self.auth_keys.is_empty() {
            return reject_duplicates(&self.auth_keys, "--auth-key");
        }
        let Some(text) = env else {
            return Ok(());
        };
        for part in text.split(';').filter(|part| !part.is_empty()) {
            let pair = parse_auth_key(part).map_err(|e| format!("KIRBY_CSMS_AUTH_KEY: {e}"))?;
            self.auth_keys.push(pair);
        }
        reject_duplicates(&self.auth_keys, "KIRBY_CSMS_AUTH_KEY")
    }

    /// With profile 1 or 2 some key must exist once every source is loaded.
    pub fn require_keys(&self, config: &CsmsConfig) -> Result<(), String> {
        let profile = self.security_profile;
        if matches!(profile, 1 | 2)
            && config.security.keys.is_empty()
            && config.security.default_key.is_none()
        {
            return Err(format!(
                "--security-profile {profile} has no authorization key: no ID=KEY entry in \
                 --auth-key, --auth-keys-file or KIRBY_CSMS_AUTH_KEY, and no --default-auth-key"
            ));
        }
        Ok(())
    }

    /// Checks the combination of the security flags (the engine checks TLS itself).
    pub fn validate_security(&self) -> Result<(), String> {
        if self.ping_interval != 0 && self.ping_interval < self.pong_timeout {
            return Err(format!(
                "--ping-interval {} must not be less than --pong-timeout {} (or 0 to disable)",
                self.ping_interval, self.pong_timeout
            ));
        }
        let profile = self.security_profile;
        let tls = self.tls_cert.is_some();
        if profile >= 2 && !tls {
            return Err(format!(
                "--security-profile {profile} needs --tls-cert and --tls-key"
            ));
        }
        if profile == 3 && self.tls_client_ca.is_none() {
            return Err("--security-profile 3 needs --tls-client-ca".to_owned());
        }
        if profile != 3 && self.tls_client_ca.is_some() {
            return Err("--tls-client-ca is only used with --security-profile 3".to_owned());
        }
        let has_keys = self.has_auth_key_source();
        if matches!(profile, 1 | 2) && !has_keys {
            return Err(format!(
                "--security-profile {profile} needs --auth-key, --auth-keys-file, \
                 --default-auth-key, --default-auth-key-file or a [[station]] block with auth_key_file"
            ));
        }
        if !matches!(profile, 1 | 2) && has_keys {
            return Err(
                "authorization keys are only used with --security-profile 1 or 2".to_owned(),
            );
        }
        Ok(())
    }
}

/// Splits `ID=KEY` at the first `=`.
fn parse_auth_key(value: &str) -> Result<(String, String), String> {
    match value.split_once('=') {
        Some((id, key)) if !id.is_empty() && !key.is_empty() => Ok((id.to_owned(), key.to_owned())),
        _ => Err("expected ID=KEY with a non-empty id and key".to_owned()),
    }
}

/// Refuses the same station id twice in one source. Names the id (sanitised), never the key.
fn reject_duplicates(keys: &[(String, String)], source: &str) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    match keys.iter().find(|(id, _)| !seen.insert(id.as_str())) {
        Some((id, _)) => Err(format!(
            "{source}: duplicate station id `{}`",
            crate::headless::sanitize(id)
        )),
        None => Ok(()),
    }
}

/// Largest `--auth-keys-file` that is read, in bytes.
pub const AUTH_KEYS_FILE_MAX: u64 = 1024 * 1024;

/// Parses the contents of an `--auth-keys-file`. A leading UTF-8 BOM is ignored; leading
/// spaces and the line end are dropped but trailing spaces belong to the key. Errors name
/// the line, never the key.
pub fn parse_auth_keys_text(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut keys: Vec<(String, String)> = Vec::new();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for (number, line) in text.lines().enumerate() {
        let line = line.trim_start().trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let pair = parse_auth_key(line).map_err(|e| format!("line {}: {e}", number + 1))?;
        if keys.iter().any(|(id, _)| *id == pair.0) {
            return Err(format!(
                "line {}: duplicate station id `{}`",
                number + 1,
                crate::headless::sanitize(&pair.0)
            ));
        }
        keys.push(pair);
    }
    Ok(keys)
}

fn non_empty_secret(value: &str) -> Result<String, String> {
    if value.is_empty() {
        Err("must not be empty".to_owned())
    } else {
        Ok(value.to_owned())
    }
}

fn parse_policy(value: &str) -> Result<SubprotocolPolicy, String> {
    let (kind, name) = match value.split_once(':') {
        Some((kind, name)) => (kind, Some(name)),
        None => (value, None),
    };
    let named = |name: Option<&str>| match name {
        Some(name) if !name.is_empty() => Ok(name.to_owned()),
        _ => Err(format!(
            "`{kind}` needs a protocol name, e.g. {kind}:ocpp1.6"
        )),
    };
    match kind {
        "require" if name.is_some_and(|name| name.contains(',')) => {
            let mut listed: Vec<String> = Vec::new();
            for part in name.unwrap_or_default().split(',') {
                let part = part.trim();
                if part.is_empty() {
                    return Err(
                        "`require` takes non-empty names, e.g. require:ocpp1.6,ocpp2.0.1"
                            .to_owned(),
                    );
                }
                if !listed.iter().any(|known| known == part) {
                    listed.push(part.to_owned());
                }
            }
            Ok(match <[String; 1]>::try_from(listed) {
                Ok([only]) => SubprotocolPolicy::Require(only),
                Err(several) => SubprotocolPolicy::RequireAny(several),
            })
        }
        "require" => named(name).map(SubprotocolPolicy::Require),
        "select" => named(name).map(SubprotocolPolicy::Select),
        "omit" | "reject" if name.is_some() => Err(format!("`{kind}` takes no value")),
        "omit" => Ok(SubprotocolPolicy::Omit),
        "reject" => Ok(SubprotocolPolicy::Reject),
        _ => Err("expected require:NAME[,NAME...], select:NAME, omit or reject".to_owned()),
    }
}

fn parse_transaction_ids(value: &str) -> Result<TransactionIds, String> {
    let bad = || "expected fixed:ID or incrementing:FIRST (integers)".to_owned();
    let (kind, number) = value.split_once(':').ok_or_else(bad)?;
    let number: i32 = number.parse().map_err(|_| bad())?;
    match kind {
        "fixed" => Ok(TransactionIds::Fixed(number)),
        "incrementing" => Ok(TransactionIds::Incrementing(number)),
        _ => Err(bad()),
    }
}

fn non_empty_path(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        Err("must not be empty".to_owned())
    } else {
        Ok(PathBuf::from(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_ui_only_for_interactive_runs() {
        let cli = |args: &[&str]| {
            Cli::parse_from(std::iter::once("kirby-csms").chain(args.iter().copied()))
        };
        assert!(cli(&[]).wants_tui(true, true));
        assert!(!cli(&[]).wants_tui(false, true));
        assert!(!cli(&[]).wants_tui(true, false));
        assert!(!cli(&["--no-tui"]).wants_tui(true, true));
        assert!(!cli(&["--json"]).wants_tui(true, true));
        assert!(!cli(&["--stdin"]).wants_tui(true, true));
        assert!(!cli(&["--script", "s.toml"]).wants_tui(true, true));
    }

    #[test]
    fn durations_are_bounded() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(std::iter::once("kirby-csms").chain(args.iter().copied()))
        };
        let huge = "18446744073709551615";
        assert!(parse(&["--script", "s.toml", "--timeout", huge]).is_err());
        assert!(parse(&["--script", "s.toml", "--timeout", "86401"]).is_err());
        assert!(parse(&["--script", "s.toml", "--timeout", "86400"]).is_ok());
        assert!(parse(&["--call-timeout", huge]).is_err());
    }

    #[test]
    fn auth_key_parsing() {
        assert_eq!(
            parse_auth_key("CP-1=a=b"),
            Ok(("CP-1".to_owned(), "a=b".to_owned()))
        );
        assert!(parse_auth_key("CP-1").is_err());
        assert!(parse_auth_key("=k").is_err());
        assert!(parse_auth_key("CP-1=").is_err());
        let text = "# keys\n\nA=1\n  B=2  \n";
        assert_eq!(
            parse_auth_keys_text(text),
            Ok(vec![
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "2  ".to_owned())
            ]),
            "leading spaces go, trailing spaces belong to the key"
        );
        assert_eq!(
            parse_auth_keys_text("\u{feff}A=1\r\nB=k \r\n"),
            Ok(vec![
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "k ".to_owned())
            ]),
            "a BOM and CRLF line ends are not part of the values"
        );
        let error = parse_auth_keys_text("A=1\nsecretlooking").unwrap_err();
        assert!(error.starts_with("line 2:") && !error.contains("secretlooking"));
    }

    #[test]
    fn env_keys_are_split_and_lose_to_flags() {
        let mut cli = Cli::parse_from(["kirby-csms"]);
        cli.apply_env_auth_keys(Some("A=1;B=x=y;")).unwrap();
        assert_eq!(cli.auth_keys.len(), 2);
        assert_eq!(cli.auth_keys[1], ("B".to_owned(), "x=y".to_owned()));
        let mut cli = Cli::parse_from(["kirby-csms", "--auth-key", "C=3;4"]);
        cli.apply_env_auth_keys(Some("A=1")).unwrap();
        assert_eq!(cli.auth_keys, [("C".to_owned(), "3;4".to_owned())]);
        let mut cli = Cli::parse_from(["kirby-csms"]);
        assert!(cli.apply_env_auth_keys(Some("nokey")).is_err());
    }

    #[test]
    fn bad_and_duplicate_keys_never_echo_the_key() {
        let mut cli = Cli::parse_from(["kirby-csms", "--auth-key", "hunter2"]);
        let error = cli.apply_env_auth_keys(None).unwrap_err();
        assert!(
            error.starts_with("--auth-key") && !error.contains("hunter2"),
            "{error}"
        );
        let mut cli = Cli::parse_from(["kirby-csms", "--auth-key", "A=s1", "--auth-key", "A=s2"]);
        let error = cli.apply_env_auth_keys(None).unwrap_err();
        assert!(error.contains("`A`") && !error.contains("s1") && !error.contains("s2"));
        let mut cli = Cli::parse_from(["kirby-csms"]);
        assert!(cli.apply_env_auth_keys(Some("A=1;A=2")).is_err());
        let error = parse_auth_keys_text("A=s1\nB=2\nA=s2").unwrap_err();
        assert!(error.starts_with("line 3:") && error.contains("`A`") && !error.contains("s2"));
        let error = parse_auth_keys_text("bad\u{1b}id=1\nbad\u{1b}id=2").unwrap_err();
        assert!(!error.contains('\u{1b}'));
    }

    #[test]
    fn profile_one_needs_a_loaded_key() {
        let cli = Cli::parse_from(["kirby-csms", "--security-profile", "1"]);
        let mut config = cli.csms_config();
        assert!(cli.require_keys(&config).is_err());
        config.security.default_key = Some("k".to_owned());
        assert!(cli.require_keys(&config).is_ok());
        let cli = Cli::parse_from(["kirby-csms"]);
        assert!(cli.require_keys(&cli.csms_config()).is_ok());
    }

    #[test]
    fn security_flag_combinations() {
        let check = |args: &[&str]| {
            Cli::parse_from(std::iter::once("kirby-csms").chain(args.iter().copied()))
                .validate_security()
        };
        assert!(check(&[]).is_ok());
        assert!(check(&["--ping-interval", "5", "--pong-timeout", "10"]).is_err());
        assert!(check(&["--ping-interval", "10", "--pong-timeout", "10"]).is_ok());
        assert!(check(&["--ping-interval", "0", "--pong-timeout", "10"]).is_ok());
        assert!(check(&["--security-profile", "1", "--auth-key", "A=k"]).is_ok());
        assert!(check(&["--security-profile", "1"]).is_err());
        assert!(check(&["--auth-key", "A=k"]).is_err());
        assert!(check(&["--security-profile", "2", "--default-auth-key", "k"]).is_err());
        assert!(
            check(&[
                "--security-profile",
                "2",
                "--default-auth-key",
                "k",
                "--tls-cert",
                "c",
                "--tls-key",
                "k"
            ])
            .is_ok()
        );
        assert!(
            check(&[
                "--security-profile",
                "3",
                "--tls-cert",
                "c",
                "--tls-key",
                "k"
            ])
            .is_err()
        );
        assert!(
            check(&[
                "--security-profile",
                "3",
                "--tls-cert",
                "c",
                "--tls-key",
                "k",
                "--tls-client-ca",
                "ca"
            ])
            .is_ok()
        );
        assert!(check(&["--tls-cert", "c", "--tls-key", "k", "--tls-client-ca", "ca"]).is_err());
    }

    #[test]
    fn default_policy_serves_both_protocols() {
        let cli = Cli::parse_from(["kirby-csms"]);
        assert_eq!(
            cli.subprotocol,
            SubprotocolPolicy::RequireAny(vec!["ocpp1.6".to_owned(), "ocpp2.0.1".to_owned()])
        );
        assert_eq!(
            parse_policy("require:ocpp1.6,ocpp2.0.1"),
            Ok(cli.subprotocol)
        );
    }

    #[test]
    fn policies() {
        assert_eq!(
            parse_policy("require:ocpp1.6"),
            Ok(SubprotocolPolicy::Require("ocpp1.6".to_owned()))
        );
        assert_eq!(parse_policy("omit"), Ok(SubprotocolPolicy::Omit));
        assert!(parse_policy("require").is_err());
        assert!(parse_policy("omit:x").is_err());
        assert!(parse_policy("nope").is_err());
    }

    #[test]
    fn require_takes_a_list_of_names() {
        let names = |list: &[&str]| list.iter().map(|name| (*name).to_owned()).collect();
        assert_eq!(
            parse_policy("require:ocpp1.6,ocpp2.0.1"),
            Ok(SubprotocolPolicy::RequireAny(names(&[
                "ocpp1.6",
                "ocpp2.0.1"
            ])))
        );
        assert_eq!(
            parse_policy("require: ocpp2.1 , ocpp1.6"),
            Ok(SubprotocolPolicy::RequireAny(names(&[
                "ocpp2.1", "ocpp1.6"
            ])))
        );
        // A list that names one protocol is the single-name policy.
        assert_eq!(
            parse_policy("require:ocpp1.6,ocpp1.6"),
            Ok(SubprotocolPolicy::Require("ocpp1.6".to_owned()))
        );
        for bad in [
            "require:,",
            "require:ocpp1.6,",
            "require:,ocpp1.6",
            "require:ocpp1.6,,ocpp2.1",
            "require:a, ,b",
            "require:",
        ] {
            assert!(parse_policy(bad).is_err(), "{bad}");
        }
        assert!(parse_policy("select:a,b").is_ok());
    }

    #[test]
    fn transaction_ids() {
        assert_eq!(
            parse_transaction_ids("fixed:7"),
            Ok(TransactionIds::Fixed(7))
        );
        assert_eq!(
            parse_transaction_ids("incrementing:100"),
            Ok(TransactionIds::Incrementing(100))
        );
        assert!(parse_transaction_ids("fixed").is_err());
        assert!(parse_transaction_ids("fixed:x").is_err());
    }

    /// Every non-hidden command with its man page file name, subcommands included.
    fn pages(command: &clap::Command, out: &mut Vec<(String, clap::Command)>) {
        out.push((
            clap_mangen::Man::new(command.clone()).get_filename(),
            command.clone(),
        ));
        for sub in command.get_subcommands().filter(|sub| !sub.is_hide_set()) {
            pages(sub, out);
        }
    }

    /// Writes the man pages to `$KIRBY_CSMS_MAN_DIR` when it is set (the release workflow does
    /// this) and checks that every visible flag and subcommand (`ctl` and its commands
    /// included) appears in its page.
    #[test]
    fn man_page_lists_every_flag() {
        let temp = tempfile::tempdir().expect("temp dir");
        let dir = std::env::var_os("KIRBY_CSMS_MAN_DIR")
            .map_or_else(|| temp.path().to_path_buf(), PathBuf::from);
        std::fs::create_dir_all(&dir).expect("man dir");
        clap_mangen::generate_to(command(), &dir).expect("generate man page");
        let mut root = command();
        root.build();
        let mut all = Vec::new();
        pages(&root, &mut all);
        assert!(
            all.iter().any(|(file, _)| file == "kirby-csms-ctl-call.1"),
            "ctl pages expected: {:?}",
            all.iter().map(|(file, _)| file).collect::<Vec<_>>()
        );
        let mut flags = 0;
        for (file, cmd) in all {
            let text = std::fs::read_to_string(dir.join(&file)).expect("page exists");
            for arg in cmd.get_arguments().filter(|arg| !arg.is_hide_set()) {
                if let Some(long) = arg.get_long() {
                    let flag = format!("\\-\\-{}", long.replace('-', "\\-"));
                    assert!(text.contains(&flag), "{file} misses --{long}");
                    flags += 1;
                }
            }
            for sub in cmd.get_subcommands().filter(|sub| !sub.is_hide_set()) {
                assert!(
                    text.contains(sub.get_name()),
                    "{file} misses {}",
                    sub.get_name()
                );
            }
        }
        assert!(flags > 10, "flags expected");
    }
}
