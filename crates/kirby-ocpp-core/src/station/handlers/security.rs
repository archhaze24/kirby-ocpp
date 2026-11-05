//! Certificate commands of the Security Whitepaper: `InstallCertificate`, `DeleteCertificate`,
//! `GetInstalledCertificateIds` and `CertificateSigned`.

use crate::ocpp::messages::{
    CertificateSigned, CertificateSignedRequest, CertificateSignedResponse,
    CertificateSignedStatus, DeleteCertificate, DeleteCertificateRequest,
    DeleteCertificateResponse, DeleteCertificateStatus, GetInstalledCertificateIds,
    GetInstalledCertificateIdsRequest, GetInstalledCertificateIdsResponse,
    GetInstalledCertificateStatus, InstallCertificate, InstallCertificateRequest,
    InstallCertificateResponse, InstallCertificateStatus,
};
use crate::ocpp::responder::Responder;
use crate::station::actor::StationActor;
use crate::station::certificates::validate_signed_chain;
use crate::station::event::LogLevel;

pub(super) fn install_certificate(
    actor: &mut StationActor,
    responder: Responder<InstallCertificate>,
    request: InstallCertificateRequest,
) {
    let max = actor.registry.certificate_store_max_length();
    let now = actor.clock.now_utc();
    let check = actor.registry.additional_root_certificate_check();
    let before = actor.certs.roots.clone();
    let status = actor.certs.install(
        request.certificate_type,
        &request.certificate,
        now,
        max,
        check,
    );
    if status == InstallCertificateStatus::Accepted {
        if actor.certs.roots != before {
            actor.use_fallback_root = false;
            actor.certs_dirty = true;
            actor.flush_now();
        }
        actor.log(
            LogLevel::Info,
            format!(
                "InstallCertificate {} -> {status}",
                request.certificate_type
            ),
        );
    } else {
        actor.log(
            LogLevel::Warn,
            format!(
                "InstallCertificate {} -> {status}",
                request.certificate_type
            ),
        );
        if status == InstallCertificateStatus::Rejected {
            actor.log(
                LogLevel::Warn,
                "certificate refused: invalid, expired, not a root, or the store is full"
                    .to_owned(),
            );
        }
    }
    actor.respond(responder, InstallCertificateResponse { status });
}

pub(super) fn delete_certificate(
    actor: &mut StationActor,
    responder: Responder<DeleteCertificate>,
    request: DeleteCertificateRequest,
) {
    // With TLS in use the last Central System root must stay, or the station could never
    // reconnect.
    let keep_last = actor.registry.security_profile() >= 2;
    let status = actor
        .certs
        .delete(&request.certificate_hash_data, keep_last);
    if status == DeleteCertificateStatus::Accepted {
        actor.certs_dirty = true;
        actor.flush_now();
    }
    actor.log(LogLevel::Info, format!("DeleteCertificate -> {status}"));
    actor.respond(responder, DeleteCertificateResponse { status });
}

pub(super) fn get_installed_certificate_ids(
    actor: &mut StationActor,
    responder: Responder<GetInstalledCertificateIds>,
    request: GetInstalledCertificateIdsRequest,
) {
    let ids = actor.certs.ids(request.certificate_type);
    let response = if ids.is_empty() {
        GetInstalledCertificateIdsResponse {
            status: GetInstalledCertificateStatus::NotFound,
            certificate_hash_data: None,
        }
    } else {
        GetInstalledCertificateIdsResponse {
            status: GetInstalledCertificateStatus::Accepted,
            certificate_hash_data: Some(ids),
        }
    };
    actor.respond(responder, response);
}

pub(super) fn certificate_signed(
    actor: &mut StationActor,
    responder: Responder<CertificateSigned>,
    request: CertificateSignedRequest,
) {
    let outcome = match actor.secrets.pending_key_pem.clone() {
        None => Err("no certificate request is pending".to_owned()),
        Some(key) => validate_signed_chain(
            &request.certificate_chain,
            &key,
            actor.clock.now_utc(),
            actor.registry.certificate_signed_max_chain_size(),
        )
        .map(|()| key)
        .map_err(|e| e.0),
    };
    match outcome {
        Ok(key) => {
            actor.certs.charge_point_chain = Some(request.certificate_chain.trim().to_owned());
            actor.secrets.charge_point_key_pem = Some(key);
            actor.secrets.pending_key_pem = None;
            actor.certs_dirty = true;
            actor.mark_secrets_dirty();
            actor.flush_now();
            actor.log(
                LogLevel::Success,
                "CertificateSigned accepted; reconnecting to use the new certificate".to_owned(),
            );
            actor.reconnect_for_new_security_parameters();
            actor.respond(
                responder,
                CertificateSignedResponse {
                    status: CertificateSignedStatus::Accepted,
                },
            );
        }
        Err(reason) => {
            actor.log(
                LogLevel::Warn,
                format!("CertificateSigned rejected: {reason}"),
            );
            actor.respond(
                responder,
                CertificateSignedResponse {
                    status: CertificateSignedStatus::Rejected,
                },
            );
            actor.security_event("InvalidChargePointCertificate", Some(&reason));
        }
    }
}
