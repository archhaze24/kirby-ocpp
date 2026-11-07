//! Generic loading of TOML configuration files.
//!
//! This module knows nothing about command lines. A binary describes its settings with a
//! [`Schema`] (tables, keys, value kinds); [`load`] and [`parse_str`] validate a file against
//! it and return the merged [`Settings`] of the top level and one selected profile.
//! [`render`] writes settings back as a file that [`parse_str`] accepts.
//!
//! Rules enforced here (documented in `docs/configuration.md`):
//! * at most [`MAX_CONFIG_BYTES`] bytes, valid UTF-8 and TOML;
//! * optional top-level `schema = 1` and `protocol = "<schema protocol>"`, nothing else;
//! * unknown tables and keys are errors, reported with their key path;
//! * wrong value types are errors, reported with their key path;
//! * a [`Kind::Secret`] key is always an error: a file never carries a secret inline;
//! * `[profile.NAME]` tables repeat the sections of the top level; the selected profile
//!   replaces top-level values key by key;
//! * relative [`Kind::Path`] values resolve against the directory of the file.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Largest accepted configuration file in bytes.
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// The only accepted `schema` value.
pub const SCHEMA_VERSION: i64 = 1;

/// Value kind of a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A string.
    Text,
    /// A string holding a file system path; relative paths resolve against the file.
    Path,
    /// An integer.
    Integer,
    /// A boolean of a setting that takes a value.
    Bool,
    /// A boolean of a pure on/off switch.
    Switch,
    /// An array of strings.
    TextList,
    /// A secret: the key exists only to refuse it. `hint` names the allowed alternatives.
    Secret { hint: &'static str },
}

/// Check of a text value: `Err` carries the reason.
pub type TextCheck = fn(&str) -> Result<(), String>;

/// One key of a section.
#[derive(Debug, Clone, Copy)]
pub struct KeySpec {
    /// Key name, unique in the whole schema.
    pub name: &'static str,
    /// Value kind.
    pub kind: Kind,
    /// Extra check of a [`Kind::Text`] value; the error is shown with the key path.
    pub check: Option<TextCheck>,
}

impl KeySpec {
    /// A key of `kind` without an extra check.
    pub const fn new(name: &'static str, kind: Kind) -> Self {
        Self {
            name,
            kind,
            check: None,
        }
    }

    /// A text key with an extra check.
    pub const fn checked(name: &'static str, check: fn(&str) -> Result<(), String>) -> Self {
        Self {
            name,
            kind: Kind::Text,
            check: Some(check),
        }
    }
}

/// A table of keys, addressed by its path (`["tls"]`, `["station", "v16"]`).
#[derive(Debug, Clone, Copy)]
pub struct Section {
    /// Table path from the top level of the file (or of a profile).
    pub path: &'static [&'static str],
    /// Keys of the table.
    pub keys: &'static [KeySpec],
}

/// All sections of one binary's configuration file.
#[derive(Debug, Clone, Copy)]
pub struct Schema {
    /// The accepted `protocol` value.
    pub protocol: &'static str,
    /// Sections in rendering order.
    pub sections: &'static [Section],
}

impl Schema {
    fn section(&self, path: &[&str]) -> Option<&Section> {
        self.sections.iter().find(|section| section.path == path)
    }

    fn has_table(&self, path: &[&str]) -> bool {
        self.sections
            .iter()
            .any(|section| section.path.starts_with(path))
    }

    /// Looks a key up by name in any section.
    pub fn key(&self, name: &str) -> Option<&KeySpec> {
        self.sections
            .iter()
            .flat_map(|section| section.keys)
            .find(|key| key.name == name)
    }

    /// The dotted table path (`tls`, `station.v16`) that holds `key`.
    pub fn section_of(&self, key: &str) -> Option<String> {
        self.sections
            .iter()
            .find(|section| section.keys.iter().any(|spec| spec.name == key))
            .map(|section| section.path.join("."))
    }

