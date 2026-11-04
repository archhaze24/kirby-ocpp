//! Security Whitepaper behaviour of the actor: security events, certificate requests and the
//! transport rules of the security profiles.
//!
//! * **Security events** are queued (bounded, persisted) and sent one `SecurityEventNotification`
//!   each once the station is registered. A failure caused by the link (not connected, closed,
//!   not registered) puts the event back; a CSMS error answer drops it (a CSMS without the
//!   whitepaper answers `NotImplemented`).
//! * **Profiles** (`SecurityProfile`): 0 and 1 open the connection as configured; 1 and 2 use the
//!   `AuthorizationKey` as Basic Auth password unless the URL carries one; 3 uses the stored
//!   charge point certificate as TLS client identity unless `tls.cert_file` is given. The
//!   installed Central System roots are extra trust anchors. All of this is applied when a
//!   connection is opened, so changes take effect at the next (re)connect.

use serde_json::Value;

use super::actor::StationActor;
use super::calls::{Origin, Reply};
use super::command::Outcome;
use super::commands::call_failure;
use super::event::LogLevel;
use super::security::{PendingSecurityEvent, push_bounded};
use crate::config::{IdentityPem, StationConfig};
use crate::error::{CallError, StationError};
use crate::ocpp::messages::{
    CertificateUse, GenericStatus, SecurityEventNotification, SecurityEventNotificationRequest,
    SignCertificate, SignCertificateRequest,
};
use crate::ocpp::types::RegistrationStatus;
use crate::station::certificates::generate_csr;
use crate::station::config_registry::{AUTHORIZATION_KEY, SECURITY_PROFILE};
use crate::ws::ConnectError;

impl StationActor {
    /// Reports a security event: queued, then sent as soon as the station is registered.
    pub(super) fn security_event(&mut self, event_type: &str, tech_info: Option<&str>) {
        let event = PendingSecurityEvent::new(event_type, self.clock.now_utc(), tech_info);
        self.log(
            LogLevel::Warn,
            format!(
                "security event {}{}",
                event.event_type,
                event
                    .tech_info
                    .as_deref()
                    .map_or_else(String::new, |t| format!(": {t}"))
            ),
        );
        let duplicate = self
            .security_events
            .iter()
            .any(|e| e.event_type == event.event_type && e.tech_info == event.tech_info);
        if !duplicate {
            push_bounded(&mut self.security_events, event);
            self.mark_dirty();
        }
        self.flush_security_events();
    }

    /// Sends the queued security events if the station is registered.
    pub(super) fn flush_security_events(&mut self) {
        if self.link.conn.is_none() || self.state.registration != Some(RegistrationStatus::Accepted)
        {
            return;
        }
        let events = std::mem::take(&mut self.security_events);
        if events.is_empty() {
            return;
        }
        self.mark_dirty();
        for event in events {
            let request = SecurityEventNotificationRequest {
                event_type: event.event_type.clone(),
                timestamp: event.timestamp,
                tech_info: event.tech_info.clone(),
            };
            self.call::<SecurityEventNotification>(
                Origin::Station,
                &request,
                move |actor, result| match result {
                    Ok(_) => {}
                    Err(
                        CallError::NotConnected
                        | CallError::Closed { .. }
                        | CallError::NotRegistered { .. },
                    ) => {
                        push_bounded(&mut actor.security_events, event);
                        actor.security_events.sort_by_key(|e| e.timestamp);
                        actor.mark_dirty();
                    }
                    Err(error) => actor.log(
                        LogLevel::Warn,
                        format!("security event {} dropped: {error}", event.event_type),
                    ),
                },
            );
        }
    }

    /// `Command::SendSecurityEvent`.
    pub(super) fn send_security_event(
        &mut self,
        event_type: &str,
        tech_info: Option<&str>,
    ) -> Result<Outcome, StationError> {
        let canonical = super::security::canonical_event_type(event_type).ok_or_else(|| {
            StationError::InvalidArgument(format!("unknown security event type `{event_type}`"))
        })?;
        self.security_event(canonical, tech_info);
        Ok(Outcome::Done)
    }

    /// `Command::RequestCertificate` (also the answer to `ExtendedTriggerMessage`): a new key pair
    /// and CSR go to the CSMS in `SignCertificate`; the key waits for `CertificateSigned`.
    pub(super) fn request_certificate(&mut self, reply: Reply) {
        let organization = match self.registry.cpo_name() {
            "" => "Kirby".to_owned(),
            name => name.to_owned(),
        };
        let (csr, key) = match generate_csr(&self.config.charge_point_id, &organization) {
            Ok(pair) => pair,
            Err(error) => {
                let _ = reply.send(Err(StationError::InvalidArgument(format!(
                    "cannot create a certificate request: {error}"
                ))));
                return;
            }
        };
        if self.link.conn.is_none() {
            let _ = reply.send(Err(StationError::NotConnected));
            return;
        }
        let previous = self.secrets.pending_key_pem.replace(key);
        self.mark_secrets_dirty();
        self.call::<SignCertificate>(
            Origin::Station,
            &SignCertificateRequest { csr },
            move |actor, result| {
                let answer = match result {
                    Ok(response) => {
                        if response.status == GenericStatus::Accepted {
                            actor.log(
                                LogLevel::Info,
                                "SignCertificate accepted; waiting for CertificateSigned"
                                    .to_owned(),
                            );
                        } else {
                            actor.secrets.pending_key_pem = previous;
                            actor.mark_secrets_dirty();
                            actor.log(
                                LogLevel::Warn,
                                "SignCertificate was rejected by the CSMS".to_owned(),
                            );
                        }
                        let payload = serde_json::to_value(&response).unwrap_or(Value::Null);
                        Ok(Outcome::Response { payload })
                    }
                    Err(error) => {
                        actor.secrets.pending_key_pem = previous;
                        actor.mark_secrets_dirty();
                        Err(call_failure(error))
                    }
                };
                let _ = reply.send(answer);
            },
        );
    }

