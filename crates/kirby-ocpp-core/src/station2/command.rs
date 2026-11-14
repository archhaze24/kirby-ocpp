//! Commands accepted by an OCPP 2.0.1 station and the outcomes they produce.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ocpp::v2::types::{ChargingState, Component, IdTokenType, StopReason, Variable};
use crate::station::scripted_reply::ScriptedReply;

/// Everything a UI, a headless runner or a scenario can ask the station to do.
///
/// Commands are plain data (`Clone`, `Debug`, serde) and carry no reply channel: the answer
/// is the `Result<Outcome2, StationError>` returned by
/// [`Station2Handle::send`](super::Station2Handle::send). The future resolves when the
/// command has completed: commands that are a CALL complete when the CSMS answered, commands
/// that change local state as soon as the state changed. EVSE and connector numbers are
/// integers; transaction ids are strings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Command2 {
    /// Open the connection. Resets the reconnect attempt counter.
    Connect,
    /// Close the connection and stop automatic reconnects.
    Disconnect,
    /// `Disconnect` followed by `Connect`.
    Reconnect,
    /// Send `BootNotification`.
    Boot,
    /// Send `Heartbeat`.
    Heartbeat,
    /// Simulate an EV plugging in.
    PlugIn { evse: u32 },
    /// Simulate the EV being unplugged.
    Unplug { evse: u32 },
    /// The driver presents a token: authorizes it, then starts or stops a transaction
    /// according to the start and stop points. Without `evse` the first EVSE with an EV
    /// connected and no transaction is used.
    PresentToken {
        evse: Option<u32>,
        id_token: String,
        token_type: IdTokenType,
    },
    /// Send a bare `Authorize`.
    Authorize {
        id_token: String,
        token_type: IdTokenType,
    },
    /// Stop the transaction of an EVSE locally.
    StopTransaction { evse: u32, reason: StopReason },
    /// Advance the simulated meter of an EVSE by `wh_delta`.
    AdvanceMeter { evse: u32, wh_delta: u64 },
    /// Set the charging state of the transaction on an EVSE.
    SetChargingState { evse: u32, state: ChargingState },
    /// Inject a fault on an EVSE, or on one of its connectors.
    Fault { evse: u32, connector: Option<u32> },
    /// Clear a fault.
    ClearFault { evse: u32, connector: Option<u32> },
    /// Re-send a `StatusNotification` with the current state of a connector.
    StatusNotification { evse: u32, connector: u32 },
    /// Send `SecurityEventNotification`.
    SendSecurityEvent {
        event_type: String,
        tech_info: Option<String>,
    },
    /// Send `DataTransfer`.
    DataTransfer {
        vendor_id: String,
        message_id: Option<String>,
        data: Option<Value>,
    },
    /// Send an arbitrary CALL, optionally bypassing local schema validation.
    SendRaw {
        action: String,
        payload: Value,
        validate: bool,
    },
    /// Change a device-model variable locally, as `SetVariables` would. The field names
    /// follow `SetVariableData`, so recordings redact secret values.
    SetVariable {
        component: Component,
        variable: Variable,
        #[serde(rename = "attributeValue")]
        value: String,
    },
    /// Take one meter sample on an EVSE as the periodic timer would: inside a transaction an
    /// `Updated` event with trigger reason `MeterValuePeriodic`, outside one a `MeterValues`.
    SampleMeter { evse: u32 },
    /// Arm an answer for the next inbound CALLs of one action (a scenario `reply` step). See
    /// [`crate::station::scripted_reply`]; the action name is checked against the 2.0.1
    /// registry.
    ArmScriptedReply { reply: ScriptedReply },
    /// Remove every armed scripted reply. The outcome is a `Response` whose payload lists the
    /// ones that were not fully used: `{"unused":[{"tag":..,"action":..,"remaining":..}]}`.
    ClearScriptedReplies,
    /// Stop the station: close the connection gracefully, flush state, end the actor.
    Shutdown,
}

impl Command2 {
    /// Stable snake_case name (the serde tag), for logs and error messages.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Disconnect => "disconnect",
            Self::Reconnect => "reconnect",
            Self::Boot => "boot",
            Self::Heartbeat => "heartbeat",
            Self::PlugIn { .. } => "plug_in",
            Self::Unplug { .. } => "unplug",
            Self::PresentToken { .. } => "present_token",
            Self::Authorize { .. } => "authorize",
            Self::StopTransaction { .. } => "stop_transaction",
            Self::AdvanceMeter { .. } => "advance_meter",
            Self::SetChargingState { .. } => "set_charging_state",
            Self::Fault { .. } => "fault",
            Self::ClearFault { .. } => "clear_fault",
            Self::StatusNotification { .. } => "status_notification",
            Self::SendSecurityEvent { .. } => "send_security_event",
            Self::DataTransfer { .. } => "data_transfer",
            Self::SendRaw { .. } => "send_raw",
            Self::SetVariable { .. } => "set_variable",
            Self::SampleMeter { .. } => "sample_meter",
            Self::ArmScriptedReply { .. } => "arm_scripted_reply",
            Self::ClearScriptedReplies => "clear_scripted_replies",
            Self::Shutdown => "shutdown",
        }
    }
}

/// What a successfully handled command produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Outcome2 {
    /// Done; nothing to report.
    Done,
    /// A CALL was answered: its CALLRESULT payload.
    Response { payload: Value },
    /// A transaction started on `evse`.
    TransactionStarted { evse: u32, transaction_id: String },
    /// An authorization finished. `status` is the `idTokenInfo` status; `id_token_info` the
    /// answer's `idTokenInfo` as received, when there was one.
    Authorized {
        status: String,
        id_token_info: Option<Value>,
    },
}
