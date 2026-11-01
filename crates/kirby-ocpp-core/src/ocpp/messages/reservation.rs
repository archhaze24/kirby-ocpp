//! Reservation profile.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{ConnectorId, IdTag, ReservationId};
use crate::ocpp::types::wire_enum;

wire_enum! {
    /// `ReservationStatus` of `ReserveNow.conf`.
    ReservationStatus {
        Accepted => "Accepted",
        Faulted => "Faulted",
        Occupied => "Occupied",
        Rejected => "Rejected",
        Unavailable => "Unavailable",
    }
}

/// `ReserveNow.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReserveNowRequest {
    pub connector_id: ConnectorId,
    pub expiry_date: DateTime<Utc>,
    pub id_tag: IdTag,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id_tag: Option<IdTag>,
    pub reservation_id: ReservationId,
}

/// `ReserveNow.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReserveNowResponse {
    pub status: ReservationStatus,
}

action!(
    /// `ReserveNow` (central system -> charge point).
    ReserveNow = "ReserveNow", ReserveNowRequest => ReserveNowResponse, InboundAction
);

wire_enum! {
    /// `CancelReservationStatus`.
    CancelReservationStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
    }
}

/// `CancelReservation.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelReservationRequest {
    pub reservation_id: ReservationId,
}

/// `CancelReservation.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelReservationResponse {
    pub status: CancelReservationStatus,
}

action!(
    /// `CancelReservation` (central system -> charge point).
    CancelReservation = "CancelReservation", CancelReservationRequest => CancelReservationResponse, InboundAction
);
