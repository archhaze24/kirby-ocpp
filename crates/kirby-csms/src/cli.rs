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
/// Precedence: command-line flag, then environment variable, then default.
#[derive(Debug, Parser)]
#[command(
    name = "kirby-csms",
    version,
    about,
    after_help = "Examples:\n  \
  kirby-csms --listen 0.0.0.0:9000\n  \
  kirby-csms --listen 127.0.0.1:0 --json --subprotocol omit\n  \
  kirby-csms --tls-cert cert.pem --tls-key key.pem\n  \
  kirby-csms --script suite.toml --junit junit.xml --timeout 120\n  \
  echo '{\"station\":\"CP-1\",\"action\":\"Reset\",\"payload\":{\"type\":\"Soft\"}}' | kirby-csms\n\n\
Exit codes: 0 success or clean shutdown, 1 script failed or timed out (or runtime error), 2 usage or configuration error, 130 script interrupted by SIGINT."
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
    /// it private (not readable by others); the file's permissions are not checked
    #[arg(long, env = "KIRBY_CSMS_AUTH_KEYS_FILE", value_name = "FILE", value_parser = non_empty_path)]
    pub auth_keys_file: Option<PathBuf>,

    /// Basic Auth key for stations without an ID=KEY entry
    #[arg(
        long, env = "KIRBY_CSMS_DEFAULT_AUTH_KEY", value_name = "KEY",
        hide_env_values = true, value_parser = non_empty_secret
    )]
    pub default_auth_key: Option<String>,

    /// WebSocket subprotocol policy: require:NAME (refuse clients that do not offer it), select:NAME
    /// (always select it), omit (select nothing), reject (refuse every upgrade)
    #[arg(
        long, env = "KIRBY_CSMS_SUBPROTOCOL", value_name = "POLICY",
        default_value = "require:ocpp1.6", value_parser = parse_policy
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

    /// Read JSON-line commands from stdin even on a terminal (automatic when stdin is not a
    /// terminal and there is no --script)
    #[arg(
        long, env = "KIRBY_CSMS_STDIN", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub stdin: bool,

    /// Never start the terminal UI; print events to stdout instead (automatic when stdout or
    /// stdin is not a terminal, and with --json, --script or --stdin)
    #[arg(
        long, env = "KIRBY_CSMS_NO_TUI", action = ArgAction::SetTrue,
        value_parser = BoolishValueParser::new()
    )]
    pub no_tui: bool,

    /// Run this TOML script against the connecting stations, print the report and exit
    #[arg(long, env = "KIRBY_CSMS_SCRIPT", value_name = "FILE", value_parser = non_empty_path)]
    pub script: Option<PathBuf>,

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

    /// Give up (and fail) the script after this many seconds
    #[arg(
        long, value_name = "SECONDS", default_value_t = 60,
        value_parser = clap::value_parser!(u64).range(1..=86_400), requires = "script"
    )]
    pub timeout: u64,

    /// Print the third-party licence notices and exit
    #[arg(long)]
    pub licenses: bool,
}

/// Parses a `--var` argument.
fn script_var(text: &str) -> Result<(String, serde_json::Value), String> {
    kirby_ocpp_core::scenario::vars::parse_assignment(text)
}

impl Cli {
    /// Whether the terminal UI should be tried: only for an interactive run.
    pub fn wants_tui(&self, stdin_is_terminal: bool, stdout_is_terminal: bool) -> bool {
        !self.no_tui
            && !self.json
            && !self.stdin
            && self.script.is_none()
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
        let has_keys = !self.auth_keys.is_empty()
            || !self.auth_key_raw.is_empty()
            || self.auth_keys_file.is_some()
            || self.default_auth_key.is_some();
        if matches!(profile, 1 | 2) && !has_keys {
            return Err(format!(
                "--security-profile {profile} needs --auth-key, --auth-keys-file or --default-auth-key"
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

/// Parses the contents of an `--auth-keys-file`. Errors name the line, never the key.
pub fn parse_auth_keys_text(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut keys: Vec<(String, String)> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
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
        "require" => named(name).map(SubprotocolPolicy::Require),
        "select" => named(name).map(SubprotocolPolicy::Select),
        "omit" | "reject" if name.is_some() => Err(format!("`{kind}` takes no value")),
        "omit" => Ok(SubprotocolPolicy::Omit),
        "reject" => Ok(SubprotocolPolicy::Reject),
        _ => Err("expected require:NAME, select:NAME, omit or reject".to_owned()),
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
                ("B".to_owned(), "2".to_owned())
            ])
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
}