    /// Whether a TLS client identity is available (CLI files or the stored certificate).
    fn has_client_identity(&self) -> bool {
        self.config.tls.cert_file.is_some()
            || (self.certs.charge_point_chain.is_some()
                && self.secrets.charge_point_key_pem.is_some())
    }

    /// Transport rules for raising `SecurityProfile`; `Err` carries the reason.
    pub(super) fn check_security_change(&self, key: &str, value: &str) -> Result<(), String> {
        if !key.eq_ignore_ascii_case(SECURITY_PROFILE) {
            return Ok(());
        }
        let Ok(new) = value.parse::<u8>() else {
            return Ok(()); // the registry rejects it
        };
        if new > 3 {
            return Ok(());
        }
        let current = self.registry.security_profile();
        if new < current {
            return Err(format!(
                "the security profile cannot be lowered ({current} -> {new})"
            ));
        }
        if new == current {
            return Ok(());
        }
        let secure_url = self.config.url.to_ascii_lowercase().starts_with("wss://");
        if new >= 2 && !secure_url {
            return Err(format!("profile {new} needs a wss:// URL"));
        }
        if new == 3 && !self.has_client_identity() {
            return Err(
                "profile 3 needs a client certificate (CertificateSigned or tls cert_file)"
                    .to_owned(),
            );
        }
        let has_password = url::Url::parse(&self.config.url).is_ok_and(|u| u.password().is_some());
        if matches!(new, 1 | 2) && !has_password && self.registry.authorization_key().is_none() {
            return Err(format!(
                "profile {new} needs an AuthorizationKey (Basic Auth password)"
            ));
        }
        Ok(())
    }

    /// Applies the security profile to the config used to open a connection.
    pub(super) fn apply_security_transport(&self, config: &mut StationConfig) {
        let profile = self.registry.security_profile();
        if matches!(profile, 1 | 2)
            && let Some(key) = self.registry.authorization_key()
            && let Ok(mut url) = url::Url::parse(&config.url)
            && url.password().is_none()
        {
            let _ = url.set_username(&config.charge_point_id);
            let _ = url.set_password(Some(key));
            config.url = url.to_string();
        }
        if profile == 3
            && config.tls.cert_file.is_none()
            && let (Some(chain), Some(key)) = (
                self.certs.charge_point_chain.clone(),
                self.secrets.charge_point_key_pem.clone(),
            )
        {
            config.tls.identity_pem = Some(IdentityPem {
                chain_pem: chain,
                key_pem: key,
            });
        }
        // M05.FR.13: after a refused server certificate the next attempt trusts the fallback root.
        config.tls.extra_roots_pem = if self.use_fallback_root && self.certs.has_fallback() {
            self.certs.fallback_pems()
        } else {
            self.certs
                .root_pems(CertificateUse::CentralSystemRootCertificate)
        };
    }

    /// Security events for a failed connection attempt.
    pub(super) fn report_connect_failure(&mut self, error: &ConnectError) {
        if matches!(error, ConnectError::Tls(text) if text.contains("invalid peer certificate"))
            && self.certs.has_fallback()
        {
            self.use_fallback_root = !self.use_fallback_root;
        }
        match error {
            ConnectError::HttpStatus {
                status: 401 | 403,
                line,
            } => {
                self.security_event("FailedToAuthenticateAtCentralSystem", Some(line));
            }
            ConnectError::Tls(text) if text.contains("invalid peer certificate") => {
                self.security_event("InvalidCentralSystemCertificate", Some(text));
            }
            _ => {}
        }
    }

    /// A TLS connection was made: with the new Central System root the fallback is no longer
    /// needed (`M05.FR.12`).
    pub(super) fn on_tls_connected(&mut self) {
        let via_new_root = !self.use_fallback_root;
        self.use_fallback_root = false;
        if via_new_root && self.certs.has_fallback() && self.certs.drop_fallback() {
            self.certs_dirty = true;
            self.flush_now();
            self.log(
                LogLevel::Info,
                "connected with the new Central System root; the fallback root was removed"
                    .to_owned(),
            );
        }
    }

    /// Side effects of an accepted change of a security key (called after it was stored).
    pub(super) fn on_security_key_changed(&mut self, key: &str) {
        if key == AUTHORIZATION_KEY {
            self.secrets.authorization_key = self.registry.authorization_key().map(str::to_owned);
            self.mark_secrets_dirty();
        }
        if key == AUTHORIZATION_KEY || key == SECURITY_PROFILE {
            self.log(
                LogLevel::Info,
                format!("{key} changed; it applies at the next connection"),
            );
            self.security_event("ReconfigurationOfSecurityParameters", Some(key));
        }
    }
}
