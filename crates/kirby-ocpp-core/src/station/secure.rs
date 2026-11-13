//! Security Whitepaper (edition 4) behaviour of the actor: security events, certificate requests and the
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
//!   connection is opened, so changes take effect at the next (re)connect (this includes
//!   `AllowCentralSystemTLSWildcards`).
//! * **Failed connection attempts** raise `FailedToAuthenticateAtCentralSystem`,
//!   `InvalidCentralSystemCertificate`, `InvalidTLSVersion` and `InvalidTLSCipherSuite` once per
//!   series of failures (the series ends at the next successful connection), as the whitepaper
//!   notes of `A00.FR.315`/`319`/`322`/`419`/`426` allow.
//! * **`SignCertificate` back-off** (`CertSigningWaitMinimum`, `CertSigningRepeatTimes`): see
//!   [`StationActor::request_certificate`].
//! * **Old/new client certificate** (`A02.FR.28`/`29`, `A03.FR.24`/`25`): see
//!   [`StationActor::adopt_signed_certificate`].

use std::time::Duration;

use serde_json::Value;

use super::actor::StationActor;
use super::actor::TimerKey;
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
use crate::ws::{ConnectError, WILDCARD_REFUSED};

/// Which stored charge point certificate a connection attempt presents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StoredIdentity {
    /// None (not profile 3, or the identity comes from the CLI files).
    None,
    /// The current (newest) certificate.
    Current,
    /// The previous certificate (the new one failed; `A02.FR.28`).
    Previous,
}

/// The `SignCertificate` back-off series that a certificate request starts.
pub(super) struct SignSeries {
    id: u64,
    csr: String,
    key: String,
    /// Repeats sent so far.
    repeats: u32,
}

/// Why a TLS handshake failed, as far as the whitepaper security events care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TlsFailure {
    /// The server only offers a protocol version below TLS 1.2 (`InvalidTLSVersion`).
    Version,
    /// No cipher suite (or key exchange group) in common (`InvalidTLSCipherSuite`).
    CipherSuite,
}

/// Maps the text of a failed TLS handshake to a version or cipher suite failure. Conservative:
/// only the rustls errors that say so. Version: alert `ProtocolVersion`, a server that supports
/// neither TLS 1.2 nor 1.3 or chose a version we disabled. Cipher suite: alerts
/// `HandshakeFailure` and `InsufficientSecurity` (the server's answer to a `ClientHello` when it
/// finds no acceptable parameters; with mutual TLS a missing client certificate can also end in
/// `HandshakeFailure`, which is then misreported), `NoCipherSuitesInCommon`,
/// `NoKxGroupsInCommon`. Anything else (certificate errors, I/O errors) is not classified.
pub(super) fn classify_tls_failure(text: &str) -> Option<TlsFailure> {
    const VERSION: &[&str] = &[
        "received fatal alert: ProtocolVersion",
        "ServerDoesNotSupportTls12Or13",
        "ServerTlsVersionIsDisabledByOurConfig",
    ];
    const CIPHER: &[&str] = &[
        "received fatal alert: HandshakeFailure",
        "received fatal alert: InsufficientSecurity",
        "peer is incompatible: NoCipherSuitesInCommon",
        "peer is incompatible: NoKxGroupsInCommon",
    ];
    if VERSION.iter().any(|m| text.contains(m)) {
        Some(TlsFailure::Version)
    } else if CIPHER.iter().any(|m| text.contains(m)) {
        Some(TlsFailure::CipherSuite)
    } else {
        None
    }
}

/// Whether a failed attempt looks like the server refusing our client certificate (a fatal
/// alert about certificates, from the TLS handshake or right after it under TLS 1.3; an HTTP
/// 401/403 upgrade answer; a close or reset right after the handshake).
/// `HandshakeFailure` is deliberately not counted here.
fn is_client_certificate_refusal(error: &ConnectError) -> bool {
    if is_client_certificate_alert(error) {
        return true;
    }
    // The server completed the TLS handshake and then closed or reset the connection
    // before answering the upgrade request (see `ws::handshake::read_head`).
    matches!(error, ConnectError::Handshake(text)
        if !text.contains("received fatal alert")
            && (text.contains("closed before the upgrade response")
                || text.contains("cannot read the upgrade response")))
}

