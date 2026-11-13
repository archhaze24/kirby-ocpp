//! The list of all 39 OCPP 1.6J actions (28 core + 11 Security Whitepaper) with the direction they travel in.
//!
//! The table-driven tests at the bottom tie everything together: every listed
//! action has both bundled schemas, a representative request and response built
//! from the typed structs validates against them and survives a roundtrip, and
//! the direction recorded here agrees with the [`OutboundAction`] /
//! [`InboundAction`] impls of the marker types.
//!
//! [`OutboundAction`]: crate::ocpp::action::OutboundAction
//! [`InboundAction`]: crate::ocpp::action::InboundAction

/// Who may initiate (send the CALL of) an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionDirection {
    /// Only the charge point sends the CALL (`BootNotification`, ...).
    ChargePointToCentralSystem,
    /// Only the central system sends the CALL (`Reset`, ...).
    CentralSystemToChargePoint,
    /// Either side (`DataTransfer`).
    Both,
}

impl ActionDirection {
    /// `true` if this charge point may send the CALL.
    pub const fn charge_point_may_send(self) -> bool {
        matches!(self, Self::ChargePointToCentralSystem | Self::Both)
    }

    /// `true` if the central system may send the CALL.
    pub const fn central_system_may_send(self) -> bool {
        matches!(self, Self::CentralSystemToChargePoint | Self::Both)
    }
}

/// One entry of [`ACTIONS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionInfo {
    /// The action name as it appears in a CALL.
    pub name: &'static str,
    pub direction: ActionDirection,
}

const CP: ActionDirection = ActionDirection::ChargePointToCentralSystem;
const CS: ActionDirection = ActionDirection::CentralSystemToChargePoint;

const fn info(name: &'static str, direction: ActionDirection) -> ActionInfo {
    ActionInfo { name, direction }
}

/// All 39 actions, in alphabetical order: 14 charge point initiated (incl.
/// `DataTransfer`), 26 central system initiated (incl. `DataTransfer`). The 11 Security
/// Whitepaper extensions are `SignCertificate`, `LogStatusNotification`,
/// `SecurityEventNotification`, `SignedFirmwareStatusNotification` (charge point) and
/// `CertificateSigned`, `DeleteCertificate`, `ExtendedTriggerMessage`,
/// `GetInstalledCertificateIds`, `GetLog`, `InstallCertificate`, `SignedUpdateFirmware` (CSMS).
pub const ACTIONS: [ActionInfo; 39] = [
    info("Authorize", CP),
    info("BootNotification", CP),
    info("CancelReservation", CS),
    info("CertificateSigned", CS),
    info("ChangeAvailability", CS),
    info("ChangeConfiguration", CS),
    info("ClearCache", CS),
    info("ClearChargingProfile", CS),
    info("DataTransfer", ActionDirection::Both),
    info("DeleteCertificate", CS),
    info("DiagnosticsStatusNotification", CP),
    info("ExtendedTriggerMessage", CS),
    info("FirmwareStatusNotification", CP),
    info("GetCompositeSchedule", CS),
    info("GetConfiguration", CS),
    info("GetDiagnostics", CS),
    info("GetInstalledCertificateIds", CS),
    info("GetLocalListVersion", CS),
    info("GetLog", CS),
    info("Heartbeat", CP),
    info("InstallCertificate", CS),
    info("LogStatusNotification", CP),
    info("MeterValues", CP),
    info("RemoteStartTransaction", CS),
    info("RemoteStopTransaction", CS),
    info("ReserveNow", CS),
    info("Reset", CS),
    info("SecurityEventNotification", CP),
    info("SendLocalList", CS),
    info("SetChargingProfile", CS),
    info("SignCertificate", CP),
    info("SignedFirmwareStatusNotification", CP),
    info("SignedUpdateFirmware", CS),
    info("StartTransaction", CP),
    info("StatusNotification", CP),
    info("StopTransaction", CP),
    info("TriggerMessage", CS),
    info("UnlockConnector", CS),
    info("UpdateFirmware", CS),
];

