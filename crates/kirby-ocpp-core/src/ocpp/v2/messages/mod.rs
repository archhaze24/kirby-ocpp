//! Typed OCPP 2.0.1 requests, responses and action markers.
//!
//! Each action `Foo` has `FooRequest`, `FooResponse` and a marker `Foo`
//! implementing [`Action`](crate::ocpp::action::Action) plus the direction
//! traits (relative to the charge station).
//!
//! | module | actions |
//! |---|---|
//! | `boot_status` | `BootNotification`, `Heartbeat`, `StatusNotification` |
//! | `authorization` | `Authorize`, `ClearCache` |
//! | `transactions` | `TransactionEvent`, `MeterValues`, `GetTransactionStatus`, `CostUpdated` |
//! | `remote_control` | `Reset`, `ChangeAvailability`, `RequestStartTransaction`, `RequestStopTransaction`, `UnlockConnector`, `TriggerMessage` |
//! | `device_model` | `GetVariables`, `SetVariables`, `GetBaseReport`, `GetReport`, `NotifyReport` |
//! | `local_list` | `SendLocalList`, `GetLocalListVersion` |
//! | `reservation` | `ReserveNow`, `CancelReservation`, `ReservationStatusUpdate` |
//! | `smart_charging` | `SetChargingProfile`, `ClearChargingProfile`, `GetChargingProfiles`, `ReportChargingProfiles`, `GetCompositeSchedule`, `NotifyChargingLimit`, `ClearedChargingLimit` |
//! | `data_security` | `DataTransfer`, `SecurityEventNotification` |
//!
//! Each test block holds a table of fixtures that exercise every optional
//! branch and `customData`, validate against the bundled 2.0.1 schema and
//! roundtrip through the typed structs unchanged.

/// Declares an action marker implementing
/// [`Action`](crate::ocpp::action::Action) and the given direction traits.
macro_rules! action {
    (
        $(#[$meta:meta])*
        $name:ident = $wire:literal, $req:ty => $res:ty $(, $direction:ident)+
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy)]
        pub struct $name;

        impl $crate::ocpp::action::Action for $name {
            const NAME: &'static str = $wire;
            type Req = $req;
            type Res = $res;
        }

        $(impl $crate::ocpp::action::$direction for $name {})+
    };
}

mod authorization;
mod boot_status;
mod data_security;
mod device_model;
mod local_list;
mod remote_control;
mod reservation;
mod smart_charging;
mod transactions;

pub use authorization::{
    Authorize, AuthorizeCertificateStatus, AuthorizeRequest, AuthorizeResponse, ClearCache,
    ClearCacheRequest, ClearCacheResponse, ClearCacheStatus,
};
pub use boot_status::{
    BootNotification, BootNotificationRequest, BootNotificationResponse, BootReason,
    ConnectorStatus, Heartbeat, HeartbeatRequest, HeartbeatResponse, RegistrationStatus,
    StatusNotification, StatusNotificationRequest, StatusNotificationResponse,
};
pub use data_security::{
    DataTransfer, DataTransferRequest, DataTransferResponse, DataTransferStatus,
    SecurityEventNotification, SecurityEventNotificationRequest, SecurityEventNotificationResponse,
};
pub use device_model::{
    ComponentCriterion, ComponentVariable, DataType, GenericDeviceModelStatus, GetBaseReport,
    GetBaseReportRequest, GetBaseReportResponse, GetReport, GetReportRequest, GetReportResponse,
    GetVariableData, GetVariableResult, GetVariableStatus, GetVariables, GetVariablesRequest,
    GetVariablesResponse, Mutability, NotifyReport, NotifyReportRequest, NotifyReportResponse,
    ReportBase, ReportData, SetVariableData, SetVariableResult, SetVariableStatus, SetVariables,
    SetVariablesRequest, SetVariablesResponse, VariableAttribute, VariableCharacteristics,
};
pub use local_list::{
    AuthorizationData, GetLocalListVersion, GetLocalListVersionRequest,
    GetLocalListVersionResponse, SendLocalList, SendLocalListRequest, SendLocalListResponse,
    SendLocalListStatus, UpdateKind,
};
pub use remote_control::{
    ChangeAvailability, ChangeAvailabilityRequest, ChangeAvailabilityResponse,
    ChangeAvailabilityStatus, MessageTrigger, OperationalStatus, RequestStartStopStatus,
    RequestStartTransaction, RequestStartTransactionRequest, RequestStartTransactionResponse,
    RequestStopTransaction, RequestStopTransactionRequest, RequestStopTransactionResponse, Reset,
    ResetKind, ResetRequest, ResetResponse, ResetStatus, TriggerMessage, TriggerMessageRequest,
    TriggerMessageResponse, TriggerMessageStatus, UnlockConnector, UnlockConnectorRequest,
    UnlockConnectorResponse, UnlockStatus,
};
pub use reservation::{
    CancelReservation, CancelReservationRequest, CancelReservationResponse,
    CancelReservationStatus, ReservationStatusUpdate, ReservationStatusUpdateRequest,
    ReservationStatusUpdateResponse, ReservationUpdateStatus, ReserveNow, ReserveNowRequest,
    ReserveNowResponse, ReserveNowStatus,
};
pub use smart_charging::{
    ChargingLimit, ChargingProfile, ChargingProfileCriterion, ChargingProfileKind,
    ChargingProfilePurpose, ChargingProfileStatus, ChargingRateUnit, ChargingSchedule,
    ChargingSchedulePeriod, ClearChargingProfile, ClearChargingProfileCriteria,
    ClearChargingProfileRequest, ClearChargingProfileResponse, ClearChargingProfileStatus,
    ClearedChargingLimit, ClearedChargingLimitRequest, ClearedChargingLimitResponse,
    CompositeSchedule, CompositeScheduleStatus, GetChargingProfileStatus, GetChargingProfiles,
    GetChargingProfilesRequest, GetChargingProfilesResponse, GetCompositeSchedule,
    GetCompositeScheduleRequest, GetCompositeScheduleResponse, NotifyChargingLimit,
    NotifyChargingLimitRequest, NotifyChargingLimitResponse, RecurrencyKind,
    ReportChargingProfiles, ReportChargingProfilesRequest, ReportChargingProfilesResponse,
    SetChargingProfile, SetChargingProfileRequest, SetChargingProfileResponse,
};
pub use transactions::{
    CostUpdated, CostUpdatedRequest, CostUpdatedResponse, GetTransactionStatus,
    GetTransactionStatusRequest, GetTransactionStatusResponse, MeterValues, MeterValuesRequest,
    MeterValuesResponse, TransactionEvent, TransactionEventKind, TransactionEventRequest,
    TransactionEventResponse, TriggerReason,
};
