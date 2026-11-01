//! Placeholders for the actions implemented in stage 2b.

use crate::ocpp::action::InboundAction;
use crate::ocpp::frame::ErrorCode;
use crate::ocpp::responder::Responder;
use crate::station::actor::StationActor;

/// Answers a CALLERROR `NotSupported` (the CSMS learns the action is not available yet).
// stage 2b: Reset, RemoteStart/Stop, ReserveNow/CancelReservation, SetChargingProfile,
// ClearChargingProfile, GetCompositeSchedule, GetDiagnostics, UpdateFirmware.
pub(super) fn not_supported<A: InboundAction>(
    actor: &mut StationActor,
    responder: Responder<A>,
    _request: A::Req,
) {
    actor.respond_error(
        responder,
        ErrorCode::NotSupported,
        format!("{} is not supported by this emulator build yet", A::NAME),
    );
}
