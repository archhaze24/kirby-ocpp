//! Configuration file glue: the schema of the CSMS file and the merge of file values into the
//! command line.
//!
//! The generic parts (loading, validation, profiles, rendering) live in
//! `kirby_ocpp_core::config_file`. Here the file is applied the cheap way: the command line is
//! parsed once; every file key whose argument was not given by flag or environment variable
//! becomes an equivalent `--flag=value` argument, and the command line is parsed again with
//! those appended. That keeps one parser, one set of value checks and one set of defaults.
//! The `[[station]]` and `[[rule]]` blocks have no flag; they are typed here and applied by
//! `main`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::ArgMatches;
use clap::parser::ValueSource;
use kirby_csms_core::{SubprotocolPolicy, TransactionIds};
use kirby_ocpp_core::config_file::{
    self, Block, BlockSpec, BlockValues, KeySpec, Kind, Schema, Section, Value,
};
use kirby_ocpp_core::text::is_unsafe_char;
use serde_json::Value as Json;

use crate::EXIT_USAGE;
use crate::cli::{self, Cli};
use crate::commands::{self, FaultKind, MAX_FAULT_ACTION_LEN, MAX_FAULT_TIMES};

/// Settings of the configuration file of `kirby-csms`: one table per `--help` heading, plus
/// the `[[station]]` and `[[rule]]` blocks.
pub const SCHEMA: Schema = Schema {
    protocols: &["ocpp1.6", "ocpp2.0.1"],
    sections: &[
        Section {
            path: &["listener"],
            keys: &[
                KeySpec::new("listen", Kind::Text),
                KeySpec::new("subprotocol", Kind::Text),
            ],
        },
        Section {
            path: &["api"],
            keys: &[
                KeySpec::new("api_listen", Kind::Text),
                KeySpec::new(
                    "api_token",
                    Kind::Secret {
                        hint: "set `api_token_file` to a file holding the token, or use the \
                               environment variable KIRBY_CSMS_API_TOKEN",
                    },
                ),
                KeySpec::new("api_token_file", Kind::Path),
                KeySpec::new("api_tls_cert", Kind::Path),
                KeySpec::new("api_tls_key", Kind::Path),
                KeySpec::new("api_tls_client_ca", Kind::Path),
            ],
        },
        Section {
            path: &["tls"],
            keys: &[
                KeySpec::new("tls_cert", Kind::Path),
                KeySpec::new("tls_key", Kind::Path),
                KeySpec::new("tls_client_ca", Kind::Path),
            ],
        },
        Section {
            path: &["security"],
            keys: &[
                KeySpec::new("security_profile", Kind::Integer),
                KeySpec::new(
                    "auth_key",
                    Kind::Secret {
                        hint: "set `auth_keys_file` (lines ID=KEY), give a `[[station]]` block an \
                               `auth_key_file`, or use the environment variable KIRBY_CSMS_AUTH_KEY",
                    },
                ),
                KeySpec::new("auth_keys_file", Kind::Path),
                KeySpec::new(
                    "default_auth_key",
                    Kind::Secret {
                        hint: "set `default_auth_key_file` to a file holding the key, or use the \
                               environment variable KIRBY_CSMS_DEFAULT_AUTH_KEY",
                    },
                ),
                KeySpec::new("default_auth_key_file", Kind::Path),
                KeySpec::new("show_secrets", Kind::Switch),
            ],
        },
        Section {
            path: &["behavior"],
            keys: &[
                KeySpec::new("transaction_ids", Kind::Text),
                KeySpec::new("heartbeat_interval", Kind::Integer),
                KeySpec::new("validate_inbound", Kind::Bool),
                KeySpec::new("ping_interval", Kind::Integer),
                KeySpec::new("pong_timeout", Kind::Integer),
                KeySpec::new("call_timeout", Kind::Integer),
                KeySpec::new("history_cap", Kind::Integer),
                KeySpec::new("transaction_history_cap", Kind::Integer),
            ],
        },
        Section {
            path: &["output"],
            keys: &[
                KeySpec::new("json", Kind::Switch),
                KeySpec::new("log_file", Kind::Path),
                KeySpec::new("stdin", Kind::Switch),
                KeySpec::new("no_tui", Kind::Switch),
                KeySpec::new("color", Kind::Text),
                KeySpec::new("time", Kind::Text),
            ],
        },
    ],
    blocks: &[
        BlockSpec {
            name: "station",
            keys: &[
                KeySpec::new("id", Kind::Text),
                KeySpec::checked("protocol", check_protocol),
                KeySpec::new("auth_key_file", Kind::Path),
            ],
            required: &["id"],
            reserved: &[],
            unique: Some("id"),
            check: Some(check_station),
        },
        BlockSpec {
            name: "rule",
            keys: &[
                KeySpec::new("action", Kind::Text),
                KeySpec::new("station", Kind::Text),
                KeySpec::checked("protocol", check_protocol),
                KeySpec::new("behavior", Kind::Text),
                KeySpec::new("payload", Kind::Text),
                KeySpec::new("times", Kind::Integer),
            ],
            required: &["action", "behavior"],
            reserved: &[],
            unique: None,
            check: Some(check_rule),
        },
        BlockSpec {
            name: "command",
            keys: &[
                KeySpec::new("id", Kind::Text),
                KeySpec::new("trigger", Kind::Text),
                KeySpec::new("station", Kind::Text),
                KeySpec::checked("protocol", check_protocol),
                KeySpec::new("action", Kind::Text),
                KeySpec::new("payload", Kind::Text),
                KeySpec::new("timeout_ms", Kind::Integer),
                KeySpec::new("delay_ms", Kind::Integer),
                KeySpec::new("interval_ms", Kind::Integer),
                KeySpec::new("times", Kind::Integer),
            ],
            required: &["id", "trigger", "station", "action"],
            reserved: &[],
            unique: Some("id"),
            check: Some(check_command),
        },
    ],
};

