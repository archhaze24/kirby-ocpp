//! What the UI can send to a station, and the executor that sends it. [`Command`] is plain
//! data (the model builds it from a form); [`run`] performs it against the engine.

use kirby_csms_core::{CsmsError, CsmsHandle, Frame};
use kirby_ocpp_core::ids::{ConnectorId, IdTag};
use kirby_ocpp_core::ocpp::messages::{
    CertificateHashData, CertificateUse, ChargingProfile, ClearChargingProfileRequest,
    ExtendedMessageTrigger, GetDiagnosticsRequest, GetLogRequest, ReserveNowRequest,
    SendLocalListRequest, SignedUpdateFirmwareRequest, UpdateFirmwareRequest,
};
use kirby_ocpp_core::ocpp::types::{
    AvailabilityType, ChargingRateUnitType, MessageTrigger, ResetType,
};
use serde::Serialize;
use serde_json::{Value, json};

/// A CSMS-originated CALL.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    RemoteStart {
        id_tag: String,
        connector: Option<u32>,
    },
    RemoteStop {
        transaction_id: i32,
    },
    Reset(ResetType),
    ChangeConfiguration {
        key: String,
        value: String,
    },
    GetConfiguration {
        keys: Vec<String>,
    },
    TriggerMessage {
        message: MessageTrigger,
        connector: Option<u32>,
    },
    UnlockConnector {
        connector: u32,
    },
    ChangeAvailability {
        connector: u32,
        kind: AvailabilityType,
    },
    ClearCache,
    SetChargingProfile {
        connector: u32,
        profile: Box<ChargingProfile>,
    },
    DataTransfer {
        vendor_id: String,
        message_id: Option<String>,
        data: Option<String>,
    },
    GetLog(Box<GetLogRequest>),
    InstallCertificate {
        certificate_type: CertificateUse,
        certificate: String,
    },
    DeleteCertificate(CertificateHashData),
    GetInstalledCertificateIds(CertificateUse),
    ExtendedTriggerMessage {
        message: ExtendedMessageTrigger,
        connector: Option<u32>,
    },
    SignedUpdateFirmware(Box<SignedUpdateFirmwareRequest>),
    CertificateSigned {
        chain: String,
    },
    ClearChargingProfile(ClearChargingProfileRequest),
    GetCompositeSchedule {
        connector: u32,
        duration: i32,
        unit: Option<ChargingRateUnitType>,
    },
    ReserveNow(Box<ReserveNowRequest>),
    UpdateFirmware(Box<UpdateFirmwareRequest>),
    GetDiagnostics(Box<GetDiagnosticsRequest>),
    SendLocalList(Box<SendLocalListRequest>),
    /// Any action with a free-form payload, sent as is.
    Raw {
        action: String,
        payload: Value,
    },
}

impl Command {
    /// The OCPP action name.
    pub fn action(&self) -> &str {
        match self {
            Self::RemoteStart { .. } => "RemoteStartTransaction",
            Self::RemoteStop { .. } => "RemoteStopTransaction",
            Self::Reset(_) => "Reset",
            Self::ChangeConfiguration { .. } => "ChangeConfiguration",
            Self::GetConfiguration { .. } => "GetConfiguration",
            Self::TriggerMessage { .. } => "TriggerMessage",
            Self::UnlockConnector { .. } => "UnlockConnector",
            Self::ChangeAvailability { .. } => "ChangeAvailability",
            Self::ClearCache => "ClearCache",
            Self::SetChargingProfile { .. } => "SetChargingProfile",
            Self::DataTransfer { .. } => "DataTransfer",
            Self::GetLog(_) => "GetLog",
            Self::InstallCertificate { .. } => "InstallCertificate",
            Self::DeleteCertificate(_) => "DeleteCertificate",
            Self::GetInstalledCertificateIds(_) => "GetInstalledCertificateIds",
            Self::ExtendedTriggerMessage { .. } => "ExtendedTriggerMessage",
            Self::SignedUpdateFirmware(_) => "SignedUpdateFirmware",
            Self::CertificateSigned { .. } => "CertificateSigned",
            Self::ClearChargingProfile(_) => "ClearChargingProfile",
            Self::GetCompositeSchedule { .. } => "GetCompositeSchedule",
            Self::ReserveNow(_) => "ReserveNow",
            Self::UpdateFirmware(_) => "UpdateFirmware",
            Self::GetDiagnostics(_) => "GetDiagnostics",
            Self::SendLocalList(_) => "SendLocalList",
            Self::Raw { action, .. } => action,
        }
    }