/// Looks an action up by name.
pub fn find(name: &str) -> Option<&'static ActionInfo> {
    ACTIONS.iter().find(|a| a.name == name)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::{DateTime, TimeZone, Utc};
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    use super::*;
    use crate::ids::{ConnectorId, IdTag, ReservationId};
    use crate::ocpp::action::{Action, InboundAction, OutboundAction};
    use crate::ocpp::messages::*;
    use crate::ocpp::schema::{PayloadKind, SchemaRegistry};
    use crate::ocpp::types::*;

    fn at(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 6, 7, 8, second).unwrap()
    }

    fn tag(value: &str) -> IdTag {
        IdTag::new(value).unwrap()
    }

    fn connector(id: u32) -> ConnectorId {
        ConnectorId::new(id)
    }

    fn id_tag_info() -> IdTagInfo {
        IdTagInfo {
            status: AuthorizationStatus::Accepted,
            expiry_date: Some(at(10)),
            parent_id_tag: Some(tag("PARENT")),
        }
    }

    fn meter_values() -> Vec<MeterValue> {
        vec![MeterValue {
            timestamp: at(11),
            sampled_value: vec![
                SampledValue {
                    value: "1234".into(),
                    context: Some(ReadingContext::SamplePeriodic),
                    format: Some(ValueFormat::Raw),
                    measurand: Some(Measurand::EnergyActiveImportRegister),
                    phase: Some(Phase::L1N),
                    location: Some(Location::Outlet),
                    unit: Some(UnitOfMeasure::Wh),
                },
                SampledValue::new("7.2"),
            ],
        }]
    }

    fn charging_schedule() -> ChargingSchedule {
        ChargingSchedule {
            duration: Some(3600),
            start_schedule: Some(at(12)),
            charging_rate_unit: ChargingRateUnitType::A,
            charging_schedule_period: vec![
                ChargingSchedulePeriod {
                    start_period: 0,
                    limit: 16.0,
                    number_phases: Some(3),
                },
                ChargingSchedulePeriod {
                    start_period: 1800,
                    limit: 7.2,
                    number_phases: None,
                },
            ],
            min_charging_rate: Some(6.0),
        }
    }

    fn charging_profile() -> ChargingProfile {
        ChargingProfile {
            charging_profile_id: 7,
            transaction_id: Some(42),
            stack_level: 1,
            charging_profile_purpose: ChargingProfilePurposeType::TxProfile,
            charging_profile_kind: ChargingProfileKindType::Recurring,
            recurrency_kind: Some(RecurrencyKindType::Daily),
            valid_from: Some(at(13)),
            valid_to: Some(at(14)),
            charging_schedule: charging_schedule(),
        }
    }

    /// Records the direction impls the test sees and checks one action.
    #[derive(Default)]
    struct Checker {
        schemas: Option<SchemaRegistry>,
        seen: BTreeSet<&'static str>,
        outbound: BTreeSet<&'static str>,
        inbound: BTreeSet<&'static str>,
    }

    impl Checker {
        fn payload<T: Serialize + DeserializeOwned>(
            &self,
            action: &str,
            kind: PayloadKind,
            value: &T,
        ) {
            let json = serde_json::to_value(value).unwrap();
            let schemas = self.schemas.as_ref().unwrap();
            let validated = match kind {
                PayloadKind::Request => schemas.validate_request(action, &json),
                PayloadKind::Response => schemas.validate_response(action, &json),
            };
            if let Err(violation) = validated {
                panic!("{violation}\npayload: {json}");
            }
            let parsed: T = serde_json::from_value(json.clone())
                .unwrap_or_else(|e| panic!("{action} {kind}: {e}\npayload: {json}"));
            assert_eq!(
                serde_json::to_value(&parsed).unwrap(),
                json,
                "{action} {kind} roundtrip"
            );
        }

        fn case<A: Action>(&mut self, req: &A::Req, res: &A::Res) {
            self.payload(A::NAME, PayloadKind::Request, req);
            self.payload(A::NAME, PayloadKind::Response, res);
            assert!(self.seen.insert(A::NAME), "{} checked twice", A::NAME);
        }

        fn out<A: OutboundAction>(&mut self, req: &A::Req, res: &A::Res) {
            self.outbound.insert(A::NAME);
            self.case::<A>(req, res);
        }

        fn inb<A: InboundAction>(&mut self, req: &A::Req, res: &A::Res) {
            self.inbound.insert(A::NAME);
            self.case::<A>(req, res);
        }

        fn both<A: OutboundAction + InboundAction>(&mut self, req: &A::Req, res: &A::Res) {
            self.outbound.insert(A::NAME);
            self.inbound.insert(A::NAME);
            self.case::<A>(req, res);
        }
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "a flat table, one row per action")]
    fn every_action_has_both_schema_files_and_valid_typed_payloads() {
        let mut c = Checker {
            schemas: Some(SchemaRegistry::new().unwrap()),
            ..Checker::default()
        };

        c.out::<Authorize>(
            &AuthorizeRequest {
                id_tag: tag("ABC123"),
            },
            &AuthorizeResponse {
                id_tag_info: id_tag_info(),
            },
        );
        c.out::<BootNotification>(
            &BootNotificationRequest {
                charge_point_vendor: "kirby-ocpp".into(),
                charge_point_model: "TUI-16".into(),
                charge_point_serial_number: Some("CP-1".into()),
                charge_box_serial_number: Some("CB-1".into()),
                firmware_version: Some("1.0.0".into()),
                iccid: Some("8944".into()),
                imsi: Some("2501".into()),
                meter_type: Some("Fake".into()),
                meter_serial_number: Some("M-1".into()),
            },
            &BootNotificationResponse {
                status: RegistrationStatus::Accepted,
                current_time: at(1),
                interval: 300,
            },
        );
        c.inb::<CancelReservation>(
            &CancelReservationRequest {
                reservation_id: ReservationId::new(0),
            },
            &CancelReservationResponse {
                status: CancelReservationStatus::Accepted,
            },
        );
        c.inb::<ChangeAvailability>(
            &ChangeAvailabilityRequest {
                connector_id: connector(0),
                kind: AvailabilityType::Inoperative,
            },
            &ChangeAvailabilityResponse {
                status: AvailabilityStatus::Scheduled,
            },
        );
        c.inb::<ChangeConfiguration>(
            &ChangeConfigurationRequest {
                key: "HeartbeatInterval".into(),
                value: "60".into(),
            },
            &ChangeConfigurationResponse {
                status: ConfigurationStatus::RebootRequired,
            },
        );
        c.inb::<ClearCache>(
            &ClearCacheRequest {},
            &ClearCacheResponse {
                status: ClearCacheStatus::Accepted,
            },
        );
        c.inb::<ClearChargingProfile>(
            &ClearChargingProfileRequest {
                id: Some(424_242),
                connector_id: Some(connector(1)),
                charging_profile_purpose: Some(ChargingProfilePurposeType::TxDefaultProfile),
                stack_level: Some(0),
            },
            &ClearChargingProfileResponse {
                status: ClearChargingProfileStatus::Unknown,
            },
        );
        c.both::<DataTransfer>(
            &DataTransferRequest {
                vendor_id: "com.example".into(),
                message_id: Some("ping".into()),
                data: Some("{\"a\":1}".into()),
            },
            &DataTransferResponse {
                status: DataTransferStatus::UnknownMessageId,
                data: Some("pong".into()),
            },
        );
        c.out::<DiagnosticsStatusNotification>(
            &DiagnosticsStatusNotificationRequest {
                status: DiagnosticsStatus::UploadFailed,
            },
            &DiagnosticsStatusNotificationResponse {},
        );
        c.out::<FirmwareStatusNotification>(
            &FirmwareStatusNotificationRequest {
                status: FirmwareStatus::InstallationFailed,
            },
            &FirmwareStatusNotificationResponse {},
        );
        c.inb::<GetCompositeSchedule>(
            &GetCompositeScheduleRequest {
                connector_id: connector(1),
                duration: 3600,
                charging_rate_unit: Some(ChargingRateUnitType::W),
            },
            &GetCompositeScheduleResponse {
                status: GetCompositeScheduleStatus::Accepted,
                connector_id: Some(connector(1)),
                schedule_start: Some(at(15)),
                charging_schedule: Some(charging_schedule()),
            },
        );
        c.inb::<GetConfiguration>(
            &GetConfigurationRequest {
                key: Some(vec!["HeartbeatInterval".into(), "Nope".into()]),
            },
            &GetConfigurationResponse {
                configuration_key: Some(vec![
                    ConfigurationKey {
                        key: "HeartbeatInterval".into(),
                        readonly: false,
                        value: Some("60".into()),
                    },
                    ConfigurationKey {
                        key: "SupportedFeatureProfiles".into(),
                        readonly: true,
                        value: None,
                    },
                ]),
                unknown_key: Some(vec!["Nope".into()]),
            },
        );
        c.inb::<GetDiagnostics>(
            &GetDiagnosticsRequest {
                location: "ftp://example.com/diag".into(),
                retries: Some(3),
                retry_interval: Some(10),
                start_time: Some(at(1)),
                stop_time: Some(at(2)),
            },
            &GetDiagnosticsResponse {
                file_name: Some("diag.zip".into()),
            },
        );
        c.inb::<GetLocalListVersion>(
            &GetLocalListVersionRequest {},
            &GetLocalListVersionResponse { list_version: -1 },
        );
        c.out::<Heartbeat>(
            &HeartbeatRequest {},
            &HeartbeatResponse {
                current_time: at(3),
            },
        );
        c.out::<MeterValues>(
            &MeterValuesRequest {
                connector_id: connector(1),
                transaction_id: Some(42),
                meter_value: meter_values(),
            },
            &MeterValuesResponse {},
        );
        c.inb::<RemoteStartTransaction>(
            &RemoteStartTransactionRequest {
                connector_id: Some(connector(1)),
                id_tag: tag("ABC123"),
                charging_profile: Some(ChargingProfile {
                    transaction_id: None,
                    charging_profile_purpose: ChargingProfilePurposeType::TxProfile,
                    ..charging_profile()
                }),
            },
            &RemoteStartTransactionResponse {
                status: RemoteStartStopStatus::Accepted,
            },
        );
        c.inb::<RemoteStopTransaction>(
            &RemoteStopTransactionRequest { transaction_id: 42 },
            &RemoteStopTransactionResponse {
                status: RemoteStartStopStatus::Rejected,
            },
        );
        c.inb::<ReserveNow>(
            &ReserveNowRequest {
                connector_id: connector(1),
                expiry_date: at(20),
                id_tag: tag("ABC123"),
                parent_id_tag: Some(tag("PARENT")),
                reservation_id: ReservationId::new(5),
            },
            &ReserveNowResponse {
                status: ReservationStatus::Occupied,
            },
        );
        c.inb::<Reset>(
            &ResetRequest {
                kind: ResetType::Soft,
            },
            &ResetResponse {
                status: ResetStatus::Accepted,
            },
        );
        c.inb::<SendLocalList>(
            &SendLocalListRequest {
                list_version: 3,
                local_authorization_list: Some(vec![
                    AuthorizationData {
                        id_tag: tag("A"),
                        id_tag_info: Some(id_tag_info()),
                    },
                    AuthorizationData {
                        id_tag: tag("B"),
                        id_tag_info: None,
                    },
                ]),
                update_type: UpdateType::Differential,
            },
            &SendLocalListResponse {
                status: UpdateStatus::VersionMismatch,
            },
        );
        c.inb::<SetChargingProfile>(
            &SetChargingProfileRequest {
                connector_id: connector(1),
                cs_charging_profiles: charging_profile(),
            },
            &SetChargingProfileResponse {
                status: ChargingProfileStatus::NotSupported,
            },
        );
        c.out::<StartTransaction>(
            &StartTransactionRequest {
                connector_id: connector(1),
                id_tag: tag("ABC123"),
                meter_start: 0,
                reservation_id: Some(ReservationId::new(0)),
                timestamp: at(30),
            },
            &StartTransactionResponse {
                id_tag_info: id_tag_info(),
                transaction_id: 0,
            },
        );
        c.out::<StatusNotification>(
            &StatusNotificationRequest {
                connector_id: connector(1),
                error_code: ChargePointErrorCode::OtherError,
                info: Some("info".into()),
                status: ConnectorStatus::Faulted,
                timestamp: Some(at(31)),
                vendor_id: Some("vendor".into()),
                vendor_error_code: Some("E42".into()),
            },
            &StatusNotificationResponse {},
        );
        c.out::<StopTransaction>(
            &StopTransactionRequest {
                id_tag: Some(tag("ABC123")),
                meter_stop: 12_345,
                timestamp: at(32),
                transaction_id: 42,
                reason: Some(StopReason::EVDisconnected),
                transaction_data: Some(meter_values()),
            },
            &StopTransactionResponse {
                id_tag_info: Some(id_tag_info()),
            },
        );
        c.inb::<TriggerMessage>(
            &TriggerMessageRequest {
                requested_message: MessageTrigger::StatusNotification,
                connector_id: Some(connector(2)),
            },
            &TriggerMessageResponse {
                status: TriggerMessageStatus::NotImplemented,
            },
        );
        c.inb::<UnlockConnector>(
            &UnlockConnectorRequest {
                connector_id: connector(1),
            },
            &UnlockConnectorResponse {
                status: UnlockStatus::UnlockFailed,
            },
        );
        c.inb::<UpdateFirmware>(
            &UpdateFirmwareRequest {
                location: "https://example.com/fw.bin".into(),
                retries: Some(2),
                retrieve_date: at(40),
                retry_interval: Some(5),
            },
            &UpdateFirmwareResponse {},
        );

        // Security Whitepaper extensions.
        let hash = || CertificateHashData {
            hash_algorithm: HashAlgorithm::Sha256,
            issuer_name_hash: "ab12".into(),
            issuer_key_hash: "cd34".into(),
            serial_number: "01ff".into(),
        };
        c.out::<SignCertificate>(
            &SignCertificateRequest {
                csr: "-----BEGIN CERTIFICATE REQUEST-----".into(),
            },
            &SignCertificateResponse {
                status: GenericStatus::Accepted,
            },
        );
        c.out::<LogStatusNotification>(
            &LogStatusNotificationRequest {
                status: UploadLogStatus::Uploading,
                request_id: Some(5),
            },
            &LogStatusNotificationResponse {},
        );
        c.out::<SecurityEventNotification>(
            &SecurityEventNotificationRequest {
                event_type: "FirmwareUpdated".into(),
                timestamp: at(41),
                tech_info: Some("v2".into()),
            },
            &SecurityEventNotificationResponse {},
        );
        c.out::<SignedFirmwareStatusNotification>(
            &SignedFirmwareStatusNotificationRequest {
                status: SignedFirmwareStatus::InstallRebooting,
                request_id: Some(6),
            },
            &SignedFirmwareStatusNotificationResponse {},
        );
        c.inb::<CertificateSigned>(
            &CertificateSignedRequest {
                certificate_chain: "-----BEGIN CERTIFICATE-----".into(),
            },
            &CertificateSignedResponse {
                status: CertificateSignedStatus::Rejected,
            },
        );
        c.inb::<DeleteCertificate>(
            &DeleteCertificateRequest {
                certificate_hash_data: hash(),
            },
            &DeleteCertificateResponse {
                status: DeleteCertificateStatus::NotFound,
            },
        );
        c.inb::<ExtendedTriggerMessage>(
            &ExtendedTriggerMessageRequest {
                requested_message: ExtendedMessageTrigger::SignChargePointCertificate,
                connector_id: Some(connector(1)),
            },
            &ExtendedTriggerMessageResponse {
                status: TriggerMessageStatus::NotImplemented,
            },
        );
        c.inb::<GetInstalledCertificateIds>(
            &GetInstalledCertificateIdsRequest {
                certificate_type: CertificateUse::ManufacturerRootCertificate,
            },
            &GetInstalledCertificateIdsResponse {
                status: GetInstalledCertificateStatus::Accepted,
                certificate_hash_data: Some(vec![hash()]),
            },
        );
        c.inb::<GetLog>(
            &GetLogRequest {
                log_type: LogType::SecurityLog,
                request_id: 7,
                retries: Some(2),
                retry_interval: Some(30),
                log: LogParameters {
                    remote_location: "https://example.com/logs".into(),
                    oldest_timestamp: Some(at(42)),
                    latest_timestamp: Some(at(43)),
                },
            },
            &GetLogResponse {
                status: LogStatus::AcceptedCanceled,
                filename: Some("security.log".into()),
            },
        );
        c.inb::<InstallCertificate>(
            &InstallCertificateRequest {
                certificate_type: CertificateUse::CentralSystemRootCertificate,
                certificate: "-----BEGIN CERTIFICATE-----".into(),
            },
            &InstallCertificateResponse {
                status: InstallCertificateStatus::Failed,
            },
        );
        c.inb::<SignedUpdateFirmware>(
            &SignedUpdateFirmwareRequest {
                retries: Some(1),
                retry_interval: Some(10),
                request_id: 8,
                firmware: SignedFirmware {
                    location: "https://example.com/fw.bin".into(),
                    retrieve_date_time: at(44),
                    install_date_time: Some(at(45)),
                    signing_certificate: "-----BEGIN CERTIFICATE-----".into(),
                    signature: "c2ln".into(),
                },
            },
            &SignedUpdateFirmwareResponse {
                status: UpdateFirmwareStatus::RevokedCertificate,
            },
        );

        // The list, the schemas, the checked actions and the direction impls agree.
        let listed: BTreeSet<&str> = ACTIONS.iter().map(|a| a.name).collect();
        assert_eq!(listed.len(), 39, "duplicate names in ACTIONS");
        assert_eq!(c.seen, listed, "every listed action has a sample");
        let schemas = c.schemas.as_ref().unwrap();
        let schema_names: BTreeSet<&str> = schemas.action_names().collect();
        assert_eq!(schema_names, listed);
        for action in &ACTIONS {
            for kind in [PayloadKind::Request, PayloadKind::Response] {
                assert!(
                    schemas.schema_source(action.name, kind).is_some(),
                    "{} {kind} schema",
                    action.name
                );
            }
            assert_eq!(
                c.outbound.contains(action.name),
                action.direction.charge_point_may_send(),
                "{} OutboundAction",
                action.name
            );
            assert_eq!(
                c.inbound.contains(action.name),
                action.direction.central_system_may_send(),
                "{} InboundAction",
                action.name
            );
        }
        assert_eq!(c.outbound.len(), 14);
        assert_eq!(c.inbound.len(), 26);
        assert!(ACTIONS.windows(2).all(|w| w[0].name < w[1].name), "sorted");
    }

    #[test]
    fn find_looks_up_by_name() {
        assert_eq!(
            find("DataTransfer").unwrap().direction,
            ActionDirection::Both
        );
        assert!(find("Reset").unwrap().direction.central_system_may_send());
        assert!(!find("Reset").unwrap().direction.charge_point_may_send());
        assert!(find("reset").is_none());
    }

    #[test]
    fn minimal_payloads_validate_and_roundtrip() {
        fn check<A: Action>(req: &Value, res: &Value) {
            let schemas = SchemaRegistry::new().unwrap();
            schemas.validate_request(A::NAME, req).unwrap();
            schemas.validate_response(A::NAME, res).unwrap();
            let parsed: A::Req = serde_json::from_value(req.clone()).unwrap();
            assert_eq!(
                &serde_json::to_value(parsed).unwrap(),
                req,
                "{} req",
                A::NAME
            );
            let parsed: A::Res = serde_json::from_value(res.clone()).unwrap();
            assert_eq!(
                &serde_json::to_value(parsed).unwrap(),
                res,
                "{} res",
                A::NAME
            );
        }
        check::<BootNotification>(
            &json!({"chargePointVendor": "V", "chargePointModel": "M"}),
            &json!({"status": "Pending", "currentTime": "2026-05-06T07:08:09Z", "interval": 10}),
        );
        check::<ClearChargingProfile>(&json!({}), &json!({"status": "Accepted"}));
        check::<GetConfiguration>(&json!({}), &json!({}));
        check::<GetDiagnostics>(&json!({"location": "ftp://h/d"}), &json!({}));
        check::<RemoteStartTransaction>(&json!({"idTag": "T"}), &json!({"status": "Accepted"}));
        check::<SendLocalList>(
            &json!({"listVersion": 1, "updateType": "Full"}),
            &json!({"status": "Accepted"}),
        );
        check::<StatusNotification>(
            &json!({"connectorId": 0, "errorCode": "NoError", "status": "Available"}),
            &json!({}),
        );
        check::<StopTransaction>(
            &json!({"transactionId": 1, "timestamp": "2026-05-06T07:08:09Z", "meterStop": 5}),
            &json!({}),
        );
        check::<TriggerMessage>(
            &json!({"requestedMessage": "Heartbeat"}),
            &json!({"status": "Accepted"}),
        );
    }

    #[test]
    fn wire_names_that_differ_from_rust_field_names() {
        assert_eq!(
            serde_json::to_value(ResetRequest {
                kind: ResetType::Hard
            })
            .unwrap(),
            json!({"type": "Hard"})
        );
        assert_eq!(
            serde_json::to_value(ChangeAvailabilityRequest {
                connector_id: connector(2),
                kind: AvailabilityType::Operative,
            })
            .unwrap(),
            json!({"connectorId": 2, "type": "Operative"})
        );
        assert_eq!(
            serde_json::to_value(SetChargingProfileRequest {
                connector_id: connector(0),
                cs_charging_profiles: charging_profile(),
            })
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
            ["connectorId", "csChargingProfiles"]
        );
    }

    #[test]
    fn exact_json_of_a_status_notification_and_a_meter_values() {
        let status = StatusNotificationRequest {
            connector_id: connector(1),
            error_code: ChargePointErrorCode::NoError,
            info: None,
            status: ConnectorStatus::SuspendedEVSE,
            timestamp: Some(at(9)),
            vendor_id: None,
            vendor_error_code: None,
        };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            json!({
                "connectorId": 1,
                "errorCode": "NoError",
                "status": "SuspendedEVSE",
                "timestamp": "2026-05-06T07:08:09Z"
            })
        );
        let meter = MeterValuesRequest {
            connector_id: connector(1),
            transaction_id: None,
            meter_value: meter_values(),
        };
        let value = serde_json::to_value(&meter).unwrap();
        assert_eq!(
            value["meterValue"][0]["sampledValue"][0],
            json!({
                "value": "1234",
                "context": "Sample.Periodic",
                "format": "Raw",
                "measurand": "Energy.Active.Import.Register",
                "phase": "L1-N",
                "location": "Outlet",
                "unit": "Wh"
            })
        );
        assert_eq!(
            value["meterValue"][0]["sampledValue"][1],
            json!({"value": "7.2"})
        );
        assert!(value.get("transactionId").is_none());
    }

    #[test]
    fn unknown_enum_values_and_missing_fields_are_rejected_by_serde() {
        assert!(serde_json::from_value::<ResetRequest>(json!({"type": "Warm"})).is_err());
        assert!(serde_json::from_value::<ResetRequest>(json!({"type": "hard"})).is_err());
        assert!(serde_json::from_value::<ResetRequest>(json!({})).is_err());
        assert!(
            serde_json::from_value::<StatusNotificationRequest>(
                json!({"connectorId": 1, "errorCode": "NoError", "status": "Idle"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<TriggerMessageRequest>(
                json!({"requestedMessage": "MeterValue"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<SampledValue>(json!({"value": "1", "unit": "kw"})).is_err()
        );
        assert!(
            serde_json::from_value::<AuthorizeRequest>(json!({"idTag": "x".repeat(21)})).is_err()
        );
        assert!(
            serde_json::from_value::<StartTransactionResponse>(
                json!({"idTagInfo": {"status": "Accepted"}})
            )
            .is_err()
        );
    }

    fn wire_values<T: Copy + std::fmt::Display>(all: &[T]) -> BTreeSet<String> {
        all.iter().map(ToString::to_string).collect()
    }

    fn schema_enum(
        schemas: &SchemaRegistry,
        action: &str,
        kind: PayloadKind,
        property_path: &str,
    ) -> BTreeSet<String> {
        let source = schemas.schema_source(action, kind).unwrap();
        let schema: Value = serde_json::from_str(source).unwrap();
        let pointer = format!("{property_path}/enum");
        let values = schema
            .pointer(&pointer)
            .unwrap_or_else(|| panic!("{action} {kind}: no {pointer}"));
        values
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "a flat table, one row per action")]
    fn rust_enums_match_the_schema_enums_exactly() {
        use PayloadKind::{Request, Response};
        let schemas = SchemaRegistry::new().unwrap();
        let sample = "/properties/meterValue/items/properties/sampledValue/items/properties";
        let profile = "/properties/chargingProfile/properties";
        let table: Vec<(&str, PayloadKind, String, BTreeSet<String>)> = vec![
            (
                "MeterValues",
                Request,
                format!("{sample}/context"),
                wire_values(ReadingContext::ALL),
            ),
            (
                "MeterValues",
                Request,
                format!("{sample}/format"),
                wire_values(ValueFormat::ALL),
            ),
            (
                "MeterValues",
                Request,
                format!("{sample}/measurand"),
                wire_values(Measurand::ALL),
            ),
            (
                "MeterValues",
                Request,
                format!("{sample}/phase"),
                wire_values(Phase::ALL),
            ),
            (
                "MeterValues",
                Request,
                format!("{sample}/location"),
                wire_values(Location::ALL),
            ),
            (
                "MeterValues",
                Request,
                format!("{sample}/unit"),
                wire_values(UnitOfMeasure::ALL),
            ),
            (
                "StatusNotification",
                Request,
                "/properties/errorCode".into(),
                wire_values(ChargePointErrorCode::ALL),
            ),
            (
                "StatusNotification",
                Request,
                "/properties/status".into(),
                wire_values(ConnectorStatus::ALL),
            ),
            (
                "StopTransaction",
                Request,
                "/properties/reason".into(),
                wire_values(StopReason::ALL),
            ),
            (
                "Authorize",
                Response,
                "/properties/idTagInfo/properties/status".into(),
                wire_values(AuthorizationStatus::ALL),
            ),
            (
                "BootNotification",
                Response,
                "/properties/status".into(),
                wire_values(RegistrationStatus::ALL),
            ),
            (
                "ChangeAvailability",
                Request,
                "/properties/type".into(),
                wire_values(AvailabilityType::ALL),
            ),
            (
                "ChangeAvailability",
                Response,
                "/properties/status".into(),
                wire_values(AvailabilityStatus::ALL),
            ),
            (
                "ChangeConfiguration",
                Response,
                "/properties/status".into(),
                wire_values(ConfigurationStatus::ALL),
            ),
            (
                "ClearCache",
                Response,
                "/properties/status".into(),
                wire_values(ClearCacheStatus::ALL),
            ),
            (
                "DataTransfer",
                Response,
                "/properties/status".into(),
                wire_values(DataTransferStatus::ALL),
            ),
            (
                "DiagnosticsStatusNotification",
                Request,
                "/properties/status".into(),
                wire_values(DiagnosticsStatus::ALL),
            ),
            (
                "FirmwareStatusNotification",
                Request,
                "/properties/status".into(),
                wire_values(FirmwareStatus::ALL),
            ),
            (
                "RemoteStartTransaction",
                Request,
                format!("{profile}/chargingProfilePurpose"),
                wire_values(ChargingProfilePurposeType::ALL),
            ),
            (
                "RemoteStartTransaction",
                Request,
                format!("{profile}/chargingProfileKind"),
                wire_values(ChargingProfileKindType::ALL),
            ),
            (
                "RemoteStartTransaction",
                Request,
                format!("{profile}/recurrencyKind"),
                wire_values(RecurrencyKindType::ALL),
            ),
            (
                "RemoteStartTransaction",
                Request,
                format!("{profile}/chargingSchedule/properties/chargingRateUnit"),
                wire_values(ChargingRateUnitType::ALL),
            ),
            (
                "RemoteStartTransaction",
                Response,
                "/properties/status".into(),
                wire_values(RemoteStartStopStatus::ALL),
            ),
            (
                "RemoteStopTransaction",
                Response,
                "/properties/status".into(),
                wire_values(RemoteStartStopStatus::ALL),
            ),
            (
                "Reset",
                Request,
                "/properties/type".into(),
                wire_values(ResetType::ALL),
            ),
            (
                "Reset",
                Response,
                "/properties/status".into(),
                wire_values(ResetStatus::ALL),
            ),
            (
                "UnlockConnector",
                Response,
                "/properties/status".into(),
                wire_values(UnlockStatus::ALL),
            ),
            (
                "SendLocalList",
                Request,
                "/properties/updateType".into(),
                wire_values(UpdateType::ALL),
            ),
            (
                "SendLocalList",
                Response,
                "/properties/status".into(),
                wire_values(UpdateStatus::ALL),
            ),
            (
                "ReserveNow",
                Response,
                "/properties/status".into(),
                wire_values(ReservationStatus::ALL),
            ),
            (
                "CancelReservation",
                Response,
                "/properties/status".into(),
                wire_values(CancelReservationStatus::ALL),
            ),
            (
                "SetChargingProfile",
                Response,
                "/properties/status".into(),
                wire_values(ChargingProfileStatus::ALL),
            ),
            (
                "ClearChargingProfile",
                Request,
                "/properties/chargingProfilePurpose".into(),
                wire_values(ChargingProfilePurposeType::ALL),
            ),
            (
                "ClearChargingProfile",
                Response,
                "/properties/status".into(),
                wire_values(ClearChargingProfileStatus::ALL),
            ),
            (
                "GetCompositeSchedule",
                Request,
                "/properties/chargingRateUnit".into(),
                wire_values(ChargingRateUnitType::ALL),
            ),
            (
                "GetCompositeSchedule",
                Response,
                "/properties/status".into(),
                wire_values(GetCompositeScheduleStatus::ALL),
            ),
            (
                "TriggerMessage",
                Request,
                "/properties/requestedMessage".into(),
                wire_values(MessageTrigger::ALL),
            ),
            (
                "TriggerMessage",
                Response,
                "/properties/status".into(),
                wire_values(TriggerMessageStatus::ALL),
            ),
        ];
        for (action, kind, path, rust) in table {
            assert_eq!(
                schema_enum(&schemas, action, kind, &path),
                rust,
                "{action} {kind} {path}"
            );
        }
    }
}
