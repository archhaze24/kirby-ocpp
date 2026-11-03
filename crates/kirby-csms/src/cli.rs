//! Command-line interface. Precedence is CLI flag > environment variable (`KIRBY_CSMS_*`) >
//! default; booleans are strict (`true/false/yes/no/y/n/on/off/1/0`), like `kirby-ocpp`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::builder::BoolishValueParser;
use clap::{ArgAction, Parser};
use kirby_csms_core::{CsmsConfig, SubprotocolPolicy, TransactionIds};

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
        default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..)
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

    /// Run this TOML script against the connecting stations, print the report and exit
    #[arg(long, env = "KIRBY_CSMS_SCRIPT", value_name = "FILE", value_parser = non_empty_path)]
    pub script: Option<PathBuf>,

    /// Write the script report as JSON to this file
    #[arg(long, value_name = "PATH", value_parser = non_empty_path, requires = "script")]
    pub report: Option<PathBuf>,

    /// Write a JUnit XML report of the script to this file
    #[arg(long, value_name = "PATH", value_parser = non_empty_path, requires = "script")]
    pub junit: Option<PathBuf>,

    /// Give up (and fail) the script after this many seconds
    #[arg(
        long, value_name = "SECONDS", default_value_t = 60,
        value_parser = clap::value_parser!(u64).range(1..), requires = "script"
    )]
    pub timeout: u64,

    /// Print the third-party licence notices and exit
    #[arg(long)]
    pub licenses: bool,
}

impl Cli {
    /// The engine configuration (TLS is added by the caller).
    pub fn csms_config(&self) -> CsmsConfig {
        let mut config = CsmsConfig::new(self.listen);
        config.subprotocol = self.subprotocol.clone();
        config.transaction_ids = self.transaction_ids;
        config.heartbeat_interval = self.heartbeat_interval;
        config.history_cap = self.history_cap;
        config.transaction_history_cap = self.transaction_history_cap;
        config.call_timeout = Duration::from_secs(self.call_timeout);
        config
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
