//! Commands accepted by the station and the outcomes they produce.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::authorization::AuthorizationDecision;
use super::maintenance::{DiagnosticsOutcome, FirmwareOutcome};
use super::response_rules::ResponseRule;
use super::scripted_reply::ScriptedReply;
use crate::ids::{ConnectorId, IdTag, TxId};
use crate::ocpp::messages::{ChargingProfile, IdTagInfo};
use crate::ocpp::types::{
    AuthorizationStatus, ChargePointErrorCode, ConnectorStatus, DiagnosticsStatus, FirmwareStatus,
    Measurand, StopReason,
};

/// Everything a UI, a headless runner or a scenario can ask the station to do.
///
/// Commands are plain data (`Clone`, `Debug`, serde) so scenarios can be
/// described as data and replayed. They carry no reply channel: the answer is the
/// `Result<Outcome, StationError>` returned by [`super::StationHandle::send`].
/// The future resolves when the command has been **completed**. Commands that *are* a CALL
/// (`Boot`, `Heartbeat`, `DataTransfer`, `*StatusNotification`, `StatusNotification`,
/// `SendRaw`, `Authorize`, `StartTransaction`, ...) complete when the CSMS
/// answered, up to the call timeout. Commands that change local state (`PlugIn`, `Unplug`,
/// `SetConnectorStatus`, `Fault`, `AddConnector`, ...) complete as soon as the state changed;
/// the resulting `StatusNotification` is queued by the station's status discipline and may be
/// debounced (`MinimumStatusDuration`) or, while offline or unregistered, not sent at all.
/// UIs should run commands in a task instead of blocking their event loop.
///
/// The set mirrors the TS `Tui` action list:
/// Station tab (boot, heartbeat, reconnect, disconnect, connect, add connector),
/// Connector tab (plug, status cycle/set, StatusNotification, fault), Transaction
/// tab (authorize, start, stop, meter values), Maintenance tab (outcomes and
/// status notifications), Data tab (DataTransfer). Scenarios are compositions of
/// these commands, not commands themselves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Command {
    /// Open the connection (and boot once connected). Resets the reconnect attempt counter.
    Connect,
    /// Close the connection and stop automatic reconnects.
    Disconnect,
    /// `Disconnect` followed by `Connect`.
    Reconnect,
    /// Send `BootNotification`.
    Boot,
    /// Send `Heartbeat`.
    Heartbeat,
    /// Add a connector; the outcome carries its id.
    AddConnector,
    /// Simulate an EV plugging in.
    PlugIn { connector: ConnectorId },
    /// Simulate the EV being unplugged (stops a running transaction with `EVDisconnected`).
    Unplug { connector: ConnectorId },
    /// Set a connector's status (connector 0 allowed); the StatusNotification follows per
    /// the station's status discipline (see `station::status`).
    SetConnectorStatus {
        connector: ConnectorId,
        status: ConnectorStatus,
    },
    /// Advance a connector's status to the next one in the TS "cycle status" order.
    CycleConnectorStatus { connector: ConnectorId },
    /// Re-send a StatusNotification with the connector's current state.
    StatusNotification { connector: ConnectorId },
    /// Inject a fault (a running transaction is stopped with reason `Other`).
    Fault {
        connector: ConnectorId,
        error_code: ChargePointErrorCode,
        info: Option<String>,
        vendor_error_code: Option<String>,
    },
    /// Clear a fault on the connector.
    ClearFault { connector: ConnectorId },
    /// Send `Authorize` (or decide locally when offline).
    Authorize { id_tag: IdTag },
    /// Start a transaction on the connector (auto-plugs the EV when `Available`).
    StartTransaction {
        connector: ConnectorId,
        id_tag: IdTag,
    },
    /// Stop the connector's transaction.
    StopTransaction {
        connector: ConnectorId,
        reason: StopReason,
        /// Defaults to the idTag that started the transaction.
        id_tag: Option<IdTag>,
    },
    /// Send MeterValues, advancing the simulated meter by `wh_delta`.
    /// `measurands: None` uses the configured `MeterValuesSampledData`.
    MeterValues {
        connector: ConnectorId,
        wh_delta: u64,
        measurands: Option<Vec<Measurand>>,
    },
    /// Send `DataTransfer`.
    DataTransfer {
        vendor_id: String,
        message_id: Option<String>,
        data: Option<String>,
    },
    /// Send `DiagnosticsStatusNotification` with the given status.
    DiagnosticsStatusNotification { status: DiagnosticsStatus },
    /// Send `FirmwareStatusNotification` with the given status.
    FirmwareStatusNotification { status: FirmwareStatus },
    /// Choose how the next simulated diagnostics upload ends.
    SetDiagnosticsOutcome { outcome: DiagnosticsOutcome },
    /// Choose how the next simulated firmware update ends.
    SetFirmwareOutcome { outcome: FirmwareOutcome },
    /// Send an arbitrary CALL, optionally bypassing local schema validation (negative testing).
    SendRaw {
        action: String,
        payload: Value,
        validate: bool,
    },
    /// Stop waiting for the oldest unanswered CALL of one action: the command that sent it
    /// completes with a `cancelled` error, and a late answer is logged but ignored. Refused for
    /// `StartTransaction`, `StopTransaction` and `MeterValues`: the CSMS may already have acted
    /// on them, and the station state must follow its answer.
    CancelCall { action: String },
    /// Make the station misbehave on purpose: add (or replace, per action) a response rule for an
    /// inbound CSMS command. See [`super::response_rules`].
    SetResponseRule { rule: ResponseRule },
    /// Remove the response rules of one action, or all of them for `None`.
    ClearResponseRules { action: Option<String> },
    /// Arm an answer for the next inbound CALLs of one action (a scenario `reply` step). See
    /// [`super::scripted_reply`]. The action name is checked like a response rule's.
    ArmScriptedReply { reply: ScriptedReply },
    /// Remove every armed scripted reply. The outcome is a `Response` whose payload lists the
    /// ones that were not fully used: `{"unused":[{"tag":..,"action":..,"remaining":..}]}`.
    ClearScriptedReplies,
    /// Change a writable configuration key as `ChangeConfiguration` would (same validation,
    /// persistence and side effects), without a CSMS round trip. Unknown, read-only or invalid
    /// values are `StationError::InvalidArgument`.
    SetConfiguration { key: String, value: String },
    /// Add or replace a local list entry (a differential update; the list version goes up by one).
    SetLocalListEntry { id_tag: IdTag, info: IdTagInfo },
    /// Remove a local list entry (version up by one); an absent idTag is an error.
    RemoveLocalListEntry { id_tag: IdTag },
    /// Empty the local list and reset its version to `0` (a full update).
    ClearLocalList,
    /// Install a charging profile as `SetChargingProfile` would (same validation); a refusal is
    /// `StationError::InvalidArgument`.
    InstallChargingProfile {
        connector: ConnectorId,
        profile: ChargingProfile,
    },
    /// Remove the charging profile with this id, or all of them for `None`.
    ClearChargingProfiles { id: Option<i32> },
    /// Create a key pair and a CSR and send `SignCertificate` (Security Whitepaper). The
    /// outcome is the CSMS' `SignCertificate.conf`; the certificate arrives later as
    /// `CertificateSigned`.
    RequestCertificate,
    /// Report a security event (`SecurityEventNotification`); `event_type` must be one of the
    /// whitepaper's event names (see `station::security`). Queued while the station is offline.
    SendSecurityEvent {
        event_type: String,
        tech_info: Option<String>,
    },
    /// Stop the station: close the connection gracefully, flush state, end the actor.
    Shutdown,
}

