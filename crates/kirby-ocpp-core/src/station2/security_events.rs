//! Critical security events the station raises by itself (A04.FR.01).
//!
//! * `StartupOfTheDevice` once per station start, after the first accepted boot.
//! * `ResetOrReboot` after every reboot caused by a `Reset`, after the boot that follows it
//!   was accepted.
//!
//! An event is stamped when it happens and waits in a small outbox until the registration is
//! accepted: the boot may be answered `Pending` or `Rejected` first, or the link may drop. The
//! outbox is sent after the statuses and the transaction events of an accepted boot or
//! reconnect. An event whose CALL was cut short by a closed link goes back to the outbox
//! (A04.FR.02), so delivery is at least once: the CSMS may see the event twice when the link
//! closed after the CALL left but before the answer arrived. The outbox is not persisted: an
//! event raised before a process stop that was never delivered is lost, and so is one whose
//! CALL failed with a timeout or an error answer.

use chrono::{DateTime, Utc};

use super::actor::Station2Actor;
use super::types::Origin2;
use crate::error::CallError;
use crate::ocpp::v2::messages::{SecurityEventNotification, SecurityEventNotificationRequest};
use crate::station::LogLevel;

/// Events kept while the registration is not accepted; the oldest is dropped beyond this.
const OUTBOX_CAP: usize = 16;

/// A security event waiting for delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingSecurityEvent {
    kind: &'static str,
    timestamp: DateTime<Utc>,
}

impl Station2Actor {
    /// Records that the event `kind` happened now; it is sent once the registration is accepted.
    pub(super) fn raise_security_event(&mut self, kind: &'static str) {
        let timestamp = self.clock.now_utc();
        self.push_security_event(PendingSecurityEvent { kind, timestamp });
    }

    fn push_security_event(&mut self, event: PendingSecurityEvent) {
        let outbox = &mut self.link.security_outbox;
        if outbox.len() >= OUTBOX_CAP {
            outbox.remove(0);
        }
        outbox.push(event);
    }

    /// Sends every waiting event. Called when the registration was accepted.
    pub(super) fn flush_security_events(&mut self) {
        for event in std::mem::take(&mut self.link.security_outbox) {
            let request = SecurityEventNotificationRequest {
                kind: event.kind.to_owned(),
                timestamp: event.timestamp,
                tech_info: None,
                custom_data: None,
            };
            self.call::<SecurityEventNotification>(
                Origin2::Station,
                &request,
                move |actor, result| match result {
                    Ok(_) => {}
                    Err(
                        CallError::Closed { .. }
                        | CallError::NotConnected
                        | CallError::NotRegistered { .. },
                    ) => actor.push_security_event(event),
                    Err(error) => actor.log(
                        LogLevel::Warn,
                        format!(
                            "SecurityEventNotification {} not delivered: {error}",
                            event.kind
                        ),
                    ),
                },
            );
        }
    }
}
