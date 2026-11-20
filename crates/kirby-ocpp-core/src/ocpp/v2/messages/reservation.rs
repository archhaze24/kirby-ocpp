use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ocpp::v2::types::{CustomData, IdToken, StatusInfo, helpers, payload, wire_enum};

wire_enum! {
    /// Result of `ReserveNow` (`ReserveNowStatusEnumType`).
    ReserveNowStatus {
        Accepted = "Accepted",
        Faulted = "Faulted",
        Occupied = "Occupied",
        Rejected = "Rejected",
        Unavailable = "Unavailable",
    }
}

wire_enum! {
    /// Result of `CancelReservation` (`CancelReservationStatusEnumType`).
    CancelReservationStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
    }
}

wire_enum! {
    /// Why a reservation ended (`ReservationUpdateStatusEnumType`). `NoTransaction` exists in
    /// OCPP 2.1 only.
    ReservationUpdateStatus {
        Expired = "Expired",
        Removed = "Removed",
        NoTransaction = "NoTransaction",
    }
}

payload! {
    /// `ReserveNow.req`.
    ReserveNowRequest {
        /// The reservation id; a known id replaces that reservation (H01.FR.02).
        pub id: i32,
        #[serde(with = "helpers::ts")]
        pub expiry_date_time: DateTime<Utc>,
        /// `ConnectorEnumType` in 2.0.1, a string of at most 20 characters in 2.1; the schema
        /// of the connection checks it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub connector_type: Option<String>,
        pub id_token: IdToken,
        /// Absent: any EVSE.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evse_id: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub group_id_token: Option<IdToken>,
    }
}

payload! {
    /// `ReserveNow.conf`.
    ReserveNowResponse {
        pub status: ReserveNowStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

payload! {
    /// `CancelReservation.req`.
    CancelReservationRequest {
        pub reservation_id: i32,
    }
}

payload! {
    /// `CancelReservation.conf`.
    CancelReservationResponse {
        pub status: CancelReservationStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

payload! {
    /// `ReservationStatusUpdate.req`.
    ReservationStatusUpdateRequest {
        pub reservation_id: i32,
        pub reservation_update_status: ReservationUpdateStatus,
    }
}

payload! {
    /// `ReservationStatusUpdate.conf`: no fields besides `customData`.
    ReservationStatusUpdateResponse {}
}

action! {
    /// `ReserveNow` (CSMS -> station).
    ReserveNow = "ReserveNow", ReserveNowRequest => ReserveNowResponse, InboundAction
}
action! {
    /// `CancelReservation` (CSMS -> station).
    CancelReservation = "CancelReservation", CancelReservationRequest => CancelReservationResponse, InboundAction
}
action! {
    /// `ReservationStatusUpdate` (station -> CSMS).
    ReservationStatusUpdate = "ReservationStatusUpdate", ReservationStatusUpdateRequest => ReservationStatusUpdateResponse, OutboundAction
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::ocpp::v2::test_support::{check_action, check_enum};

    fn cd() -> Value {
        json!({"vendorId": "acme", "x": {"y": [1, null]}})
    }

    #[test]
    fn enums_match_schema() {
        check_enum::<ReserveNowStatus>("ReserveNowResponse.json", "ReserveNowStatusEnumType");
        check_enum::<CancelReservationStatus>(
            "CancelReservationResponse.json",
            "CancelReservationStatusEnumType",
        );
        check_enum::<ReservationUpdateStatus>(
            "ReservationStatusUpdateRequest.json",
            "ReservationUpdateStatusEnumType",
        );
    }

    #[test]
    fn reserve_now() {
        let req = json!({
            "customData": cd(),
            "id": 7,
            "expiryDateTime": "2026-05-06T07:08:09.250Z",
            "connectorType": "cType2",
            "idToken": {"customData": cd(), "idToken": "A", "type": "ISO14443"},
            "evseId": 2,
            "groupIdToken": {"idToken": "G", "type": "Central"}
        });
        let res = json!({
            "customData": cd(),
            "status": "Occupied",
            "statusInfo": {"customData": cd(), "reasonCode": "Reserved", "additionalInfo": "x"}
        });
        check_action::<ReserveNow>(&req, &res);
        check_action::<ReserveNow>(
            &json!({
                "id": 0,
                "expiryDateTime": "2026-05-06T07:08:09.000Z",
                "idToken": {"idToken": "A", "type": "KeyCode"}
            }),
            &json!({"status": "Accepted"}),
        );
    }

    #[test]
    fn cancel_reservation() {
        check_action::<CancelReservation>(
            &json!({"customData": cd(), "reservationId": 3}),
            &json!({
                "customData": cd(),
                "status": "Rejected",
                "statusInfo": {"reasonCode": "Unknown"}
            }),
        );
        check_action::<CancelReservation>(
            &json!({"reservationId": 0}),
            &json!({"status": "Accepted"}),
        );
    }

    #[test]
    fn reservation_status_update() {
        check_action::<ReservationStatusUpdate>(
            &json!({
                "customData": cd(),
                "reservationId": 3,
                "reservationUpdateStatus": "Removed"
            }),
            &json!({"customData": cd()}),
        );
        check_action::<ReservationStatusUpdate>(
            &json!({"reservationId": 1, "reservationUpdateStatus": "Expired"}),
            &json!({}),
        );
    }
}
