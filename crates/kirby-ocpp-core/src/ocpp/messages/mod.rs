//! Typed request/response payloads, one marker type per OCPP 1.6 action.
//!
//! All 28 actions are here, grouped by profile (the conventions are described in
//! [`crate::ocpp::action`]):
//!
//! | module | actions |
//! |---|---|
//! | `heartbeat` | `Heartbeat` |
//! | `core_profile` | `Authorize`, `BootNotification`, `ChangeAvailability`, `ChangeConfiguration`, `ClearCache`, `DataTransfer`, `GetConfiguration`, `MeterValues`, `RemoteStartTransaction`, `RemoteStopTransaction`, `Reset`, `StartTransaction`, `StatusNotification`, `StopTransaction`, `UnlockConnector` |
//! | `firmware` | `GetDiagnostics`, `DiagnosticsStatusNotification`, `FirmwareStatusNotification`, `UpdateFirmware` |
//! | `local_auth_list` | `GetLocalListVersion`, `SendLocalList` |
//! | `reservation` | `ReserveNow`, `CancelReservation` |
//! | `smart_charging` | `SetChargingProfile`, `ClearChargingProfile`, `GetCompositeSchedule` |
//! | `remote_trigger` | `TriggerMessage` |
//!
//! Shared sub-structures (`IdTagInfo`, `MeterValue`, `ChargingProfile`, ...) live in
//! `common`; the [`registry`] lists every action with its direction.
//!
//! # Decisions
//!
//! * The bundled JSON schemas, not these structs, are the source of truth for
//!   validity. The connection layer validates the raw `Value` first and only then
//!   deserializes. Consequently the structs do not `deny_unknown_fields` (the
//!   schemas already carry `additionalProperties: false`) and do not re-check
//!   string lengths other than `IdTag` (<= 20, same as the schema).
//! * Integer fields are signed and wide (`i32`, `i64` for meter readings) because
//!   the schemas say plain `integer`. A negative `retries` is schema-valid; it is
//!   the station's business to reject it, not serde's. A value that does not fit
//!   the Rust type (e.g. a `transactionId` above `i32::MAX`) still fails
//!   deserialization after passing the schema.
//! * `transactionId` is a bare `i32` on the wire; the station layer wraps it in
//!   [`crate::ids::TxId`].
//! * `location` of `GetDiagnostics`/`UpdateFirmware` is a `String`: the schema
//!   only demands `format: uri`.

/// Declares an action marker type implementing [`Action`](crate::ocpp::action::Action)
/// and the given direction marker traits.
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

mod common;
mod core_profile;
mod firmware;
mod heartbeat;
mod local_auth_list;
pub mod registry;
mod remote_trigger;
mod reservation;
mod smart_charging;

pub use common::{
    AuthorizationData, ChargingProfile, ChargingSchedule, ChargingSchedulePeriod, IdTagInfo,
    MeterValue, SampledValue,
};
pub use core_profile::{
    Authorize, AuthorizeRequest, AuthorizeResponse, AvailabilityStatus, BootNotification,
    BootNotificationRequest, BootNotificationResponse, ChangeAvailability,
    ChangeAvailabilityRequest, ChangeAvailabilityResponse, ChangeConfiguration,
    ChangeConfigurationRequest, ChangeConfigurationResponse, ClearCache, ClearCacheRequest,
    ClearCacheResponse, ClearCacheStatus, ConfigurationKey, DataTransfer, DataTransferRequest,
    DataTransferResponse, DataTransferStatus, GetConfiguration, GetConfigurationRequest,
    GetConfigurationResponse, MeterValues, MeterValuesRequest, MeterValuesResponse,
    RemoteStartStopStatus, RemoteStartTransaction, RemoteStartTransactionRequest,
    RemoteStartTransactionResponse, RemoteStopTransaction, RemoteStopTransactionRequest,
    RemoteStopTransactionResponse, Reset, ResetRequest, ResetResponse, ResetStatus,
    StartTransaction, StartTransactionRequest, StartTransactionResponse, StatusNotification,
    StatusNotificationRequest, StatusNotificationResponse, StopTransaction, StopTransactionRequest,
    StopTransactionResponse, UnlockConnector, UnlockConnectorRequest, UnlockConnectorResponse,
    UnlockStatus,
};
pub use firmware::{
    DiagnosticsStatusNotification, DiagnosticsStatusNotificationRequest,
    DiagnosticsStatusNotificationResponse, FirmwareStatusNotification,
    FirmwareStatusNotificationRequest, FirmwareStatusNotificationResponse, GetDiagnostics,
    GetDiagnosticsRequest, GetDiagnosticsResponse, UpdateFirmware, UpdateFirmwareRequest,
    UpdateFirmwareResponse,
};
pub use heartbeat::{Heartbeat, HeartbeatRequest, HeartbeatResponse};
pub use local_auth_list::{
    GetLocalListVersion, GetLocalListVersionRequest, GetLocalListVersionResponse, SendLocalList,
    SendLocalListRequest, SendLocalListResponse, UpdateStatus,
};
pub use remote_trigger::{
    TriggerMessage, TriggerMessageRequest, TriggerMessageResponse, TriggerMessageStatus,
};
pub use reservation::{
    CancelReservation, CancelReservationRequest, CancelReservationResponse,
    CancelReservationStatus, ReservationStatus, ReserveNow, ReserveNowRequest, ReserveNowResponse,
};
pub use smart_charging::{
    ChargingProfileStatus, ClearChargingProfile, ClearChargingProfileRequest,
    ClearChargingProfileResponse, ClearChargingProfileStatus, GetCompositeSchedule,
    GetCompositeScheduleRequest, GetCompositeScheduleResponse, GetCompositeScheduleStatus,
    SetChargingProfile, SetChargingProfileRequest, SetChargingProfileResponse,
};
