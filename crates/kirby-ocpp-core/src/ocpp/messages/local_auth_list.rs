//! Local authorization list management.

use serde::{Deserialize, Serialize};

use super::common::AuthorizationData;
use crate::ocpp::types::{UpdateType, wire_enum};

/// `GetLocalListVersion.req`: no fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLocalListVersionRequest {}

/// `GetLocalListVersion.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLocalListVersionResponse {
    /// `0` = no list, `-1` = local lists not supported.
    pub list_version: i32,
}

action!(
    /// `GetLocalListVersion` (central system -> charge point).
    GetLocalListVersion = "GetLocalListVersion", GetLocalListVersionRequest => GetLocalListVersionResponse, InboundAction
);

wire_enum! {
    /// `UpdateStatus` of `SendLocalList.conf`.
    UpdateStatus {
        Accepted => "Accepted",
        Failed => "Failed",
        NotSupported => "NotSupported",
        VersionMismatch => "VersionMismatch",
    }
}

/// `SendLocalList.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendLocalListRequest {
    pub list_version: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_authorization_list: Option<Vec<AuthorizationData>>,
    pub update_type: UpdateType,
}

/// `SendLocalList.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendLocalListResponse {
    pub status: UpdateStatus,
}

action!(
    /// `SendLocalList` (central system -> charge point).
    SendLocalList = "SendLocalList", SendLocalListRequest => SendLocalListResponse, InboundAction
);
