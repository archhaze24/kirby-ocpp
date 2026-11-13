//! Scenarios as data: parsing and validation (no I/O against a station).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ScenarioError;
use super::vars::{self, Vars};
use crate::ids::{ConnectorId, IdTag};
use crate::ocpp::types::{ChargePointErrorCode, ConnectorStatus, RegistrationStatus, StopReason};
use crate::station::response_rules::canonical_action;
use crate::station::security::canonical_event_type;
use crate::station::{Direction, ResponseRule, Snapshot, TransactionState};
use crate::ws::ConnectionState;

/// Default timeout of `wait_for` and `expect_call`.
pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;

/// Most iterations of one `repeat` step.
pub const MAX_REPEAT_COUNT: u32 = 1_000;

/// Deepest nesting of `repeat` steps.
pub const MAX_REPEAT_DEPTH: usize = 3;

/// Most steps a scenario may run in total once its loops are expanded.
pub const MAX_EXPANDED_STEPS: u64 = 10_000;

/// Longest accepted wait or sleep: one day.
const MAX_WAIT_MS: u64 = 86_400_000;

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_wh_delta() -> u64 {
    1_000
}

fn default_reason() -> StopReason {
    StopReason::Local
}

fn default_true() -> bool {
    true
}

fn default_times() -> u32 {
    1
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde `skip_serializing_if`"
)]
fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// The CALLERROR a `reply` step sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplyError {
    /// One of the OCPP CALLERROR codes (case-insensitive), e.g. `NotSupported`.
    pub code: String,
    #[serde(default)]
    pub description: String,
    /// The `errorDetails` object (default `{}`).
    #[serde(default = "default_payload")]
    pub details: Value,
}

/// The CALLERROR code spelled `text` (case-insensitive), if it is one of the standard ones.
pub(super) fn standard_error_code(text: &str) -> Option<crate::ocpp::frame::ErrorCode> {
    crate::ocpp::frame::ErrorCode::STANDARD
        .iter()
        .find(|code| code.as_str().eq_ignore_ascii_case(text.trim()))
        .cloned()
}

fn default_payload() -> Value {
    Value::Object(serde_json::Map::new())
}

/// A named list of steps.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Scenario {
    pub name: String,
    pub description: String,
    /// Variables declared in the `[vars]` table (strings, numbers, booleans).
    #[serde(skip_serializing_if = "Vars::is_empty")]
    pub vars: Vars,
    pub steps: Vec<Step>,
    /// Free-form labels from the reserved `tags` key (kept, not used for filtering yet).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Where the scenario came from: a file path or `builtin:NAME`; `None` for text parsed
    /// with [`Scenario::from_toml`].
    #[serde(skip)]
    pub source: Option<String>,
}

/// One step: an [`Action`] plus an optional display name and outcome assertion.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect: Option<Expect>,
    /// Variable name -> JSON pointer into the step's raw answer, stored after the step ran
    /// (`send_raw`, `data_transfer`, `expect_call` only).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub capture: BTreeMap<String, String>,
    #[serde(flatten)]
    pub action: Action,
}

/// Assertion on the outcome of a command step.
///
/// `Authorize` and `StartTx` implicitly expect `status = "Accepted"` unless the step
/// says otherwise, so a rejected tag stops the scenario.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// Authorization / registration / response `status` (e.g. `"Accepted"`, `"Blocked"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Subset the CALLRESULT payload must contain (objects: every key must match).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    /// The command must fail with a station error.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub error: bool,
}

/// What a step does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Connect {},
    Disconnect {},
    Reconnect {},
    Boot {},
    Heartbeat {},
    Plug {
        connector: ConnectorId,
    },
    Unplug {
        connector: ConnectorId,
    },
    Authorize {
        id_tag: IdTag,
    },
    /// Also spelled `start_session` (the version-neutral alias).
    #[serde(alias = "start_session")]
    StartTx {
        connector: ConnectorId,
        id_tag: IdTag,
    },
    /// Also spelled `stop_session` (the version-neutral alias).
    #[serde(alias = "stop_session")]
    StopTx {
        connector: ConnectorId,
        #[serde(default = "default_reason")]
        reason: StopReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id_tag: Option<IdTag>,
    },
    Meter {
        connector: ConnectorId,
        #[serde(default = "default_wh_delta")]
        wh_delta: u64,
    },
    /// Set a connector's status.
    Status {
        connector: ConnectorId,
        status: ConnectorStatus,
    },
    Fault {
        connector: ConnectorId,
        error_code: ChargePointErrorCode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        info: Option<String>,
    },
    ClearFault {
        connector: ConnectorId,
    },
    Sleep {
        ms: u64,
    },
    /// Wait until the station snapshot satisfies `condition`.
    WaitFor {
        condition: Condition,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
    },
    /// Wait for a CALL frame (sent or received) observed since the previous matched
    /// `expect_call` (or the scenario start).
    ExpectCall {
        call: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<Direction>,
        /// Subset the CALL payload must contain.
        #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
        matches: Option<Value>,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
    },
    SendRaw {
        call: String,
        #[serde(default = "default_payload")]
        payload: Value,
        /// Validate against the OCPP schema before sending (default `true`).
        #[serde(default = "default_true")]
        validate: bool,
    },
    DataTransfer {
        vendor_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
    },
    /// Misbehave on purpose when the CSMS sends a command, e.g. `rule = "Reset=reject*2"`
    /// (see `station::response_rules` for the syntax). Replaces the rule of the same action.
    SetRule {
        /// Rule text; validated at load time unless it has `${...}` placeholders, parsed
        /// when the step runs.
        rule: String,
    },
    /// Create a key pair and a CSR and send `SignCertificate` (Security Whitepaper).
    RequestCertificate {},
    /// Report a security event (`SecurityEventNotification`); `event_type` is one of the
    /// whitepaper's event names (case-insensitive), e.g. `TamperDetectionActivated`.
    SecurityEvent {
        event_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tech_info: Option<String>,
    },
    /// Remove the response rules of the command `call`, or all of them when omitted.
    ClearRules {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call: Option<String>,
    },
    /// Arm an answer for the next CALL(s) of the command `call` from the CSMS: exactly one of
    /// `result` (the CALLRESULT payload) and `error` (a CALLERROR). The station's own handler
    /// does not run for a CALL answered this way.
    Reply {
        call: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ReplyError>,
        /// How many CALLs the reply answers (default 1).
        #[serde(default = "default_times")]
        times: u32,
        /// Hold the answer back this many milliseconds (default 0).
        #[serde(default, skip_serializing_if = "is_zero")]
        delay_ms: u64,
        /// The scenario fails when the reply was not used up by the end (default `false`).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        must_fire: bool,
    },
    /// Run the nested `steps` `count` times. Inside, `${i}` (1-based) and `${i0}` (0-based)
    /// hold the iteration, or `${<var>}` / `${<var>0}` when `var` names the counter.
    /// Built by the TOML parser only (never deserialised through serde).
    #[serde(skip_deserializing)]
    Repeat {
        count: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        var: Option<String>,
        steps: Vec<Step>,
    },
}

/// One executed step of a scenario after its loops are expanded.
#[derive(Debug, Clone, PartialEq)]
pub struct FlatStep<'a> {
    /// The step's label with the iteration path appended, e.g. `plug [2.3]`.
    pub name: String,
    /// Stable report label: the explicit `name`, else one derived from the action and its
    /// main parameters (`start_tx c1 TAG`), with the iteration path appended.
    pub label: String,
    pub step: &'a Step,
    /// Loop counters in scope, outermost first (a later entry shadows an earlier one):
    /// `(variable name, 0-based iteration)`; the 1-based value is bound as `name`, the
    /// 0-based one as `name0`.
    pub counters: Vec<(String, u64)>,
}