fn check_command(block: &Block) -> Result<(), String> {
    crate::automation::CommandSpec::from_block(block).map(|_| ())
}

fn check_protocol(value: &str) -> Result<(), String> {
    commands::check_protocol(value).map_err(|error| {
        let known = error.strip_prefix("unsupported protocol; supported: ");
        format!(
            "`{value}` is not supported, expected one of {}",
            known.unwrap_or_default()
        )
    })
}

fn check_station(block: &Block) -> Result<(), String> {
    let id = block.text("id").unwrap_or_default();
    if id.is_empty() {
        return Err("`id` must not be empty".to_owned());
    }
    if id.chars().any(is_unsafe_char) {
        return Err("`id` must not contain control characters".to_owned());
    }
    if id.contains(['*', '?']) {
        return Err("`id` is one station id; patterns are not supported yet".to_owned());
    }
    Ok(())
}

/// What a `[[rule]]` block does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleBehavior {
    /// Answer every CALL of the action with a fixed CALLRESULT payload.
    Respond,
    /// A fault for the next `times` occurrences of the action.
    Fault(FaultKind),
}

impl RuleBehavior {
    const NAMES: &'static str = "respond, call_error, silence, close, invalid_payload";

    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "respond" => Self::Respond,
            "call_error" => Self::Fault(FaultKind::CallError),
            "silence" => Self::Fault(FaultKind::Silence),
            "close" => Self::Fault(FaultKind::Close),
            "invalid_payload" => Self::Fault(FaultKind::InvalidPayload),
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Respond => "respond",
            Self::Fault(FaultKind::CallError) => "call_error",
            Self::Fault(FaultKind::Silence) => "silence",
            Self::Fault(FaultKind::Close) => "close",
            Self::Fault(FaultKind::InvalidPayload) => "invalid_payload",
        }
    }
}

