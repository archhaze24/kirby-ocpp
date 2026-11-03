//! OCPP 1.6 Security Whitepaper extensions (edition 3): the wire types only.
//!
//! Eleven actions, none of which the station or the mock CSMS acts on yet:
//!
//! | direction | actions |
//! |---|---|
//! | charge point -> CSMS | `SignCertificate`, `LogStatusNotification`, `SecurityEventNotification`, `SignedFirmwareStatusNotification` |
//! | CSMS -> charge point | `CertificateSigned`, `DeleteCertificate`, `ExtendedTriggerMessage`, `GetInstalledCertificateIds`, `GetLog`, `InstallCertificate`, `SignedUpdateFirmware` |
//!
//! Enum type names drop the whitepaper's `EnumType` suffix (as the rest of this crate does).
//! The CSMS' answer to `ExtendedTriggerMessage` reuses [`TriggerMessageStatus`]
//! (`TriggerMessageStatusEnumType` has the same values). Length limits live in the bundled
//! schemas; the constants below only repeat them for code that builds such values.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::remote_trigger::TriggerMessageStatus;
use crate::ids::ConnectorId;
use crate::ocpp::types::wire_enum;

/// Maximum length of `csr` and of an installed `certificate` (PEM).
pub const MAX_CERTIFICATE_LEN: usize = 5500;
/// Maximum length of `certificateChain` of `CertificateSigned`.
pub const MAX_CERTIFICATE_CHAIN_LEN: usize = 10_000;
/// Maximum length of `type` of `SecurityEventNotification`.
pub const MAX_SECURITY_EVENT_TYPE_LEN: usize = 50;
/// Maximum length of `techInfo` of `SecurityEventNotification`.
pub const MAX_SECURITY_EVENT_TECH_INFO_LEN: usize = 255;
/// Maximum length of `remoteLocation` / `location` (log and signed firmware).
pub const MAX_LOCATION_LEN: usize = 512;
/// Maximum length of the firmware `signature`.
pub const MAX_FIRMWARE_SIGNATURE_LEN: usize = 800;

wire_enum! {
    /// `CertificateUseEnumType`.
    CertificateUse {
        CentralSystemRootCertificate => "CentralSystemRootCertificate",
        ManufacturerRootCertificate => "ManufacturerRootCertificate",
    }
}

wire_enum! {
    /// `HashAlgorithmEnumType`.
    HashAlgorithm {
        Sha256 => "SHA256",
        Sha384 => "SHA384",
        Sha512 => "SHA512",
    }
}

wire_enum! {
    /// `MessageTriggerEnumType` of `ExtendedTriggerMessage.req` (not the 1.6 [`MessageTrigger`](crate::ocpp::types::MessageTrigger)).
    ExtendedMessageTrigger {
        BootNotification => "BootNotification",
        LogStatusNotification => "LogStatusNotification",
        FirmwareStatusNotification => "FirmwareStatusNotification",
        Heartbeat => "Heartbeat",
        MeterValues => "MeterValues",
        SignChargePointCertificate => "SignChargePointCertificate",
        StatusNotification => "StatusNotification",
    }
}

wire_enum! {
    /// `LogEnumType`.
    LogType {
        DiagnosticsLog => "DiagnosticsLog",
        SecurityLog => "SecurityLog",
    }
}

wire_enum! {
    /// `UploadLogStatusEnumType`.
    UploadLogStatus {
        BadMessage => "BadMessage",
        Idle => "Idle",
        NotSupportedOperation => "NotSupportedOperation",
        PermissionDenied => "PermissionDenied",
        Uploaded => "Uploaded",
        UploadFailure => "UploadFailure",
        Uploading => "Uploading",
    }
}

wire_enum! {
    /// `GenericStatusEnumType` (`SignCertificate.conf`).
    GenericStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
    }
}

wire_enum! {
    /// `CertificateSignedStatusEnumType` (`CertificateSigned.conf`; the whitepaper text calls it `CertificateStatusEnumType`).
    CertificateSignedStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
    }
}

wire_enum! {
    /// `DeleteCertificateStatusEnumType`.
    DeleteCertificateStatus {
        Accepted => "Accepted",
        Failed => "Failed",
        NotFound => "NotFound",
    }
}

