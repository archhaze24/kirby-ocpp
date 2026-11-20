use serde::{Deserialize, Serialize};

use crate::ocpp::v2::types::{
    CustomData, IdToken, IdTokenInfo, StatusInfo, helpers, payload, wire_enum,
};

wire_enum! {
    /// Kind of a `SendLocalList` update (`UpdateEnumType`).
    UpdateKind {
        Differential = "Differential",
        Full = "Full",
    }
}

wire_enum! {
    /// Result of `SendLocalList` (`SendLocalListStatusEnumType`).
    SendLocalListStatus {
        Accepted = "Accepted",
        Failed = "Failed",
        VersionMismatch = "VersionMismatch",
    }
}

payload! {
    /// One entry of a local authorization list (`AuthorizationData`). In a differential
    /// update an entry without `idTokenInfo` removes the token.
    AuthorizationData {
        pub id_token: IdToken,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub id_token_info: Option<IdTokenInfo>,
    }
}

payload! {
    /// `SendLocalList.req`.
    SendLocalListRequest {
        /// Absent or non-empty.
        #[serde(default, skip_serializing_if = "helpers::omit_empty")]
        pub local_authorization_list: Option<Vec<AuthorizationData>>,
        pub version_number: i32,
        pub update_type: UpdateKind,
    }
}

payload! {
    /// `SendLocalList.conf`.
    SendLocalListResponse {
        pub status: SendLocalListStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

payload! {
    /// `GetLocalListVersion.req`: no fields besides `customData`.
    GetLocalListVersionRequest {}
}

payload! {
    /// `GetLocalListVersion.conf`.
    GetLocalListVersionResponse {
        /// `0` when no list is installed.
        pub version_number: i32,
    }
}

action! {
    /// `SendLocalList` (CSMS -> station).
    SendLocalList = "SendLocalList", SendLocalListRequest => SendLocalListResponse, InboundAction
}
action! {
    /// `GetLocalListVersion` (CSMS -> station).
    GetLocalListVersion = "GetLocalListVersion", GetLocalListVersionRequest => GetLocalListVersionResponse, InboundAction
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
        check_enum::<UpdateKind>("SendLocalListRequest.json", "UpdateEnumType");
        check_enum::<SendLocalListStatus>(
            "SendLocalListResponse.json",
            "SendLocalListStatusEnumType",
        );
    }

    #[test]
    fn send_local_list() {
        let req = json!({
            "customData": cd(),
            "localAuthorizationList": [
                {
                    "customData": cd(),
                    "idToken": {"customData": cd(), "idToken": "A", "type": "ISO14443"},
                    "idTokenInfo": {
                        "customData": cd(),
                        "status": "Accepted",
                        "cacheExpiryDateTime": "2026-05-06T07:08:09.250Z",
                        "chargingPriority": 3,
                        "language1": "en"
                    }
                },
                {"idToken": {"idToken": "B", "type": "KeyCode"}}
            ],
            "versionNumber": 7,
            "updateType": "Differential"
        });
        let res = json!({
            "customData": cd(),
            "status": "VersionMismatch",
            "statusInfo": {"customData": cd(), "reasonCode": "Stale", "additionalInfo": "x"}
        });
        check_action::<SendLocalList>(&req, &res);
        check_action::<SendLocalList>(
            &json!({"versionNumber": 1, "updateType": "Full"}),
            &json!({"status": "Accepted"}),
        );
    }

    #[test]
    fn get_local_list_version() {
        check_action::<GetLocalListVersion>(
            &json!({"customData": cd()}),
            &json!({"customData": cd(), "versionNumber": 0}),
        );
        check_action::<GetLocalListVersion>(&json!({}), &json!({"versionNumber": 12}));
    }
}