    fn table_names(&self, parent: &[&str]) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = Vec::new();
        for section in self.sections {
            if section.path.len() > parent.len()
                && section.path.starts_with(parent)
                && !names.contains(&section.path[parent.len()])
            {
                names.push(section.path[parent.len()]);
            }
        }
        names
    }
}

/// A validated setting value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Text, or a path made absolute.
    Text(String),
    /// Integer.
    Integer(i64),
    /// Boolean.
    Bool(bool),
    /// List of strings.
    List(Vec<String>),
}

/// Merged settings of a file: key name to value.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    values: BTreeMap<&'static str, Value>,
}

impl Settings {
    /// The value of `key`, if the file set it.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    /// Every setting in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &Value)> {
        self.values.iter().map(|(key, value)| (*key, value))
    }

    /// Whether no key is set.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// A configuration file problem; the text names the file and the key path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConfigFileError(String);

/// Reads, validates and merges `path`.
///
/// # Errors
/// The file cannot be read, is larger than [`MAX_CONFIG_BYTES`], is not valid TOML, breaks a
/// schema rule, or `profile` is not defined in it.
pub fn load(
    path: &Path,
    schema: &Schema,
    profile: Option<&str>,
) -> Result<Settings, ConfigFileError> {
    let fail = |reason: String| {
        ConfigFileError(format!("configuration file {}: {reason}", path.display()))
    };
    let file = std::fs::File::open(path).map_err(|error| fail(error.to_string()))?;
    let metadata = file.metadata().map_err(|error| fail(error.to_string()))?;
    if !metadata.is_file() {
        return Err(fail("not a regular file".to_owned()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| fail(error.to_string()))?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(fail(format!("larger than {MAX_CONFIG_BYTES} bytes")));
    }
    let text = String::from_utf8(bytes).map_err(|_| fail("not valid UTF-8".to_owned()))?;
    parse_str(&text, path, schema, profile)
}

/// Validates and merges `text`, which came from `path` (used for messages and to resolve
/// relative paths; the file system is not touched).
///
/// # Errors
/// As [`load`], minus the read errors.
pub fn parse_str(
    text: &str,
    path: &Path,
    schema: &Schema,
    profile: Option<&str>,
) -> Result<Settings, ConfigFileError> {
    let origin = path.display().to_string();
    let fail = |reason: String| ConfigFileError(format!("configuration file {origin}: {reason}"));
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return Err(fail(format!("larger than {MAX_CONFIG_BYTES} bytes")));
    }
    let root: toml::Table = match toml::from_str(text) {
        Ok(table) => table,
        Err(error) => {
            // The message only: the default rendering quotes the offending line, which may
            // hold a secret.
            let place = error.span().map_or_else(String::new, |span| {
                let before = &text[..span.start.min(text.len())];
                let line = before.matches('\n').count() + 1;
                let column = before.rsplit('\n').next().map_or(0, str::len) + 1;
                format!("line {line}, column {column}: ")
            });
            return Err(fail(format!("{place}{}", error.message())));
        }
    };
    let base = base_dir(path).map_err(fail)?;
    let checker = Checker {
        schema,
        base: &base,
    };

    let mut top = BTreeMap::new();
    let mut profiles: BTreeMap<String, BTreeMap<&'static str, Value>> = BTreeMap::new();
    let mut top_tables = toml::Table::new();
    for (key, value) in &root {
        match key.as_str() {
            "schema" => match value.as_integer() {
                Some(SCHEMA_VERSION) => {}
                Some(other) => {
                    return Err(fail(format!(
                        "key `schema`: version {other} is not supported, only {SCHEMA_VERSION}"
                    )));
                }
                None => return Err(fail(type_error("schema", "an integer", value))),
            },
            "protocol" => match value.as_str() {
                Some(name) if name == schema.protocol => {}
                Some(other) => {
                    return Err(fail(format!(
                        "key `protocol`: `{other}` is not supported, only `{}`",
                        schema.protocol
                    )));
                }
                None => return Err(fail(type_error("protocol", "a string", value))),
            },
            "profile" => {
                let Some(table) = value.as_table() else {
                    return Err(fail(type_error("profile", "a table", value)));
                };
                for (name, body) in table {
                    if name.is_empty() {
                        return Err(fail(
                            "`profile`: a profile name must not be empty".to_owned(),
                        ));
                    }
                    let shown = format!("profile.{name}");
                    let Some(body) = body.as_table() else {
                        return Err(fail(type_error(&shown, "a table", body)));
                    };
                    let mut values = BTreeMap::new();
                    checker
                        .walk(body, &[], &shown, true, &mut values)
                        .map_err(fail)?;
                    profiles.insert(name.clone(), values);
                }
            }
            _ => {
                top_tables.insert(key.clone(), value.clone());
            }
        }
    }
    checker
        .walk(&top_tables, &[], "", false, &mut top)
        .map_err(fail)?;

    if let Some(name) = profile {
        let Some(chosen) = profiles.remove(name) else {
            let available = if profiles.is_empty() {
                "the file defines no profiles".to_owned()
            } else {
                format!(
                    "available profiles: {}",
                    profiles.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            };
            return Err(fail(format!("unknown profile `{name}`; {available}")));
        };
        top.extend(chosen);
    }
    Ok(Settings { values: top })
}

fn base_dir(path: &Path) -> Result<PathBuf, String> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    std::path::absolute(parent.unwrap_or_else(|| Path::new(".")))
        .map_err(|error| format!("cannot resolve the directory of the file: {error}"))
}