wire_enum! {
    /// `GetInstalledCertificateStatusEnumType`.
    GetInstalledCertificateStatus {
        Accepted => "Accepted",
        NotFound => "NotFound",
    }
}

wire_enum! {
    /// `InstallCertificateStatusEnumType`.
    InstallCertificateStatus {
        Accepted => "Accepted",
        Failed => "Failed",
        Rejected => "Rejected",
    }
}

wire_enum! {
    /// `LogStatusEnumType` (`GetLog.conf`).
    LogStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
        AcceptedCanceled => "AcceptedCanceled",
    }
}

wire_enum! {
    /// `FirmwareStatusEnumType` of `SignedFirmwareStatusNotification.req`.
    SignedFirmwareStatus {
        Downloaded => "Downloaded",
        DownloadFailed => "DownloadFailed",
        Downloading => "Downloading",
        DownloadScheduled => "DownloadScheduled",
        DownloadPaused => "DownloadPaused",
        Idle => "Idle",
        InstallationFailed => "InstallationFailed",
        Installing => "Installing",
        Installed => "Installed",
        InstallRebooting => "InstallRebooting",
        InstallScheduled => "InstallScheduled",
        InstallVerificationFailed => "InstallVerificationFailed",
        InvalidSignature => "InvalidSignature",
        SignatureVerified => "SignatureVerified",
    }
}

wire_enum! {
    /// `UpdateFirmwareStatusEnumType` (`SignedUpdateFirmware.conf`).
    UpdateFirmwareStatus {
        Accepted => "Accepted",
        Rejected => "Rejected",
        AcceptedCanceled => "AcceptedCanceled",
        InvalidCertificate => "InvalidCertificate",
        RevokedCertificate => "RevokedCertificate",
    }
}

/// `CertificateHashDataType`: identifies an installed certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CertificateHashData {
    pub hash_algorithm: HashAlgorithm,
    /// Hash of the issuer's distinguished name (hex, at most 128 characters).
    pub issuer_name_hash: String,
    /// Hash of the issuer's public key (hex, at most 128 characters).
    pub issuer_key_hash: String,
    /// Serial number of the certificate (hex, at most 40 characters).
    pub serial_number: String,
}

/// `LogParametersType`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogParameters {
    /// Upload target (at most 512 characters).
    pub remote_location: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_timestamp: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_timestamp: Option<DateTime<Utc>>,
}

/// `FirmwareType`: the signed firmware image description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedFirmware {
    /// Download source (at most 512 characters).
    pub location: String,
    pub retrieve_date_time: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_date_time: Option<DateTime<Utc>>,
    /// PEM certificate the firmware signature verifies against (at most 5500 characters).
    pub signing_certificate: String,
    /// Base64 signature of the firmware (at most 800 characters).
    pub signature: String,
}

/// `SignCertificate.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignCertificateRequest {
    /// PKCS#10 certificate signing request, PEM (at most 5500 characters).
    pub csr: String,
}

/// `SignCertificate.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignCertificateResponse {
    pub status: GenericStatus,
}

action!(
    /// `SignCertificate` (charge point -> central system).
    SignCertificate = "SignCertificate", SignCertificateRequest => SignCertificateResponse, OutboundAction
);

/// `CertificateSigned.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CertificateSignedRequest {
    /// PEM certificate chain, leaf first (at most 10000 characters).
    pub certificate_chain: String,
}

/// `CertificateSigned.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CertificateSignedResponse {
    pub status: CertificateSignedStatus,
}

action!(
    /// `CertificateSigned` (central system -> charge point).
    CertificateSigned = "CertificateSigned", CertificateSignedRequest => CertificateSignedResponse, InboundAction
);

/// `InstallCertificate.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallCertificateRequest {
    pub certificate_type: CertificateUse,
    /// PEM certificate (at most 5500 characters).
    pub certificate: String,
}

/// `InstallCertificate.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallCertificateResponse {
    pub status: InstallCertificateStatus,
}

action!(
    /// `InstallCertificate` (central system -> charge point).
    InstallCertificate = "InstallCertificate", InstallCertificateRequest => InstallCertificateResponse, InboundAction
);

/// `DeleteCertificate.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteCertificateRequest {
    pub certificate_hash_data: CertificateHashData,
}

/// `DeleteCertificate.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteCertificateResponse {
    pub status: DeleteCertificateStatus,
}

