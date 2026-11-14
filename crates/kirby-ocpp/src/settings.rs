//! Configuration file glue: the schema of the emulator's file and the merge of file values
//! into the command line.
//!
//! The generic parts (loading, validation, profiles, rendering) live in
//! `kirby_ocpp_core::config_file`. Here the file is applied the cheap way: the command line is
//! parsed once; every file key whose argument was not given by flag or environment variable
//! becomes an equivalent `--flag=value` argument, and the command line is parsed again with
//! those appended. That keeps one parser, one set of value checks and one set of defaults.

use std::ffi::OsString;
use std::path::Path;

use clap::ArgMatches;
use clap::parser::ValueSource;
use kirby_ocpp_core::config_file::{
    self, KeySpec, Kind, Schema, Section, Value, reject_url_password,
};

use crate::EXIT_USAGE;
use crate::cli;

/// Settings of the configuration file of `kirby-ocpp`: one table per `--help` heading.
pub const SCHEMA: Schema = Schema {
    protocol: "ocpp1.6",
    sections: &[
        Section {
            path: &["connection"],
            keys: &[
                KeySpec::checked("url", reject_url_password),
                KeySpec::new("ws_subprotocol", Kind::Text),
                KeySpec::new("ws_ping", Kind::Integer),
                KeySpec::new("call_timeout_ms", Kind::Integer),
            ],
        },
        Section {
            path: &["identity"],
            keys: &[
                KeySpec::new("id", Kind::Text),
                KeySpec::new("vendor", Kind::Text),
                KeySpec::new("model", Kind::Text),
                KeySpec::new("serial", Kind::Text),
                KeySpec::new("firmware_version", Kind::Text),
            ],
        },
        Section {
            path: &["station"],
            keys: &[
                KeySpec::new("connector", Kind::Integer),
                KeySpec::new("connectors", Kind::Integer),
                KeySpec::new("heartbeat", Kind::Integer),
                KeySpec::new("nominal_power_w", Kind::Integer),
                KeySpec::new("voltage_v", Kind::Integer),
                KeySpec::new("phases", Kind::Integer),
                KeySpec::new("no_persist", Kind::Switch),
                KeySpec::new("state_dir", Kind::Path),
            ],
        },
        // Reserved for settings that exist in OCPP 1.6 only; none yet.
        Section {
            path: &["station", "v16"],
            keys: &[],
        },
        Section {
            path: &["tls"],
            keys: &[
                KeySpec::new("tls_ca", Kind::Path),
                KeySpec::new("tls_cert", Kind::Path),
                KeySpec::new("tls_key", Kind::Path),
                KeySpec::new("tls_server_name", Kind::Text),
                KeySpec::new("tls_skip_verify", Kind::Switch),
                KeySpec::new("additional_root_certificate_check", Kind::Switch),
                KeySpec::new("security_profile_fallback", Kind::Bool),
                KeySpec::new("authorization_key_encoding", Kind::Text),
            ],
        },
        Section {
            path: &["reconnect"],
            keys: &[
                KeySpec::new("reconnect", Kind::Bool),
                KeySpec::new("no_reconnect", Kind::Switch),
                KeySpec::new("reconnect_initial_ms", Kind::Integer),
                KeySpec::new("reconnect_max_ms", Kind::Integer),
                KeySpec::new("reconnect_max_attempts", Kind::Integer),
            ],
        },
        Section {
            path: &["fleet"],
            keys: &[
                KeySpec::new("count", Kind::Integer),
                KeySpec::new("id_template", Kind::Text),
                KeySpec::new("first_index", Kind::Integer),
                KeySpec::new("stagger_ms", Kind::Integer),
            ],
        },
        Section {
            path: &["faults"],
            keys: &[KeySpec::new("rule", Kind::TextList)],
        },
        Section {
            path: &["output"],
            keys: &[
                KeySpec::new("show_secrets", Kind::Switch),
                KeySpec::new("no_tui", Kind::Switch),
                KeySpec::new("json", Kind::Switch),
                KeySpec::new("log_file", Kind::Path),
                KeySpec::new("verbose", Kind::Switch),
                KeySpec::new("quiet", Kind::Switch),
                KeySpec::new("color", Kind::Text),
                KeySpec::new("time", Kind::Text),
            ],
        },
    ],
    blocks: &[],
};

/// A command-line argument that was filled from the file.
struct Synthesized {
    flag: String,
    key: &'static str,
}

/// Parses the command line, with the configuration file applied when `--config` is given.
/// Exits like clap on `--help`, `--version` and usage errors; a configuration error exits 2.
pub fn parse_matches(args: Vec<OsString>) -> ArgMatches {
    let first = cli::command().get_matches_from(args.clone());
    let offline = matches!(
        first.subcommand_name(),
        Some("scenarios" | "version" | "completions")
    );
    let config = first.get_one::<std::path::PathBuf>("config");
    let profile = first.get_one::<String>("profile");
    if offline || (config.is_none() && profile.is_none()) {
        return first;
    }
    let Some(path) = config else {
        fail("--profile (or KIRBY_OCPP_PROFILE) needs --config (or KIRBY_OCPP_CONFIG)");
    };
    let settings = match config_file::load(path, &SCHEMA, profile.map(String::as_str)) {
        Ok(settings) => settings,
        Err(error) => fail(&error.to_string()),
    };
    let (extra, synthesized) = synthesize(&first, &settings);
    if extra.is_empty() {
        return first;
    }
    let mut merged = args;
    let at = merged
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(merged.len());
    merged.splice(at..at, extra);
    match cli::command().try_get_matches_from(merged) {
        Ok(matches) => matches,
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
    let persist_flag = matches
        .subcommand_matches("run")
        .is_some_and(|run| run.get_flag("persist"));
    let mut args = Vec::new();
    let mut synthesized = Vec::new();
    for (key, value) in settings.iter() {
        if explicit(matches, key) {
            continue;
        }
        // Exclusions: the file never turns a flag or variable that is set into a conflict.
        let excluded = match key {
            "no_persist" => persist_flag,
            "no_reconnect" => explicit(matches, "reconnect"),
            "verbose" => explicit(matches, "quiet"),
            "quiet" => explicit(matches, "verbose"),
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