fn type_name(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::String(_) => "a string",
        toml::Value::Integer(_) => "an integer",
        toml::Value::Float(_) => "a float",
        toml::Value::Boolean(_) => "a boolean",
        toml::Value::Datetime(_) => "a date or time",
        toml::Value::Array(_) => "an array",
        toml::Value::Table(_) => "a table",
    }
}

fn type_error(key: &str, expected: &str, found: &toml::Value) -> String {
    format!(
        "key `{key}`: expected {expected}, found {}",
        type_name(found)
    )
}

struct Checker<'a> {
    schema: &'a Schema,
    base: &'a Path,
}

impl Checker<'_> {
    /// Validates `table`, which sits at `section` (a path below the top level or the profile),
    /// and stores the values by key name. `shown` is the key path prefix for messages.
    fn walk(
        &self,
        table: &toml::Table,
        section: &[&str],
        shown: &str,
        in_profile: bool,
        out: &mut BTreeMap<&'static str, Value>,
    ) -> Result<(), String> {
        for (key, value) in table {
            let key_path = if shown.is_empty() {
                key.clone()
            } else {
                format!("{shown}.{key}")
            };
            let mut child: Vec<&str> = section.to_vec();
            child.push(key);
            if self.schema.has_table(&child) {
                let Some(inner) = value.as_table() else {
                    return Err(type_error(&key_path, "a table", value));
                };
                self.walk(inner, &child, &key_path, in_profile, out)?;
                continue;
            }
            let spec = self
                .schema
                .section(section)
                .and_then(|found| found.keys.iter().find(|spec| spec.name == key));
            let Some(spec) = spec else {
                return Err(self.unknown(&key_path, key, value, section, in_profile));
            };
            let converted = self.convert(spec, &key_path, value)?;
            out.insert(spec.name, converted);
        }
        Ok(())
    }

    fn unknown(
        &self,
        key_path: &str,
        key: &str,
        value: &toml::Value,
        section: &[&str],
        in_profile: bool,
    ) -> String {
        if in_profile && matches!(key, "schema" | "protocol" | "profile") {
            return format!("key `{key_path}`: `{key}` is only valid at the top level of the file");
        }
        let mut valid: Vec<&str> = self.schema.table_names(section);
        let what = if value.is_table() {
            "table"
        } else {
            valid = self
                .schema
                .section(section)
                .map(|found| found.keys.iter().map(|spec| spec.name).collect())
                .unwrap_or_default();
            "key"
        };
        let mut message = format!("unknown {what} `{key_path}`");
        if section.is_empty() && !value.is_table() {
            let tables = self.schema.table_names(&[]);
            let _ = write!(
                message,
                "; settings belong in a table, one of: {}",
                tables.join(", ")
            );
        } else if !valid.is_empty() {
            let _ = write!(message, "; expected one of: {}", valid.join(", "));
        }
        message
    }

    fn convert(
        &self,
        spec: &KeySpec,
        key_path: &str,
        value: &toml::Value,
    ) -> Result<Value, String> {
        match spec.kind {
            Kind::Secret { hint } => Err(format!(
                "key `{key_path}`: a configuration file never holds a secret; {hint}"
            )),
            Kind::Text => {
                let Some(text) = value.as_str() else {
                    return Err(type_error(key_path, "a string", value));
                };
                if let Some(check) = spec.check {
                    check(text).map_err(|reason| format!("key `{key_path}`: {reason}"))?;
                }
                Ok(Value::Text(text.to_owned()))
            }
            Kind::Path => {
                let Some(text) = value.as_str() else {
                    return Err(type_error(key_path, "a string", value));
                };
                if text.is_empty() {
                    return Err(format!("key `{key_path}`: must not be empty"));
                }
                let joined = self.base.join(text);
                let resolved = joined.to_str().ok_or_else(|| {
                    format!("key `{key_path}`: the resolved path is not valid UTF-8")
                })?;
                Ok(Value::Text(resolved.to_owned()))
            }
            Kind::Integer => value
                .as_integer()
                .map(Value::Integer)
                .ok_or_else(|| type_error(key_path, "an integer", value)),
            Kind::Bool | Kind::Switch => value
                .as_bool()
                .map(Value::Bool)
                .ok_or_else(|| type_error(key_path, "a boolean", value)),
            Kind::TextList => {
                let Some(items) = value.as_array() else {
                    return Err(type_error(key_path, "an array of strings", value));
                };
                let mut list = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    let Some(text) = item.as_str() else {
                        return Err(type_error(
                            &format!("{key_path}[{index}]"),
                            "a string",
                            item,
                        ));
                    };
                    list.push(text.to_owned());
                }
                Ok(Value::List(list))
            }
        }
    }
}