    /// The request payload as it goes on the wire.
    pub fn payload(&self) -> Value {
        match self {
            Self::RemoteStart { id_tag, connector } => {
                let mut v = json!({ "idTag": id_tag });
                if let Some(c) = connector {
                    v["connectorId"] = json!(c);
                }
                v
            }
            Self::RemoteStop { transaction_id } => json!({ "transactionId": transaction_id }),
            Self::Reset(kind) => json!({ "type": kind.as_str() }),
            Self::ChangeConfiguration { key, value } => json!({ "key": key, "value": value }),
            Self::GetConfiguration { keys } if keys.is_empty() => json!({}),
            Self::GetConfiguration { keys } => json!({ "key": keys }),
            Self::TriggerMessage { message, connector } => {
                let mut v = json!({ "requestedMessage": message.as_str() });
                if let Some(c) = connector {
                    v["connectorId"] = json!(c);
                }
                v
            }
            Self::UnlockConnector { connector } => json!({ "connectorId": connector }),
            Self::ChangeAvailability { connector, kind } => {
                json!({ "connectorId": connector, "type": kind.as_str() })
            }
            Self::ClearCache => json!({}),
            Self::SetChargingProfile { connector, profile } => {
                json!({ "connectorId": connector, "csChargingProfiles": profile })
            }
            Self::DataTransfer {
                vendor_id,
                message_id,
                data,
            } => {
                let mut v = json!({ "vendorId": vendor_id });
                if let Some(m) = message_id {
                    v["messageId"] = json!(m);
                }
                if let Some(d) = data {
                    v["data"] = json!(d);
                }
                v
            }
            Self::GetLog(req) => json!(req),
            Self::InstallCertificate {
                certificate_type,
                certificate,
            } => json!({
                "certificateType": certificate_type.as_str(),
                "certificate": certificate,
            }),
            Self::DeleteCertificate(hash) => json!({ "certificateHashData": hash }),
            Self::GetInstalledCertificateIds(kind) => {
                json!({ "certificateType": kind.as_str() })
            }
            Self::ExtendedTriggerMessage { message, connector } => {
                let mut v = json!({ "requestedMessage": message.as_str() });
                if let Some(c) = connector {
                    v["connectorId"] = json!(c);
                }
                v
            }
            Self::SignedUpdateFirmware(req) => json!(req),
            Self::CertificateSigned { chain } => json!({ "certificateChain": chain }),
            Self::ClearChargingProfile(req) => json!(req),
            Self::GetCompositeSchedule {
                connector,
                duration,
                unit,
            } => {
                let mut v = json!({ "connectorId": connector, "duration": duration });
                if let Some(u) = unit {
                    v["chargingRateUnit"] = json!(u.as_str());
                }
                v
            }
            Self::ReserveNow(req) => json!(req),
            Self::UpdateFirmware(req) => json!(req),
            Self::GetDiagnostics(req) => json!(req),
            Self::SendLocalList(req) => json!(req),
            Self::Raw { payload, .. } => payload.clone(),
        }
    }

    /// Short description for notices and the confirmation.
    pub fn label(&self) -> String {
        match self {
            Self::Reset(kind) => format!("Reset ({kind})"),
            Self::RemoteStop { transaction_id } => {
                format!("RemoteStopTransaction ({transaction_id})")
            }
            Self::ClearChargingProfile(req) if clears_everything(req) => {
                "ClearChargingProfile (ALL charging profiles)".to_owned()
            }
            other => other.action().to_owned(),
        }
    }