fn check_rule(block: &Block) -> Result<(), String> {
    let action = block.text("action").unwrap_or_default();
    if action.is_empty() {
        return Err("`action` must not be empty".to_owned());
    }
    if action.len() > MAX_FAULT_ACTION_LEN {
        return Err(format!(
            "`action` must be at most {MAX_FAULT_ACTION_LEN} bytes"
        ));
    }
    if action.chars().any(is_unsafe_char) {
        return Err("`action` must not contain control characters".to_owned());
    }
    if let Some(station) = block.text("station") {
        commands::check_station_id(station)
            .map_err(|message| message.replace("the station id", "`station`"))?;
        if station.contains(['*', '?']) {
            return Err("`station` is one station id; patterns are not supported yet".to_owned());
        }
    }
    let name = block.text("behavior").unwrap_or_default();
    let Some(behavior) = RuleBehavior::parse(name) else {
        return Err(format!(
            "`behavior` must be one of: {}",
            RuleBehavior::NAMES
        ));
    };
    let payload = block.text("payload");
    match behavior {
        RuleBehavior::Respond | RuleBehavior::Fault(FaultKind::InvalidPayload) => {
            let Some(text) = payload else {
                return Err(format!("behavior `{name}` needs a `payload` (JSON text)"));
            };
            let parsed: Json = serde_json::from_str(text)
                .map_err(|error| format!("`payload` is not valid JSON: {error}"))?;
            if behavior == RuleBehavior::Respond && !parsed.is_object() {
                return Err("`payload` of a `respond` rule must be a JSON object".to_owned());
            }
        }
        RuleBehavior::Fault(_) => {
            if payload.is_some() {
                return Err(format!("behavior `{name}` takes no `payload`"));
            }
        }
    }
    if behavior == RuleBehavior::Respond
        && (block.text("station").is_some() || block.text("protocol").is_some())
        && !commands::is_rule_action(action, block.text("protocol"))
    {
        return Err(format!(
            "`action` {action} is not a charge-point action for this protocol, so a scoped \
             `respond` rule would never apply"
        ));
    }
    if let Some(times) = block.integer("times") {
        if behavior == RuleBehavior::Respond {
            return Err("`times` applies to faults only; a `respond` rule has no count".to_owned());
        }
        let in_range = usize::try_from(times).is_ok_and(|n| (1..=MAX_FAULT_TIMES).contains(&n));
        if !in_range {
            return Err(format!("`times` must be 1 to {MAX_FAULT_TIMES}"));
        }
    }
    Ok(())
}

/// A `[[station]]` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationBlock {
    /// The station id.
    pub id: String,
    /// File holding the Basic Auth key of the station.
    pub auth_key_file: Option<PathBuf>,
}

/// A `[[rule]]` block.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleBlock {
    /// The action the rule applies to.
    pub action: String,
    /// The one station the rule applies to; `None` applies to every station.
    pub station: Option<String>,
    /// The negotiated subprotocol the rule is limited to; `None` applies to every protocol.
    pub protocol: Option<String>,
    /// What the rule does.
    pub behavior: RuleBehavior,
    /// CALLRESULT payload (`respond`, `invalid_payload`).
    pub payload: Option<Json>,
    /// Occurrences a fault applies to.
    pub times: usize,
}

/// The `[[station]]` and `[[rule]]` blocks of the file (after the profile is applied).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Blocks {
    /// `[[station]]` blocks in file order.
    pub stations: Vec<StationBlock>,
    /// `[[rule]]` blocks in file order.
    pub rules: Vec<RuleBlock>,
    /// Scheduled commands and connection hooks in file order.
    pub commands: Vec<crate::automation::CommandSpec>,
    /// Key paths (`security.default_auth_key_file`) whose value came from the file, not from
    /// a flag or variable; errors name them instead of the flag.
    pub file_keys: Vec<String>,
}

