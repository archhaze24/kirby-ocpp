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
//! * relative [`Kind::Path`] values resolve against the directory of the file;
//! * a [`BlockSpec`] describes an array of tables (`[[station]]`): keys, required and reserved
//!   keys, a unique key and a check per element; a selected profile that defines the array
//!   replaces the top-level one.

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

/// Check of a whole block after its keys were converted: `Err` carries the reason.
pub type BlockCheck = fn(&Block) -> Result<(), String>;

/// An array of tables (`[[station]]`): every element holds the same keys.
#[derive(Debug, Clone, Copy)]
pub struct BlockSpec {
    /// Name of the array at the top level (and inside a profile).
    pub name: &'static str,
    /// Keys of one element.
    pub keys: &'static [KeySpec],
    /// Keys every element must set.
    pub required: &'static [&'static str],
    /// Keys that are reserved for later releases: `(key, message)`. Using one is an error
    /// that carries the message.
    pub reserved: &'static [(&'static str, &'static str)],
    /// A key whose value must differ between the elements of one array.
    pub unique: Option<&'static str>,
    /// Check of one element after its keys were converted.
    pub check: Option<BlockCheck>,
}

/// The protocol rendered at the top of a file: the first accepted one.
fn default_protocol(schema: &Schema) -> &'static str {
    schema.protocols.first().copied().unwrap_or_default()
}

/// All sections of one binary's configuration file.
#[derive(Debug, Clone, Copy)]
pub struct Schema {
    /// The accepted top-level `protocol` values; the first is the rendered default.
    pub protocols: &'static [&'static str],
    /// Sections in rendering order.
    pub sections: &'static [Section],
    /// Arrays of tables in rendering order, after the sections.
    pub blocks: &'static [BlockSpec],
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

    fn block(&self, name: &str) -> Option<&BlockSpec> {
        self.blocks.iter().find(|block| block.name == name)
    }

    fn table_names(&self, parent: &[&str]) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = Vec::new();
        if parent.is_empty() {
            names.extend(self.blocks.iter().map(|block| block.name));
        }
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

/// One element of an array of tables: key name to value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Block {
    values: BTreeMap<&'static str, Value>,
}

impl Block {
    /// The value of `key`, if the element set it.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    /// The text value of `key`, if the element set it.
    pub fn text(&self, key: &str) -> Option<&str> {
        match self.values.get(key) {
            Some(Value::Text(text)) => Some(text),
            _ => None,
        }
    }

    /// The integer value of `key`, if the element set it.
    pub fn integer(&self, key: &str) -> Option<i64> {
        match self.values.get(key) {
            Some(Value::Integer(number)) => Some(*number),
            _ => None,
        }
    }
}

/// Merged settings of a file: key name to value.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    values: BTreeMap<&'static str, Value>,
    blocks: BTreeMap<&'static str, Vec<Block>>,
}

impl Settings {
    /// The elements of the array of tables `name` (empty when the file has none). A selected
    /// profile that defines the array replaces the one of the top level as a whole.
    pub fn blocks(&self, name: &str) -> &[Block] {
        self.blocks.get(name).map_or(&[], Vec::as_slice)
    }

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
        self.values.is_empty() && self.blocks.is_empty()
    }
}

/// A configuration file problem; the text names the file and the key path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConfigFileError(String);

/// Arrays of tables by name.
type Blocks = BTreeMap<&'static str, Vec<Block>>;

