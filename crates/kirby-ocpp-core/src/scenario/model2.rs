//! Scenario steps and `wait_for` conditions for OCPP 2.0.1.
//!
//! The 1.6 grammar in [`super::model`] is typed with 1.6 connectors and idTags; the steps
//! whose parameters differ in 2.0.1 live here. Protocol-neutral steps (`connect`,
//! `disconnect`, `reconnect`, `boot`, `heartbeat`, `sleep`, `expect_call`, `send_raw`,
//! `reply`, `repeat`) reuse the [`Action`](super::Action) variants. A 2.0.1 scenario file is
//! parsed in its own mode and carries these steps as [`Action::V201`](super::Action::V201).
//!
//! The wire shape (key names, defaults) is the contract of 2.0.1 scenario files: EVSE numbers
//! start at 1, tokens are `id_token` plus `type`, and `StopReason` / `ChargingState` use the
//! 2.0.1 names.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::{Action, MAX_WAIT_MS, Step, default_timeout_ms, default_wh_delta, static_text};
use crate::ocpp::Protocol;
use crate::ocpp::v2::messages::{ConnectorStatus, RegistrationStatus};
use crate::ocpp::v2::types::{ChargingState, IdTokenType, StopReason, WireEnum};

fn default_token_type() -> IdTokenType {
    IdTokenType::Iso14443
}

fn default_stop_reason() -> StopReason {
    StopReason::Local
}

/// A device-model component as a `set_variable` step names it. The file may also write a
/// bare string for the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// The EVSE the component belongs to (EVSE-level components).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evse: Option<u32>,
}

/// A device-model variable as a `set_variable` step names it. The file may also write a bare
/// string for the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariableRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

/// What a 2.0.1 step does, for the steps whose parameters differ from 1.6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action2 {
    /// Simulate an EV plugging in.
    Plug {
        evse: u32,
    },
    Unplug {
        evse: u32,
    },
    /// Send `Authorize`. Expects `status = "Accepted"` unless the step says otherwise.
    Authorize {
        id_token: String,
        #[serde(rename = "type", default = "default_token_type")]
        token_type: IdTokenType,
    },
    /// Present a token on an EVSE and start a transaction. Also spelled `start_session`.
    /// Expects `status = "Accepted"` and a started transaction.
    #[serde(alias = "start_session")]
    StartTx {
        evse: u32,
        id_token: String,
        #[serde(rename = "type", default = "default_token_type")]
        token_type: IdTokenType,
    },
    /// Present a token without expecting an outcome (it may also stop a transaction).
    PresentToken {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evse: Option<u32>,
        id_token: String,
        #[serde(rename = "type", default = "default_token_type")]
        token_type: IdTokenType,
    },
    /// Stop the transaction of an EVSE. Also spelled `stop_session`.
    #[serde(alias = "stop_session")]
    StopTx {
        evse: u32,
        #[serde(default = "default_stop_reason")]
        reason: StopReason,
    },
    /// Advance the EVSE meter by `wh_delta`, then sample it.
    Meter {
        evse: u32,
        #[serde(default = "default_wh_delta")]
        wh_delta: u64,
    },
    /// Set the charging state of the transaction on an EVSE.
    ChargingState {
        evse: u32,
        state: ChargingState,
    },
    Fault {
        evse: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connector: Option<u32>,
    },
    ClearFault {
        evse: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connector: Option<u32>,
    },
    /// Re-send the `StatusNotification` of a connector.
    StatusNotification {
        evse: u32,
        connector: u32,
    },
    /// Change a device-model variable locally.
    SetVariable {
        component: ComponentRef,
        variable: VariableRef,
        value: String,
    },
    /// Send `SecurityEventNotification`; any 1 to 50 character event type.
    SecurityEvent {
        event_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tech_info: Option<String>,
    },
    /// Send `DataTransfer`; `data` is any JSON value.
    DataTransfer {
        vendor_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    /// Wait until the station snapshot satisfies `condition`.
    WaitFor {
        condition: Condition2,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
    },
}

/// A condition on the 2.0.1 station snapshot, for `wait_for`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition2 {
    Connected {},
    Disconnected {},
    Registration {
        status: RegistrationStatus,
    },
    ConnectorStatus {
        evse: u32,
        connector: u32,
        status: ConnectorStatus,
    },
    /// A transaction is running on the EVSE.
    TxActive {
        evse: u32,
    },
    TxIdle {
        evse: u32,
    },
    /// The transaction on the EVSE is in this charging state.
    ChargingState {
        evse: u32,
        state: ChargingState,
    },
    /// No transaction events are queued for delivery (`queued_events == 0`).
    NoPendingMessages {},
}

