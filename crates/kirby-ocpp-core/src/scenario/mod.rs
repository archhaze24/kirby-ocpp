//! Scenarios: sequences of station actions described as data, plus a runner.
//!
//! UI-free: the TUI, the headless CLI and tests all use [`run_scenario`] /
//! [`ScenarioRunner`]. Scenario files are TOML (see [`builtin`] for examples for the format).

pub mod builtin;
mod junit;
mod model;
mod runner;
pub mod vars;

use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::watch;

pub use self::junit::{JunitSuite, junit_xml};
pub use self::model::{Action, Condition, DEFAULT_TIMEOUT_MS, Expect, Scenario, Step};
pub use self::runner::{
    RunStatus, ScenarioProgress, ScenarioReport, ScenarioRunner, StepDetail, StepReport,
    StepStatus, run_scenario, run_scenario_with_events, run_scenario_with_vars,
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
pub const OCPP_VERSION: &str = "1.6";

/// Version of this crate (and, by workspace versioning, of the tools built on it).
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Checks the optional `ocpp_version` and `schema` keys of a scenario or script file.
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
    if let Some(schema) = schema
        && schema != i64::from(SCHEMA_VERSION)
    {
        return Err(format!(
            "unsupported schema {schema} (this build reads schema {SCHEMA_VERSION})"
        ));
    }
    Ok(())
}

/// Replaces the value of an `AuthorizationKey` `ChangeConfiguration` in step parameters (the
/// same rule as [`crate::ocpp::redact_secrets`] applies to frames) with the redaction marker.
pub fn redact_params(params: &mut serde_json::Value) {
    use serde_json::Value;
    let Some(fields) = params.as_object_mut() else {
        return;
    };
    if fields.get("call").and_then(Value::as_str) != Some("ChangeConfiguration") {
        return;
    }
    for key in ["payload", "match", "payload_contains", "expect_payload"] {
        if let Some(Value::Object(payload)) = fields.get_mut(key)
            && payload
                .get("key")
                .and_then(Value::as_str)
                .is_some_and(|k| k.eq_ignore_ascii_case("AuthorizationKey"))
            && let Some(value) = payload.get_mut("value")
        {
            *value = Value::from(crate::ocpp::REDACTED);
        }
    }
}

/// Current time as RFC 3339 UTC with millisecond precision, for report `started_at`.
#[must_use]
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Converts a byte offset in `text` to 1-based `(line, column)` (columns count characters).
#[must_use]
pub fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = text.get(..offset).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |tail| tail.chars().count())
        + 1;
    (line, column)
}

/// Edit distance with adjacent transpositions counting as one edit.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut rows = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    rows[0] = (0..=b.len()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let same = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (rows[i - 1][j] + 1)
                .min(rows[i][j - 1] + 1)
                .min(rows[i - 1][j - 1] + same);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[a.len()][b.len()]
}

/// The closest of `candidates` to `word`, when it is close enough to be a likely typo.
#[must_use]
pub fn closest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let limit = (word.chars().count() / 3).clamp(1, 3);
    candidates
        .into_iter()
        .map(|candidate| (edit_distance(word, candidate), candidate))
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
