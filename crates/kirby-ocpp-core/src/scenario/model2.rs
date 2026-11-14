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

use super::model::{MAX_WAIT_MS, Step, default_timeout_ms, default_wh_delta};
use crate::ocpp::v2::messages::{ConnectorStatus, RegistrationStatus};
use crate::ocpp::v2::types::{ChargingState, IdTokenType, StopReason};

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

/// Parses one `[[steps]]` table of a 2.0.1 file; `open_loops` is the number of `repeat` steps
/// around it. The loader is not written yet: every 2.0.1 file fails to load with a clear
/// message.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the 2.0.1 loader consumes the table once it is implemented"
)]
pub(super) fn parse_step(_table: toml::Table, _open_loops: usize) -> Result<Step, String> {
    Err("`ocpp_version = \"2.0.1\"`: OCPP 2.0.1 scenario files are not implemented yet".to_owned())
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

    /// Checks what can be known before the step runs. The scaffold covers the EVSE numbers and
    /// the wait limits; the remaining checks (names, lengths, shorthand) belong to the
    /// 2.0.1 loader.
    ///
    /// # Errors
    /// A message naming the offending key.
    pub fn validate(&self) -> Result<(), String> {
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