/// Steps that read and write like their 1.6 counterparts.
const NEUTRAL_TAGS: [&str; 9] = [
    "connect",
    "disconnect",
    "reconnect",
    "boot",
    "heartbeat",
    "sleep",
    "expect_call",
    "send_raw",
    "reply",
];

/// Steps typed by [`Action2`], with their aliases.
const V201_TAGS: [&str; 17] = [
    "plug",
    "unplug",
    "authorize",
    "start_tx",
    "start_session",
    "present_token",
    "stop_tx",
    "stop_session",
    "meter",
    "charging_state",
    "fault",
    "clear_fault",
    "status_notification",
    "set_variable",
    "security_event",
    "data_transfer",
    "wait_for",
];

/// Every step name a 2.0.1 file may use, for the "unknown variant" message and its suggestion.
fn known_tags() -> impl Iterator<Item = &'static str> {
    NEUTRAL_TAGS.into_iter().chain(["repeat"]).chain(V201_TAGS)
}

/// The load error for a step that exists in 1.6 only.
fn refused(tag: &str, hint: &str) -> String {
    format!("action `{tag}`: not available for OCPP 2.0.1 ({hint})")
}

/// Reads the `action` of one 2.x step from its table (`action` still in it, `name`,
/// `expect` and `capture` already taken out). `tag` is the step's `action` text and `protocol`
/// the version the file declares (2.0.1 or 2.1).
///
/// # Errors
/// A message for an unknown or 1.6-only step, an unknown or missing key and a bad value.
pub(super) fn parse_action(
    mut table: toml::Table,
    tag: &str,
    protocol: Protocol,
) -> Result<Action, String> {
    match tag {
        "set_rule" | "clear_rules" => {
            return Err(refused(
                tag,
                "response rules name OCPP 1.6 commands; use a `reply` step to answer a CSMS command",
            ));
        }
        "request_certificate" => {
            return Err(refused(tag, "use `send_raw` with `SignCertificate`"));
        }
        "status" => {
            return Err(refused(
                tag,
                "`status` sets a 1.6 connector status; use `plug`, `unplug`, `fault` or `clear_fault` to change it and `status_notification` to resend it",
            ));
        }
        _ => {}
    }
    let value_error = |message: String| {
        let mut message = super::with_suggestion(message);
        if message.contains("unknown field `id_tag`") {
            message.push_str("; OCPP 2.0.1 steps use `id_token`");
        } else if message.contains("unknown field `connector`") {
            message.push_str("; OCPP 2.0.1 steps use `evse`");
        }
        format!("action `{tag}`: {message}")
    };
    if NEUTRAL_TAGS.contains(&tag) {
        return toml::Value::Table(table)
            .try_into::<Action>()
            .map_err(|e| value_error(e.message().to_owned()));
    }
    if !V201_TAGS.contains(&tag) {
        let list = known_tags()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "action `{tag}`: {}",
            super::with_suggestion(format!("unknown variant `{tag}`, expected one of {list}"))
        ));
    }
    if tag == "set_variable" {
        expand_set_variable(&mut table)?;
    }
    let action = toml::Value::Table(table)
        .try_into::<Action2>()
        .map_err(|e| value_error(e.message().to_owned()))?;
    check_values_of(&action, protocol).map_err(|e| format!("action `{tag}`: {e}"))?;
    Ok(Action::V201(Box::new(action)))
}

/// Rejects values that exist only in OCPP 2.1 when the file declares an earlier version, with
/// the message the 2.0.1 enumerations always gave. The types accept the 2.1 values.
fn check_values_of(action: &Action2, protocol: Protocol) -> Result<(), String> {
    fn unknown(text: &str, known: &[&str]) -> String {
        let list = known
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("unknown variant `{text}`, expected one of {list}")
    }
    if protocol == Protocol::V21 {
        return Ok(());
    }
    match action {
        Action2::Authorize { token_type, .. }
        | Action2::StartTx { token_type, .. }
        | Action2::PresentToken { token_type, .. } => {
            if let IdTokenType::Other(text) = token_type {
                let known: Vec<&str> = IdTokenType::KNOWN.iter().map(IdTokenType::as_str).collect();
                return Err(unknown(text, &known));
            }
        }
        Action2::StopTx {
            reason: StopReason::ReqEnergyTransferRejected,
            ..
        } => {
            let known: Vec<&str> = StopReason::ALL
                .iter()
                .filter(|reason| **reason != StopReason::ReqEnergyTransferRejected)
                .map(WireEnum::as_str)
                .collect();
            return Err(unknown(
                StopReason::ReqEnergyTransferRejected.as_str(),
                &known,
            ));
        }
        _ => {}
    }
    Ok(())
}