/// The strict form used by the profile fallback (`A05.FR.08`): an HTTP 401/403 upgrade answer
/// or a TLS alert that says the server refused OUR client certificate. Closes, resets,
/// server-certificate errors and generic TLS errors are not evidence about the profile and
/// can be forged by a man in the middle (downgrade resistance).
fn is_client_certificate_alert(error: &ConnectError) -> bool {
    const ALERTS: &[&str] = &[
        "BadCertificate",
        "UnsupportedCertificate",
        "CertificateRevoked",
        "CertificateExpired",
        "CertificateUnknown",
        "UnknownCA",
        "CertificateRequired",
        "AccessDenied",
    ];
    let has_alert = |text: &str| {
        ALERTS
            .iter()
            .any(|alert| text.contains(&format!("received fatal alert: {alert}")))
    };
    match error {
        ConnectError::HttpStatus {
            status: 401 | 403, ..
        } => true,
        ConnectError::Handshake(text) | ConnectError::Tls(text) | ConnectError::Network(text) => {
            has_alert(text)
        }
        _ => false,
    }
}

/// Which dimension of the next attempt a failure speaks about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureClass {
    /// The server certificate was not accepted (root dimension).
    ServerCertificate,
    /// The server refused our client certificate (client certificate dimension).
    ClientRefusal,
    /// Anything else.
    Other,
}

fn classify_failure(error: &ConnectError) -> FailureClass {
    if matches!(error, ConnectError::Tls(text) if text.contains("invalid peer certificate")) {
        FailureClass::ServerCertificate
    } else if is_client_certificate_refusal(error) {
        FailureClass::ClientRefusal
    } else {
        FailureClass::Other
    }
}

/// Consecutive failed attempts of any kind after which the client certificate alternates
/// (`A02.FR.28`) when the failure is not clearly a refusal.
const ALTERNATE_AFTER_FAILURES: u32 = 3;

