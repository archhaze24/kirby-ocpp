//! Typed CSMS commands: [`CsmsHandle::call_typed`] and thin wrappers for the 19 commands of OCPP 1.6
//! and the 7 of the Security Whitepaper a central system sends. Requests and responses are the typed messages of `kirby-ocpp-core`.

use kirby_ocpp_core::ids::{ConnectorId, IdTag, ReservationId};
use kirby_ocpp_core::ocpp::messages::{
    CancelReservation, CancelReservationRequest, CancelReservationResponse, CertificateHashData,
    CertificateSigned, CertificateSignedRequest, CertificateSignedResponse, CertificateUse,
    ChangeAvailability, ChangeAvailabilityRequest, ChangeAvailabilityResponse, ChangeConfiguration,
    ChangeConfigurationRequest, ChangeConfigurationResponse, ChargingProfile, ClearCache,
    ClearCacheRequest, ClearCacheResponse, ClearChargingProfile, ClearChargingProfileRequest,
    ClearChargingProfileResponse, DataTransfer, DataTransferRequest, DataTransferResponse,
    DeleteCertificate, DeleteCertificateRequest, DeleteCertificateResponse, ExtendedMessageTrigger,
    ExtendedTriggerMessage, ExtendedTriggerMessageRequest, ExtendedTriggerMessageResponse,
    GetCompositeSchedule, GetCompositeScheduleRequest, GetCompositeScheduleResponse,
    GetConfiguration, GetConfigurationRequest, GetConfigurationResponse, GetDiagnostics,
    GetDiagnosticsRequest, GetDiagnosticsResponse, GetInstalledCertificateIds,
    GetInstalledCertificateIdsRequest, GetInstalledCertificateIdsResponse, GetLocalListVersion,
    GetLocalListVersionRequest, GetLocalListVersionResponse, GetLog, GetLogRequest, GetLogResponse,
    InstallCertificate, InstallCertificateRequest, InstallCertificateResponse,
    RemoteStartTransaction, RemoteStartTransactionRequest, RemoteStartTransactionResponse,
    RemoteStopTransaction, RemoteStopTransactionRequest, RemoteStopTransactionResponse, ReserveNow,
    ReserveNowRequest, ReserveNowResponse, Reset, ResetRequest, ResetResponse, SendLocalList,
    SendLocalListRequest, SendLocalListResponse, SetChargingProfile, SetChargingProfileRequest,
    SetChargingProfileResponse, SignedUpdateFirmware, SignedUpdateFirmwareRequest,
    SignedUpdateFirmwareResponse, TriggerMessage, TriggerMessageRequest, TriggerMessageResponse,
    UnlockConnector, UnlockConnectorRequest, UnlockConnectorResponse, UpdateFirmware,
    UpdateFirmwareRequest, UpdateFirmwareResponse,
};
use kirby_ocpp_core::ocpp::types::{
    AvailabilityType, ChargingRateUnitType, MessageTrigger, ResetType,
};
use kirby_ocpp_core::ocpp::{Frame, InboundAction};

use crate::error::CsmsError;
use crate::server::CsmsHandle;

impl CsmsHandle {
    /// Sends a typed command and returns the typed answer.
    ///
    /// `A` is any action the central system may send (`InboundAction`, including
    /// `DataTransfer`). The request is serialised and validated against the embedded OCPP 1.6J
    /// JSON schema before anything is sent ([`CsmsError::InvalidRequest`]); the CALLRESULT is
    /// validated and deserialised too ([`CsmsError::InvalidResponse`]). A CALLERROR answer
    /// becomes [`CsmsError::CallError`]. Queueing, timeout and the other errors are those of
    /// [`CsmsHandle::call`].
    pub async fn call_typed<A: InboundAction>(
        &self,
        station: &str,
        req: A::Req,
    ) -> Result<A::Res, CsmsError> {
        let invalid = |reason: String| CsmsError::InvalidRequest {
            action: A::NAME.to_owned(),
            reason,
        };
        let payload = serde_json::to_value(&req).map_err(|e| invalid(e.to_string()))?;
        // The schemas of the protocol the station negotiated (1.6 when unknown); none known
        // for a protocol means no validation.
        let protocol = self.station_protocol(station);
        if let Some(schemas) = self.schemas_for(protocol) {
            schemas
                .validate_request(A::NAME, &payload)
                .map_err(|e| invalid(e.errors.join("; ")))?;
        }
        let frame = self.call(station, A::NAME, payload).await?;
        let invalid_response = |reason: String| CsmsError::InvalidResponse {
            station: station.to_owned(),
            action: A::NAME.to_owned(),
            reason,
        };
        match frame {
            Frame::CallResult { payload, .. } => {
                if let Some(schemas) = self.schemas_for(protocol) {
                    schemas
                        .validate_response(A::NAME, &payload)
                        .map_err(|e| invalid_response(e.errors.join("; ")))?;
                }
                serde_json::from_value(payload).map_err(|e| invalid_response(e.to_string()))
            }
            Frame::CallError {
                code, description, ..
            } => Err(CsmsError::CallError {
                station: station.to_owned(),
                action: A::NAME.to_owned(),
                code,
                description,
            }),
            _ => Err(invalid_response("not a reply".to_owned())),
        }
    }

