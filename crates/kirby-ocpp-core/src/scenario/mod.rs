//! Scenarios: sequences of station actions described as data, plus a runner.
//!
//! UI-free: the TUI, the headless CLI and tests all use [`run_scenario`] /
//! [`ScenarioRunner`]. Scenario files are TOML (see [`builtin`] for examples for the format).

pub mod builtin;
mod junit;
mod model;
mod model2;
mod runner;
mod target2;
pub mod vars;

use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::watch;

pub use self::junit::{
    JunitRetries, JunitRun, JunitSuite, junit_xml, junit_xml_retried, junit_xml_retried_for,
    junit_xml_runs, junit_xml_runs_for,
};
pub use self::model::{
    Action, Condition, DEFAULT_TIMEOUT_MS, Expect, Scenario, Step, read_scenario_file,
};
pub use self::model2::{Action2, ComponentRef, Condition2, VariableRef};
pub use self::runner::{
    RunStatus, ScenarioProgress, ScenarioReport, ScenarioRunner, StepDetail, StepReport,
    StepStatus, run_scenario, run_scenario_subscribed, run_scenario_with_events,
    run_scenario_with_vars,
};
pub use self::vars::Vars;

/// Why a scenario could not be loaded or started.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ScenarioError {
    /// The file is not a valid scenario.
    #[error("invalid scenario: {0}")]
    Invalid(String),
    /// The file is not a valid scenario; `line` and `column` (1-based) are known when the
    /// parser could point at the spot.
    #[error("{}{}: invalid scenario: {message}", path.display(), location(*line, *column))]
    InvalidFile {
        path: PathBuf,
        line: Option<usize>,
        column: Option<usize>,
        message: String,
    },
    #[error("cannot read scenario file {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A scenario is already running on this station.
    #[error("a scenario is already running on this station")]
    AlreadyRunning,
}

fn location(line: Option<usize>, column: Option<usize>) -> String {
    match (line, column) {
        (Some(line), Some(column)) => format!(":{line}:{column}"),
        (Some(line), None) => format!(":{line}"),
        _ => String::new(),
    }
}

/// Version of the scenario file format, the script format and every JSON report.
pub const SCHEMA_VERSION: u32 = 1;

/// The OCPP version scenarios and reports speak (the only one this build supports).
pub const OCPP_VERSION: &str = crate::ocpp::Protocol::V16.ocpp_version();

/// Version of this crate (and, by workspace versioning, of the tools built on it).
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The OCPP versions a scenario file may declare, for messages.
const SCENARIO_VERSIONS: &str = "1.6 and 2.0.1";

/// Reads the optional `ocpp_version` key of a scenario file: `None` when absent, the protocol
/// otherwise. Scenario files may declare `"1.6"` or `"2.0.1"`.
///
/// # Errors
/// A message for any other value.
pub fn parse_contract_version(
    ocpp_version: Option<&str>,
) -> Result<Option<crate::ocpp::Protocol>, String> {
    use crate::ocpp::Protocol;
    match ocpp_version {
        None => Ok(None),
        Some(text) if text == Protocol::V16.ocpp_version() => Ok(Some(Protocol::V16)),
        Some(text) if text == Protocol::V201.ocpp_version() => Ok(Some(Protocol::V201)),
        Some(version) => Err(format!(
            "`ocpp_version = {version:?}`: this build supports OCPP {SCENARIO_VERSIONS} only"
        )),
    }
}

fn check_schema_key(schema: Option<i64>) -> Result<(), String> {
    if let Some(schema) = schema
        && schema != i64::from(SCHEMA_VERSION)
    {
        return Err(format!(
            "unsupported schema {schema} (this build reads schema {SCHEMA_VERSION})"
        ));
    }
    Ok(())
}

