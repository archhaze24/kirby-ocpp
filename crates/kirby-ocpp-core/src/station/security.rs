//! Security events of the Security Whitepaper (edition 4): the event type list and the bounded
//! queue of events that could not be sent yet (the station was offline). Pure data, no I/O.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ocpp::messages::{MAX_SECURITY_EVENT_TECH_INFO_LEN, MAX_SECURITY_EVENT_TYPE_LEN};

/// Event types defined by the whitepaper (appendix "Security Events").
pub const SECURITY_EVENT_TYPES: &[&str] = &[
    "FirmwareUpdated",
    "FailedToAuthenticateAtCentralSystem",
    "CentralSystemFailedToAuthenticate",
    "SettingSystemTime",
    "StartupOfTheDevice",
    "ResetOrReboot",
    "SecurityLogWasCleared",
    "ReconfigurationOfSecurityParameters",
    "MemoryExhaustion",
    "InvalidMessages",
    "AttemptedReplayAttacks",
    "TamperDetectionActivated",
    "InvalidFirmwareSignature",
    "InvalidFirmwareSigningCertificate",
    "InvalidCentralSystemCertificate",
    "InvalidChargePointCertificate",
    "InvalidTLSVersion",
    "InvalidTLSCipherSuite",
    "MaintenanceLoginAccepted",
    "MaintenanceLoginFailed",
    "DiscardedRenewedClientCertificate",
];

/// Most events kept while offline; the oldest are dropped first.
pub const MAX_QUEUED_EVENTS: usize = 100;

/// The canonical spelling of a known event type (case-insensitive match).
pub fn canonical_event_type(name: &str) -> Option<&'static str> {
    SECURITY_EVENT_TYPES
        .iter()
        .find(|known| known.eq_ignore_ascii_case(name.trim()))
        .copied()
}

/// A security event waiting to be reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSecurityEvent {
    pub event_type: String,
    pub timestamp: DateTime<Utc>,
    pub tech_info: Option<String>,
}

impl PendingSecurityEvent {
    /// A new event; `tech_info` is cleaned of control characters and cut to the schema limit.
    pub fn new(event_type: &str, timestamp: DateTime<Utc>, tech_info: Option<&str>) -> Self {
        debug_assert!(event_type.len() <= MAX_SECURITY_EVENT_TYPE_LEN);
        Self {
            event_type: event_type.to_owned(),
            timestamp,
            tech_info: tech_info.filter(|t| !t.is_empty()).map(|t| {
                strip_control(t)
                    .chars()
                    .take(MAX_SECURITY_EVENT_TECH_INFO_LEN)
                    .collect()
            }),
        }
    }
}

/// Replaces control characters (server-controlled text may carry newlines or escape codes).
pub fn strip_control(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Appends `event`, dropping the oldest entries beyond [`MAX_QUEUED_EVENTS`].
pub fn push_bounded(queue: &mut Vec<PendingSecurityEvent>, event: PendingSecurityEvent) {
    queue.push(event);
    if queue.len() > MAX_QUEUED_EVENTS {
        let excess = queue.len() - MAX_QUEUED_EVENTS;
        queue.drain(..excess);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_match_case_insensitively() {
        assert_eq!(canonical_event_type("resetorreboot"), Some("ResetOrReboot"));
        assert_eq!(canonical_event_type("Nope"), None);
        assert!(
            SECURITY_EVENT_TYPES
                .iter()
                .all(|t| t.len() <= MAX_SECURITY_EVENT_TYPE_LEN)
        );
    }

    #[test]
    fn queue_is_bounded_and_drops_the_oldest() {
        let mut queue = Vec::new();
        for n in 0..MAX_QUEUED_EVENTS + 5 {
            let info = n.to_string();
            push_bounded(
                &mut queue,
                PendingSecurityEvent::new("ResetOrReboot", DateTime::UNIX_EPOCH, Some(&info)),
            );
        }
        assert_eq!(queue.len(), MAX_QUEUED_EVENTS);
        assert_eq!(queue[0].tech_info.as_deref(), Some("5"));
    }
}