/// `set_variable` accepts a bare string for `component` and `variable` (the name) and plain
/// numbers and booleans for `value` (the wire type is a string).
fn expand_set_variable(table: &mut toml::Table) -> Result<(), String> {
    for key in ["component", "variable"] {
        match table.remove(key) {
            None => {}
            Some(toml::Value::String(name)) => {
                let mut part = toml::Table::new();
                part.insert("name".to_owned(), toml::Value::String(name));
                table.insert(key.to_owned(), toml::Value::Table(part));
            }
            Some(part @ toml::Value::Table(_)) => {
                table.insert(key.to_owned(), part);
            }
            Some(_) => {
                return Err(format!(
                    "action `set_variable`: `{key}` must be a name or a table with `name`"
                ));
            }
        }
    }
    match table.remove("value") {
        Some(toml::Value::Integer(n)) => {
            table.insert("value".to_owned(), toml::Value::String(n.to_string()));
        }
        Some(toml::Value::Boolean(b)) => {
            table.insert("value".to_owned(), toml::Value::String(b.to_string()));
        }
        Some(other) => {
            table.insert("value".to_owned(), other);
        }
        None => {}
    }
    Ok(())
}

/// Characters of `text` once placeholders are resolved; `None` for text with a `${name}`
/// placeholder, which is checked when the step runs.
fn static_len(text: &str) -> Option<usize> {
    static_text(text).map(|text| text.chars().count())
}

/// A bound on a text key that is known at load time.
fn check_len(key: &str, text: &str, min: usize, max: usize) -> Result<(), String> {
    match static_len(text) {
        Some(len) if len < min || len > max => Err(if min == 0 {
            format!("`{key}` must be at most {max} characters")
        } else {
            format!("`{key}` must be {min} to {max} characters")
        }),
        _ => Ok(()),
    }
}

/// Points every EVSE-bound 2.0.1 step at `evse` and every token at `id_token` (when given),
/// recursing into `repeat`. A `set_variable` component keeps its own EVSE.
pub(super) fn retarget_steps(steps: &mut [Step], evse: Option<u32>, id_token: Option<&str>) {
    let set_evse = |slot: &mut u32| {
        if let Some(evse) = evse {
            *slot = evse;
        }
    };
    let set_token = |slot: &mut String| {
        if let Some(token) = id_token {
            token.clone_into(slot);
        }
    };
    for step in steps {
        match &mut step.action {
            Action::Repeat { steps, .. } => retarget_steps(steps, evse, id_token),
            Action::V201(action) => match &mut **action {
                Action2::Plug { evse: e }
                | Action2::Unplug { evse: e }
                | Action2::StopTx { evse: e, .. }
                | Action2::Meter { evse: e, .. }
                | Action2::ChargingState { evse: e, .. }
                | Action2::Fault { evse: e, .. }
                | Action2::ClearFault { evse: e, .. }
                | Action2::StatusNotification { evse: e, .. }
                | Action2::WaitFor {
                    condition:
                        Condition2::ConnectorStatus { evse: e, .. }
                        | Condition2::TxActive { evse: e }
                        | Condition2::TxIdle { evse: e }
                        | Condition2::ChargingState { evse: e, .. },
                    ..
                } => set_evse(e),
                Action2::StartTx {
                    evse: e,
                    id_token: t,
                    ..
                } => {
                    set_evse(e);
                    set_token(t);
                }
                Action2::PresentToken {
                    evse: e,
                    id_token: t,
                    ..
                } => {
                    if let Some(e) = e {
                        set_evse(e);
                    }
                    set_token(t);
                }
                Action2::Authorize { id_token: t, .. } => set_token(t),
                _ => {}
            },
            _ => {}
        }
    }
}