/// Values and blocks one profile sets.
type Profile = (BTreeMap<&'static str, Value>, Blocks);

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
                let column = before.rsplit('\n').next().map_or(0, |s| s.chars().count()) + 1;
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
    let mut top_blocks: BTreeMap<&'static str, Vec<Block>> = BTreeMap::new();
    let mut profiles: BTreeMap<String, Profile> = BTreeMap::new();
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
                Some(name) if schema.protocols.contains(&name) => {}
                Some(other) => {
                    return Err(fail(format!(
                        "key `protocol`: `{other}` is not supported, expected one of {}",
                        schema.protocols.join(", ")
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
                    let mut blocks = BTreeMap::new();
                    checker
                        .walk(body, &[], &shown, true, (&mut values, &mut blocks))
                        .map_err(fail)?;
                    profiles.insert(name.clone(), (values, blocks));
                }
            }
            _ => {
                top_tables.insert(key.clone(), value.clone());
            }
        }
    }
    checker
        .walk(&top_tables, &[], "", false, (&mut top, &mut top_blocks))
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
        top.extend(chosen.0);
        top_blocks.extend(chosen.1);
    }
    Ok(Settings {
        values: top,
        blocks: top_blocks,
    })
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
        out: (&mut BTreeMap<&'static str, Value>, &mut Blocks),
    ) -> Result<(), String> {
        let (out, blocks_out) = out;
        for (key, value) in table {
            let key_path = if shown.is_empty() {
                key.clone()
            } else {
                format!("{shown}.{key}")
            };
            if section.is_empty()
                && let Some(spec) = self.schema.block(key)
            {
                let list = self.block_list(spec, &key_path, value)?;
                blocks_out.insert(spec.name, list);
                continue;
            }
            let mut child: Vec<&str> = section.to_vec();
            child.push(key);
            if self.schema.has_table(&child) {
                let Some(inner) = value.as_table() else {
                    return Err(type_error(&key_path, "a table", value));
                };
                self.walk(inner, &child, &key_path, in_profile, (out, blocks_out))?;
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

    /// Validates an array of tables.
    fn block_list(
        &self,
        spec: &BlockSpec,
        key_path: &str,
        value: &toml::Value,
    ) -> Result<Vec<Block>, String> {
        let Some(items) = value.as_array() else {
            return Err(type_error(key_path, "an array of tables", value));
        };
        let mut list: Vec<Block> = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let shown = format!("{key_path}[{index}]");
            let Some(table) = item.as_table() else {
                return Err(type_error(&shown, "a table", item));
            };
            let mut block = Block::default();
            for (name, entry) in table {
                let entry_path = format!("{shown}.{name}");
                if let Some((_, message)) = spec.reserved.iter().find(|(key, _)| key == name) {
                    return Err(format!("key `{entry_path}`: {message}"));
                }
                let Some(key_spec) = spec.keys.iter().find(|key| key.name == name) else {
                    let valid: Vec<&str> = spec.keys.iter().map(|key| key.name).collect();
                    return Err(format!(
                        "unknown key `{entry_path}`; expected one of: {}",
                        valid.join(", ")
                    ));
                };
                let converted = self.convert(key_spec, &entry_path, entry)?;
                block.values.insert(key_spec.name, converted);
            }
            if let Some(missing) = spec
                .required
                .iter()
                .find(|name| !block.values.contains_key(*name))
            {
                return Err(format!("`{shown}`: the key `{missing}` is required"));
            }
            if let Some(check) = spec.check {
                check(&block).map_err(|reason| format!("`{shown}`: {reason}"))?;
            }
            if let Some(unique) = spec.unique
                && let Some(value) = block.values.get(unique)
                && list
                    .iter()
                    .any(|other| other.values.get(unique) == Some(value))
            {
                return Err(format!(
                    "`{shown}`: the value of `{unique}` repeats an earlier element of `{key_path}`"
                ));
            }
            list.push(block);
        }
        Ok(list)
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
    render_with_blocks(schema, values, &[], comments)
}

/// One element of an array of tables to render: key name and value, in any order.
pub type BlockValues<'a> = Vec<(&'a str, Value)>;

/// Like [`render`], with arrays of tables: `blocks` maps an array name of the schema to its
/// elements. Each element is written as `[[name]]` with its keys in schema order.
pub fn render_with_blocks(
    schema: &Schema,
    values: &[(&str, Value)],
    blocks: &[(&str, Vec<BlockValues<'_>>)],
    comments: &[String],
) -> String {
    let mut out = String::new();
    for line in comments {
        let _ = writeln!(out, "# {}", line.replace(['\n', '\r'], " "));
    }
    if !comments.is_empty() {
        out.push('\n');
    }
    let _ = writeln!(out, "schema = {SCHEMA_VERSION}");
    let _ = writeln!(out, "protocol = {}", toml_string(default_protocol(schema)));
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
    for spec in schema.blocks {
        let Some((_, elements)) = blocks.iter().find(|(name, _)| *name == spec.name) else {
            continue;
        };
        for element in elements {
            let _ = writeln!(out, "\n[[{}]]", spec.name);
            for key in spec.keys {
                if let Some((_, value)) = element.iter().find(|(name, _)| *name == key.name) {
                    let _ = writeln!(out, "{} = {}", key.name, toml_value(value));
                }
            }
        }
    }
    out
}

/// The settings as a JSON object: `schema`, `protocol`, one object per table of the schema
/// (keys that `values` lacks are left out) and one array per block name.
pub fn render_json(
    schema: &Schema,
    values: &[(&str, Value)],
    blocks: &[(&str, Vec<BlockValues<'_>>)],
) -> serde_json::Value {
    let json_value = |value: &Value| match value {
        Value::Text(text) => serde_json::Value::from(text.as_str()),
        Value::Integer(number) => serde_json::Value::from(*number),
        Value::Bool(flag) => serde_json::Value::from(*flag),
        Value::List(items) => serde_json::Value::from(items.clone()),
    };
    let mut root = serde_json::Map::new();
    root.insert("schema".to_owned(), SCHEMA_VERSION.into());
    root.insert("protocol".to_owned(), default_protocol(schema).into());
    for section in schema.sections {
        let mut table = serde_json::Map::new();
        for spec in section.keys {
            if let Some((_, value)) = values.iter().find(|(name, _)| *name == spec.name) {
                table.insert(spec.name.to_owned(), json_value(value));
            }
        }
        if table.is_empty() {
            continue;
        }
        if let Some(target) = json_table_at(&mut root, section.path) {
            target.extend(table);
        }
    }
    for spec in schema.blocks {
        let Some((_, elements)) = blocks.iter().find(|(name, _)| *name == spec.name) else {
            continue;
        };
        let list: Vec<serde_json::Value> = elements
            .iter()
            .map(|element| {
                let mut object = serde_json::Map::new();
                for key in spec.keys {
                    if let Some((_, value)) = element.iter().find(|(name, _)| *name == key.name) {
                        object.insert(key.name.to_owned(), json_value(value));
                    }
                }
                serde_json::Value::Object(object)
            })
            .collect();
        root.insert(spec.name.to_owned(), list.into());
    }
    serde_json::Value::Object(root)
}

/// The JSON object at `path` below `root`, created on the way; `None` when a value is in
/// the way.
fn json_table_at<'a>(
    root: &'a mut serde_json::Map<String, serde_json::Value>,
    path: &[&str],
) -> Option<&'a mut serde_json::Map<String, serde_json::Value>> {
    let Some((first, rest)) = path.split_first() else {
        return Some(root);
    };
    let entry = root
        .entry((*first).to_owned())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    json_table_at(entry.as_object_mut()?, rest)
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
    if has_password(url) {
        Err(
            "the URL carries a password, which a configuration file never holds; \
             pass the URL with its password through the command-line flag or the \
             environment variable"
                .to_owned(),
        )
    } else {
        Ok(())
    }
}

/// `url` without the password of its userinfo (`user:secret@host` becomes `user@host`).
pub fn strip_url_password(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) if !parsed.cannot_be_a_base() => {
            if parsed.password().is_none() {
                return url.to_owned();
            }
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        _ => match split_userinfo(url) {
            Some((head, userinfo, tail)) if userinfo.contains(':') => {
                let user = userinfo.split(':').next().unwrap_or_default();
                format!("{head}{user}@{tail}")
            }
            _ => url.to_owned(),
        },
    }
}

/// Whether the URL has a password. A string the `url` crate cannot parse is checked by text:
/// anything with a colon before its last `@` counts.
fn has_password(url: &str) -> bool {
    match url::Url::parse(url) {
        Ok(parsed) if !parsed.cannot_be_a_base() => parsed.password().is_some(),
        _ => split_userinfo(url).is_some_and(|(_, userinfo, _)| userinfo.contains(':')),
    }
}

/// Splits `scheme://` + userinfo + `@` + rest at the last `@`, for text that is not a valid URL
/// (delimiters inside the userinfo cannot be told from the end of the authority).
fn split_userinfo(url: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let at = rest.rfind('@')?;
    let head_len = scheme.len() + 3;
    Some((&url[..head_len], &rest[..at], &rest[at + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: Schema = Schema {
        protocols: &["ocpp1.6"],
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
        blocks: &[BlockSpec {
            name: "entry",
            keys: &[
                KeySpec::new("id", Kind::Text),
                KeySpec::new("count", Kind::Integer),
                KeySpec::new("file", Kind::Path),
                KeySpec::new("token", Kind::Secret { hint: "use `file`" }),
            ],
            required: &["id"],
            reserved: &[("scope", "per-scope entries are not supported yet")],
            unique: Some("id"),
            check: Some(|block| match block.integer("count") {
                Some(number) if number < 0 => Err("`count` must not be negative".to_owned()),
                _ => Ok(()),
            }),
        }],
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
        assert!(
            err("protocol = \"ocpp2.0.1\"\n")
                .contains("`ocpp2.0.1` is not supported, expected one of ocpp1.6")
        );
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
    fn syntax_error_columns_count_characters() {
        let ascii = err("[connection]\nurl = \"aa\" x\n");
        let wide = err("[connection]\nurl = \"éé\" x\n");
        let column = |message: &str| {
            message
                .split("column ")
                .nth(1)
                .and_then(|rest| rest.split(':').next())
                .map(str::to_owned)
        };
        assert!(column(&ascii).is_some(), "{ascii}");
        assert_eq!(column(&ascii), column(&wide), "{ascii} / {wide}");
    }

    #[test]
    fn url_passwords_hidden_from_the_first_slash_are_rejected() {
        for url in [
            "ws:///CP:hunter2@h:9000/ocpp",
            "ws:\\\\CP:hunter2@h/ocpp",
            "ws://CP:hunter2@h:9000/ocpp",
        ] {
            // A TOML literal string keeps the backslashes.
            let text = format!("[connection]\nurl = '{url}'\n");
            let message = err(&text);
            assert!(message.contains("`connection.url`"), "{url}: {message}");
            assert!(!message.contains("hunter2"), "{url}: {message}");
            assert!(!strip_url_password(url).contains("hunter2"), "{url}");
        }
        assert!(reject_url_password("ws:///CP@h/ocpp").is_ok());
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
        // Opening a directory fails on Windows ("Access is denied") before the file type check.
        assert!(
            directory.to_string().starts_with("configuration file "),
            "{directory}"
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
    fn blocks_are_validated_and_profiles_replace_them() {
        let text = "[[entry]]\nid = \"a\"\nfile = \"k/a.key\"\n\n[[entry]]\nid = \"b\"\ncount = 2\n\n\
                    [[profile.p.entry]]\nid = \"z\"\n";
        let plain = parse(text, None).expect("valid");
        let list = plain.blocks("entry");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].text("id"), Some("a"));
        let expected = std::path::absolute("/etc/k").expect("abs").join("k/a.key");
        assert_eq!(list[0].text("file"), expected.to_str());
        assert_eq!(list[1].integer("count"), Some(2));
        let chosen = parse(text, Some("p")).expect("valid");
        assert_eq!(chosen.blocks("entry").len(), 1);
        assert_eq!(chosen.blocks("entry")[0].text("id"), Some("z"));
        assert_eq!(plain.blocks("other").len(), 0);
    }

    #[test]
    fn block_errors_name_the_element_and_key() {
        assert!(err("[[entry]]\ncount = 1\n").contains("`entry[0]`: the key `id` is required"));
        assert!(err("[[entry]]\nid = \"a\"\nnope = 1\n").contains("unknown key `entry[0].nope`"));
        assert!(
            err("[[entry]]\nid = \"a\"\nscope = \"x\"\n")
                .contains("key `entry[0].scope`: per-scope entries are not supported yet")
        );
        assert!(
            err("[[entry]]\nid = \"a\"\n[[entry]]\nid = \"a\"\n")
                .contains("`entry[1]`: the value of `id` repeats")
        );
        assert!(err("[[entry]]\nid = \"a\"\ncount = -1\n").contains("`entry[0]`: `count` must"));
        assert!(err("entry = 3\n").contains("key `entry`: expected an array of tables"));
        assert!(err("[[profile.p.entry]]\ncount = 1\n").contains("`profile.p.entry[0]`"));
        let secret = err("[[entry]]\nid = \"a\"\ntoken = \"hunter2\"\n");
        assert!(secret.contains("key `entry[0].token`"), "{secret}");
        assert!(secret.contains("use `file`"), "{secret}");
        assert!(!secret.contains("hunter2"), "{secret}");
    }

    #[test]
    fn blocks_render_and_round_trip() {
        let values = [("ws_ping", Value::Integer(5))];
        let key_path = std::path::absolute("/x/a.key")
            .expect("abs")
            .to_str()
            .expect("utf8")
            .to_owned();
        let blocks = [(
            "entry",
            vec![
                vec![
                    ("file", Value::Text(key_path.clone())),
                    ("id", Value::Text("a".to_owned())),
                ],
                vec![
                    ("id", Value::Text("b".to_owned())),
                    ("count", Value::Integer(3)),
                ],
            ],
        )];
        let text = render_with_blocks(&SCHEMA, &values, &blocks, &[]);
        let parsed = parse(&text, None).expect("rendered text parses");
        assert_eq!(parsed.blocks("entry").len(), 2);
        assert_eq!(
            parsed.blocks("entry")[0].text("file"),
            Some(key_path.as_str())
        );
        let json = render_json(&SCHEMA, &values, &blocks);
        assert_eq!(json["connection"]["ws_ping"], 5);
        assert_eq!(json["entry"][1]["count"], 3);
        assert_eq!(json["protocol"], "ocpp1.6");
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