/// A condition on the station snapshot, for `wait_for`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    Connected {},
    Disconnected {},
    Registration {
        status: RegistrationStatus,
    },
    ConnectorStatus {
        connector: ConnectorId,
        status: ConnectorStatus,
    },
    /// A transaction is running (acknowledged by the CSMS) on the connector.
    TxActive {
        connector: ConnectorId,
    },
    TxIdle {
        connector: ConnectorId,
    },
    /// No transaction messages are queued for delivery.
    NoPendingMessages {},
}

impl Condition {
    /// Does `snapshot` satisfy the condition?
    pub fn holds(&self, snapshot: &Snapshot) -> bool {
        match self {
            Self::Connected {} => snapshot.connection == ConnectionState::Connected,
            Self::Disconnected {} => snapshot.connection == ConnectionState::Disconnected,
            Self::Registration { status } => snapshot.registration == Some(*status),
            Self::ConnectorStatus { connector, status } => snapshot
                .connector(*connector)
                .is_some_and(|c| c.status == *status),
            Self::TxActive { connector } => snapshot
                .connector(*connector)
                .is_some_and(|c| matches!(c.transaction, TransactionState::Active(_))),
            Self::TxIdle { connector } => snapshot
                .connector(*connector)
                .is_some_and(|c| c.transaction.is_idle()),
            Self::NoPendingMessages {} => snapshot.pending_offline_messages == 0,
        }
    }
}

impl Action {
    /// Stable snake_case name (the serde tag).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Connect {} => "connect",
            Self::Disconnect {} => "disconnect",
            Self::Reconnect {} => "reconnect",
            Self::Boot {} => "boot",
            Self::Heartbeat {} => "heartbeat",
            Self::Plug { .. } => "plug",
            Self::Unplug { .. } => "unplug",
            Self::Authorize { .. } => "authorize",
            Self::StartTx { .. } => "start_tx",
            Self::StopTx { .. } => "stop_tx",
            Self::Meter { .. } => "meter",
            Self::Status { .. } => "status",
            Self::Fault { .. } => "fault",
            Self::ClearFault { .. } => "clear_fault",
            Self::Sleep { .. } => "sleep",
            Self::WaitFor { .. } => "wait_for",
            Self::ExpectCall { .. } => "expect_call",
            Self::SendRaw { .. } => "send_raw",
            Self::DataTransfer { .. } => "data_transfer",
            Self::SetRule { .. } => "set_rule",
            Self::ClearRules { .. } => "clear_rules",
            Self::Reply { .. } => "reply",
            Self::RequestCertificate {} => "request_certificate",
            Self::SecurityEvent { .. } => "security_event",
            Self::Repeat { .. } => "repeat",
        }
    }
}

impl Step {
    /// The name shown in reports: the explicit `name`, else the action name.
    pub fn label(&self) -> String {
        self.explicit_name()
            .map_or_else(|| self.action.name().to_owned(), str::to_owned)
    }

    /// The `name` of the step unless it is empty or only whitespace (then the derived label
    /// is used, so a report row is never nameless).
    fn explicit_name(&self) -> Option<&str> {
        self.name.as_deref().filter(|name| !name.trim().is_empty())
    }

    /// The stable label used in machine-readable reports: the explicit `name`, else the action
    /// name plus its main parameters, for example `expect_call Heartbeat out`,
    /// `wait_for tx_active c1` or `start_tx c1 TAG`. Never just a position.
    pub fn stable_label(&self) -> String {
        if let Some(name) = self.explicit_name() {
            return name.to_owned();
        }
        let Ok(Value::Object(fields)) = serde_json::to_value(&self.action) else {
            return self.action.name().to_owned();
        };
        let mut parts = vec![self.action.name().to_owned()];
        let mut text = |value: Option<&Value>| match value {
            Some(Value::String(s)) if !s.is_empty() => parts.push(s.clone()),
            Some(Value::Number(n)) => parts.push(n.to_string()),
            _ => {}
        };
        if let Some(Value::Object(condition)) = fields.get("condition") {
            text(condition.get("kind"));
            if let Some(connector) = condition.get("connector").and_then(Value::as_u64) {
                text(Some(&Value::String(format!("c{connector}"))));
            }
            text(condition.get("status"));
        }
        for key in ["call", "event_type", "vendor_id", "rule"] {
            text(fields.get(key));
        }
        if let Some(connector) = fields.get("connector").and_then(Value::as_u64) {
            text(Some(&Value::String(format!("c{connector}"))));
        }
        for key in ["direction", "id_tag", "status", "error_code"] {
            text(fields.get(key));
        }
        if let Some(ms) = fields.get("ms").and_then(Value::as_u64) {
            text(Some(&Value::String(format!("{ms}ms"))));
        }
        let label = parts.join(" ");
        match label.char_indices().nth(80) {
            Some((at, _)) => format!("{}...", &label[..at]),
            None => label,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScenario {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    ocpp_version: Option<String>,
    #[serde(default)]
    schema: Option<i64>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    vars: toml::Table,
    #[serde(default)]
    steps: Vec<toml::Spanned<toml::Table>>,
}

/// A load failure with the byte offset it points at, when known.
struct Located {
    message: String,
    offset: Option<usize>,
}

impl Located {
    fn at(message: impl Into<String>, offset: usize) -> Self {
        Self {
            message: message.into(),
            offset: Some(offset),
        }
    }

    fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            offset: None,
        }
    }
}

impl From<ScenarioError> for Located {
    fn from(error: ScenarioError) -> Self {
        let message = match error {
            ScenarioError::Invalid(message) => message,
            other => other.to_string(),
        };
        Self::plain(message)
    }
}

/// Largest scenario file [`Scenario::load`] reads.
pub const MAX_SCENARIO_FILE_BYTES: u64 = 1024 * 1024;

/// Reads a regular file of at most [`MAX_SCENARIO_FILE_BYTES`] as UTF-8 (a FIFO or device
/// would block or never end).
pub fn read_scenario_file(path: &Path) -> std::io::Result<String> {
    use std::io::{Error, ErrorKind, Read};
    if !std::fs::metadata(path)?.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "not a regular file"));
    }
    let mut text = String::new();
    let read = std::fs::File::open(path)?
        .take(MAX_SCENARIO_FILE_BYTES + 1)
        .read_to_string(&mut text)?;
    if read as u64 > MAX_SCENARIO_FILE_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("larger than {MAX_SCENARIO_FILE_BYTES} bytes"),
        ));
    }
    Ok(text)
}

impl Scenario {
    /// Parses and validates a scenario from TOML text.
    ///
    /// # Errors
    /// [`ScenarioError::Invalid`] naming the offending step for unknown actions, missing or
    /// unknown fields and bad values.
    pub fn from_toml(text: &str) -> Result<Self, ScenarioError> {
        Self::parse(text).map_err(|e| {
            ScenarioError::Invalid(match e.offset {
                Some(offset) => {
                    let (line, column) = super::line_col(text, offset);
                    format!("{} (line {line}, column {column})", e.message)
                }
                None => e.message,
            })
        })
    }