    /// `RemoteStartTransaction`. `connector: None` lets the station pick one.
    pub async fn remote_start_transaction(
        &self,
        station: &str,
        id_tag: IdTag,
        connector: Option<ConnectorId>,
        profile: Option<ChargingProfile>,
    ) -> Result<RemoteStartTransactionResponse, CsmsError> {
        let req = RemoteStartTransactionRequest {
            connector_id: connector,
            id_tag,
            charging_profile: profile,
        };
        self.call_typed::<RemoteStartTransaction>(station, req)
            .await
    }

    /// `RemoteStopTransaction`.
    pub async fn remote_stop_transaction(
        &self,
        station: &str,
        transaction_id: i32,
    ) -> Result<RemoteStopTransactionResponse, CsmsError> {
        let req = RemoteStopTransactionRequest { transaction_id };
        self.call_typed::<RemoteStopTransaction>(station, req).await
    }

    /// `Reset`.
    pub async fn reset(&self, station: &str, kind: ResetType) -> Result<ResetResponse, CsmsError> {
        self.call_typed::<Reset>(station, ResetRequest { kind })
            .await
    }

    /// `ChangeConfiguration`.
    pub async fn change_configuration(
        &self,
        station: &str,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<ChangeConfigurationResponse, CsmsError> {
        let req = ChangeConfigurationRequest {
            key: key.into(),
            value: value.into(),
        };
        self.call_typed::<ChangeConfiguration>(station, req).await
    }

    /// `GetConfiguration`. An empty `keys` asks for the whole configuration.
    pub async fn get_configuration(
        &self,
        station: &str,
        keys: Vec<String>,
    ) -> Result<GetConfigurationResponse, CsmsError> {
        let req = GetConfigurationRequest {
            key: (!keys.is_empty()).then_some(keys),
        };
        self.call_typed::<GetConfiguration>(station, req).await
    }

    /// `TriggerMessage`.
    pub async fn trigger_message(
        &self,
        station: &str,
        requested: MessageTrigger,
        connector: Option<ConnectorId>,
    ) -> Result<TriggerMessageResponse, CsmsError> {
        let req = TriggerMessageRequest {
            requested_message: requested,
            connector_id: connector,
        };
        self.call_typed::<TriggerMessage>(station, req).await
    }

    /// `UnlockConnector`.
    pub async fn unlock_connector(
        &self,
        station: &str,
        connector: ConnectorId,
    ) -> Result<UnlockConnectorResponse, CsmsError> {
        let req = UnlockConnectorRequest {
            connector_id: connector,
        };
        self.call_typed::<UnlockConnector>(station, req).await
    }

    /// `ChangeAvailability`.
    pub async fn change_availability(
        &self,
        station: &str,
        connector: ConnectorId,
        kind: AvailabilityType,
    ) -> Result<ChangeAvailabilityResponse, CsmsError> {
        let req = ChangeAvailabilityRequest {
            connector_id: connector,
            kind,
        };
        self.call_typed::<ChangeAvailability>(station, req).await
    }

    /// `ClearCache`.
    pub async fn clear_cache(&self, station: &str) -> Result<ClearCacheResponse, CsmsError> {
        self.call_typed::<ClearCache>(station, ClearCacheRequest {})
            .await
    }

    /// `SetChargingProfile`.
    pub async fn set_charging_profile(
        &self,
        station: &str,
        connector: ConnectorId,
        profile: ChargingProfile,
    ) -> Result<SetChargingProfileResponse, CsmsError> {
        let req = SetChargingProfileRequest {
            connector_id: connector,
            cs_charging_profiles: profile,
        };
        self.call_typed::<SetChargingProfile>(station, req).await
    }

    /// `ClearChargingProfile` (all filters are in the request).
    pub async fn clear_charging_profile(
        &self,
        station: &str,
        req: ClearChargingProfileRequest,
    ) -> Result<ClearChargingProfileResponse, CsmsError> {
        self.call_typed::<ClearChargingProfile>(station, req).await
    }

    /// `GetCompositeSchedule`.
    pub async fn get_composite_schedule(
        &self,
        station: &str,
        connector: ConnectorId,
        duration: i32,
        unit: Option<ChargingRateUnitType>,
    ) -> Result<GetCompositeScheduleResponse, CsmsError> {
        let req = GetCompositeScheduleRequest {
            connector_id: connector,
            duration,
            charging_rate_unit: unit,
        };
        self.call_typed::<GetCompositeSchedule>(station, req).await
    }

    /// `DataTransfer` (CSMS to station).
    pub async fn data_transfer(
        &self,
        station: &str,
        vendor_id: impl Into<String>,
        message_id: Option<String>,
        data: Option<String>,
    ) -> Result<DataTransferResponse, CsmsError> {
        let req = DataTransferRequest {
            vendor_id: vendor_id.into(),
            message_id,
            data,
        };
        self.call_typed::<DataTransfer>(station, req).await
    }

    /// `SendLocalList`.
    pub async fn send_local_list(
        &self,
        station: &str,
        req: SendLocalListRequest,
    ) -> Result<SendLocalListResponse, CsmsError> {
        self.call_typed::<SendLocalList>(station, req).await
    }

    /// `GetLocalListVersion`.
    pub async fn get_local_list_version(
        &self,
        station: &str,
    ) -> Result<GetLocalListVersionResponse, CsmsError> {
        self.call_typed::<GetLocalListVersion>(station, GetLocalListVersionRequest {})
            .await
    }

    /// `ReserveNow`.
    pub async fn reserve_now(
        &self,
        station: &str,
        req: ReserveNowRequest,
    ) -> Result<ReserveNowResponse, CsmsError> {
        self.call_typed::<ReserveNow>(station, req).await
    }

    /// `CancelReservation`.
    pub async fn cancel_reservation(
        &self,
        station: &str,
        reservation_id: ReservationId,
    ) -> Result<CancelReservationResponse, CsmsError> {
        let req = CancelReservationRequest { reservation_id };
        self.call_typed::<CancelReservation>(station, req).await
    }

    /// `UpdateFirmware`.
    pub async fn update_firmware(
        &self,
        station: &str,
        req: UpdateFirmwareRequest,
    ) -> Result<UpdateFirmwareResponse, CsmsError> {
        self.call_typed::<UpdateFirmware>(station, req).await
    }

    /// `GetDiagnostics`.
    pub async fn get_diagnostics(
        &self,
        station: &str,
        req: GetDiagnosticsRequest,
    ) -> Result<GetDiagnosticsResponse, CsmsError> {
        self.call_typed::<GetDiagnostics>(station, req).await
    }

    /// `CertificateSigned` (Security Whitepaper): hands the station its signed certificate chain
    /// (PEM, leaf first).
    pub async fn certificate_signed(
        &self,
        station: &str,
        certificate_chain: impl Into<String>,
    ) -> Result<CertificateSignedResponse, CsmsError> {
        let req = CertificateSignedRequest {
            certificate_chain: certificate_chain.into(),
        };
        self.call_typed::<CertificateSigned>(station, req).await
    }

    /// `InstallCertificate`: installs a root certificate (PEM).
    pub async fn install_certificate(
        &self,
        station: &str,
        certificate_type: CertificateUse,
        certificate: impl Into<String>,
    ) -> Result<InstallCertificateResponse, CsmsError> {
        let req = InstallCertificateRequest {
            certificate_type,
            certificate: certificate.into(),
        };
        self.call_typed::<InstallCertificate>(station, req).await
    }

    /// `DeleteCertificate`.
    pub async fn delete_certificate(
        &self,
        station: &str,
        certificate_hash_data: CertificateHashData,
    ) -> Result<DeleteCertificateResponse, CsmsError> {
        let req = DeleteCertificateRequest {
            certificate_hash_data,
        };
        self.call_typed::<DeleteCertificate>(station, req).await
    }

    /// `GetInstalledCertificateIds`.
    pub async fn get_installed_certificate_ids(
        &self,
        station: &str,
        certificate_type: CertificateUse,
    ) -> Result<GetInstalledCertificateIdsResponse, CsmsError> {
        let req = GetInstalledCertificateIdsRequest { certificate_type };
        self.call_typed::<GetInstalledCertificateIds>(station, req)
            .await
    }

    /// `ExtendedTriggerMessage`.
    pub async fn extended_trigger_message(
        &self,
        station: &str,
        requested: ExtendedMessageTrigger,
        connector: Option<ConnectorId>,
    ) -> Result<ExtendedTriggerMessageResponse, CsmsError> {
        let req = ExtendedTriggerMessageRequest {
            requested_message: requested,
            connector_id: connector,
        };
        self.call_typed::<ExtendedTriggerMessage>(station, req)
            .await
    }

    /// `GetLog`.
    pub async fn get_log(
        &self,
        station: &str,
        req: GetLogRequest,
    ) -> Result<GetLogResponse, CsmsError> {
        self.call_typed::<GetLog>(station, req).await
    }

    /// `SignedUpdateFirmware`.
    pub async fn signed_update_firmware(
        &self,
        station: &str,
        req: SignedUpdateFirmwareRequest,
    ) -> Result<SignedUpdateFirmwareResponse, CsmsError> {
        self.call_typed::<SignedUpdateFirmware>(station, req).await
    }
}