/// `A05.FR.08`: profile-attributable connection failures after a `SecurityProfile` change
/// that make the station go back to the previous profile.
pub(super) const PROFILE_FALLBACK_AFTER_FAILURES: u32 = 3;

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
        push_bounded(&mut self.security_events, event);
        self.mark_dirty();
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
            self.security_seq += 1;
            let seq = self.security_seq;
            self.security_in_flight.push((seq, event.clone()));
            let request = SecurityEventNotificationRequest {
                event_type: event.event_type.clone(),
                timestamp: event.timestamp,
                tech_info: event.tech_info.clone(),
            };
            self.call::<SecurityEventNotification>(
                Origin::Station,
                &request,
                move |actor, result| {
                    actor.security_in_flight.retain(|(s, _)| *s != seq);
                    actor.mark_dirty();
                    match result {
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
                    }
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
    ///
    /// This starts a back-off series (A02.FR.17 to A02.FR.19): when no `CertificateSigned`
    /// arrives within `CertSigningWaitMinimum` seconds of an `Accepted` answer, the same CSR is
    /// sent again, each time after twice the previous wait, at most `CertSigningRepeatTimes`
    /// times; then the station stops until it is asked again (a new request or trigger starts a
    /// fresh series). Beyond the whitepaper text, a `Rejected` or timed-out `SignCertificate`
    /// answer is repeated the same way. A `CertificateSigned`, accepted or not, ends the series
    /// (A02.FR.20). When the timer fires with no registered connection the repeat is deferred by
    /// the same wait without being counted.
    pub(super) fn request_certificate(&mut self, reply: Reply) {
        let organization = match self.registry.cpo_name() {
            "" => "kirby-ocpp".to_owned(),
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
        self.next_sign_series += 1;
        let id = self.next_sign_series;
        self.timers.cancel(&TimerKey::CertSigning);
        self.sign_series = Some(SignSeries {
            id,
            csr,
            key,
            repeats: 0,
        });
        self.send_sign_certificate(id, Some(reply));
    }

    /// Sends `SignCertificate` for series `id` (the first request or a repeat).
    fn send_sign_certificate(&mut self, id: u64, reply: Option<Reply>) {
        let Some(series) = self.sign_series.as_ref().filter(|s| s.id == id) else {
            if let Some(reply) = reply {
                let _ = reply.send(Err(StationError::NotConnected));
            }
            return;
        };
        let csr = series.csr.clone();
        let key = series.key.clone();
        let previous = self.secrets.pending_key_pem.replace(key.clone());
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
                            actor.restore_pending_key(&key, previous);
                            actor.log(
                                LogLevel::Warn,
                                "SignCertificate was rejected by the CSMS".to_owned(),
                            );
                        }
                        actor.arm_cert_signing_retry(id);
                        let payload = serde_json::to_value(&response).unwrap_or(Value::Null);
                        Ok(Outcome::Response { payload })
                    }
                    Err(error) => {
                        actor.restore_pending_key(&key, previous);
                        actor.arm_cert_signing_retry(id);
                        Err(call_failure(error))
                    }
                };
                if let Some(reply) = reply {
                    let _ = reply.send(answer);
                }
            },
        );
    }

    /// Waits `CertSigningWaitMinimum` doubled once per repeat sent so far, then repeats; stops
    /// the series when `CertSigningRepeatTimes` repeats were sent. A superseded series does
    /// nothing.
    fn arm_cert_signing_retry(&mut self, id: u64) {
        let limit = self.registry.cert_signing_repeat_times();
        let Some(series) = self.sign_series.as_ref().filter(|s| s.id == id) else {
            return;
        };
        if series.repeats >= limit {
            self.stop_sign_series(&format!(
                "SignCertificate: no CertificateSigned after {limit} repeat(s); \
                 giving up until a new request or trigger"
            ));
            return;
        }
        let wait = self.cert_signing_wait(series.repeats);
        self.timers.set(TimerKey::CertSigning, wait);
    }

    /// The back-off after `repeats` repeats: `CertSigningWaitMinimum` doubled `repeats` times.
    fn cert_signing_wait(&self, repeats: u32) -> Duration {
        let base = self.registry.cert_signing_wait_minimum_secs();
        Duration::from_secs(base.saturating_mul(1_u64.checked_shl(repeats).unwrap_or(u64::MAX)))
    }

    /// Ends the back-off series (no timer, no repeats).
    pub(super) fn stop_sign_series(&mut self, why: &str) {
        self.timers.cancel(&TimerKey::CertSigning);
        if self.sign_series.take().is_some() {
            self.log(LogLevel::Info, why.to_owned());
        }
    }

    /// The `CertSigningWaitMinimum` back-off expired without a `CertificateSigned`.
    pub(super) fn on_cert_signing_timer(&mut self) {
        let Some(series) = self.sign_series.as_mut() else {
            return;
        };
        let id = series.id;
        let repeats = series.repeats;
        if self.link.conn.is_none() || self.state.registration != Some(RegistrationStatus::Accepted)
        {
            let wait = self.cert_signing_wait(repeats);
            self.timers.set(TimerKey::CertSigning, wait);
            return;
        }
        let limit = self.registry.cert_signing_repeat_times();
        if let Some(series) = self.sign_series.as_mut() {
            series.repeats += 1;
        }
        self.log(
            LogLevel::Warn,
            format!(
                "no CertificateSigned yet; sending SignCertificate again (repeat {}/{limit})",
                repeats + 1
            ),
        );
        self.send_sign_certificate(id, None);
    }

    /// Puts `previous` back after a failed `SignCertificate`, unless a newer request has replaced
    /// the key in the meantime (`key` is what the failed request stored).
    fn restore_pending_key(&mut self, key: &str, previous: Option<String>) {
        if self.secrets.pending_key_pem.as_deref() == Some(key) {
            self.secrets.pending_key_pem = previous;
            self.mark_secrets_dirty();
        }
    }

    /// `CertificateSigned` was accepted: `chain` with the pending `key` becomes the charge point
    /// certificate. The certificate it replaces is kept as the previous one (not when a previous
    /// one is still waiting: the last one known to work stays) until the new one has connected
    /// once; if the new one never connects, the connection alternates with the previous one
    /// (`A02.FR.28`, `A03.FR.24`) and in the end `DiscardedRenewedClientCertificate` is reported
    /// (`A02.FR.29`, `A03.FR.25`). See [`Self::client_certificate_connected`].
    pub(super) fn adopt_signed_certificate(&mut self, chain: &str, key: String) {
        if self.certs.previous_charge_point_chain.is_none()
            && let (Some(old_chain), Some(old_key)) = (
                self.certs.charge_point_chain.take(),
                self.secrets.charge_point_key_pem.take(),
            )
        {
            self.certs.previous_charge_point_chain = Some(old_chain);
            self.secrets.previous_charge_point_key_pem = Some(old_key);
        }
        self.certs.charge_point_chain = Some(chain.trim().to_owned());
        self.secrets.charge_point_key_pem = Some(key);
        self.secrets.pending_key_pem = None;
        self.use_previous_cert = false;
        self.cert_failures = 0;
        self.new_cert_refused = false;
    }

    /// A connection with a stored client certificate succeeded. The new certificate working
    /// discards the previous one; the previous one working means the new one failed: it is
    /// discarded (the previous becomes the current one again) and reported.
    fn client_certificate_connected(&mut self) {
        let used = self.connect_identity;
        let has_previous = self.certs.previous_charge_point_chain.is_some();
        self.use_previous_cert = false;
        self.cert_failures = 0;
        let refused = std::mem::take(&mut self.new_cert_refused);
        if used == StoredIdentity::Previous && has_previous && !refused {
            self.log(
                LogLevel::Info,
                "connected with the previous client certificate; the new one was not refused and stays".to_owned(),
            );
            return;
        }
        match used {
            StoredIdentity::Current if has_previous => {
                self.certs.previous_charge_point_chain = None;
                self.secrets.previous_charge_point_key_pem = None;
                self.log(
                    LogLevel::Info,
                    "the new client certificate works; the previous one was discarded".to_owned(),
                );
            }
            StoredIdentity::Previous if has_previous => {
                self.certs.charge_point_chain = self.certs.previous_charge_point_chain.take();
                self.secrets.charge_point_key_pem =
                    self.secrets.previous_charge_point_key_pem.take();
                self.log(
                    LogLevel::Warn,
                    "the new client certificate did not work; it was discarded and the previous one is used again"
                        .to_owned(),
                );
                self.security_event(
                    "DiscardedRenewedClientCertificate",
                    Some("the renewed client certificate could not establish a connection"),
                );
            }
            _ => return,
        }
        self.certs_dirty = true;
        self.mark_secrets_dirty();
        self.flush_now();
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
        // A00.FR.005, A05.FR.01, A05.FR.10: lowering is only possible from 3 to 2 and only with
        // `AllowSecurityProfileDowngrade` true; 2 or 3 to 1 (or 0) never.
        let downgrade =
            current == 3 && new == 2 && self.registry.allow_security_profile_downgrade();
        if new < current && !downgrade {
            return Err(format!(
                "the security profile cannot be lowered ({current} -> {new})"
            ));
        }
        // A05.FR.01: setting the current profile again is Rejected for profiles 2 and 3;
        // 0 and 1 stay a no-op.
        if new == current {
            return if new > 1 {
                Err(format!("the security profile is already {current}"))
            } else {
                Ok(())
            };
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

    /// Applies the security profile to the config used to open a connection. Returns which
    /// stored client certificate the connection presents.
    pub(super) fn apply_security_transport(&self, config: &mut StationConfig) -> StoredIdentity {
        let profile = self.registry.security_profile();
        config.tls.allow_wildcard_certificates = self.registry.allow_central_system_tls_wildcards();
        if matches!(profile, 1 | 2)
            && let Some(key) = self.registry.authorization_key()
            && let Ok(mut url) = url::Url::parse(&config.url)
            && url.password().is_none()
        {
            let _ = url.set_username(&config.charge_point_id);
            let _ = url.set_password(Some(key));
            config.url = url.to_string();
        }
        let mut identity = StoredIdentity::None;
        if profile == 3 && config.tls.cert_file.is_none() {
            let previous = self
                .certs
                .previous_charge_point_chain
                .clone()
                .zip(self.secrets.previous_charge_point_key_pem.clone())
                .filter(|_| self.use_previous_cert);
            let current = self
                .certs
                .charge_point_chain
                .clone()
                .zip(self.secrets.charge_point_key_pem.clone());
            let (chosen, which) = match (previous, current) {
                (Some(pair), _) => (Some(pair), StoredIdentity::Previous),
                (None, current) => (current, StoredIdentity::Current),
            };
            if let Some((chain_pem, key_pem)) = chosen {
                config.tls.identity_pem = Some(IdentityPem { chain_pem, key_pem });
                identity = which;
            }
        }
        // M05.FR.13: after a refused server certificate the next attempt trusts the fallback root.
        config.tls.extra_roots_pem = if self.use_fallback_root && self.certs.has_fallback() {
            self.certs.fallback_pems()
        } else {
            self.certs
                .root_pems(CertificateUse::CentralSystemRootCertificate)
        };
        identity
    }

    /// `A02.FR.28`: while a renewed client certificate waits for its first successful
    /// connection (a previous one is kept), failed attempts alternate between the new and the
    /// previous certificate so a bad renewal cannot strand the station. A refusal (TLS alert,
    /// HTTP 401/403, connection closed or reset right after the TLS handshake) switches at
    /// once; any other failure (timeout, refused TCP, DNS) after [`ALTERNATE_AFTER_FAILURES`]
    /// consecutive ones, so a short outage does not flip the choice constantly. Nothing is
    /// discarded here: that needs a successful connection ([`Self::client_certificate_connected`]).
    fn alternate_client_certificate(&mut self, error: &ConnectError) {
        if self.connect_identity == StoredIdentity::None
            || self.certs.previous_charge_point_chain.is_none()
        {
            self.cert_failures = 0;
            return;
        }
        let class = classify_failure(error);
        if class == FailureClass::ServerCertificate {
            // Says nothing about the client certificate: the root dimension handles it.
            return;
        }
        self.cert_failures += 1;
        let refused = class == FailureClass::ClientRefusal;
        if refused && self.connect_identity == StoredIdentity::Current {
            self.new_cert_refused = true;
        }
        if !refused && self.cert_failures < ALTERNATE_AFTER_FAILURES {
            return;
        }
        self.cert_failures = 0;
        self.use_previous_cert = !self.use_previous_cert;
        let next = if self.use_previous_cert {
            "previous"
        } else {
            "new"
        };
        let why = if refused {
            "the server refused the client certificate".to_owned()
        } else {
            format!("{ALTERNATE_AFTER_FAILURES} failed attempts in a row")
        };
        self.log(
            LogLevel::Warn,
            format!("{why}; the next attempt uses the {next} one"),
        );
    }

    /// Security events for a failed connection attempt, and the next attempt's certificate
    /// choices. `FailedToAuthenticateAtCentralSystem`, `InvalidCentralSystemCertificate`,
    /// `InvalidTLSVersion` and `InvalidTLSCipherSuite` are reported once per series of failed
    /// attempts (the whitepaper notes say that is enough and keeps the offline queue small);
    /// the series ends at the next successful connection.
    ///
    /// Returns `true` when the failure made the station fall back to the previous security
    /// profile (`A05.FR.08`): the caller reconnects at once.
    pub(super) fn report_connect_failure(&mut self, error: &ConnectError) -> bool {
        let certificate_error = classify_failure(error) == FailureClass::ServerCertificate;
        let wildcard = matches!(error, ConnectError::Tls(text) if text.contains(WILDCARD_REFUSED));
        if certificate_error && !wildcard && self.certs.has_fallback() {
            self.use_fallback_root = !self.use_fallback_root;
        }
        self.alternate_client_certificate(error);
        match error {
            ConnectError::HttpStatus {
                status: 401 | 403,
                line,
            } => {
                self.series_security_event("FailedToAuthenticateAtCentralSystem", line);
            }
            ConnectError::Tls(text) if certificate_error => {
                self.series_security_event("InvalidCentralSystemCertificate", text);
            }
            ConnectError::Tls(text) => match classify_tls_failure(text) {
                Some(TlsFailure::Version) => self.series_security_event("InvalidTLSVersion", text),
                Some(TlsFailure::CipherSuite) => {
                    self.series_security_event("InvalidTLSCipherSuite", text);
                }
                None => {}
            },
            _ => {}
        }
        self.count_profile_failure(error)
    }

    /// `A05.FR.08`: counts a profile-attributable failure while a profile change waits for its
    /// first successful connection; at the threshold the previous profile is restored.
    fn count_profile_failure(&mut self, error: &ConnectError) -> bool {
        let Some(mut pending) = self.pending_profile_switch else {
            return false;
        };
        if !self.config.security_profile_fallback {
            self.pending_profile_switch = None;
            self.mark_dirty();
            return false;
        }
        if !is_client_certificate_alert(error) {
            // "In a row": anything else (offline, timeout, close, server certificate) restarts
            // the count.
            if pending.failures != 0 {
                pending.failures = 0;
                self.pending_profile_switch = Some(pending);
                self.mark_dirty();
            }
            return false;
        }
        pending.failures += 1;
        if pending.failures < PROFILE_FALLBACK_AFTER_FAILURES {
            self.pending_profile_switch = Some(pending);
            self.mark_dirty();
            return false;
        }
        self.fall_back_to_previous_profile(pending.previous_profile);
        true
    }

    /// Restores the previous profile (and the previous `AuthorizationKey` when it changed in the
    /// meantime) without the lowering rules: the new profile never worked (`A05.FR.09` only
    /// bars lowering after a connection with it).
    fn fall_back_to_previous_profile(&mut self, previous: u8) {
        let current = self.registry.security_profile();
        self.pending_profile_switch = None;
        self.mark_dirty();
        if previous > 3 {
            self.log(
                LogLevel::Warn,
                format!(
                    "profile fallback dropped: the recorded previous profile {previous} is invalid"
                ),
            );
            return;
        }
        self.registry.set_security_profile_unchecked(previous);
        // Profile 3 does not use the key; only 1 and 2 get the previous credential back.
        let restore = matches!(previous, 1 | 2);
        if let Some(key) = self.secrets.previous_authorization_key.take() {
            if restore {
                self.registry.set_authorization_key(&key);
                self.secrets.authorization_key = Some(key);
            }
            if self.persistence_secrets_locked() {
                self.log(
                    LogLevel::Warn,
                    "the secrets file is locked: the restored AuthorizationKey is not saved during this run"
                        .to_owned(),
                );
            }
            self.mark_secrets_dirty();
        }
        self.log(
            LogLevel::Warn,
            format!(
                "security profile {current} could not connect {PROFILE_FALLBACK_AFTER_FAILURES} times; falling back to profile {previous}"
            ),
        );
        self.security_event(
            "ReconfigurationOfSecurityParameters",
            Some(&format!(
                "SecurityProfile fell back from {current} to {previous} after {PROFILE_FALLBACK_AFTER_FAILURES} failed connection attempts"
            )),
        );
        self.flush_now();
    }

    /// Reports `event_type` unless the current series of failed attempts already did.
    fn series_security_event(&mut self, event_type: &'static str, tech_info: &str) {
        if self.failure_series.insert(event_type) {
            self.security_event(event_type, Some(tech_info));
        }
    }

    /// A connection attempt succeeded: the series of failures ends and a stored client
    /// certificate that worked settles the old/new alternation.
    pub(super) fn on_connection_established(&mut self) {
        self.failure_series.clear();
        // A05.FR.09: a connection with the new profile settles the change; the previous
        // credential is not needed any more (nor is it for a profile that uses the key).
        let settled = self.pending_profile_switch.take().is_some();
        if settled {
            self.mark_dirty();
        }
        if (settled || self.registry.security_profile() >= 1)
            && self.secrets.previous_authorization_key.take().is_some()
        {
            self.mark_secrets_dirty();
        }
        self.client_certificate_connected();
    }

    /// A TLS connection was made: with the new Central System root the fallback is no longer
    /// needed (`M05.FR.12`). Only a connection that could have been verified against the
    /// installed Central System roots counts: with none installed (only `--tls-ca` or the
    /// fallback in the trust set) the fallback stays.
    pub(super) fn on_tls_connected(&mut self) {
        let via_new_root = !self.use_fallback_root
            && !self
                .certs
                .root_pems(CertificateUse::CentralSystemRootCertificate)
                .is_empty();
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
    pub(super) fn on_security_key_changed(&mut self, key: &str, previous_profile: u8) {
        if key == AUTHORIZATION_KEY {
            let new = self.registry.authorization_key().map(str::to_owned);
            // The oldest unproven key is the one to go back to (`A05.FR.08`).
            if self.registry.security_profile() == 0 {
                // Nothing uses the key at profile 0 and no switch is pending: keep no old one.
                self.secrets.previous_authorization_key = None;
            } else if self.secrets.previous_authorization_key.is_none()
                && self.secrets.authorization_key != new
            {
                self.secrets.previous_authorization_key = self
                    .secrets
                    .authorization_key
                    .clone()
                    .filter(|k| !k.is_empty());
            }
            self.secrets.authorization_key = new;
            self.mark_secrets_dirty();
        }
        if key == SECURITY_PROFILE && !self.config.security_profile_fallback {
            if self.pending_profile_switch.take().is_some() {
                self.mark_dirty();
            }
        } else if key == SECURITY_PROFILE && self.registry.security_profile() != previous_profile {
            let before = self
                .pending_profile_switch
                .map_or(previous_profile, |p| p.previous_profile);
            self.pending_profile_switch = (before != self.registry.security_profile()).then_some(
                crate::store::PendingProfileSwitch {
                    previous_profile: before,
                    failures: 0,
                },
            );
            self.mark_dirty();
        }
        if key == AUTHORIZATION_KEY || key == SECURITY_PROFILE {
            self.log(
                LogLevel::Info,
                format!("{key} changed; reconnecting so the new parameters are used"),
            );
            self.security_event("ReconfigurationOfSecurityParameters", Some(key));
            self.reconnect_for_new_security_parameters();
        }
    }

    /// A05.FR.06/07, A02.FR.08, A01: after a change of the security parameters the station
    /// disconnects and reconnects. Modelled on the hard reset: it waits for the reply and the
    /// CALLs in flight, then reconnects (the timer runs after the reply was queued).
    pub(super) fn reconnect_for_new_security_parameters(&mut self) {
        if self.link.conn.is_none() || self.hard_reset_deadline.is_some() {
            return;
        }
        self.schedule_reconnect_after_reply();
    }
}

#[cfg(test)]
mod tests {
    use rustls::{AlertDescription, Error, PeerIncompatible};

    use super::*;

    /// The text `ws::Connection::connect` builds for a handshake error.
    fn shown(error: Error) -> String {
        format!("TLS handshake failed: {}", std::io::Error::other(error))
    }

    #[test]
    fn tls_failures_are_classified_conservatively() {
        use TlsFailure::{CipherSuite, Version};
        let alert = |a| shown(Error::AlertReceived(a));
        assert_eq!(
            classify_tls_failure(&alert(AlertDescription::ProtocolVersion)),
            Some(Version)
        );
        assert_eq!(
            classify_tls_failure(&shown(Error::PeerIncompatible(
                PeerIncompatible::ServerDoesNotSupportTls12Or13
            ))),
            Some(Version)
        );
        for a in [
            AlertDescription::HandshakeFailure,
            AlertDescription::InsufficientSecurity,
        ] {
            assert_eq!(classify_tls_failure(&alert(a)), Some(CipherSuite));
        }
        assert_eq!(
            classify_tls_failure(&shown(Error::PeerIncompatible(
                PeerIncompatible::NoCipherSuitesInCommon
            ))),
            Some(CipherSuite)
        );
        // Everything else is not a version or cipher suite problem.
        for other in [
            alert(AlertDescription::BadCertificate),
            alert(AlertDescription::InternalError),
            shown(Error::InvalidCertificate(rustls::CertificateError::Expired)),
            "TLS handshake failed: unexpected end of file".to_owned(),
        ] {
            assert_eq!(classify_tls_failure(&other), None, "{other}");
        }
    }

    #[test]
    fn client_certificate_refusals_are_recognised() {
        let refusal = |a| {
            is_client_certificate_refusal(&ConnectError::Handshake(format!(
                "cannot read the upgrade response: {}",
                std::io::Error::other(Error::AlertReceived(a))
            )))
        };
        assert!(refusal(AlertDescription::UnknownCA));
        assert!(refusal(AlertDescription::BadCertificate));
        assert!(!refusal(AlertDescription::HandshakeFailure));
        assert!(!is_client_certificate_refusal(&ConnectError::Timeout));
        assert!(!is_client_certificate_refusal(&ConnectError::Network(
            "connection refused".to_owned()
        )));
    }
}
