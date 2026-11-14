//! The embedded OCPP 2.0.1 schema table (see `schemas/ocpp201/README.md`).

use super::schema::SchemaTable;

/// Expands to the table of embedded schema files: `(action, request json, response json)`.
macro_rules! embedded_schemas {
    ($($action:literal),+ $(,)?) => {
        &[$((
            $action,
            include_str!(concat!("../../schemas/ocpp201/", $action, "Request.json")),
            include_str!(concat!("../../schemas/ocpp201/", $action, "Response.json")),
        )),+]
    };
}

/// Every OCPP 2.0.1 action with its embedded request and response schema source, in
/// alphabetical order.
pub(super) const EMBEDDED_V201: SchemaTable = embedded_schemas![
    "Authorize",
    "BootNotification",
    "CancelReservation",
    "CertificateSigned",
    "ChangeAvailability",
    "ClearCache",
    "ClearChargingProfile",
    "ClearDisplayMessage",
    "ClearVariableMonitoring",
    "ClearedChargingLimit",
    "CostUpdated",
    "CustomerInformation",
    "DataTransfer",
    "DeleteCertificate",
    "FirmwareStatusNotification",
    "Get15118EVCertificate",
    "GetBaseReport",
    "GetCertificateStatus",
    "GetChargingProfiles",
    "GetCompositeSchedule",
    "GetDisplayMessages",
    "GetInstalledCertificateIds",
    "GetLocalListVersion",
    "GetLog",
    "GetMonitoringReport",
    "GetReport",
    "GetTransactionStatus",
    "GetVariables",
    "Heartbeat",
    "InstallCertificate",
    "LogStatusNotification",
    "MeterValues",
    "NotifyChargingLimit",
    "NotifyCustomerInformation",
    "NotifyDisplayMessages",
    "NotifyEVChargingNeeds",
    "NotifyEVChargingSchedule",
    "NotifyEvent",
    "NotifyMonitoringReport",
    "NotifyReport",
    "PublishFirmware",
    "PublishFirmwareStatusNotification",
    "ReportChargingProfiles",
    "RequestStartTransaction",
    "RequestStopTransaction",
    "ReservationStatusUpdate",
    "ReserveNow",
    "Reset",
    "SecurityEventNotification",
    "SendLocalList",
    "SetChargingProfile",
    "SetDisplayMessage",
    "SetMonitoringBase",
    "SetMonitoringLevel",
    "SetNetworkProfile",
    "SetVariableMonitoring",
    "SetVariables",
    "SignCertificate",
    "StatusNotification",
    "TransactionEvent",
    "TriggerMessage",
    "UnlockConnector",
    "UnpublishFirmware",
    "UpdateFirmware",
];