    /// Commands that disrupt the station or wipe its state ask first.
    pub fn needs_confirm(&self) -> bool {
        match self {
            Self::Reset(_) => true,
            // No filter set: the station drops every charging profile.
            Self::ClearChargingProfile(req) => clears_everything(req),
            _ => false,
        }
    }

    /// Whether a successful answer is worth a popup instead of just a notice.
    pub fn answer_is_content(&self, response: &Value) -> bool {
        match self {
            Self::GetConfiguration { .. }
            | Self::GetCompositeSchedule { .. }
            | Self::GetInstalledCertificateIds(_)
            | Self::Raw { .. } => true,
            Self::DataTransfer { .. } => response.get("data").is_some(),
            _ => false,
        }
    }
}

/// A `ClearChargingProfile` with no filter drops every profile of the station.
fn clears_everything(req: &ClearChargingProfileRequest) -> bool {
    req.id.is_none()
        && req.connector_id.is_none()
        && req.charging_profile_purpose.is_none()
        && req.stack_level.is_none()
}

/// A failed command, already worded for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdError(pub String);

/// The answer to a command, or why there was none.
#[derive(Debug, Clone, PartialEq)]
pub struct CmdOutcome {
    pub result: Result<Value, CmdError>,
}

fn describe(error: &CsmsError) -> CmdError {
    CmdError(match error {
        CsmsError::CallError {
            code, description, ..
        } => format!("CALLERROR {code}: {description}"),
        CsmsError::Timeout { timeout, .. } => {
            format!("no answer within {}s", timeout.as_secs_f32().round())
        }
        CsmsError::NotConnected { .. } => "station is not connected".to_owned(),
        CsmsError::UnknownStation { .. } => "unknown station".to_owned(),
        CsmsError::ConnectionClosed { .. } => "connection closed before the answer".to_owned(),
        CsmsError::InvalidRequest { reason, .. } => format!("invalid request: {reason}"),
        CsmsError::InvalidResponse { reason, .. } => format!("invalid answer: {reason}"),
        other => other.to_string(),
    })
}

fn finish<T: Serialize>(result: Result<T, CsmsError>) -> Result<Value, CmdError> {
    match result {
        Ok(res) => serde_json::to_value(res).map_err(|e| CmdError(e.to_string())),
        Err(e) => Err(describe(&e)),
    }
}

