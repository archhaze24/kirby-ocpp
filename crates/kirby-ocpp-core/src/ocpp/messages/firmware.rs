//! Firmware management and diagnostics.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ocpp::types::{DiagnosticsStatus, FirmwareStatus};

/// `GetDiagnostics.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDiagnosticsRequest {
    /// Upload target (a URI; FTP/FTPS/HTTP/HTTPS).
    pub location: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_interval: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_time: Option<DateTime<Utc>>,
}

/// `GetDiagnostics.conf`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDiagnosticsResponse {
    /// Name of the file that will be uploaded; absent if there is nothing to upload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

action!(
    /// `GetDiagnostics` (central system -> charge point).
    GetDiagnostics = "GetDiagnostics", GetDiagnosticsRequest => GetDiagnosticsResponse, InboundAction
);

/// `DiagnosticsStatusNotification.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsStatusNotificationRequest {
    pub status: DiagnosticsStatus,
}

/// `DiagnosticsStatusNotification.conf`: no fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsStatusNotificationResponse {}

action!(
    /// `DiagnosticsStatusNotification` (charge point -> central system).
    DiagnosticsStatusNotification = "DiagnosticsStatusNotification",
    DiagnosticsStatusNotificationRequest => DiagnosticsStatusNotificationResponse, OutboundAction
);

/// `FirmwareStatusNotification.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareStatusNotificationRequest {
    pub status: FirmwareStatus,
}

/// `FirmwareStatusNotification.conf`: no fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareStatusNotificationResponse {}

action!(
    /// `FirmwareStatusNotification` (charge point -> central system).
    FirmwareStatusNotification = "FirmwareStatusNotification",
    FirmwareStatusNotificationRequest => FirmwareStatusNotificationResponse, OutboundAction
);

/// `UpdateFirmware.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFirmwareRequest {
    /// Download source (a URI).
    pub location: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries: Option<i32>,
    pub retrieve_date: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_interval: Option<i32>,
}

/// `UpdateFirmware.conf`: no fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFirmwareResponse {}

action!(
    /// `UpdateFirmware` (central system -> charge point).
    UpdateFirmware = "UpdateFirmware", UpdateFirmwareRequest => UpdateFirmwareResponse, InboundAction
);
