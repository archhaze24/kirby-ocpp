//! Scenarios as data: parsing and validation (no I/O against a station).

use std::collections::BTreeMap;
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
    StartTx {
        connector: ConnectorId,
        id_tag: IdTag,
    },
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
            Self::RequestCertificate {} => "request_certificate",
            Self::SecurityEvent { .. } => "security_event",
        }
    }
}

impl Step {
    /// The name shown in reports: the explicit `name`, else the action name.
    pub fn label(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| self.action.name().to_owned())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScenario {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    vars: toml::Table,
    #[serde(default)]
    steps: Vec<toml::Table>,
}

impl Scenario {
    /// Parses and validates a scenario from TOML text.
    ///
    /// # Errors
    /// [`ScenarioError::Invalid`] naming the offending step for unknown actions, missing or
    /// unknown fields and bad values.
    pub fn from_toml(text: &str) -> Result<Self, ScenarioError> {
        let raw: RawScenario =
            toml::from_str(text).map_err(|e| ScenarioError::invalid(e.message()))?;
        if raw.name.trim().is_empty() {
            return Err(ScenarioError::invalid("`name` must not be empty"));
        }
        if raw.steps.is_empty() {
            return Err(ScenarioError::invalid(
                "a scenario needs at least one `[[steps]]`",
            ));
        }
        let vars = parse_vars(raw.vars)?;
        let steps = raw
            .steps
            .into_iter()
            .enumerate()
            .map(|(i, table)| parse_step(table).map_err(|e| ScenarioError::step(i, &e)))
            .collect::<Result<Vec<_>, _>>()?;
        let scenario = Self {
            name: raw.name,
            description: raw.description,
            vars,
            steps,
        };
        scenario.validate()?;
        Ok(scenario)
    }

    /// Reads and parses a scenario file.
    ///
    /// # Errors
    /// [`ScenarioError::Io`] or [`ScenarioError::Invalid`].
    pub fn load(path: &Path) -> Result<Self, ScenarioError> {
        let text = std::fs::read_to_string(path).map_err(|source| ScenarioError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_toml(&text)
    }

    fn validate(&self) -> Result<(), ScenarioError> {
        for (i, step) in self.steps.iter().enumerate() {
            let bad = |msg: &str| Err(ScenarioError::step(i, msg));
            match &step.action {
                Action::Plug { connector }
                | Action::Unplug { connector }
                | Action::StartTx { connector, .. }
                | Action::StopTx { connector, .. }
                | Action::Meter { connector, .. }
                    if connector.is_station() =>
                {
                    return bad("`connector` must be 1 or more");
                }
                Action::WaitFor { timeout_ms: 0, .. }
                | Action::ExpectCall { timeout_ms: 0, .. } => {
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
                Action::SendRaw { call, .. } | Action::ExpectCall { call, .. }
                    if call.is_empty() =>
                {
                    return bad("`call` must not be empty");
                }
                Action::SecurityEvent { event_type, .. }
                    if !has_placeholder(event_type)
                        && canonical_event_type(event_type).is_none() =>
                {
                    return bad(&format!("unknown security event type `{event_type}`"));
                }
                Action::ClearRules { call: Some(call) } if !has_placeholder(call) => {
                    if let Err(error) = canonical_action(call) {
                        return bad(&error.to_string());
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Points every connector-bound step at `connector` and every idTag at `id_tag`
    /// (when given). Used to adapt the built-ins to the station being tested.
    pub fn retarget(&mut self, connector: Option<ConnectorId>, id_tag: Option<&IdTag>) {
        for step in &mut self.steps {
            match &mut step.action {
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
}

fn retarget_connector(slot: &mut ConnectorId, to: Option<ConnectorId>) {
    if let Some(to) = to {
        *slot = to;
    }
}

/// Does `text` contain a `${` (a placeholder or an escape)? Such text is checked when it runs.
fn has_placeholder(text: &str) -> bool {
    text.contains("${")
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

fn parse_step(mut table: toml::Table) -> Result<Step, String> {
    let name = match table.remove("name") {
        None => None,
        Some(toml::Value::String(s)) => Some(s),
        Some(_) => return Err("`name` must be a string".to_owned()),
    };
    let expect = match table.remove("expect") {
        None => None,
        Some(value) => Some(
            value
                .try_into::<Expect>()
                .map_err(|e| format!("`expect`: {}", e.message()))?,
        ),
    };
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
        .map_err(|e| format!("action `{tag}`: {}", e.message()))?;
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
        && !has_placeholder(rule)
    {
        *rule = rule
            .parse::<ResponseRule>()
            .map_err(|e| format!("action `{tag}`: {e}"))?
            .to_string();
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
        ] {
            let e = err(&format!("{HEAD}[[steps]]\n{step}\n"));
            assert!(e.contains("step 1"), "{e}");
        }
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
}
