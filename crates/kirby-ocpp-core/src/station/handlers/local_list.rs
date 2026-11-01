//! Local authorization list and authorization cache: `SendLocalList`,
//! `GetLocalListVersion`, `ClearCache` (the rules live in `station::local_auth_list`).

use crate::ocpp::messages::{
    ClearCache, ClearCacheRequest, ClearCacheResponse, ClearCacheStatus, GetLocalListVersion,
    GetLocalListVersionRequest, GetLocalListVersionResponse, SendLocalList, SendLocalListRequest,
    SendLocalListResponse, UpdateStatus,
};
use crate::ocpp::responder::Responder;
use crate::station::actor::StationActor;
use crate::station::event::LogLevel;
use crate::station::local_auth_list::LocalListLimits;

pub(super) fn send_local_list(
    actor: &mut StationActor,
    responder: Responder<SendLocalList>,
    request: SendLocalListRequest,
) {
    let limits = LocalListLimits::from_config(&actor.registry);
    let status = actor.local_list.apply(&request, &limits);
    if status == UpdateStatus::Accepted {
        actor.local_list_dirty = true;
        actor.flush_now();
        actor.log(
            LogLevel::Info,
            format!(
                "local list updated to version {} ({} entries)",
                actor.local_list.version(),
                actor.local_list.len()
            ),
        );
    } else {
        actor.log(LogLevel::Warn, format!("SendLocalList -> {status}"));
    }
    actor.respond(responder, SendLocalListResponse { status });
}

pub(super) fn get_version(
    actor: &mut StationActor,
    responder: Responder<GetLocalListVersion>,
    _request: GetLocalListVersionRequest,
) {
    let enabled = actor.registry.local_auth_list_enabled();
    let list_version = actor.local_list.reported_version(enabled);
    actor.respond(responder, GetLocalListVersionResponse { list_version });
}

pub(super) fn clear_cache(
    actor: &mut StationActor,
    responder: Responder<ClearCache>,
    _request: ClearCacheRequest,
) {
    actor.auth_cache.clear();
    actor.flush_now();
    actor.respond(
        responder,
        ClearCacheResponse {
            status: ClearCacheStatus::Accepted,
        },
    );
}