impl Blocks {
    fn from_settings(settings: &config_file::Settings) -> Result<Self, String> {
        if settings.blocks("command").len() > crate::automation::MAX_COMMANDS {
            return Err("at most 256 `[[command]]` blocks are allowed".to_owned());
        }
        let commands = settings
            .blocks("command")
            .iter()
            .map(crate::automation::CommandSpec::from_block)
            .collect::<Result<Vec<_>, _>>()?;
        let stations = settings
            .blocks("station")
            .iter()
            .map(|block| StationBlock {
                id: block.text("id").unwrap_or_default().to_owned(),
                auth_key_file: block.text("auth_key_file").map(PathBuf::from),
            })
            .collect();
        let rules = settings
            .blocks("rule")
            .iter()
            .filter_map(|block| {
                let behavior = RuleBehavior::parse(block.text("behavior")?)?;
                Some(RuleBlock {
                    action: block.text("action")?.to_owned(),
                    station: block.text("station").map(str::to_owned),
                    protocol: block.text("protocol").map(str::to_owned),
                    behavior,
                    payload: block
                        .text("payload")
                        .and_then(|text| serde_json::from_str(text).ok()),
                    times: block
                        .integer("times")
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(1),
                })
            })
            .collect();
        Ok(Self {
            stations,
            rules,
            commands,
            file_keys: Vec::new(),
        })
    }

    /// The blocks as renderable elements.
    fn render_values(&self) -> Vec<(&'static str, Vec<BlockValues<'static>>)> {
        let stations = self
            .stations
            .iter()
            .map(|station| {
                let mut values = vec![("id", Value::Text(station.id.clone()))];
                if let Some(file) = &station.auth_key_file {
                    values.push(("auth_key_file", Value::Text(absolute_text(file))));
                }
                values
            })
            .collect();
        let rules = self
            .rules
            .iter()
            .map(|rule| {
                let mut values = vec![
                    ("action", Value::Text(rule.action.clone())),
                    ("behavior", Value::Text(rule.behavior.name().to_owned())),
                ];
                if let Some(station) = &rule.station {
                    values.push(("station", Value::Text(station.clone())));
                }
                if let Some(protocol) = &rule.protocol {
                    values.push(("protocol", Value::Text(protocol.clone())));
                }
                if let Some(payload) = &rule.payload {
                    values.push(("payload", Value::Text(payload.to_string())));
                }
                if matches!(rule.behavior, RuleBehavior::Fault(_)) {
                    let times = i64::try_from(rule.times).unwrap_or(i64::MAX);
                    values.push(("times", Value::Integer(times)));
                }
                values
            })
            .collect();
        let commands = self
            .commands
            .iter()
            .map(crate::automation::CommandSpec::render_values)
            .collect();
        vec![
            ("station", stations),
            ("rule", rules),
            ("command", commands),
        ]
    }
}

/// A command-line argument that was filled from the file.
struct Synthesized {
    flag: String,
    key: &'static str,
}