action!(
    /// `DeleteCertificate` (central system -> charge point).
    DeleteCertificate = "DeleteCertificate", DeleteCertificateRequest => DeleteCertificateResponse, InboundAction
);

/// `GetInstalledCertificateIds.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetInstalledCertificateIdsRequest {
    pub certificate_type: CertificateUse,
}

/// `GetInstalledCertificateIds.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetInstalledCertificateIdsResponse {
    pub status: GetInstalledCertificateStatus,
    /// At least one entry when present (the schema demands `minItems: 1`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_hash_data: Option<Vec<CertificateHashData>>,
}

action!(
    /// `GetInstalledCertificateIds` (central system -> charge point).
    GetInstalledCertificateIds = "GetInstalledCertificateIds",
    GetInstalledCertificateIdsRequest => GetInstalledCertificateIdsResponse, InboundAction
);

/// `GetLog.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLogRequest {
    pub log_type: LogType,
    pub request_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_interval: Option<i32>,
    pub log: LogParameters,
}

/// `GetLog.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetLogResponse {
    pub status: LogStatus,
    /// Name of the file that will be uploaded (at most 255 characters).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

action!(
    /// `GetLog` (central system -> charge point).
    GetLog = "GetLog", GetLogRequest => GetLogResponse, InboundAction
);

/// `LogStatusNotification.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogStatusNotificationRequest {
    pub status: UploadLogStatus,
    /// The `requestId` of the `GetLog` being reported on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<i32>,
}

/// `LogStatusNotification.conf`: no fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogStatusNotificationResponse {}

action!(
    /// `LogStatusNotification` (charge point -> central system).
    LogStatusNotification = "LogStatusNotification",
    LogStatusNotificationRequest => LogStatusNotificationResponse, OutboundAction
);

/// `SecurityEventNotification.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityEventNotificationRequest {
    /// Security event name from the whitepaper's list (at most 50 characters).
    #[serde(rename = "type")]
    pub event_type: String,
    pub timestamp: DateTime<Utc>,
    /// Free text details (at most 255 characters).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tech_info: Option<String>,
}

/// `SecurityEventNotification.conf`: no fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityEventNotificationResponse {}

action!(
    /// `SecurityEventNotification` (charge point -> central system).
    SecurityEventNotification = "SecurityEventNotification",
    SecurityEventNotificationRequest => SecurityEventNotificationResponse, OutboundAction
);

/// `ExtendedTriggerMessage.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtendedTriggerMessageRequest {
    pub requested_message: ExtendedMessageTrigger,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_id: Option<ConnectorId>,
}

/// `ExtendedTriggerMessage.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtendedTriggerMessageResponse {
    pub status: TriggerMessageStatus,
}

action!(
    /// `ExtendedTriggerMessage` (central system -> charge point).
    ExtendedTriggerMessage = "ExtendedTriggerMessage",
    ExtendedTriggerMessageRequest => ExtendedTriggerMessageResponse, InboundAction
);

/// `SignedUpdateFirmware.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedUpdateFirmwareRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_interval: Option<i32>,
    pub request_id: i32,
    pub firmware: SignedFirmware,
}

/// `SignedUpdateFirmware.conf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedUpdateFirmwareResponse {
    pub status: UpdateFirmwareStatus,
}

action!(
    /// `SignedUpdateFirmware` (central system -> charge point).
    SignedUpdateFirmware = "SignedUpdateFirmware",
    SignedUpdateFirmwareRequest => SignedUpdateFirmwareResponse, InboundAction
);

/// `SignedFirmwareStatusNotification.req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedFirmwareStatusNotificationRequest {
    pub status: SignedFirmwareStatus,
    /// The `requestId` of the `SignedUpdateFirmware` being reported on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<i32>,
}

/// `SignedFirmwareStatusNotification.conf`: no fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedFirmwareStatusNotificationResponse {}

action!(
    /// `SignedFirmwareStatusNotification` (charge point -> central system).
    SignedFirmwareStatusNotification = "SignedFirmwareStatusNotification",
    SignedFirmwareStatusNotificationRequest => SignedFirmwareStatusNotificationResponse, OutboundAction
);

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::ocpp::action::Action;
    use crate::ocpp::frame::ErrorCode;
    use crate::ocpp::schema::{PayloadKind, SchemaRegistry};

    fn registry() -> SchemaRegistry {
        SchemaRegistry::new().expect("bundled schemas compile")
    }

    fn hash() -> Value {
        json!({
            "hashAlgorithm": "SHA256",
            "issuerNameHash": "ab12",
            "issuerKeyHash": "cd34",
            "serialNumber": "01ff"
        })
    }

    fn firmware() -> Value {
        json!({
            "location": "https://example.com/fw.bin",
            "retrieveDateTime": "2026-05-06T07:08:09Z",
            "signingCertificate": "PEM",
            "signature": "c2ln"
        })
    }

    /// `(action, valid request, valid response)` for every security action.
    fn samples() -> Vec<(&'static str, Value, Value)> {
        vec![
            (
                SignCertificate::NAME,
                json!({"csr": "PEM"}),
                json!({"status": "Accepted"}),
            ),
            (
                CertificateSigned::NAME,
                json!({"certificateChain": "PEM"}),
                json!({"status": "Rejected"}),
            ),
            (
                InstallCertificate::NAME,
                json!({"certificateType": "CentralSystemRootCertificate", "certificate": "PEM"}),
                json!({"status": "Failed"}),
            ),
            (
                DeleteCertificate::NAME,
                json!({"certificateHashData": hash()}),
                json!({"status": "NotFound"}),
            ),
            (
                GetInstalledCertificateIds::NAME,
                json!({"certificateType": "ManufacturerRootCertificate"}),
                json!({"status": "Accepted", "certificateHashData": [hash()]}),
            ),
            (
                GetLog::NAME,
                json!({
                    "logType": "DiagnosticsLog",
                    "requestId": 1,
                    "log": {"remoteLocation": "https://example.com/logs"}
                }),
                json!({"status": "Accepted", "filename": "log.txt"}),
            ),
            (
                LogStatusNotification::NAME,
                json!({"status": "Uploaded"}),
                json!({}),
            ),
            (
                SecurityEventNotification::NAME,
                json!({"type": "StartupOfTheDevice", "timestamp": "2026-05-06T07:08:09Z"}),
                json!({}),
            ),
            (
                ExtendedTriggerMessage::NAME,
                json!({"requestedMessage": "LogStatusNotification"}),
                json!({"status": "Accepted"}),
            ),
            (
                SignedUpdateFirmware::NAME,
                json!({"requestId": 3, "firmware": firmware()}),
                json!({"status": "InvalidCertificate"}),
            ),
            (
                SignedFirmwareStatusNotification::NAME,
                json!({"status": "SignatureVerified"}),
                json!({}),
            ),
        ]
    }

    #[test]
    fn eleven_actions_with_valid_samples_validate() {
        let registry = registry();
        let samples = samples();
        assert_eq!(samples.len(), 11);
        for (action, request, response) in samples {
            registry
                .validate_request(action, &request)
                .unwrap_or_else(|e| panic!("{e}"));
            registry
                .validate_response(action, &response)
                .unwrap_or_else(|e| panic!("{e}"));
        }
    }

    #[test]
    fn every_schema_compiles_and_is_embedded() {
        let registry = registry();
        for (action, _, _) in samples() {
            for kind in [PayloadKind::Request, PayloadKind::Response] {
                assert!(
                    registry.schema_source(action, kind).is_some(),
                    "{action} {kind}"
                );
            }
        }
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "a flat table of mutations")]
    fn mutated_requests_are_rejected() {
        let registry = registry();
        let long = |n: usize| Value::String("x".repeat(n));
        let cases: Vec<(&str, Value, ErrorCode)> = vec![
            (
                "SignCertificate",
                json!({}),
                ErrorCode::OccurenceConstraintViolation,
            ),
            (
                "SignCertificate",
                json!({"csr": long(5501)}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "SignCertificate",
                json!({"csr": 1}),
                ErrorCode::TypeConstraintViolation,
            ),
            (
                "SignCertificate",
                json!({"csr": "a", "extra": 1}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "CertificateSigned",
                json!({"certificateChain": long(10_001)}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "InstallCertificate",
                json!({"certificateType": "V2GRootCertificate", "certificate": "a"}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "InstallCertificate",
                json!({"certificateType": "CentralSystemRootCertificate"}),
                ErrorCode::OccurenceConstraintViolation,
            ),
            (
                "DeleteCertificate",
                json!({"certificateHashData": {"hashAlgorithm": "MD5", "issuerNameHash": "a", "issuerKeyHash": "b", "serialNumber": "c"}}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "DeleteCertificate",
                json!({"certificateHashData": {"hashAlgorithm": "SHA256", "issuerNameHash": "a", "issuerKeyHash": "b", "serialNumber": long(41)}}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "DeleteCertificate",
                json!({"certificateHashData": {"hashAlgorithm": "SHA256", "issuerNameHash": "a", "issuerKeyHash": "b"}}),
                ErrorCode::OccurenceConstraintViolation,
            ),
            (
                "GetInstalledCertificateIds",
                json!({}),
                ErrorCode::OccurenceConstraintViolation,
            ),
            (
                "GetLog",
                json!({"logType": "Other", "requestId": 1, "log": {"remoteLocation": "a"}}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "GetLog",
                json!({"logType": "SecurityLog", "requestId": 1, "log": {}}),
                ErrorCode::OccurenceConstraintViolation,
            ),
            (
                "GetLog",
                json!({"logType": "SecurityLog", "requestId": 1, "log": {"remoteLocation": long(513)}}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "GetLog",
                json!({"logType": "SecurityLog", "requestId": 1, "log": {"remoteLocation": "a", "oldestTimestamp": "yesterday"}}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "LogStatusNotification",
                json!({"status": "Done"}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "SecurityEventNotification",
                json!({"type": long(51), "timestamp": "2026-05-06T07:08:09Z"}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "SecurityEventNotification",
                json!({"type": "a", "timestamp": "2026-05-06T07:08:09Z", "techInfo": long(256)}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "SecurityEventNotification",
                json!({"type": "a"}),
                ErrorCode::OccurenceConstraintViolation,
            ),
            (
                "ExtendedTriggerMessage",
                json!({"requestedMessage": "DiagnosticsStatusNotification"}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "SignedUpdateFirmware",
                json!({"requestId": 1, "firmware": {"location": "a", "retrieveDateTime": "2026-05-06T07:08:09Z", "signingCertificate": "c"}}),
                ErrorCode::OccurenceConstraintViolation,
            ),
            (
                "SignedUpdateFirmware",
                json!({"requestId": 1, "firmware": {"location": "a", "retrieveDateTime": "2026-05-06T07:08:09Z", "signingCertificate": "c", "signature": long(801)}}),
                ErrorCode::PropertyConstraintViolation,
            ),
            (
                "SignedUpdateFirmware",
                json!({"firmware": firmware()}),
                ErrorCode::OccurenceConstraintViolation,
            ),
            (
                "SignedFirmwareStatusNotification",
                json!({"status": "Downloading", "requestId": "1"}),
                ErrorCode::TypeConstraintViolation,
            ),
            (
                "SignedFirmwareStatusNotification",
                json!({"status": "Installing "}),
                ErrorCode::PropertyConstraintViolation,
            ),
        ];
        for (action, payload, code) in cases {
            let violation = registry
                .validate_request(action, &payload)
                .expect_err(&format!("{action} {payload}"));
            assert_eq!(violation.error_code(), code, "{action} {payload}");
        }
    }

    #[test]
    fn mutated_responses_are_rejected() {
        let registry = registry();
        for (action, response) in [
            ("SignCertificate", json!({"status": "Failed"})),
            ("CertificateSigned", json!({})),
            ("InstallCertificate", json!({"status": "NotFound"})),
            ("DeleteCertificate", json!({"status": "Rejected"})),
            (
                "GetInstalledCertificateIds",
                json!({"status": "Accepted", "certificateHashData": []}),
            ),
            (
                "GetLog",
                json!({"status": "Accepted", "filename": "x".repeat(256)}),
            ),
            ("ExtendedTriggerMessage", json!({"status": "Busy"})),
            ("SignedUpdateFirmware", json!({"status": "Failed"})),
            ("LogStatusNotification", json!({"status": "Uploaded"})),
        ] {
            assert!(
                registry.validate_response(action, &response).is_err(),
                "{action} {response}"
            );
        }
    }

    fn assert_wire<T>(all: &[T], wire: &[&str])
    where
        T: Copy + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + PartialEq,
    {
        assert_eq!(all.len(), wire.len());
        for (value, wire) in all.iter().zip(wire) {
            assert_eq!(serde_json::to_value(value).unwrap(), json!(wire));
            assert_eq!(serde_json::from_value::<T>(json!(wire)).unwrap(), *value);
        }
    }

    #[test]
    fn enum_wire_strings_are_exact() {
        assert_wire(
            CertificateUse::ALL,
            &[
                "CentralSystemRootCertificate",
                "ManufacturerRootCertificate",
            ],
        );
        assert_wire(HashAlgorithm::ALL, &["SHA256", "SHA384", "SHA512"]);
        assert_wire(
            ExtendedMessageTrigger::ALL,
            &[
                "BootNotification",
                "LogStatusNotification",
                "FirmwareStatusNotification",
                "Heartbeat",
                "MeterValues",
                "SignChargePointCertificate",
                "StatusNotification",
            ],
        );
        assert_wire(LogType::ALL, &["DiagnosticsLog", "SecurityLog"]);
        assert_wire(
            UploadLogStatus::ALL,
            &[
                "BadMessage",
                "Idle",
                "NotSupportedOperation",
                "PermissionDenied",
                "Uploaded",
                "UploadFailure",
                "Uploading",
            ],
        );
        assert_wire(GenericStatus::ALL, &["Accepted", "Rejected"]);
        assert_wire(CertificateSignedStatus::ALL, &["Accepted", "Rejected"]);
        assert_wire(
            DeleteCertificateStatus::ALL,
            &["Accepted", "Failed", "NotFound"],
        );
        assert_wire(
            GetInstalledCertificateStatus::ALL,
            &["Accepted", "NotFound"],
        );
        assert_wire(
            InstallCertificateStatus::ALL,
            &["Accepted", "Failed", "Rejected"],
        );
        assert_wire(
            LogStatus::ALL,
            &["Accepted", "Rejected", "AcceptedCanceled"],
        );
        assert_wire(
            SignedFirmwareStatus::ALL,
            &[
                "Downloaded",
                "DownloadFailed",
                "Downloading",
                "DownloadScheduled",
                "DownloadPaused",
                "Idle",
                "InstallationFailed",
                "Installing",
                "Installed",
                "InstallRebooting",
                "InstallScheduled",
                "InstallVerificationFailed",
                "InvalidSignature",
                "SignatureVerified",
            ],
        );
        assert_wire(
            UpdateFirmwareStatus::ALL,
            &[
                "Accepted",
                "Rejected",
                "AcceptedCanceled",
                "InvalidCertificate",
                "RevokedCertificate",
            ],
        );
    }

    #[test]
    fn typed_samples_roundtrip_their_json() {
        fn roundtrip<A: Action>(request: &Value, response: &Value) {
            let req: A::Req = serde_json::from_value(request.clone()).unwrap();
            assert_eq!(
                &serde_json::to_value(req).unwrap(),
                request,
                "{} req",
                A::NAME
            );
            let res: A::Res = serde_json::from_value(response.clone()).unwrap();
            assert_eq!(
                &serde_json::to_value(res).unwrap(),
                response,
                "{} res",
                A::NAME
            );
        }
        for (action, request, response) in samples() {
            match action {
                "SignCertificate" => roundtrip::<SignCertificate>(&request, &response),
                "CertificateSigned" => roundtrip::<CertificateSigned>(&request, &response),
                "InstallCertificate" => roundtrip::<InstallCertificate>(&request, &response),
                "DeleteCertificate" => roundtrip::<DeleteCertificate>(&request, &response),
                "GetInstalledCertificateIds" => {
                    roundtrip::<GetInstalledCertificateIds>(&request, &response);
                }
                "GetLog" => roundtrip::<GetLog>(&request, &response),
                "LogStatusNotification" => roundtrip::<LogStatusNotification>(&request, &response),
                "SecurityEventNotification" => {
                    roundtrip::<SecurityEventNotification>(&request, &response);
                }
                "ExtendedTriggerMessage" => {
                    roundtrip::<ExtendedTriggerMessage>(&request, &response);
                }
                "SignedUpdateFirmware" => roundtrip::<SignedUpdateFirmware>(&request, &response),
                "SignedFirmwareStatusNotification" => {
                    roundtrip::<SignedFirmwareStatusNotification>(&request, &response);
                }
                other => panic!("no roundtrip for {other}"),
            }
        }
    }
}
