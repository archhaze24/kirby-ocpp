use serde::{Deserialize, Serialize};

use crate::ocpp::v2::types::{
    CustomData, IdToken, IdTokenInfo, OcspRequestData, StatusInfo, helpers, payload, wire_enum,
};

wire_enum! {
    /// Result of the ISO 15118 contract certificate check
    /// (`AuthorizeCertificateStatusEnumType`).
    AuthorizeCertificateStatus {
        Accepted = "Accepted",
        SignatureError = "SignatureError",
        CertificateExpired = "CertificateExpired",
        CertificateRevoked = "CertificateRevoked",
        NoCertificateAvailable = "NoCertificateAvailable",
        CertChainError = "CertChainError",
        ContractCancelled = "ContractCancelled",
    }
}

wire_enum! {
    /// Result of `ClearCache` (`ClearCacheStatusEnumType`).
    ClearCacheStatus {
        Accepted = "Accepted",
        Rejected = "Rejected",
    }
}

payload! {
    /// `Authorize.req`.
    AuthorizeRequest {
        pub id_token: IdToken,
        /// PEM contract certificate, max 5500 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub certificate: Option<String>,
        /// One to four entries; absent or non-empty.
        #[serde(default, skip_serializing_if = "helpers::omit_empty")]
        pub iso15118_certificate_hash_data: Option<Vec<OcspRequestData>>,
    }
}

payload! {
    /// `Authorize.conf`.
    AuthorizeResponse {
        pub id_token_info: IdTokenInfo,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub certificate_status: Option<AuthorizeCertificateStatus>,
    }
}

payload! {
    /// `ClearCache.req`: no fields besides `customData`.
    ClearCacheRequest {}
}

payload! {
    /// `ClearCache.conf`.
    ClearCacheResponse {
        pub status: ClearCacheStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub status_info: Option<StatusInfo>,
    }
}

action! {
    /// `Authorize` (station -> CSMS).
    Authorize = "Authorize", AuthorizeRequest => AuthorizeResponse, OutboundAction
}
action! {
    /// `ClearCache` (CSMS -> station).
    ClearCache = "ClearCache", ClearCacheRequest => ClearCacheResponse, InboundAction
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::ocpp::v2::test_support::{check_action, check_enum};

    fn cd() -> Value {
        json!({"vendorId": "acme", "x": {"y": [1, null]}})
    }

    fn id_token() -> Value {
        json!({
            "customData": cd(),
            "additionalInfo": [
                {"customData": cd(), "additionalIdToken": "extra", "type": "vendorKind"}
            ],
            "idToken": "ABC123",
            "type": "eMAID"
        })
    }

    #[test]
    fn enums_match_schema() {
        check_enum::<AuthorizeCertificateStatus>(
            "AuthorizeResponse.json",
            "AuthorizeCertificateStatusEnumType",
        );
        check_enum::<ClearCacheStatus>("ClearCacheResponse.json", "ClearCacheStatusEnumType");
    }

    #[test]
    fn authorize() {
        let req = json!({
            "customData": cd(),
            "idToken": id_token(),
            "certificate": "-----BEGIN CERTIFICATE-----",
            "iso15118CertificateHashData": [{
                "customData": cd(),
                "hashAlgorithm": "SHA384",
                "issuerNameHash": "aa",
                "issuerKeyHash": "bb",
                "serialNumber": "01",
                "responderURL": "http://ocsp.example"
            }]
        });
        let res = json!({
            "customData": cd(),
            "idTokenInfo": {
                "customData": cd(),
                "status": "NotAtThisTime",
                "cacheExpiryDateTime": "2026-05-06T07:08:09.250Z",
                "chargingPriority": -9,
                "language1": "en",
                "evseId": [1, 2],
                "groupIdToken": id_token(),
                "language2": "de",
                "personalMessage": {
                    "customData": cd(),
                    "format": "UTF8",
                    "language": "en",
                    "content": "Hello"
                }
            },
            "certificateStatus": "CertChainError"
        });
        check_action::<Authorize>(&req, &res);
        check_action::<Authorize>(
            &json!({"idToken": {"idToken": "T", "type": "NoAuthorization"}}),
            &json!({"idTokenInfo": {"status": "Accepted"}}),
        );
    }

    #[test]
    fn clear_cache() {
        check_action::<ClearCache>(
            &json!({"customData": cd()}),
            &json!({
                "customData": cd(),
                "status": "Rejected",
                "statusInfo": {"customData": cd(), "reasonCode": "NoCache", "additionalInfo": "x"}
            }),
        );
        check_action::<ClearCache>(&json!({}), &json!({"status": "Accepted"}));
    }
}
