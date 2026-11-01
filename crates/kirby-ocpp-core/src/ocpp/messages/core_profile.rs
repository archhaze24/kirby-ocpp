//! Core profile: the actions every OCPP 1.6 charge point implements.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::common::{ChargingProfile, IdTagInfo, MeterValue};
use crate::ids::{ConnectorId, IdTag, ReservationId};
use crate::ocpp::types::{
    AvailabilityType, ChargePointErrorCode, ConfigurationStatus, ConnectorStatus,
    RegistrationStatus, ResetType, StopReason, wire_enum,
};

// ---- Authorize -------------------------------------------------------------

/// `Authorize.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizeRequest {
    pub id_tag: IdTag,
}

/// `Authorize.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizeResponse {
    pub id_tag_info: IdTagInfo,
}

action!(
    /// `Authorize` (charge point -> central system).
    Authorize = "Authorize", AuthorizeRequest => AuthorizeResponse, OutboundAction
);

// ---- BootNotification ------------------------------------------------------

/// `BootNotification.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootNotificationRequest {
    pub charge_point_vendor: String,
    pub charge_point_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charge_point_serial_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charge_box_serial_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iccid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imsi: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meter_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meter_serial_number: Option<String>,
}

/// `BootNotification.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootNotificationResponse {
    pub status: RegistrationStatus,
    pub current_time: DateTime<Utc>,
    /// Heartbeat interval in seconds (or retry interval when not `Accepted`).
    pub interval: i32,
}

action!(
    /// `BootNotification` (charge point -> central system).
    BootNotification = "BootNotification", BootNotificationRequest => BootNotificationResponse, OutboundAction
);

// ---- ChangeAvailability ----------------------------------------------------

wire_enum! {
    /// `AvailabilityStatus` of `ChangeAvailability.conf`.
    AvailabilityStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
        Scheduled => "Scheduled",
    }
}

/// `ChangeAvailability.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeAvailabilityRequest {
    pub connector_id: ConnectorId,
    /// Wire name `type`.
    #[serde(rename = "type")]
    pub kind: AvailabilityType,
}

/// `ChangeAvailability.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeAvailabilityResponse {
    pub status: AvailabilityStatus,
}

action!(
    /// `ChangeAvailability` (central system -> charge point).
    ChangeAvailability = "ChangeAvailability", ChangeAvailabilityRequest => ChangeAvailabilityResponse, InboundAction
);

// ---- ChangeConfiguration ---------------------------------------------------

/// `ChangeConfiguration.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeConfigurationRequest {
    pub key: String,
    pub value: String,
}

/// `ChangeConfiguration.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeConfigurationResponse {
    pub status: ConfigurationStatus,
}

action!(
    /// `ChangeConfiguration` (central system -> charge point).
    ChangeConfiguration = "ChangeConfiguration", ChangeConfigurationRequest => ChangeConfigurationResponse, InboundAction
);

// ---- ClearCache ------------------------------------------------------------

wire_enum! {
    /// `ClearCacheStatus`.
    ClearCacheStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
    }
}

/// `ClearCache.req`: no fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearCacheRequest {}

/// `ClearCache.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearCacheResponse {
    pub status: ClearCacheStatus,
}

action!(
    /// `ClearCache` (central system -> charge point).
    ClearCache = "ClearCache", ClearCacheRequest => ClearCacheResponse, InboundAction
);

// ---- DataTransfer ----------------------------------------------------------

wire_enum! {
    /// `DataTransferStatus`.
    DataTransferStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
        UnknownMessageId => "UnknownMessageId",
        UnknownVendorId => "UnknownVendorId",
    }
}

/// `DataTransfer.req` (both directions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataTransferRequest {
    pub vendor_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Free-form data (a string; vendors usually put JSON in it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

/// `DataTransfer.conf` (both directions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataTransferResponse {
    pub status: DataTransferStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

action!(
    /// `DataTransfer` (either side may initiate it).
    DataTransfer = "DataTransfer", DataTransferRequest => DataTransferResponse, OutboundAction, InboundAction
);

// ---- GetConfiguration ------------------------------------------------------

/// `GetConfiguration.req`. Without `key` the whole configuration is requested.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetConfigurationRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<Vec<String>>,
}

/// One entry of `GetConfiguration.conf.configurationKey`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurationKey {
    pub key: String,
    pub readonly: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// `GetConfiguration.conf`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetConfigurationResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration_key: Option<Vec<ConfigurationKey>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_key: Option<Vec<String>>,
}

action!(
    /// `GetConfiguration` (central system -> charge point).
    GetConfiguration = "GetConfiguration", GetConfigurationRequest => GetConfigurationResponse, InboundAction
);