/// Writes `values` (key name, value) as a configuration file: `schema`, `protocol`, then
/// the schema's sections in order, keys in schema order. Keys that `values` lacks are left
/// out. `comments` become `#` lines at the top. The text is accepted by [`parse_str`] with
/// the same schema.
pub fn render(schema: &Schema, values: &[(&str, Value)], comments: &[String]) -> String {
    let mut out = String::new();
    for line in comments {
        let _ = writeln!(out, "# {}", line.replace(['\n', '\r'], " "));
    }
    if !comments.is_empty() {
        out.push('\n');
    }
    let _ = writeln!(out, "schema = {SCHEMA_VERSION}");
    let _ = writeln!(out, "protocol = {}", toml_string(schema.protocol));
    for section in schema.sections {
        let lines: Vec<String> = section
            .keys
            .iter()
            .filter_map(|spec| {
                let (_, value) = values.iter().find(|(name, _)| *name == spec.name)?;
                Some(format!("{} = {}", spec.name, toml_value(value)))
            })
            .collect();
        if lines.is_empty() {
            continue;
        }
        let _ = writeln!(out, "\n[{}]", section.path.join("."));
        for line in lines {
            let _ = writeln!(out, "{line}");
        }
    }
    out
}

fn toml_value(value: &Value) -> String {
    match value {
        Value::Text(text) => toml_string(text),
        Value::Integer(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(|item| toml_string(item)).collect();
            format!("[{}]", parts.join(", "))
        }
    }
}

/// A TOML basic string.
fn toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `Ok` when `url` carries no password (`scheme://user:password@host`). Use as the check of a
/// URL key: a file must not hold a credential.
///
/// # Errors
/// The URL has a password.
pub fn reject_url_password(url: &str) -> Result<(), String> {
    match split_userinfo(url) {
        Some((_, userinfo, _)) if userinfo.contains(':') => Err(
            "the URL carries a password, which a configuration file never holds; \
             pass the URL with its password through the command-line flag or the \
             environment variable"
                .to_owned(),
        ),
        _ => Ok(()),
    }
}

