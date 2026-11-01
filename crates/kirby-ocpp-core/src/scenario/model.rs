//! Scenarios as data: parsing and validation (no I/O against a station).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ScenarioError;
use crate::ids::{ConnectorId, IdTag};
use crate::ocpp::types::{ChargePointErrorCode, ConnectorStatus, RegistrationStatus, StopReason};
use crate::station::response_rules::canonical_action;
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
    pub steps: Vec<Step>,
}

/// One step: an [`Action`] plus an optional display name and outcome assertion.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect: Option<Expect>,
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
        #[serde(with = "rule_text")]
        rule: ResponseRule,
    },
    /// Remove the response rules of the command `call`, or all of them when omitted.
    ClearRules {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call: Option<String>,
    },
}

/// A response rule written as its compact text form in TOML.
mod rule_text {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::station::ResponseRule;

    pub fn serialize<S: Serializer>(rule: &ResponseRule, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(rule)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<ResponseRule, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
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
        let steps = raw
            .steps
            .into_iter()
            .enumerate()
            .map(|(i, table)| parse_step(table).map_err(|e| ScenarioError::step(i, &e)))
            .collect::<Result<Vec<_>, _>>()?;
        let scenario = Self {
            name: raw.name,
            description: raw.description,
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
                Action::ClearRules { call: Some(call) } => {
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
    let tag = match table.get("action") {
        None => return Err("missing field `action`".to_owned()),
        Some(toml::Value::String(s)) => s.clone(),
        Some(_) => return Err("`action` must be a string".to_owned()),
    };
    let action = toml::Value::Table(table)
        .try_into::<Action>()
        .map_err(|e| format!("action `{tag}`: {}", e.message()))?;
    Ok(Step {
        name,
        expect,
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
            Action::SetRule { rule } if rule.to_string() == "Reset=reject*2"
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
    fn structural_errors() {
        assert!(err("[[steps]]\naction = \"connect\"\n").contains("name"));
        assert!(err(HEAD).contains("at least one"));
        assert!(err("name = \" \"\n[[steps]]\naction = \"connect\"\n").contains("empty"));
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