/// Sends `command` to `station` and waits for the answer (the engine's call timeout applies).
pub async fn run(handle: &CsmsHandle, station: &str, command: &Command) -> CmdOutcome {
    let bad_tag = |e: kirby_ocpp_core::ids::IdTagError| CmdError(e.to_string());
    let result = match command {
        Command::RemoteStart { id_tag, connector } => match IdTag::new(id_tag.clone()) {
            Ok(tag) => finish(
                handle
                    .remote_start_transaction(station, tag, connector.map(ConnectorId::new), None)
                    .await,
            ),
            Err(e) => Err(bad_tag(e)),
        },
        Command::RemoteStop { transaction_id } => finish(
            handle
                .remote_stop_transaction(station, *transaction_id)
                .await,
        ),
        Command::Reset(kind) => finish(handle.reset(station, *kind).await),
        Command::ChangeConfiguration { key, value } => finish(
            handle
                .change_configuration(station, key.clone(), value.clone())
                .await,
        ),
        Command::GetConfiguration { keys } => {
            finish(handle.get_configuration(station, keys.clone()).await)
        }
        Command::TriggerMessage { message, connector } => finish(
            handle
                .trigger_message(station, *message, connector.map(ConnectorId::new))
                .await,
        ),
        Command::UnlockConnector { connector } => finish(
            handle
                .unlock_connector(station, ConnectorId::new(*connector))
                .await,
        ),
        Command::ChangeAvailability { connector, kind } => finish(
            handle
                .change_availability(station, ConnectorId::new(*connector), *kind)
                .await,
        ),
        Command::ClearCache => finish(handle.clear_cache(station).await),
        Command::SetChargingProfile { connector, profile } => finish(
            handle
                .set_charging_profile(station, ConnectorId::new(*connector), (**profile).clone())
                .await,
        ),
        Command::DataTransfer {
            vendor_id,
            message_id,
            data,
        } => finish(
            handle
                .data_transfer(station, vendor_id.clone(), message_id.clone(), data.clone())
                .await,
        ),
        Command::GetLog(req) => finish(handle.get_log(station, (**req).clone()).await),
        Command::InstallCertificate {
            certificate_type,
            certificate,
        } => finish(
            handle
                .install_certificate(station, *certificate_type, certificate.clone())
                .await,
        ),
        Command::DeleteCertificate(hash) => {
            finish(handle.delete_certificate(station, hash.clone()).await)
        }
        Command::GetInstalledCertificateIds(kind) => {
            finish(handle.get_installed_certificate_ids(station, *kind).await)
        }
        Command::ExtendedTriggerMessage { message, connector } => finish(
            handle
                .extended_trigger_message(station, *message, connector.map(ConnectorId::new))
                .await,
        ),
        Command::SignedUpdateFirmware(req) => finish(
            handle
                .signed_update_firmware(station, (**req).clone())
                .await,
        ),
        Command::CertificateSigned { chain } => {
            finish(handle.certificate_signed(station, chain.clone()).await)
        }
        Command::ClearChargingProfile(req) => {
            finish(handle.clear_charging_profile(station, req.clone()).await)
        }
        Command::GetCompositeSchedule {
            connector,
            duration,
            unit,
        } => finish(
            handle
                .get_composite_schedule(station, ConnectorId::new(*connector), *duration, *unit)
                .await,
        ),
        Command::ReserveNow(req) => finish(handle.reserve_now(station, (**req).clone()).await),
        Command::UpdateFirmware(req) => {
            finish(handle.update_firmware(station, (**req).clone()).await)
        }
        Command::GetDiagnostics(req) => {
            finish(handle.get_diagnostics(station, (**req).clone()).await)
        }
        Command::SendLocalList(req) => {
            finish(handle.send_local_list(station, (**req).clone()).await)
        }
        Command::Raw { action, payload } => match handle
            .call(station, action, payload.clone())
            .await
        {
            Ok(Frame::CallResult { payload, .. }) => Ok(payload),
            Ok(Frame::CallError {
                code, description, ..
            }) => Err(CmdError(format!("CALLERROR {code}: {description}"))),
            Ok(Frame::Call { .. }) => Err(CmdError("the station answered with a CALL".to_owned())),
            Err(e) => Err(describe(&e)),
        },
    };
    CmdOutcome { result }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_match_the_wire_format() {
        let start = Command::RemoteStart {
            id_tag: "ABC".into(),
            connector: Some(2),
        };
        assert_eq!(start.action(), "RemoteStartTransaction");
        assert_eq!(start.payload(), json!({"idTag": "ABC", "connectorId": 2}));
        let start = Command::RemoteStart {
            id_tag: "ABC".into(),
            connector: None,
        };
        assert_eq!(start.payload(), json!({"idTag": "ABC"}));
        assert_eq!(
            Command::Reset(ResetType::Hard).payload(),
            json!({"type": "Hard"})
        );
        assert_eq!(
            Command::GetConfiguration { keys: vec![] }.payload(),
            json!({})
        );
        assert_eq!(
            Command::GetConfiguration {
                keys: vec!["A".into()]
            }
            .payload(),
            json!({"key": ["A"]})
        );
        assert_eq!(
            Command::TriggerMessage {
                message: MessageTrigger::Heartbeat,
                connector: None
            }
            .payload(),
            json!({"requestedMessage": "Heartbeat"})
        );
        assert_eq!(
            Command::DataTransfer {
                vendor_id: "V".into(),
                message_id: Some("m".into()),
                data: None
            }
            .payload(),
            json!({"vendorId": "V", "messageId": "m"})
        );
        assert!(Command::Reset(ResetType::Soft).needs_confirm());
        assert!(!Command::ClearCache.needs_confirm());
    }
}