impl Action2 {
    /// Stable snake_case name (the serde tag).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Plug { .. } => "plug",
            Self::Unplug { .. } => "unplug",
            Self::Authorize { .. } => "authorize",
            Self::StartTx { .. } => "start_tx",
            Self::PresentToken { .. } => "present_token",
            Self::StopTx { .. } => "stop_tx",
            Self::Meter { .. } => "meter",
            Self::ChargingState { .. } => "charging_state",
            Self::Fault { .. } => "fault",
            Self::ClearFault { .. } => "clear_fault",
            Self::StatusNotification { .. } => "status_notification",
            Self::SetVariable { .. } => "set_variable",
            Self::SecurityEvent { .. } => "security_event",
            Self::DataTransfer { .. } => "data_transfer",
            Self::WaitFor { .. } => "wait_for",
        }
    }

    /// Checks what can be known before the step runs: EVSE and connector numbers, wait
    /// limits and the lengths the OCPP 2.0.1 schemas set. Text with a `${name}` placeholder is
    /// checked when the step runs.
    ///
    /// # Errors
    /// A message naming the offending key.
    pub fn validate(&self) -> Result<(), String> {
        self.validate_for(Protocol::V201)
    }

    /// [`Self::validate`] with the lengths of `protocol`: an `id_token` may have 255
    /// characters in OCPP 2.1 and 36 in 2.0.1.
    ///
    /// # Errors
    /// A message naming the offending key.
    pub fn validate_for(&self, protocol: Protocol) -> Result<(), String> {
        self.validate_numbers()?;
        self.validate_texts(protocol)
    }

    fn validate_texts(&self, protocol: Protocol) -> Result<(), String> {
        let max_token = if protocol == Protocol::V21 { 255 } else { 36 };
        let token = |id_token: &str, token_type: &IdTokenType| {
            // `NoAuthorization` tokens carry no value.
            let min = usize::from(*token_type != IdTokenType::NoAuthorization);
            check_len("id_token", id_token, min, max_token)
        };
        match self {
            Self::Authorize {
                id_token,
                token_type,
            }
            | Self::StartTx {
                id_token,
                token_type,
                ..
            }
            | Self::PresentToken {
                id_token,
                token_type,
                ..
            } => token(id_token, token_type),
            Self::SecurityEvent {
                event_type,
                tech_info,
            } => {
                check_len("event_type", event_type, 1, 50)?;
                tech_info
                    .as_deref()
                    .map_or(Ok(()), |text| check_len("tech_info", text, 0, 255))
            }
            Self::DataTransfer {
                vendor_id,
                message_id,
                ..
            } => {
                check_len("vendor_id", vendor_id, 1, 255)?;
                message_id
                    .as_deref()
                    .map_or(Ok(()), |text| check_len("message_id", text, 0, 50))
            }
            Self::SetVariable {
                component,
                variable,
                value,
            } => {
                check_len("component", &component.name, 1, 50)?;
                if let Some(instance) = &component.instance {
                    check_len("component.instance", instance, 1, 50)?;
                }
                check_len("variable", &variable.name, 1, 50)?;
                if let Some(instance) = &variable.instance {
                    check_len("variable.instance", instance, 1, 50)?;
                }
                check_len("value", value, 0, 2500)
            }
            _ => Ok(()),
        }
    }

    fn validate_numbers(&self) -> Result<(), String> {
        let evse = match self {
            Self::Plug { evse }
            | Self::Unplug { evse }
            | Self::StartTx { evse, .. }
            | Self::StopTx { evse, .. }
            | Self::Meter { evse, .. }
            | Self::ChargingState { evse, .. }
            | Self::Fault { evse, .. }
            | Self::ClearFault { evse, .. }
            | Self::StatusNotification { evse, .. }
            | Self::PresentToken {
                evse: Some(evse), ..
            }
            | Self::WaitFor {
                condition:
                    Condition2::ConnectorStatus { evse, .. }
                    | Condition2::TxActive { evse }
                    | Condition2::TxIdle { evse }
                    | Condition2::ChargingState { evse, .. },
                ..
            } => Some(*evse),
            _ => None,
        };
        if evse == Some(0) {
            return Err("`evse` must be 1 or more".to_owned());
        }
        let connector = match self {
            Self::Fault {
                connector: Some(connector),
                ..
            }
            | Self::ClearFault {
                connector: Some(connector),
                ..
            }
            | Self::StatusNotification { connector, .. }
            | Self::WaitFor {
                condition: Condition2::ConnectorStatus { connector, .. },
                ..
            } => Some(*connector),
            _ => None,
        };
        if connector == Some(0) {
            return Err("`connector` must be 1 or more".to_owned());
        }
        if let Self::SetVariable { component, .. } = self
            && component.evse == Some(0)
        {
            return Err("`component.evse` must be 1 or more".to_owned());
        }
        if let Self::WaitFor { timeout_ms, .. } = self {
            if *timeout_ms == 0 {
                return Err("`timeout_ms` must be positive".to_owned());
            }
            if *timeout_ms > MAX_WAIT_MS {
                return Err("`timeout_ms` must be at most 86400000 (one day)".to_owned());
            }
        }
        Ok(())
    }
}