/// Parses the command line, with the configuration file applied when `--config` is given.
/// Exits like clap on `--help`, `--version` and usage errors; a configuration error exits 2.
pub fn parse_matches(args: Vec<OsString>) -> (ArgMatches, Blocks) {
    use clap::error::ErrorKind;
    let command = cli::command();
    let mut strict = true;
    let first = match command.clone().try_get_matches_from(args.clone()) {
        Ok(matches) => matches,
        Err(error) => {
            // A flag that needs or excludes another one may be satisfied by the file, so the
            // file is read first and the strict parse runs again after the merge.
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                error.exit();
            }
            match command
                .clone()
                .ignore_errors(true)
                .try_get_matches_from(args.clone())
            {
                Ok(matches)
                    if matches.get_one::<PathBuf>("config").is_some()
                        || matches.get_one::<String>("profile").is_some() =>
                {
                    strict = false;
                    matches
                }
                _ => error.exit(),
            }
        }
    };
    let config = first.get_one::<PathBuf>("config");
    let profile = first.get_one::<String>("profile");
    if config.is_none() && profile.is_none() {
        return (first, Blocks::default());
    }
    let Some(path) = config else {
        fail("--profile (or KIRBY_CSMS_PROFILE) needs --config (or KIRBY_CSMS_CONFIG)");
    };
    let settings = match config_file::load(path, &SCHEMA, profile.map(String::as_str)) {
        Ok(settings) => settings,
        Err(error) => fail(&error.to_string()),
    };
    let mut blocks = Blocks::from_settings(&settings).unwrap_or_else(|error| fail(&error));
    let (extra, synthesized) = synthesize(&first, &settings);
    blocks.file_keys = synthesized
        .iter()
        .map(|item| {
            SCHEMA.section_of(item.key).map_or_else(
                || item.key.to_owned(),
                |table| format!("{table}.{}", item.key),
            )
        })
        .collect();
    if extra.is_empty() && strict {
        return (first, blocks);
    }
    let mut merged = args;
    let at = merged
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(merged.len());
    merged.splice(at..at, extra);
    match command.try_get_matches_from(merged) {
        Ok(matches) => (matches, blocks),
        Err(error) => {
            use clap::error::ContextKind;
            let code = error.exit_code();
            let _ = error.print();
            let blamed = error
                .get(ContextKind::InvalidArg)
                .map(ToString::to_string)
                .and_then(|text| text.split_whitespace().next().map(str::to_owned));
            if let Some(item) = synthesized
                .iter()
                .find(|item| blamed.as_deref() == Some(&format!("--{}", item.flag)))
            {
                let key = SCHEMA.section_of(item.key).map_or_else(
                    || item.key.to_owned(),
                    |table| format!("{table}.{}", item.key),
                );
                eprintln!(
                    "note: the value of --{} comes from key `{key}` of the configuration file {}",
                    item.flag,
                    path.display()
                );
            }
            std::process::exit(code);
        }
    }
}

fn fail(message: &str) -> ! {
    eprintln!("error: {message}");
    std::process::exit(i32::from(EXIT_USAGE));
}

fn explicit(matches: &ArgMatches, id: &str) -> bool {
    matches!(
        matches.value_source(id),
        Some(ValueSource::CommandLine | ValueSource::EnvVariable)
    )
}

/// The arguments that carry the file values the command line left open.
fn synthesize(
    matches: &ArgMatches,
    settings: &config_file::Settings,
) -> (Vec<OsString>, Vec<Synthesized>) {
    let mut args = Vec::new();
    let mut synthesized = Vec::new();
    for (key, value) in settings.iter() {
        if explicit(matches, key) {
            continue;
        }
        // Exclusions: the file never turns a flag or variable that is set into a conflict.
        let excluded = match key {
            "api_token_file" => explicit(matches, "api_token"),
            "default_auth_key_file" => explicit(matches, "default_auth_key"),
            _ => false,
        };
        if excluded {
            continue;
        }
        let flag = key.replace('_', "-");
        let before = args.len();
        match value {
            Value::Text(text) => args.push(format!("--{flag}={text}").into()),
            Value::Integer(number) => args.push(format!("--{flag}={number}").into()),
            Value::Bool(on) => match SCHEMA.key(key).map(|spec| spec.kind) {
                Some(Kind::Switch) => {
                    if *on {
                        args.push(format!("--{flag}").into());
                    }
                }
                _ => args.push(format!("--{flag}={on}").into()),
            },
            Value::List(items) => {
                for item in items {
                    args.push(format!("--{flag}={item}").into());
                }
            }
        }
        if args.len() > before {
            synthesized.push(Synthesized { flag, key });
        }
    }
    (args, synthesized)
}

