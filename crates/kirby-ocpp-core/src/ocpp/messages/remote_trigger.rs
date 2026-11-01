//! Remote trigger profile.

use serde::{Deserialize, Serialize};

use crate::ids::ConnectorId;
use crate::ocpp::types::{MessageTrigger, wire_enum};

wire_enum! {
    /// `TriggerMessageStatus`.
    TriggerMessageStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
        NotImplemented => "NotImplemented",
    }
}

/// `TriggerMessage.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerMessageRequest {
    pub requested_message: MessageTrigger,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_id: Option<ConnectorId>,
}

/// `TriggerMessage.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerMessageResponse {
    pub status: TriggerMessageStatus,
}

action!(
    /// `TriggerMessage` (central system -> charge point).
    TriggerMessage = "TriggerMessage", TriggerMessageRequest => TriggerMessageResponse, InboundAction
);
