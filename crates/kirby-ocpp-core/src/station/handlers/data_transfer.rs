//! Inbound `DataTransfer` (TS `handleDataTransfer`): data for the station's own vendor id is
//! accepted and echoed, any other vendor id is `UnknownVendorId`. The `messageId` is not
//! interpreted (TS parity; a vendor-specific emulator would branch here).

use crate::ocpp::messages::{
    DataTransfer, DataTransferRequest, DataTransferResponse, DataTransferStatus,
};
use crate::ocpp::responder::Responder;
use crate::station::actor::StationActor;

pub(super) fn data_transfer(
    actor: &mut StationActor,
    responder: Responder<DataTransfer>,
    request: DataTransferRequest,
) {
    let response = if request.vendor_id == actor.config.vendor {
        DataTransferResponse {
            status: DataTransferStatus::Accepted,
            data: request.data,
        }
    } else {
        DataTransferResponse {
            status: DataTransferStatus::UnknownVendorId,
            data: None,
        }
    };
    actor.respond(responder, response);
}