/// Checks the optional `ocpp_version` and `schema` keys of a CSMS script file. Scripts speak
/// OCPP 1.6 only for now, so this accepts `"1.6"` / `1` and nothing else; scenario files use
/// [`parse_contract_version`].
///
/// # Errors
/// A message for any value other than `"1.6"` / `1`.
pub fn check_contract_keys(ocpp_version: Option<&str>, schema: Option<i64>) -> Result<(), String> {
    if let Some(version) = ocpp_version
        && version != OCPP_VERSION
    {
        return Err(format!(
            "`ocpp_version = {version:?}`: this build supports OCPP {OCPP_VERSION} only"
        ));
    }
    check_schema_key(schema)
}

/// [`check_contract_keys`] for scenario files: the declared protocol, if any.
pub(crate) fn check_scenario_contract(
    ocpp_version: Option<&str>,
    schema: Option<i64>,
) -> Result<Option<crate::ocpp::Protocol>, String> {
    let protocol = parse_contract_version(ocpp_version)?;
    check_schema_key(schema)?;
    Ok(protocol)
}

/// Checks the version keys of a CSMS script file before the file is parsed in full, so a file
/// written for a later version reports the version problem and not "unknown field". Text that
/// is not valid TOML is left to the real parser.
///
/// # Errors
/// A wrong value type, an unsupported `schema` or an unsupported `ocpp_version`.
pub fn precheck_contract(text: &str) -> Result<(), String> {
    precheck_with(text, check_contract_keys)
}

/// [`precheck_contract`] for scenario files (`"2.0.1"` is a valid version there).
pub(crate) fn precheck_scenario_contract(text: &str) -> Result<(), String> {
    precheck_with(text, |version, schema| {
        check_scenario_contract(version, schema).map(drop)
    })
}

fn precheck_with(
    text: &str,
    check: impl FnOnce(Option<&str>, Option<i64>) -> Result<(), String>,
) -> Result<(), String> {
    let Ok(table) = text.parse::<toml::Table>() else {
        return Ok(());
    };
    let ocpp_version = match table.get("ocpp_version") {
        None => None,
        Some(toml::Value::String(s)) => Some(s.as_str()),
        Some(_) => return Err("`ocpp_version` must be a string".to_owned()),
    };
    let schema = match table.get("schema") {
        None => None,
        Some(toml::Value::Integer(n)) => Some(*n),
        Some(_) => return Err("`schema` must be an integer".to_owned()),
    };
    if let Some(tags) = table.get("tags")
        && !tags
            .as_array()
            .is_some_and(|a| a.iter().all(toml::Value::is_str))
    {
        return Err("`tags` must be an array of strings".to_owned());
    }
    check(ocpp_version, schema)
}

/// Step keys reserved for OCPP 2.x steps.
const RESERVED_STEP_KEYS: [&str; 2] = ["evse", "id_token"];

/// Rejects the reserved step keys with a dedicated message.
///
/// # Errors
/// A message naming the key and the action.
pub fn check_reserved_keys(table: &toml::Table) -> Result<(), String> {
    for key in RESERVED_STEP_KEYS {
        if table.contains_key(key) {
            let action = table.get("action").and_then(toml::Value::as_str);
            let prefix = action.map_or_else(String::new, |a| format!("action `{a}`: "));
            return Err(format!(
                "{prefix}`{key}` is reserved for OCPP 2.x steps; not supported in this build"
            ));
        }
    }
    Ok(())
}

/// Replaces secret values in step parameters with the redaction marker: the `AuthorizationKey`
/// value and the values of secret device-model variables (the same rule
/// [`crate::ocpp::redact_secrets`] applies to frames). Works on the shape of every payload
/// key (`payload`, `match`, `payload_contains`, `result`, `expect.payload`), whatever the
/// call is named, and on the `value` of a 2.0.1 `set_variable` step.
pub fn redact_params(params: &mut serde_json::Value) {
    use serde_json::Value;
    let Some(fields) = params.as_object_mut() else {
        return;
    };
    if let Some(Value::Object(expect)) = fields.get_mut("expect")
        && let Some(payload) = expect.get_mut("payload")
    {
        redact_payload(payload);
    }
    for key in [
        "expect_payload",
        "payload",
        "match",
        "payload_contains",
        "result",
    ] {
        if let Some(payload) = fields.get_mut(key) {
            redact_payload(payload);
        }
    }
    // A `set_variable` step stores `component`, `variable` and `value` (the payload rule
    // looks for `attributeValue`), so the value is hidden when the names say it is a secret.
    if crate::ocpp::frame::names_secret_variable(fields)
        && let Some(value) = fields.get_mut("value")
    {
        *value = Value::from(crate::ocpp::REDACTED);
    }
}