// ---- MeterValues -----------------------------------------------------------

/// `MeterValues.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterValuesRequest {
    pub connector_id: ConnectorId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<i32>,
    pub meter_value: Vec<MeterValue>,
}

/// `MeterValues.conf`: no fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterValuesResponse {}

action!(
    /// `MeterValues` (charge point -> central system).
    MeterValues = "MeterValues", MeterValuesRequest => MeterValuesResponse, OutboundAction
);

// ---- RemoteStartTransaction / RemoteStopTransaction ------------------------

wire_enum! {
    /// `RemoteStartStopStatus` of `RemoteStartTransaction.conf` and `RemoteStopTransaction.conf`.
    RemoteStartStopStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
    }
}

/// `RemoteStartTransaction.req`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStartTransactionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_id: Option<ConnectorId>,
    pub id_tag: IdTag,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charging_profile: Option<ChargingProfile>,
}

/// `RemoteStartTransaction.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStartTransactionResponse {
    pub status: RemoteStartStopStatus,
}

action!(
    /// `RemoteStartTransaction` (central system -> charge point).
    RemoteStartTransaction = "RemoteStartTransaction", RemoteStartTransactionRequest => RemoteStartTransactionResponse, InboundAction
);

/// `RemoteStopTransaction.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStopTransactionRequest {
    pub transaction_id: i32,
}

/// `RemoteStopTransaction.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStopTransactionResponse {
    pub status: RemoteStartStopStatus,
}

action!(
    /// `RemoteStopTransaction` (central system -> charge point).
    RemoteStopTransaction = "RemoteStopTransaction", RemoteStopTransactionRequest => RemoteStopTransactionResponse, InboundAction
);

// ---- Reset -----------------------------------------------------------------

wire_enum! {
    /// `ResetStatus`.
    ResetStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
    }
}

/// `Reset.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetRequest {
    /// Wire name `type`.
    #[serde(rename = "type")]
    pub kind: ResetType,
}

/// `Reset.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetResponse {
    pub status: ResetStatus,
}

action!(
    /// `Reset` (central system -> charge point).
    Reset = "Reset", ResetRequest => ResetResponse, InboundAction
);

// ---- StartTransaction ------------------------------------------------------

/// `StartTransaction.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTransactionRequest {
    pub connector_id: ConnectorId,
    pub id_tag: IdTag,
    /// Meter reading in Wh at the start of the transaction.
    pub meter_start: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reservation_id: Option<ReservationId>,
    pub timestamp: DateTime<Utc>,
}

/// `StartTransaction.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTransactionResponse {
    pub id_tag_info: IdTagInfo,
    pub transaction_id: i32,
}

action!(
    /// `StartTransaction` (charge point -> central system).
    StartTransaction = "StartTransaction", StartTransactionRequest => StartTransactionResponse, OutboundAction
);

// ---- StatusNotification ----------------------------------------------------

/// `StatusNotification.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusNotificationRequest {
    pub connector_id: ConnectorId,
    pub error_code: ChargePointErrorCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<String>,
    pub status: ConnectorStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_error_code: Option<String>,
}

/// `StatusNotification.conf`: no fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusNotificationResponse {}

action!(
    /// `StatusNotification` (charge point -> central system).
    StatusNotification = "StatusNotification", StatusNotificationRequest => StatusNotificationResponse, OutboundAction
);

// ---- StopTransaction -------------------------------------------------------

/// `StopTransaction.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopTransactionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_tag: Option<IdTag>,
    /// Meter reading in Wh at the end of the transaction.
    pub meter_stop: i64,
    pub timestamp: DateTime<Utc>,
    pub transaction_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<StopReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_data: Option<Vec<MeterValue>>,
}

/// `StopTransaction.conf`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopTransactionResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_tag_info: Option<IdTagInfo>,
}

action!(
    /// `StopTransaction` (charge point -> central system).
    StopTransaction = "StopTransaction", StopTransactionRequest => StopTransactionResponse, OutboundAction
);

// ---- UnlockConnector -------------------------------------------------------

wire_enum! {
    /// `UnlockStatus`.
    UnlockStatus {
        Unlocked => "Unlocked",
        UnlockFailed => "UnlockFailed",
        NotSupported => "NotSupported",
    }
}

/// `UnlockConnector.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnlockConnectorRequest {
    pub connector_id: ConnectorId,
}

/// `UnlockConnector.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnlockConnectorResponse {
    pub status: UnlockStatus,
}

action!(
    /// `UnlockConnector` (central system -> charge point).
    UnlockConnector = "UnlockConnector", UnlockConnectorRequest => UnlockConnectorResponse, InboundAction
);