    fn parse(text: &str) -> Result<Self, Located> {
        super::precheck_contract(text).map_err(Located::plain)?;
        let raw: RawScenario = toml::from_str(text).map_err(|e| Located {
            message: super::with_suggestion(e.message().to_owned()),
            offset: e.span().map(|span| span.start),
        })?;
        super::check_contract_keys(raw.ocpp_version.as_deref(), raw.schema)
            .map_err(Located::plain)?;
        if raw.name.trim().is_empty() {
            return Err(Located::plain("`name` must not be empty"));
        }
        if raw.steps.is_empty() {
            return Err(Located::plain("a scenario needs at least one `[[steps]]`"));
        }
        let vars = parse_vars(raw.vars)?;
        let steps = raw
            .steps
            .into_iter()
            .enumerate()
            .map(|(i, table)| {
                let offset = table.span().start;
                parse_step(table.into_inner(), 0)
                    .map_err(|e| Located::at(format!("step {}: {e}", i + 1), offset))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let scenario = Self {
            name: raw.name,
            description: raw.description,
            vars,
            steps,
            tags: raw.tags,
            source: None,
        };
        scenario.validate()?;
        Ok(scenario)
    }

    /// Reads and parses a scenario file.
    ///
    /// # Errors
    /// [`ScenarioError::Io`] or [`ScenarioError::Invalid`].
    pub fn load(path: &Path) -> Result<Self, ScenarioError> {
        let text = read_scenario_file(path).map_err(|source| ScenarioError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        match Self::parse(&text) {
            Ok(mut scenario) => {
                scenario.source = Some(path.display().to_string());
                Ok(scenario)
            }
            Err(e) => {
                let position = e.offset.map(|offset| super::line_col(&text, offset));
                Err(ScenarioError::InvalidFile {
                    path: path.to_path_buf(),
                    line: position.map(|(line, _)| line),
                    column: position.map(|(_, column)| column),
                    message: e.message,
                })
            }
        }
    }

    /// The steps that actually run, in order: every `repeat` expanded, each name suffixed with
    /// its iteration path (` [2]`, nested ` [2.3]`). Steps outside loops keep their label.
    #[must_use]
    pub fn expanded_steps(&self) -> Vec<FlatStep<'_>> {
        let mut out = Vec::new();
        expand(&self.steps, &mut Vec::new(), &mut out);
        out
    }

    /// Number of steps that run (the length of [`Scenario::expanded_steps`]).
    #[must_use]
    pub fn expanded_len(&self) -> u64 {
        count_expanded(&self.steps)
    }

    /// Does the scenario know a variable `name`: declared in `[vars]`, a loop counter, or
    /// referenced by a `${name}` placeholder somewhere in its steps? A `--var` for any other
    /// name has no effect.
    #[must_use]
    pub fn uses_variable(&self, name: &str) -> bool {
        if self.vars.get(name).is_some() {
            return true;
        }
        let mut counters = BTreeSet::new();
        collect_counter_names(&self.steps, &mut counters);
        if counters.contains(name) {
            return true;
        }
        let needle = format!("${{{name}}}");
        self.steps
            .iter()
            .any(|step| serde_json::to_value(step).is_ok_and(|value| mentions(&value, &needle)))
    }

    fn validate(&self) -> Result<(), ScenarioError> {
        let total = self.expanded_len();
        if total > MAX_EXPANDED_STEPS {
            return Err(ScenarioError::invalid(format!(
                "scenario `{}` expands to {total} steps, the limit is {MAX_EXPANDED_STEPS}",
                self.name
            )));
        }
        let mut reserved = BTreeSet::new();
        collect_counter_names(&self.steps, &mut reserved);
        for name in self.vars.iter().map(|(name, _)| name) {
            if reserved.contains(name) {
                return Err(ScenarioError::invalid(format!(
                    "`[vars]` `{name}`: is a loop counter name used by a `repeat` step"
                )));
            }
        }
        validate_steps(&self.steps, &mut Vec::new(), &reserved).map_err(ScenarioError::invalid)
    }

    /// Points every connector-bound step at `connector` and every idTag at `id_tag`
    /// (when given). Used to adapt the built-ins to the station being tested.
    pub fn retarget(&mut self, connector: Option<ConnectorId>, id_tag: Option<&IdTag>) {
        retarget_steps(&mut self.steps, connector, id_tag);
    }
}

fn retarget_steps(steps: &mut [Step], connector: Option<ConnectorId>, id_tag: Option<&IdTag>) {
    for step in steps {
        match &mut step.action {
            Action::Repeat { steps, .. } => retarget_steps(steps, connector, id_tag),
            Action::Plug { connector: c }
            | Action::Unplug { connector: c }
            | Action::Meter { connector: c, .. }
            | Action::Fault { connector: c, .. }
            | Action::ClearFault { connector: c }
            | Action::Status { connector: c, .. }
            | Action::WaitFor {
                condition:
                    Condition::ConnectorStatus { connector: c, .. }
                    | Condition::TxActive { connector: c }
                    | Condition::TxIdle { connector: c },
                ..
            } => retarget_connector(c, connector),
            Action::StartTx {
                connector: c,
                id_tag: t,
            } => {
                retarget_connector(c, connector);
                if let Some(tag) = id_tag {
                    t.clone_from(tag);
                }
            }
            Action::StopTx {
                connector: c,
                id_tag: t,
                ..
            } => {
                retarget_connector(c, connector);
                if let (Some(tag), Some(old)) = (id_tag, t.as_mut()) {
                    old.clone_from(tag);
                }
            }
            Action::Authorize { id_tag: t } => {
                if let Some(tag) = id_tag {
                    t.clone_from(tag);
                }
            }
            _ => {}
        }
    }
}

fn validate_steps(
    steps: &[Step],
    path: &mut Vec<usize>,
    reserved: &BTreeSet<String>,
) -> Result<(), String> {
    for (i, step) in steps.iter().enumerate() {
        path.push(i + 1);
        let label = path
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(".");
        let bad = |msg: &str| Err(format!("step {label}: {msg}"));
        if let Some(name) = step.capture.keys().find(|name| reserved.contains(*name)) {
            return bad(&format!("`capture` `{name}` is a loop counter name"));
        }
        match &step.action {
            Action::Repeat { steps: inner, .. } => {
                validate_steps(inner, path, reserved)?;
            }
            Action::Plug { connector }
            | Action::Unplug { connector }
            | Action::StartTx { connector, .. }
            | Action::StopTx { connector, .. }
            | Action::Meter { connector, .. }
                if connector.is_station() =>
            {
                return bad("`connector` must be 1 or more");
            }
            Action::WaitFor { timeout_ms: 0, .. } | Action::ExpectCall { timeout_ms: 0, .. } => {
                return bad("`timeout_ms` must be positive");
            }
            Action::WaitFor { timeout_ms: ms, .. }
            | Action::ExpectCall { timeout_ms: ms, .. }
            | Action::Sleep { ms }
                if *ms > MAX_WAIT_MS =>
            {
                // Beyond this `Instant + Duration` could overflow and panic the runner.
                return bad("`timeout_ms` / `ms` must be at most 86400000 (one day)");
            }
            Action::SendRaw { call, .. } | Action::ExpectCall { call, .. } if call.is_empty() => {
                return bad("`call` must not be empty");
            }
            Action::SecurityEvent { event_type, .. }
                if static_text(event_type)
                    .is_some_and(|text| canonical_event_type(&text).is_none()) =>
            {
                return bad(&format!("unknown security event type `{event_type}`"));
            }
            Action::ClearRules { call: Some(call) } => {
                if let Some(text) = static_text(call)
                    && let Err(error) = canonical_action(&text)
                {
                    return bad(&error.to_string());
                }
            }
            Action::Reply {
                call,
                result,
                error,
                times,
                delay_ms,
                ..
            } => {
                if let Err(message) = validate_reply(call, result.as_ref(), error.as_ref()) {
                    return bad(&format!("action `reply`: {message}"));
                }
                if *times == 0 || *times > crate::station::scripted_reply::MAX_TIMES {
                    return bad(&format!(
                        "action `reply`: `times` must be between 1 and {}",
                        crate::station::scripted_reply::MAX_TIMES
                    ));
                }
                if *delay_ms > crate::station::response_rules::MAX_DELAY_MS {
                    return bad(&format!(
                        "action `reply`: `delay_ms` must be at most {}",
                        crate::station::response_rules::MAX_DELAY_MS
                    ));
                }
            }
            _ => {}
        }
        path.pop();
    }
    Ok(())
}

/// Checks the shape of a `reply` step (what can be known before the step runs).
fn validate_reply(
    call: &str,
    result: Option<&Value>,
    error: Option<&ReplyError>,
) -> Result<(), String> {
    let action = match static_text(call) {
        Some(text) => Some(
            canonical_action(&text)
                .map_err(|e| e.to_string())?
                .to_owned(),
        ),
        None => None,
    };
    match (result, error) {
        (Some(_), Some(_)) => return Err("`result` and `error` exclude each other".to_owned()),
        (None, None) => return Err("needs `result` or `error`".to_owned()),
        (Some(result), None) => {
            if !result.is_object() {
                return Err("`result` must be a table (the CALLRESULT payload)".to_owned());
            }
            // A payload without placeholders can be checked against the response schema now.
            if let Some(action) = &action
                && !result.to_string().contains('$')
                && let Some(registry) = schema_registry()
                && let Err(violation) = registry.validate_response(action, result)
            {
                return Err(format!(
                    "`result` is not a valid {action} response: {violation}"
                ));
            }
        }
        (None, Some(error)) => {
            if let Some(text) = static_text(&error.code)
                && standard_error_code(&text).is_none()
            {
                return Err(format!(
                    "unknown error code `{text}` (expected one of the ten OCPP 1.6 CALLERROR codes)"
                ));
            }
            if !error.details.is_object() {
                return Err("`error.details` must be a table".to_owned());
            }
        }
    }
    Ok(())
}

/// The bundled schemas, compiled once on first use (`None` if they cannot be compiled).
pub(super) fn schema_registry() -> Option<&'static crate::ocpp::schema::SchemaRegistry> {
    static REGISTRY: std::sync::OnceLock<Option<crate::ocpp::schema::SchemaRegistry>> =
        std::sync::OnceLock::new();
    REGISTRY
        .get_or_init(|| crate::ocpp::schema::SchemaRegistry::new().ok())
        .as_ref()
}

/// Counter names (`i`, `i0`, or `<var>`, `<var>0`) of every loop below `steps`.
fn collect_counter_names(steps: &[Step], out: &mut BTreeSet<String>) {
    for step in steps {
        if let Action::Repeat { var, steps, .. } = &step.action {
            let name = var.as_deref().unwrap_or("i");
            out.insert(name.to_owned());
            out.insert(format!("{name}0"));
            collect_counter_names(steps, out);
        }
    }
}

fn count_expanded(steps: &[Step]) -> u64 {
    steps
        .iter()
        .map(|step| match &step.action {
            Action::Repeat { count, steps, .. } => {
                u64::from(*count).saturating_mul(count_expanded(steps))
            }
            _ => 1,
        })
        .fold(0, u64::saturating_add)
}

fn expand<'a>(steps: &'a [Step], counters: &mut Vec<(String, u64)>, out: &mut Vec<FlatStep<'a>>) {
    for step in steps {
        if let Action::Repeat {
            count,
            var,
            steps: inner,
        } = &step.action
        {
            let name = var.as_deref().unwrap_or("i");
            for iteration in 0..u64::from(*count) {
                counters.push((name.to_owned(), iteration));
                expand(inner, counters, out);
                counters.pop();
            }
        } else {
            let mut name = step.label();
            let mut label = step.stable_label();
            if !counters.is_empty() {
                let path = counters
                    .iter()
                    .map(|(_, iteration)| (iteration + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(".");
                name = format!("{name} [{path}]");
                label = format!("{label} [{path}]");
            }
            out.push(FlatStep {
                name,
                label,
                step,
                counters: counters.clone(),
            });
        }
    }
}

fn retarget_connector(slot: &mut ConnectorId, to: Option<ConnectorId>) {
    if let Some(to) = to {
        *slot = to;
    }
}

/// Does any string in `value` contain the placeholder `needle` (not as a `$${` escape)?
fn mentions(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text.match_indices(needle).any(|(at, _)| {
            // A `$` right before the needle makes it the `$${` escape.
            !text[..at].ends_with('$')
        }),
        Value::Array(items) => items.iter().any(|item| mentions(item, needle)),
        Value::Object(map) => map.values().any(|item| mentions(item, needle)),
        _ => false,
    }
}

/// The text as it will read at run time when it holds no placeholder (`$${` escapes are
/// resolved to `${`); `None` for text with a `${name}` placeholder, which is checked when it
/// runs.
fn static_text(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("$${") {
            out.push_str("${");
            rest = after;
        } else if tail.starts_with("${") {
            return None;
        } else {
            out.push('$');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    Some(out)
}

fn parse_vars(table: toml::Table) -> Result<Vars, ScenarioError> {
    let mut out = Vars::new();
    for (name, value) in table {
        let bad = |msg: &str| ScenarioError::invalid(format!("`[vars]` `{name}`: {msg}"));
        if !vars::is_valid_name(&name) {
            return Err(bad("not a valid variable name"));
        }
        if vars::is_builtin(&name) {
            return Err(bad("is a built-in variable and cannot be redefined"));
        }
        let json = match value {
            toml::Value::String(s) => Value::String(s),
            toml::Value::Integer(i) => Value::from(i),
            toml::Value::Float(f) => serde_json::Number::from_f64(f)
                .map(Value::Number)
                .ok_or_else(|| bad("must be a finite number"))?,
            toml::Value::Boolean(b) => Value::Bool(b),
            _ => return Err(bad("must be a string, a number or a boolean")),
        };
        out.insert(&name, json).map_err(|e| bad(&e))?;
    }
    Ok(out)
}

fn parse_capture(value: toml::Value) -> Result<BTreeMap<String, String>, String> {
    let toml::Value::Table(table) = value else {
        return Err("`capture` must be a table of name = \"/json/pointer\"".to_owned());
    };
    let mut out = BTreeMap::new();
    for (name, pointer) in table {
        if !vars::is_valid_name(&name) {
            return Err(format!("`capture`: `{name}` is not a valid variable name"));
        }
        if vars::is_builtin(&name) {
            return Err(format!("`capture`: `{name}` is a built-in variable"));
        }
        let toml::Value::String(pointer) = pointer else {
            return Err(format!("`capture`: `{name}` must be a JSON pointer string"));
        };
        vars::check_pointer(&pointer).map_err(|e| format!("`capture` `{name}`: {e}"))?;
        out.insert(name, pointer);
    }
    Ok(out)
}

fn parse_repeat(
    mut table: toml::Table,
    name: Option<String>,
    open_loops: usize,
) -> Result<Step, String> {
    if open_loops >= MAX_REPEAT_DEPTH {
        return Err(format!(
            "`repeat` is nested deeper than {MAX_REPEAT_DEPTH} levels"
        ));
    }
    table.remove("action");
    let count = match table.remove("count") {
        None => return Err("action `repeat`: missing field `count`".to_owned()),
        Some(toml::Value::Integer(n)) => u32::try_from(n)
            .ok()
            .filter(|n| (1..=MAX_REPEAT_COUNT).contains(n))
            .ok_or_else(|| {
                format!("action `repeat`: `count` must be between 1 and {MAX_REPEAT_COUNT}")
            })?,
        Some(_) => return Err("action `repeat`: `count` must be an integer".to_owned()),
    };
    let var = match table.remove("var") {
        None => None,
        Some(toml::Value::String(v)) => {
            if !vars::is_valid_name(&v) || vars::is_builtin(&v) {
                return Err(format!(
                    "action `repeat`: `var` `{v}` is not a valid counter name"
                ));
            }
            Some(v)
        }
        Some(_) => return Err("action `repeat`: `var` must be a string".to_owned()),
    };
    let raw = match table.remove("steps") {
        Some(toml::Value::Array(items)) if !items.is_empty() => items,
        Some(toml::Value::Array(_)) | None => {
            return Err("action `repeat`: needs at least one nested `[[steps.steps]]`".to_owned());
        }
        Some(_) => return Err("action `repeat`: `steps` must be an array of tables".to_owned()),
    };
    if let Some(key) = table.keys().next() {
        return Err(format!("action `repeat`: unknown field `{key}`"));
    }
    let mut steps = Vec::with_capacity(raw.len());
    for (i, item) in raw.into_iter().enumerate() {
        let toml::Value::Table(inner) = item else {
            return Err(format!("repeat step {}: must be a table", i + 1));
        };
        steps.push(
            parse_step(inner, open_loops + 1).map_err(|e| format!("repeat step {}: {e}", i + 1))?,
        );
    }
    Ok(Step {
        name,
        expect: None,
        capture: BTreeMap::new(),
        action: Action::Repeat { count, var, steps },
    })
}

/// `open_loops` is the number of `repeat` steps around this one.
fn parse_step(mut table: toml::Table, open_loops: usize) -> Result<Step, String> {
    super::check_reserved_keys(&table)?;
    let name = match table.remove("name") {
        None => None,
        Some(toml::Value::String(s)) => Some(s),
        Some(_) => return Err("`name` must be a string".to_owned()),
    };
    if table.get("action").and_then(toml::Value::as_str) == Some("repeat") {
        if table.contains_key("expect") || table.contains_key("capture") {
            return Err(
                "action `repeat`: `expect` and `capture` are not allowed on a loop".to_owned(),
            );
        }
        return parse_repeat(table, name, open_loops);
    }
    let expect = match table.remove("expect") {
        None => None,
        Some(value) => Some(
            value
                .try_into::<Expect>()
                .map_err(|e| format!("`expect`: {}", e.message()))?,
        ),
    };
    if let Some(expect) = &expect
        && expect.error
        && (expect.status.is_some() || expect.payload.is_some())
    {
        return Err("`expect`: `error = true` excludes `status` and `payload`".to_owned());
    }
    let capture = match table.remove("capture") {
        None => BTreeMap::new(),
        Some(value) => parse_capture(value)?,
    };
    let tag = match table.get("action") {
        None => return Err("missing field `action`".to_owned()),
        Some(toml::Value::String(s)) => s.clone(),
        Some(_) => return Err("`action` must be a string".to_owned()),
    };
    let action = toml::Value::Table(table)
        .try_into::<Action>()
        .map_err(|e| {
            format!(
                "action `{tag}`: {}",
                super::with_suggestion(e.message().to_owned())
            )
        })?;
    let mut action = action;
    if !capture.is_empty()
        && !matches!(
            action,
            Action::SendRaw { .. } | Action::DataTransfer { .. } | Action::ExpectCall { .. }
        )
    {
        return Err(format!(
            "action `{tag}`: `capture` is only valid on send_raw, data_transfer and expect_call steps"
        ));
    }
    if let Action::SetRule { rule } = &mut action
        && let Some(text) = static_text(rule)
    {
        let normalized = text
            .parse::<ResponseRule>()
            .map_err(|e| format!("action `{tag}`: {e}"))?
            .to_string();
        // Text with `$${` escapes stays as written: it is unescaped when the step runs.
        if text == *rule {
            *rule = normalized;
        }
    }
    Ok(Step {
        name,
        expect,
        capture,
        action,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(text: &str) -> String {
        Scenario::from_toml(text).unwrap_err().to_string()
    }

    const HEAD: &str = "name = \"t\"\n";

    fn loop_toml(extra: &str, count: &str, inner: &str) -> String {
        format!("name = \"t\"\n{extra}[[steps]]\naction = \"repeat\"\ncount = {count}\n{inner}")
    }

    const HEARTBEAT: &str = "[[steps.steps]]\naction = \"heartbeat\"\n";

    #[test]
    fn repeat_expands_with_names_and_counters() {
        let s = Scenario::from_toml(&loop_toml(
            "",
            "3",
            &format!("{HEARTBEAT}[[steps.steps]]\naction = \"repeat\"\ncount = 2\nvar = \"k\"\n[[steps.steps.steps]]\naction = \"sleep\"\nms = 1\n"),
        ))
        .unwrap();
        assert_eq!(s.expanded_len(), 3 * (1 + 2));
        let flat = s.expanded_steps();
        assert_eq!(flat.len(), 9);
        assert_eq!(flat[0].name, "heartbeat [1]");
        assert_eq!(flat[2].name, "sleep [1.2]");
        assert_eq!(flat[2].counters, [("i".to_owned(), 0), ("k".to_owned(), 1)]);
        assert_eq!(flat[8].name, "sleep [3.2]");
    }

    #[test]
    fn repeat_bad_shapes_are_load_errors() {
        for (count, inner, needle) in [
            ("0", HEARTBEAT, "between 1 and 1000"),
            ("1001", HEARTBEAT, "between 1 and 1000"),
            ("-1", HEARTBEAT, "between 1 and 1000"),
            ("\"2\"", HEARTBEAT, "must be an integer"),
            ("2", "", "at least one nested"),
            (
                "2",
                "bogus = 1\n[[steps.steps]]\naction = \"heartbeat\"\n",
                "unknown field `bogus`",
            ),
            (
                "2",
                "[[steps.steps]]\naction = \"heartbeat\"\nbogus = 1\n",
                "repeat step 1",
            ),
            (
                "2",
                "capture = { a = \"/x\" }\n[[steps.steps]]\naction = \"heartbeat\"\n",
                "not allowed on a loop",
            ),
            (
                "2",
                "expect = { status = \"x\" }\n[[steps.steps]]\naction = \"heartbeat\"\n",
                "not allowed on a loop",
            ),
            (
                "2",
                "var = \"1x\"\n[[steps.steps]]\naction = \"heartbeat\"\n",
                "not a valid counter name",
            ),
            (
                "2",
                "var = \"index\"\n[[steps.steps]]\naction = \"heartbeat\"\n",
                "not a valid counter name",
            ),
            (
                "2",
                "[[steps.steps]]\naction = \"plug\"\nconnector = 0\n",
                "step 1.1: `connector`",
            ),
        ] {
            let e = err(&loop_toml("", count, inner));
            assert!(e.contains(needle), "{count} {inner}: {e}");
        }
    }

    #[test]
    fn repeat_nesting_is_capped_at_three_levels() {
        let leaf = "action = \"heartbeat\"\n";
        let three = format!(
            "{HEAD}[[steps]]\naction = \"repeat\"\ncount = 2\n[[steps.steps]]\naction = \"repeat\"\ncount = 2\n[[steps.steps.steps]]\naction = \"repeat\"\ncount = 2\n[[steps.steps.steps.steps]]\n{leaf}"
        );
        assert_eq!(Scenario::from_toml(&three).unwrap().expanded_len(), 8);
        let four = format!(
            "{HEAD}[[steps]]\naction = \"repeat\"\ncount = 2\n[[steps.steps]]\naction = \"repeat\"\ncount = 2\n[[steps.steps.steps]]\naction = \"repeat\"\ncount = 2\n[[steps.steps.steps.steps]]\naction = \"repeat\"\ncount = 2\n[[steps.steps.steps.steps.steps]]\n{leaf}"
        );
        assert!(err(&four).contains("deeper than 3"), "{}", err(&four));
    }

    #[test]
    fn expanded_size_is_capped_at_load_time() {
        let ok = loop_toml("", "1000", &HEARTBEAT.repeat(10));
        assert_eq!(Scenario::from_toml(&ok).unwrap().expanded_len(), 10_000);
        let too_many = loop_toml("", "1000", &HEARTBEAT.repeat(11));
        let e = err(&too_many);
        assert!(
            e.contains("`t`") && e.contains("11000") && e.contains("10000"),
            "{e}"
        );
        // Nested loops multiply.
        let nested = format!(
            "{HEAD}[[steps]]\naction = \"repeat\"\ncount = 100\n[[steps.steps]]\naction = \"repeat\"\ncount = 101\n[[steps.steps.steps]]\naction = \"heartbeat\"\n"
        );
        assert!(err(&nested).contains("10100"), "{}", err(&nested));
    }

    #[test]
    fn counter_names_are_reserved_for_vars_and_captures() {
        let e = err(&loop_toml("[vars]\ni = 1\n", "2", HEARTBEAT));
        assert!(e.contains("loop counter"), "{e}");
        let e = err(&loop_toml(
            "[vars]\nk0 = 1\n",
            "2",
            &format!("var = \"k\"\n{HEARTBEAT}"),
        ));
        assert!(e.contains("loop counter"), "{e}");
        let e = err(&loop_toml(
            "",
            "2",
            "[[steps.steps]]\naction = \"send_raw\"\ncall = \"Heartbeat\"\ncapture = { i = \"/currentTime\" }\n",
        ));
        assert!(e.contains("loop counter"), "{e}");
        // Without a loop using the name it stays a normal variable.
        Scenario::from_toml(&format!(
            "{HEAD}[vars]\ni = 1\n[[steps]]\naction = \"heartbeat\"\n"
        ))
        .unwrap();
    }

    #[test]
    fn repeat_cannot_be_deserialised_through_serde() {
        assert!(
            serde_json::from_value::<Action>(serde_json::json!({"action": "repeat", "count": 1}))
                .is_err()
        );
    }

    #[test]
    fn parses_a_full_scenario() {
        let s = Scenario::from_toml(
            r#"
name = "t"
description = "d"
[[steps]]
action = "plug"
connector = 1
[[steps]]
name = "tag"
action = "authorize"
id_tag = "ABC"
expect = { status = "Blocked" }
[[steps]]
action = "stop_tx"
connector = 1
[[steps]]
action = "wait_for"
condition = { kind = "connector_status", connector = 1, status = "Charging" }
[[steps]]
action = "expect_call"
call = "StartTransaction"
match = { connectorId = 1 }
direction = "out"
[[steps]]
action = "send_raw"
call = "Heartbeat"
"#,
        )
        .unwrap();
        assert_eq!(s.steps.len(), 6);
        assert_eq!(s.steps[1].label(), "tag");
        assert_eq!(s.steps[0].label(), "plug");
        assert_eq!(
            s.steps[1].expect.as_ref().unwrap().status.as_deref(),
            Some("Blocked")
        );
        assert!(matches!(
            s.steps[2].action,
            Action::StopTx {
                reason: StopReason::Local,
                ..
            }
        ));
        assert!(matches!(
            s.steps[3].action,
            Action::WaitFor {
                timeout_ms: DEFAULT_TIMEOUT_MS,
                ..
            }
        ));
        assert!(matches!(
            s.steps[5].action,
            Action::SendRaw { validate: true, .. }
        ));
    }

    #[test]
    fn unknown_action_is_reported_with_the_step() {
        let e = err(&format!(
            "{HEAD}[[steps]]\naction = \"plug\"\nconnector = 1\n[[steps]]\naction = \"explode\"\n"
        ));
        assert!(e.contains("step 2"), "{e}");
        assert!(e.contains("explode"), "{e}");
    }

    #[test]
    fn missing_action_and_missing_fields() {
        assert!(
            err(&format!("{HEAD}[[steps]]\nconnector = 1\n")).contains("missing field `action`")
        );
        let e = err(&format!(
            "{HEAD}[[steps]]\naction = \"start_tx\"\nconnector = 1\n"
        ));
        assert!(e.contains("missing field `id_tag`"), "{e}");
    }

    #[test]
    fn unknown_fields_are_rejected_everywhere() {
        let e = err(&format!(
            "{HEAD}[[steps]]\naction = \"plug\"\nconnector = 1\nbogus = 2\n"
        ));
        assert!(e.contains("bogus"), "{e}");
        assert!(
            err(&format!(
                "{HEAD}extra = 1\n[[steps]]\naction = \"connect\"\n"
            ))
            .contains("extra")
        );
        let e = err(&format!(
            "{HEAD}[[steps]]\naction = \"connect\"\nexpect = {{ nope = 1 }}\n"
        ));
        assert!(e.contains("nope"), "{e}");
        let e = err(&format!(
            "{HEAD}[[steps]]\naction = \"wait_for\"\ncondition = {{ kind = \"connected\", x = 1 }}\n"
        ));
        assert!(e.contains('x'), "{e}");
    }

    #[test]
    fn bad_values_are_rejected() {
        let e = err(&format!(
            "{HEAD}[[steps]]\naction = \"stop_tx\"\nconnector = 1\nreason = \"Nope\"\n"
        ));
        assert!(e.contains("Nope"), "{e}");
        let e = err(&format!(
            "{HEAD}[[steps]]\naction = \"plug\"\nconnector = 0\n"
        ));
        assert!(e.contains("connector"), "{e}");
        let e = err(&format!(
            "{HEAD}[[steps]]\naction = \"wait_for\"\ncondition = {{ kind = \"connected\" }}\ntimeout_ms = 0\n"
        ));
        assert!(e.contains("timeout_ms"), "{e}");
        for step in [
            "action = \"sleep\"\nms = 9000000000000000000",
            "action = \"expect_call\"\ncall = \"Heartbeat\"\ntimeout_ms = 9000000000000000000",
        ] {
            let e = err(&format!("{HEAD}[[steps]]\n{step}\n"));
            assert!(e.contains("one day"), "{e}");
        }
        let e = err(&format!(
            "{HEAD}[[steps]]\naction = \"authorize\"\nid_tag = \"{}\"\n",
            "x".repeat(40)
        ));
        assert!(e.contains("step 1"), "{e}");
    }

    #[test]
    fn rule_steps_parse_and_roundtrip() {
        let s = Scenario::from_toml(&format!(
            "{HEAD}[[steps]]\naction = \"set_rule\"\nrule = \"reset=reject*2\"\n[[steps]]\naction = \"clear_rules\"\ncall = \"Reset\"\n[[steps]]\naction = \"clear_rules\"\n"
        ))
        .unwrap();
        assert!(matches!(
            &s.steps[0].action,
            Action::SetRule { rule } if rule == "Reset=reject*2"
        ));
        let action = &s.steps[0].action;
        let back: Action = serde_json::from_str(&serde_json::to_string(action).unwrap()).unwrap();
        assert_eq!(&back, action);
        for step in [
            "action = \"set_rule\"\nrule = \"Reset=explode\"",
            "action = \"set_rule\"\nrule = \"Nope=reject\"",
            "action = \"clear_rules\"\ncall = \"Heartbeat\"",
            // An escape is not a placeholder: the text is checked after unescaping.
            "action = \"set_rule\"\nrule = \"Reset=explode$${x}\"",
            "action = \"clear_rules\"\ncall = \"Nope$${x}\"",
            "action = \"security_event\"\nevent_type = \"Nope$${x}\"",
        ] {
            let e = err(&format!("{HEAD}[[steps]]\n{step}\n"));
            assert!(e.contains("step 1"), "{e}");
        }
    }

    #[test]
    fn load_refuses_big_files_and_non_files() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.toml");
        let pad = "#".repeat(usize::try_from(MAX_SCENARIO_FILE_BYTES).unwrap());
        std::fs::write(
            &big,
            format!("{HEAD}[[steps]]\naction = \"heartbeat\"\n{pad}"),
        )
        .unwrap();
        assert!(
            Scenario::load(&big)
                .unwrap_err()
                .to_string()
                .contains("larger than")
        );
        let e = Scenario::load(dir.path()).unwrap_err().to_string();
        assert!(e.contains("not a regular file"), "{e}");
        let ok = dir.path().join("ok.toml");
        std::fs::write(&ok, format!("{HEAD}[[steps]]\naction = \"heartbeat\"\n")).unwrap();
        assert!(Scenario::load(&ok).is_ok());
    }

    #[test]
    fn uses_variable_sees_declared_counter_and_referenced_names() {
        let s = Scenario::from_toml(
            "name = \"t\"\n[vars]\na = \"x\"\n[[steps]]\naction = \"repeat\"\ncount = 2\nvar = \"k\"\n[[steps.steps]]\naction = \"data_transfer\"\nvendor_id = \"v\"\ndata = \"${b} $${esc}\"\n",
        )
        .unwrap();
        assert!(s.uses_variable("a") && s.uses_variable("k") && s.uses_variable("k0"));
        assert!(s.uses_variable("b"));
        assert!(!s.uses_variable("esc") && !s.uses_variable("zzz"));
    }

    #[test]
    fn security_steps_parse_and_validate_event_types() {
        let s = Scenario::from_toml(&format!(
            "{HEAD}[[steps]]\naction = \"request_certificate\"\n[[steps]]\naction = \"security_event\"\nevent_type = \"tamperdetectionactivated\"\ntech_info = \"lid\"\n"
        ))
        .unwrap();
        assert!(matches!(s.steps[0].action, Action::RequestCertificate {}));
        assert!(matches!(
            &s.steps[1].action,
            Action::SecurityEvent { tech_info: Some(t), .. } if t == "lid"
        ));
        let e = err(&format!(
            "{HEAD}[[steps]]\naction = \"security_event\"\nevent_type = \"Nope\"\n"
        ));
        assert!(e.contains("step 1") && e.contains("Nope"), "{e}");
    }

    #[test]
    fn structural_errors() {
        assert!(err("[[steps]]\naction = \"connect\"\n").contains("name"));
        assert!(err(HEAD).contains("at least one"));
        assert!(err("name = \" \"\n[[steps]]\naction = \"connect\"\n").contains("empty"));
    }

    #[test]
    fn vars_and_capture_parse() {
        let s = Scenario::from_toml(
            "name = \"t\"\n[vars]\na = \"x\"\nn = 5\nf = 1.5\nb = true\n[[steps]]\naction = \"data_transfer\"\nvendor_id = \"${a}\"\ncapture = { tok = \"/data/t\", all = \"\" }\n[[steps]]\naction = \"set_rule\"\nrule = \"reset=delay:${n}\"\n[[steps]]\naction = \"set_rule\"\nrule = \"reset=reject\"\n"
        )
        .unwrap();
        assert_eq!(s.vars.len(), 4);
        assert_eq!(s.vars.get("n"), Some(&serde_json::json!(5)));
        assert_eq!(s.steps[0].capture["tok"], "/data/t");
        assert_eq!(s.steps[0].capture["all"], "");
        assert!(
            matches!(&s.steps[1].action, Action::SetRule { rule } if rule == "reset=delay:${n}")
        );
        assert!(matches!(&s.steps[2].action, Action::SetRule { rule } if rule == "Reset=reject"));
    }

    #[test]
    fn bad_vars_and_capture_are_rejected() {
        let step = "[[steps]]\naction = \"connect\"\n";
        for (vars, needle) in [
            ("[vars]\nx = [1]\n", "string, a number"),
            ("[vars]\nx = {a = 1}\n", "string, a number"),
            ("[vars]\nx = 1979-05-27\n", "string, a number"),
            ("[vars]\n\"a-b\" = 1\n", "valid variable name"),
            ("[vars]\nindex = 1\n", "built-in"),
            ("[vars]\nstation_id = \"x\"\n", "built-in"),
        ] {
            let e = err(&format!("{HEAD}{vars}{step}"));
            assert!(e.contains(needle), "{vars}: {e}");
        }
        for (capture, action, needle) in [
            ("x = \"/a\"", "connect", "only valid on"),
            ("x = \"/a\"", "heartbeat", "only valid on"),
            (
                "\"a b\" = \"/a\"",
                "send_raw\"\ncall = \"Heartbeat",
                "valid variable name",
            ),
            (
                "index = \"/a\"",
                "send_raw\"\ncall = \"Heartbeat",
                "built-in",
            ),
            (
                "x = \"a\"",
                "send_raw\"\ncall = \"Heartbeat",
                "JSON pointer",
            ),
            ("x = 1", "send_raw\"\ncall = \"Heartbeat", "pointer string"),
        ] {
            let e = err(&format!(
                "{HEAD}[[steps]]\naction = \"{action}\"\ncapture = {{ {capture} }}\n"
            ));
            assert!(e.contains(needle) && e.contains("step 1"), "{capture}: {e}");
        }
        let e = err(&format!(
            "{HEAD}[[steps]]\naction = \"connect\"\ncapture = 3\n"
        ));
        assert!(e.contains("capture"), "{e}");
    }

    #[test]
    fn retarget_rewrites_connectors_and_tags() {
        let mut s = Scenario::from_toml(&format!(
            "{HEAD}[[steps]]\naction = \"start_tx\"\nconnector = 1\nid_tag = \"A\"\n[[steps]]\naction = \"authorize\"\nid_tag = \"A\"\n"
        ))
        .unwrap();
        s.retarget(Some(ConnectorId::new(2)), Some(&IdTag::new("B").unwrap()));
        assert!(
            matches!(&s.steps[0].action, Action::StartTx { connector, id_tag }
            if connector.get() == 2 && id_tag.as_str() == "B")
        );
        assert!(
            matches!(&s.steps[1].action, Action::Authorize { id_tag } if id_tag.as_str() == "B")
        );
    }

    #[test]
    fn version_keys_win_over_unknown_fields_and_types_are_friendly() {
        let step = "[[steps]]\naction = \"heartbeat\"\n";
        let e = err(&format!("name = \"t\"\nschema = 2\nfuture_key = 1\n{step}"));
        assert!(e.contains("unsupported schema 2"), "{e}");
        let e = err(&format!(
            "name = \"t\"\nocpp_version = \"2.0.1\"\nfuture_key = 1\n{step}"
        ));
        assert!(e.contains("OCPP 1.6 only"), "{e}");
        let e = err(&format!("name = \"t\"\nschema = \"1\"\n{step}"));
        assert!(e.contains("`schema` must be an integer"), "{e}");
        let e = err(&format!("name = \"t\"\nocpp_version = 1.6\n{step}"));
        assert!(e.contains("`ocpp_version` must be a string"), "{e}");
        let e = err(&format!("name = \"t\"\ntags = \"x\"\n{step}"));
        assert!(e.contains("`tags` must be an array of strings"), "{e}");
    }

    #[test]
    fn blank_step_names_fall_back_to_the_derived_label() {
        for name in ["", "   ", "\t"] {
            let s = Scenario::from_toml(&format!(
                "name = \"t\"\n[[steps]]\naction = \"heartbeat\"\nname = \"{name}\"\n"
            ))
            .unwrap();
            assert_eq!(s.steps[0].label(), "heartbeat");
            assert_eq!(s.steps[0].stable_label(), "heartbeat");
            let flat = s.expanded_steps();
            assert!(!flat[0].name.trim().is_empty() && !flat[0].label.trim().is_empty());
        }
    }

    #[test]
    fn expect_error_excludes_status_and_payload() {
        for expect in [
            "{ error = true, status = \"x\" }",
            "{ error = true, payload = { a = 1 } }",
        ] {
            let e = err(&format!(
                "name = \"t\"\n[[steps]]\naction = \"heartbeat\"\nexpect = {expect}\n"
            ));
            assert!(
                e.contains("step 1") && e.contains("`error = true` excludes"),
                "{e}"
            );
        }
        assert!(
            Scenario::from_toml(
                "name = \"t\"\n[[steps]]\naction = \"heartbeat\"\nexpect = { error = true }\n"
            )
            .is_ok()
        );
    }

    #[test]
    fn reserved_step_keys_have_their_own_message() {
        for text in [
            "[[steps]]\naction = \"heartbeat\"\nevse = 1\n",
            "[[steps]]\naction = \"plug\"\nconnector = 1\nid_token = \"x\"\n",
            "[[steps]]\naction = \"repeat\"\ncount = 2\nevse = 1\n[[steps.steps]]\naction = \"heartbeat\"\n",
        ] {
            let e = err(&format!("name = \"t\"\n{text}"));
            assert!(
                e.contains("is reserved for OCPP 2.x steps; not supported in this build"),
                "{e}"
            );
            assert!(!e.contains("unknown field"), "{e}");
        }
    }

    #[test]
    fn contract_keys_are_checked() {
        let step = "[[steps]]\naction = \"heartbeat\"\n";
        let ok = Scenario::from_toml(&format!(
            "name = \"t\"\nocpp_version = \"1.6\"\nschema = 1\ntags = [\"a\", \"b\"]\n{step}"
        ))
        .unwrap();
        assert_eq!(ok.tags, ["a", "b"]);
        let e = err(&format!("name = \"t\"\nocpp_version = \"2.0.1\"\n{step}"));
        assert!(e.contains("OCPP 1.6 only") && e.contains("2.0.1"), "{e}");
        let e = err(&format!("name = \"t\"\nschema = 2\n{step}"));
        assert!(e.contains("unsupported schema 2"), "{e}");
        let e = err(&format!("name = \"t\"\ntags = [1]\n{step}"));
        assert!(e.contains("string"), "{e}");
        let e = err(&format!("name = \"t\"\ncolour = 1\n{step}"));
        assert!(e.contains("unknown field `colour`"), "{e}");
    }

    #[test]
    fn typos_get_a_suggestion_and_errors_a_position() {
        let e = err("name = \"t\"\n[[steps]]\naction = \"heartbaet\"\n");
        assert!(e.contains("did you mean `heartbeat`"), "{e}");
        assert!(
            e.contains("step 1") && e.contains("line 2, column 1"),
            "{e}"
        );
        let e = err("name = \"t\"\n[[steps]]\naction = \"plug\"\nconnecter = 1\n");
        assert!(e.contains("step 1"), "{e}");
        let e = err("nmae = \"t\"\n");
        assert!(e.contains("did you mean `name`"), "{e}");
        let e = err("name = \"t\"\n[[steps]]\naction = \"zzzzzzzz\"\n");
        assert!(!e.contains("did you mean"), "{e}");
    }

    #[test]
    fn load_errors_name_the_file_and_position() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "name = \"t\"\nnmae = 1\n").unwrap();
        let e = Scenario::load(&path).unwrap_err().to_string();
        assert!(
            e.starts_with(&format!("{}:2:1: invalid scenario", path.display())),
            "{e}"
        );
        std::fs::write(&path, "name = \"t\"\n[[steps]]\naction = \"nope\"\n").unwrap();
        let e = Scenario::load(&path).unwrap_err().to_string();
        assert!(e.contains(":2:1:") && e.contains("step 1"), "{e}");
        std::fs::write(&path, "name = \"t\"\n[[steps]]\naction = \"heartbeat\"\n").unwrap();
        let ok = Scenario::load(&path).unwrap();
        assert_eq!(ok.source.as_deref(), Some(path.to_str().unwrap()));
    }

    #[test]
    fn stable_labels_are_derived_from_the_action() {
        let s = Scenario::from_toml(
            r#"
name = "t"
[[steps]]
action = "expect_call"
call = "Heartbeat"
direction = "out"
[[steps]]
action = "wait_for"
condition = { kind = "tx_active", connector = 1 }
[[steps]]
action = "start_tx"
connector = 1
id_tag = "TAG"
[[steps]]
name = "explicit"
action = "heartbeat"
[[steps]]
action = "sleep"
ms = 5
"#,
        )
        .unwrap();
        let labels: Vec<String> = s.steps.iter().map(Step::stable_label).collect();
        assert_eq!(
            labels,
            [
                "expect_call Heartbeat out",
                "wait_for tx_active c1",
                "start_tx c1 TAG",
                "explicit",
                "sleep 5ms"
            ]
        );
    }

    fn reply_step(fields: &str) -> String {
        format!("name = \"t\"\n[[steps]]\naction = \"reply\"\n{fields}")
    }

    #[test]
    fn reply_step_loads_with_defaults_and_a_stable_label() {
        let s = Scenario::from_toml(&reply_step(
            "call = \"reset\"\nresult = { status = \"Rejected\" }\n",
        ))
        .unwrap();
        let Action::Reply {
            call,
            result,
            error,
            times,
            delay_ms,
            must_fire,
        } = &s.steps[0].action
        else {
            panic!("{:?}", s.steps[0].action);
        };
        assert_eq!(call, "reset");
        assert_eq!(result.as_ref().unwrap()["status"], "Rejected");
        assert!(error.is_none());
        assert_eq!((*times, *delay_ms, *must_fire), (1, 0, false));
        assert_eq!(s.steps[0].stable_label(), "reply reset");
        let e = Scenario::from_toml(&reply_step(
            "call = \"Reset\"\nerror = { code = \"GenericError\" }\ntimes = 3\ndelay_ms = 10\nmust_fire = true\n",
        ))
        .unwrap();
        let Action::Reply { error, times, .. } = &e.steps[0].action else {
            panic!();
        };
        assert_eq!(error.as_ref().unwrap().code, "GenericError");
        assert_eq!(*times, 3);
    }

    #[test]
    fn reply_step_bad_shapes_are_load_errors() {
        for (fields, needle) in [
            ("call = \"Reset\"\n", "needs `result` or `error`"),
            (
                "call = \"Reset\"\nresult = { status = \"Accepted\" }\nerror = { code = \"GenericError\" }\n",
                "exclude each other",
            ),
            ("call = \"Nonsense\"\nresult = {}\n", "unknown action"),
            (
                "result = { status = \"Accepted\" }\n",
                "missing field `call`",
            ),
            (
                "call = \"Reset\"\nresult = { status = \"Accepted\" }\nbogus = 1\n",
                "unknown field `bogus`",
            ),
            (
                "call = \"Reset\"\nresult = { status = \"Accepted\" }\ntimes = 0\n",
                "`times` must be between 1 and 1000",
            ),
            (
                "call = \"Reset\"\nresult = { status = \"Accepted\" }\ntimes = 1001\n",
                "`times` must be between 1 and 1000",
            ),
            (
                "call = \"Reset\"\nresult = { status = \"Accepted\" }\ndelay_ms = 3600001\n",
                "`delay_ms` must be at most 3600000",
            ),
            (
                "call = \"Reset\"\nresult = \"Accepted\"\n",
                "`result` must be a table",
            ),
            (
                "call = \"Reset\"\nresult = { status = \"Maybe\" }\n",
                "not a valid Reset response",
            ),
            (
                "call = \"Reset\"\nerror = { code = \"Nope\" }\n",
                "unknown error code `Nope`",
            ),
            (
                "call = \"Reset\"\nerror = { code = \"GenericError\", extra = 1 }\n",
                "unknown field `extra`",
            ),
            (
                "call = \"Reset\"\nerror = { code = \"GenericError\", details = 1 }\n",
                "`error.details` must be a table",
            ),
            (
                "call = \"Reset\"\nresult = { status = \"Accepted\" }\ncapture = { a = \"/x\" }\n",
                "`capture` is only valid on",
            ),
            (
                "call = \"Reset\"\nresult = { status = \"Accepted\" }\nevse = 1\n",
                "`evse` is reserved for OCPP 2.x steps",
            ),
        ] {
            let e = err(&reply_step(fields));
            assert!(e.contains(needle), "{fields}: {e}");
        }
    }

    #[test]
    fn reply_step_checks_what_it_can_when_it_has_placeholders() {
        // A placeholder defers the schema check and the action check to run time.
        Scenario::from_toml(&format!(
            "name = \"t\"\n[vars]\nr = \"Rejected\"\n{}",
            "[[steps]]\naction = \"reply\"\ncall = \"Reset\"\nresult = { status = \"${r}\" }\n"
        ))
        .unwrap();
        // The action name may be a placeholder too.
        Scenario::from_toml(&format!(
            "name = \"t\"\n[vars]\na = \"Reset\"\n{}",
            "[[steps]]\naction = \"reply\"\ncall = \"${a}\"\nresult = { status = \"Rejected\" }\n"
        ))
        .unwrap();
    }
}