/// Replaces the `value` next to `"key": "AuthorizationKey"` anywhere inside `payload` with the
/// redaction marker. For failure evidence (expected and actual payloads), which is not tied to a
/// call name.
pub use crate::ocpp::redact_payload;

/// A copy of `payload` with [`redact_payload`] applied.
#[must_use]
pub fn redacted(payload: &serde_json::Value) -> serde_json::Value {
    let mut copy = payload.clone();
    redact_payload(&mut copy);
    copy
}

/// Current time as RFC 3339 UTC with millisecond precision, for report `started_at`.
#[must_use]
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Converts a byte offset in `text` to 1-based `(line, column)` (columns count characters).
#[must_use]
pub fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |tail| tail.chars().count())
        + 1;
    (line, column)
}

/// Longest name a suggestion is computed for.
const SUGGEST_MAX_CHARS: usize = 64;

/// Edit distance with adjacent transpositions counting as one edit, or `None` when it is
/// certainly above `max`. Keeps three rows.
fn edit_distance(a: &str, b: &str, max: usize) -> Option<usize> {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let mut older: Vec<usize> = Vec::new();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut row = vec![0usize; b.len() + 1];
        row[0] = i;
        for j in 1..=b.len() {
            let same = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (previous[j] + 1)
                .min(row[j - 1] + 1)
                .min(previous[j - 1] + same);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(older[j - 2] + 1);
            }
            row[j] = best;
        }
        older = std::mem::replace(&mut previous, row);
    }
    Some(previous[b.len()])
}

/// The closest of `candidates` to `word`, when it is close enough to be a likely typo.
#[must_use]
pub fn closest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let length = word.chars().count();
    if length > SUGGEST_MAX_CHARS {
        return None;
    }
    let limit = (length / 3).clamp(1, 3);
    candidates
        .into_iter()
        .filter(|candidate| candidate.chars().count() <= SUGGEST_MAX_CHARS)
        .filter_map(|candidate| Some((edit_distance(word, candidate, limit)?, candidate)))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

/// Appends `; did you mean `x`?` to a serde "unknown field/variant `y`, expected one of ..."
/// message when `y` is close to one of the expected names. Other messages pass through.
#[must_use]
pub fn with_suggestion(message: String) -> String {
    let quoted = |text: &str| -> Vec<String> {
        text.split('`')
            .skip(1)
            .step_by(2)
            .map(str::to_owned)
            .collect()
    };
    let Some(at) = message
        .find("unknown field `")
        .or_else(|| message.find("unknown variant `"))
    else {
        return message;
    };
    let tail = &message[at..];
    let names = quoted(tail);
    let Some((unknown, expected)) = names.split_first() else {
        return message;
    };
    match closest(unknown, expected.iter().map(String::as_str)) {
        Some(found) => format!("{message}; did you mean `{found}`?"),
        None => message,
    }
}

impl ScenarioError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