impl Command {
    /// Stable snake_case name (the serde tag), for logs and error messages.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Disconnect => "disconnect",
            Self::Reconnect => "reconnect",
            Self::Boot => "boot",
            Self::Heartbeat => "heartbeat",
            Self::AddConnector => "add_connector",
            Self::PlugIn { .. } => "plug_in",
            Self::Unplug { .. } => "unplug",
            Self::SetConnectorStatus { .. } => "set_connector_status",
            Self::CycleConnectorStatus { .. } => "cycle_connector_status",
            Self::StatusNotification { .. } => "status_notification",
            Self::Fault { .. } => "fault",
            Self::ClearFault { .. } => "clear_fault",
            Self::Authorize { .. } => "authorize",
            Self::StartTransaction { .. } => "start_transaction",
            Self::StopTransaction { .. } => "stop_transaction",
            Self::MeterValues { .. } => "meter_values",
            Self::DataTransfer { .. } => "data_transfer",
            Self::DiagnosticsStatusNotification { .. } => "diagnostics_status_notification",
            Self::FirmwareStatusNotification { .. } => "firmware_status_notification",
            Self::SetDiagnosticsOutcome { .. } => "set_diagnostics_outcome",
            Self::SetFirmwareOutcome { .. } => "set_firmware_outcome",
            Self::SendRaw { .. } => "send_raw",
            Self::CancelCall { .. } => "cancel_call",
            Self::SetResponseRule { .. } => "set_response_rule",
            Self::ClearResponseRules { .. } => "clear_response_rules",
            Self::ArmScriptedReply { .. } => "arm_scripted_reply",
            Self::ClearScriptedReplies => "clear_scripted_replies",
            Self::SetConfiguration { .. } => "set_configuration",
            Self::SetLocalListEntry { .. } => "set_local_list_entry",
            Self::RemoveLocalListEntry { .. } => "remove_local_list_entry",
            Self::ClearLocalList => "clear_local_list",
            Self::InstallChargingProfile { .. } => "install_charging_profile",
            Self::ClearChargingProfiles { .. } => "clear_charging_profiles",
            Self::RequestCertificate => "request_certificate",
            Self::SendSecurityEvent { .. } => "send_security_event",
            Self::Shutdown => "shutdown",
        }
    }
}

/// What a successfully handled command produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Outcome {
    /// Done; nothing to report.
    Done,
    /// [`Command::AddConnector`]: the new connector.
    ConnectorAdded { connector: ConnectorId },
    /// [`Command::Authorize`]: the decision (which may be a rejection).
    Authorized(AuthorizationDecision),
    /// [`Command::StartTransaction`]: started. `tx_id` is [`TxId::Local`] if the
    /// station is offline and the start is queued.
    TransactionStarted { connector: ConnectorId, tx_id: TxId },
    /// [`Command::StartTransaction`]: the CSMS rejected the idTag; no local state was created.
    TransactionRejected { status: AuthorizationStatus },
    /// A CALL was answered: its CALLRESULT payload (`Boot`, `Heartbeat`, `DataTransfer`, `SendRaw`, ...).
    Response { payload: Value },
}