/// `url` without the password of its userinfo (`user:secret@host` becomes `user@host`).
pub fn strip_url_password(url: &str) -> String {
    match split_userinfo(url) {
        Some((head, userinfo, tail)) if userinfo.contains(':') => {
            let user = userinfo.split(':').next().unwrap_or_default();
            format!("{head}{user}@{tail}")
        }
        _ => url.to_owned(),
    }
}

/// Splits `scheme://` + userinfo + `@` + rest, when the authority has a userinfo.
fn split_userinfo(url: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let at = rest[..end].rfind('@')?;
    let head_len = scheme.len() + 3;
    Some((&url[..head_len], &rest[..at], &rest[at + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: Schema = Schema {
        protocol: "ocpp1.6",
        sections: &[
            Section {
                path: &["connection"],
                keys: &[
                    KeySpec::checked("url", reject_url_password),
                    KeySpec::new("ws_ping", Kind::Integer),
                    KeySpec::new(
                        "api_token",
                        Kind::Secret {
                            hint: "set `api_token_file` to a file holding it",
                        },
                    ),
                ],
            },
            Section {
                path: &["tls"],
                keys: &[
                    KeySpec::new("tls_ca", Kind::Path),
                    KeySpec::new("tls_skip_verify", Kind::Switch),
                ],
            },
            Section {
                path: &["faults"],
                keys: &[KeySpec::new("rule", Kind::TextList)],
            },
            Section {
                path: &["station"],
                keys: &[KeySpec::new("connectors", Kind::Integer)],
            },
            Section {
                path: &["station", "v16"],
                keys: &[],
            },
        ],
    };

    fn parse(text: &str, profile: Option<&str>) -> Result<Settings, String> {
        parse_str(text, Path::new("/etc/k/cfg.toml"), &SCHEMA, profile).map_err(|e| e.to_string())
    }

    fn err(text: &str) -> String {
        parse(text, None).expect_err("must fail")
    }

    #[test]
    fn accepts_a_complete_file() {
        let settings = parse(
            "schema = 1\nprotocol = \"ocpp1.6\"\n[connection]\nurl = \"ws://h/x\"\nws_ping = 5\n\
             [faults]\nrule = [\"Reset=reject\"]\n[tls]\ntls_skip_verify = true\n[station.v16]\n",
            None,
        )
        .expect("valid");
        assert_eq!(settings.get("url"), Some(&Value::Text("ws://h/x".into())));
        assert_eq!(settings.get("ws_ping"), Some(&Value::Integer(5)));
        assert_eq!(
            settings.get("rule"),
            Some(&Value::List(vec!["Reset=reject".into()]))
        );
        assert_eq!(settings.get("tls_skip_verify"), Some(&Value::Bool(true)));
    }

    #[test]
    fn empty_file_is_valid() {
        assert!(parse("", None).expect("valid").is_empty());
    }

    #[test]
    fn unknown_key_names_the_path_and_the_choices() {
        let message = err("[connection]\nurll = \"x\"\n");
        assert!(message.contains("/etc/k/cfg.toml"), "{message}");
        assert!(
            message.contains("unknown key `connection.urll`"),
            "{message}"
        );
        assert!(message.contains("url"), "{message}");
    }

    #[test]
    fn unknown_table_and_top_level_key_are_rejected() {
        assert!(err("[nope]\nx = 1\n").contains("unknown table `nope`"));
        assert!(err("url = \"x\"\n").contains("unknown key `url`"));
        assert!(err("ocpp_version = \"1.6\"\n").contains("unknown key `ocpp_version`"));
        assert!(err("[station.v16]\nx = 1\n").contains("unknown key `station.v16.x`"));
        assert!(err("[station.v20]\nx = 1\n").contains("unknown table `station.v20`"));
    }

    #[test]
    fn wrong_types_name_the_key() {
        assert!(
            err("[connection]\nws_ping = \"5\"\n")
                .contains("key `connection.ws_ping`: expected an integer, found a string")
        );
        assert!(err("[tls]\ntls_skip_verify = 1\n").contains("expected a boolean"));
        assert!(err("[faults]\nrule = \"x\"\n").contains("expected an array of strings"));
        assert!(err("[faults]\nrule = [\"a\", 2]\n").contains("`faults.rule[1]`"));
        assert!(err("connection = 3\n").contains("key `connection`: expected a table"));
    }

    #[test]
    fn schema_and_protocol_are_checked() {
        assert!(err("schema = 2\n").contains("version 2 is not supported"));
        assert!(err("schema = \"1\"\n").contains("key `schema`"));
        assert!(err("protocol = \"ocpp2.0.1\"\n").contains("`ocpp2.0.1` is not supported"));
        assert!(err("protocol = 16\n").contains("key `protocol`: expected a string"));
        assert!(parse("schema = 1\nprotocol = \"ocpp1.6\"\n", None).is_ok());
    }

    #[test]
    fn profile_overlays_top_level_key_by_key() {
        let text = "[connection]\nurl = \"ws://a/x\"\nws_ping = 5\n\
                    [profile.lab.connection]\nurl = \"ws://lab/x\"\n\
                    [profile.lab.faults]\nrule = [\"Reset=drop\"]\n";
        let plain = parse(text, None).expect("valid");
        assert_eq!(plain.get("url"), Some(&Value::Text("ws://a/x".into())));
        assert_eq!(plain.get("rule"), None);
        let lab = parse(text, Some("lab")).expect("valid");
        assert_eq!(lab.get("url"), Some(&Value::Text("ws://lab/x".into())));
        assert_eq!(lab.get("ws_ping"), Some(&Value::Integer(5)));
        assert_eq!(
            lab.get("rule"),
            Some(&Value::List(vec!["Reset=drop".into()]))
        );
    }

    #[test]
    fn unknown_profile_lists_the_available_ones() {
        let text = "[profile.b.connection]\nws_ping = 1\n[profile.a.connection]\nws_ping = 2\n";
        let message = parse(text, Some("zzz")).expect_err("must fail");
        assert!(message.contains("unknown profile `zzz`"), "{message}");
        assert!(message.contains("available profiles: a, b"), "{message}");
        let none = parse("", Some("x")).expect_err("must fail");
        assert!(none.contains("defines no profiles"), "{none}");
    }

    #[test]
    fn profiles_are_validated_even_when_not_selected() {
        let message = err("[profile.x.connection]\nbogus = 1\n");
        assert!(
            message.contains("`profile.x.connection.bogus`"),
            "{message}"
        );
    }

    #[test]
    fn profile_cannot_nest_or_carry_schema_and_protocol() {
        for key in [
            "schema = 1",
            "protocol = \"ocpp1.6\"",
            "[profile.x.profile]",
        ] {
            let message = err(&format!("[profile.x]\n{key}\n"));
            assert!(
                message.contains("only valid at the top level"),
                "{key}: {message}"
            );
        }
        assert!(err("profile = 1\n").contains("key `profile`: expected a table"));
        assert!(err("[profile]\nx = 1\n").contains("`profile.x`"));
    }

    #[test]
    fn inline_secret_is_rejected_without_echoing_it() {
        let message = err("[connection]\napi_token = \"hunter2\"\n");
        assert!(message.contains("`connection.api_token`"), "{message}");
        assert!(message.contains("api_token_file"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
    }

    #[test]
    fn url_password_is_rejected_without_echoing_it() {
        let message = err("[connection]\nurl = \"ws://cp:hunter2@h:9000/x\"\n");
        assert!(message.contains("`connection.url`"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
        assert!(parse("[connection]\nurl = \"ws://cp@h:9000/x\"\n", None).is_ok());
    }

    #[test]
    fn syntax_errors_show_position_not_the_line() {
        let message = err("[connection]\nurl = \"hunter2\n");
        assert!(message.contains("line 2"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
    }

    #[test]
    fn relative_paths_resolve_against_the_file_directory() {
        let settings = parse("[tls]\ntls_ca = \"certs/ca.pem\"\n", None).expect("valid");
        let expected = std::path::absolute("/etc/k")
            .expect("abs")
            .join("certs/ca.pem");
        assert_eq!(
            settings.get("tls_ca"),
            Some(&Value::Text(expected.to_str().expect("utf8").to_owned()))
        );
        let absolute = std::path::absolute("/var/ca.pem").expect("abs");
        let text = format!(
            "[tls]\ntls_ca = {}\n",
            toml_string(absolute.to_str().expect("utf8"))
        );
        let settings = parse(&text, None).expect("valid");
        assert_eq!(
            settings.get("tls_ca"),
            Some(&Value::Text(absolute.to_str().expect("utf8").to_owned()))
        );
        assert!(err("[tls]\ntls_ca = \"\"\n").contains("must not be empty"));
    }

    #[test]
    fn size_limit_applies_to_files_and_text() {
        let big = format!("# {}\n", "x".repeat(2 * 1024 * 1024));
        assert!(err(&big).contains("larger than"));
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.toml");
        std::fs::write(&path, &big).expect("write");
        let message = load(&path, &SCHEMA, None)
            .expect_err("must fail")
            .to_string();
        assert!(message.contains("larger than"), "{message}");
        assert!(message.contains("big.toml"), "{message}");
    }

    #[test]
    fn load_reports_missing_and_non_utf8_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = load(&dir.path().join("nope.toml"), &SCHEMA, None).expect_err("must fail");
        assert!(missing.to_string().contains("nope.toml"));
        let path = dir.path().join("bin.toml");
        std::fs::write(&path, [0xff, 0xfe]).expect("write");
        assert!(
            load(&path, &SCHEMA, None)
                .expect_err("must fail")
                .to_string()
                .contains("UTF-8")
        );
        let directory = load(dir.path(), &SCHEMA, None).expect_err("must fail");
        assert!(
            directory.to_string().contains("not a regular file")
                || directory.to_string().contains("directory")
        );
    }

    #[test]
    fn render_round_trips_through_parse() {
        let values = vec![
            ("url", Value::Text("ws://h/\"x\"\\y".into())),
            ("ws_ping", Value::Integer(-3)),
            ("tls_skip_verify", Value::Bool(false)),
            ("rule", Value::List(vec!["A=b".into(), "C=d*2".into()])),
            ("connectors", Value::Integer(2)),
        ];
        let text = render(
            &SCHEMA,
            &values,
            &["first".to_owned(), "second\nline".to_owned()],
        );
        assert!(text.starts_with("# first\n# second line\n\nschema = 1\nprotocol = \"ocpp1.6\"\n"));
        let parsed = parse(&text, None).expect("rendered text parses");
        for (key, value) in &values {
            assert_eq!(parsed.get(key), Some(value), "{key}");
        }
        let again = render(
            &SCHEMA,
            &parsed
                .iter()
                .map(|(k, v)| (k, v.clone()))
                .collect::<Vec<_>>(),
            &["first".to_owned(), "second\nline".to_owned()],
        );
        assert_eq!(again, text);
    }

    #[test]
    fn url_password_helpers() {
        assert_eq!(strip_url_password("ws://u:p@h:1/x"), "ws://u@h:1/x");
        assert_eq!(strip_url_password("ws://u@h/x"), "ws://u@h/x");
        assert_eq!(strip_url_password("ws://h/a@b:c"), "ws://h/a@b:c");
        assert_eq!(strip_url_password("not a url"), "not a url");
        assert!(reject_url_password("ws://h/a@b:c").is_ok());
        assert!(reject_url_password("wss://u:p@h/x").is_err());
        assert!(reject_url_password("wss://u@h/x").is_ok());
    }
}