/// Cooperative cancellation for a running scenario. Cloneable; all clones share state.
#[derive(Debug, Clone)]
pub struct CancelToken {
    tx: Arc<watch::Sender<bool>>,
    rx: watch::Receiver<bool>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self {
            tx: Arc::new(tx),
            rx,
        }
    }

    pub fn cancel(&self) {
        self.tx.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolves once [`CancelToken::cancel`] was called.
    pub async fn cancelled(&self) {
        let mut rx = self.rx.clone();
        // The sender lives in `self`, so the channel cannot close.
        let _ = rx.wait_for(|cancelled| *cancelled).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redact_payload_finds_the_key_anywhere() {
        let mut value = json!({
            "a": [{"key": "authorizationkey", "value": "secret"}],
            "b": {"key": "Other", "value": "kept"},
        });
        redact_payload(&mut value);
        assert_eq!(value["a"][0]["value"], "***");
        assert_eq!(value["b"]["value"], "kept");
    }

    #[test]
    fn redact_params_covers_expect_payloads() {
        let mut params = json!({
            "call": "Other",
            "expect": {"payload": {"key": "AuthorizationKey", "value": "secret"}},
            "expect_payload": {"key": "AuthorizationKey", "value": "secret"},
        });
        redact_params(&mut params);
        assert!(!params.to_string().contains("secret"));
    }

    const SENTINEL: &str = "sentinel-made-up-password-1f3a";

    fn password_payload() -> serde_json::Value {
        json!({"setVariableData": [{
            "attributeValue": SENTINEL,
            "component": {"name": "SecurityCtrlr"},
            "variable": {"name": "BasicAuthPassword"},
        }]})
    }

    #[test]
    fn redact_params_hides_device_model_secrets_in_every_payload_key() {
        for (key, call) in [
            ("payload", "SetVariables"),
            ("match", "SetVariables"),
            ("payload_contains", "SetVariables"),
            ("result", "GetVariables"),
        ] {
            let mut params = json!({"call": call});
            params[key] = password_payload();
            redact_params(&mut params);
            assert!(
                !params.to_string().contains(SENTINEL),
                "{key} leaked: {params}"
            );
            assert!(params.to_string().contains("***"), "{key}: {params}");
        }
    }

    #[test]
    fn redact_params_is_shape_based_for_any_call_name() {
        let mut params = json!({
            "call": "Whatever",
            "payload": {"key": "AuthorizationKey", "value": SENTINEL},
            "expect": {"payload": password_payload()},
        });
        redact_params(&mut params);
        assert!(!params.to_string().contains(SENTINEL), "{params}");
        // Other values stay readable.
        let mut params = json!({"call": "Heartbeat", "payload": {"key": "Other", "value": "kept"}});
        redact_params(&mut params);
        assert_eq!(params["payload"]["value"], "kept");
    }

    #[test]
    fn redact_params_hides_the_value_of_a_set_variable_step() {
        let mut params = json!({
            "component": {"name": "securityctrlr"},
            "variable": {"name": "BasicAuthPassword", "instance": "x"},
            "value": SENTINEL,
        });
        redact_params(&mut params);
        assert_eq!(params["value"], "***");
        assert_eq!(params["component"]["name"], "securityctrlr");
        assert!(!params.to_string().contains(SENTINEL));

        let mut params = json!({
            "component": {"name": "OCPPCommCtrlr"},
            "variable": {"name": "HeartbeatInterval"},
            "value": "30",
        });
        redact_params(&mut params);
        assert_eq!(params["value"], "30");
    }

    #[test]
    fn line_col_clamps_to_a_char_boundary() {
        let text = "a\u{e9}b\nc";
        // Offset 2 is inside the two-byte `e acute`.
        assert_eq!(line_col(text, 2), (1, 2));
        assert_eq!(line_col(text, 100), (2, 2));
    }

    #[test]
    fn suggestions_are_bounded() {
        assert_eq!(closest("slepe", ["sleep", "plug"]), Some("sleep"));
        let long = "x".repeat(10_000);
        assert_eq!(closest(&long, ["sleep"]), None);
        assert_eq!(closest("sleep", [long.as_str()]), None);
        assert_eq!(closest("sleep", ["sleeeeeeeeeeeeeeeeeeeeeeeep"]), None);
        assert_eq!(closest("sleep", ["selep"]), Some("selep"));
    }
}