/// An absolute form of `path` for printing; unchanged when the working directory is unknown.
pub fn absolute_text(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn subprotocol_text(policy: &SubprotocolPolicy) -> String {
    match policy {
        SubprotocolPolicy::Require(name) => format!("require:{name}"),
        SubprotocolPolicy::RequireAny(names) => format!("require:{}", names.join(",")),
        SubprotocolPolicy::Select(name) => format!("select:{name}"),
        SubprotocolPolicy::Omit => "omit".to_owned(),
        SubprotocolPolicy::Reject => "reject".to_owned(),
        _ => "custom".to_owned(),
    }
}

fn transaction_ids_text(ids: TransactionIds) -> String {
    match ids {
        TransactionIds::Fixed(id) => format!("fixed:{id}"),
        TransactionIds::Incrementing(first) => format!("incrementing:{first}"),
    }
}

impl Cli {
    /// Every setting that has a value, as `(key, value)`; paths absolute, no secrets.
    fn effective_values(&self) -> Vec<(&'static str, Value)> {
        let text = |value: &str| Value::Text(value.to_owned());
        let int = |value: u64| Value::Integer(i64::try_from(value).unwrap_or(i64::MAX));
        let path = |value: &Path| Value::Text(absolute_text(value));
        let mut values: Vec<(&'static str, Value)> = vec![
            ("listen", text(&self.listen.to_string())),
            ("subprotocol", text(&subprotocol_text(&self.subprotocol))),
        ];
        if let Some(addr) = self.api_listen {
            values.push(("api_listen", text(&addr.to_string())));
        }
        for (key, file) in [
            ("api_token_file", &self.api_token_file),
            ("api_tls_cert", &self.api_tls_cert),
            ("api_tls_key", &self.api_tls_key),
            ("api_tls_client_ca", &self.api_tls_client_ca),
            ("tls_cert", &self.tls_cert),
            ("tls_key", &self.tls_key),
            ("tls_client_ca", &self.tls_client_ca),
        ] {
            if let Some(file) = file {
                values.push((key, path(file)));
            }
        }
        values.push(("security_profile", int(u64::from(self.security_profile))));
        for (key, file) in [
            ("auth_keys_file", &self.auth_keys_file),
            ("default_auth_key_file", &self.default_auth_key_file),
        ] {
            if let Some(file) = file {
                values.push((key, path(file)));
            }
        }
        values.push(("show_secrets", Value::Bool(self.show_secrets)));
        values.push((
            "transaction_ids",
            text(&transaction_ids_text(self.transaction_ids)),
        ));
        values.push((
            "heartbeat_interval",
            int(u64::from(self.heartbeat_interval)),
        ));
        values.push(("validate_inbound", Value::Bool(self.validate_inbound)));
        values.push(("ping_interval", int(u64::from(self.ping_interval))));
        values.push(("pong_timeout", int(u64::from(self.pong_timeout))));
        values.push(("call_timeout", int(self.call_timeout)));
        values.push((
            "history_cap",
            int(u64::try_from(self.history_cap).unwrap_or(u64::MAX)),
        ));
        values.push((
            "transaction_history_cap",
            int(u64::try_from(self.transaction_history_cap).unwrap_or(u64::MAX)),
        ));
        values.push(("json", Value::Bool(self.json)));
        if let Some(file) = &self.log_file {
            values.push(("log_file", path(file)));
        }
        values.push(("stdin", Value::Bool(self.stdin)));
        values.push(("no_tui", Value::Bool(self.no_tui)));
        values.push(("color", text(self.color.name())));
        values.push(("time", text(self.time.mode().name())));
        values
    }

    /// The effective settings as a configuration file (`--print-config=toml`).
    pub fn config_toml(&self) -> String {
        let mut comments =
            vec!["Effective kirby-csms configuration; valid input for --config.".to_owned()];
        if self.api_token.is_some() || !self.auth_keys.is_empty() || self.default_auth_key.is_some()
        {
            comments.push(
                "Inline secrets are not printed; use api_token_file, auth_keys_file, \
                 default_auth_key_file or a [[station]] block with auth_key_file."
                    .to_owned(),
            );
        }
        config_file::render_with_blocks(
            &SCHEMA,
            &self.effective_values(),
            &self.blocks.render_values(),
            &comments,
        )
    }

    /// The effective settings as JSON (`--print-config`).
    pub fn config_json(&self) -> Json {
        config_file::render_json(
            &SCHEMA,
            &self.effective_values(),
            &self.blocks.render_values(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_protocols_match_the_registry() {
        let registry: Vec<&str> = kirby_csms_core::registry::protocols().collect();
        assert_eq!(SCHEMA.protocols, registry.as_slice());
    }
}
