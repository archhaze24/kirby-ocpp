//! Reservation timers and restore (rules: [`super::reservation`]).
//!
//! One `ReservationExpiry(connector)` timer per live reservation, armed with
//! `next_expiry_delay` (never longer than 24 h; an early wake-up re-arms). On wake-up
//! `expire_due` releases what has expired and the released connectors are notified.

use super::actor::{StationActor, TimerKey};
use super::event::LogLevel;
use super::reservation::expire_due;
use crate::ids::ConnectorId;

impl StationActor {
    /// Arms (or fires at once) the expiry timer of the reservation on `id`, if any.
    pub(super) fn arm_reservation(&mut self, id: ConnectorId) {
        let now = self.clock.now_utc();
        let delay = self
            .find_connector(id)
            .and_then(|c| c.reservation.as_ref())
            .map(|r| r.next_expiry_delay(now));
        match delay {
            Some(Some(delay)) => self.timers.set(TimerKey::ReservationExpiry(id), delay),
            Some(None) => self.on_reservation_timer(id),
            None => {
                self.timers.cancel(&TimerKey::ReservationExpiry(id));
            }
        }
    }

    /// Releases expired reservations (all connectors) and re-arms the live ones.
    pub(super) fn on_reservation_timer(&mut self, _id: ConnectorId) {
        let released = self.release_expired_reservations();
        let live: Vec<ConnectorId> = self
            .state
            .connectors
            .iter()
            .filter(|c| c.reservation.is_some())
            .map(|c| c.id)
            .collect();
        for id in live {
            self.arm_reservation(id);
        }
        drop(released);
    }

    /// `expire_due` plus notifications; returns the released connectors.
    pub(super) fn release_expired_reservations(&mut self) -> Vec<ConnectorId> {
        let now = self.clock.now_utc();
        let released = expire_due(&mut self.state.connectors, now);
        for id in &released {
            self.timers.cancel(&TimerKey::ReservationExpiry(*id));
            self.log(
                LogLevel::Warn,
                format!("reservation on connector {id} expired"),
            );
            self.status_changed(*id);
        }
        if !released.is_empty() {
            self.flush_now();
        }
        released
    }

    /// After the persisted state was restored: expire what ran out while the station was
    /// down, re-time the live reservations, re-arm the meters.
    pub(super) fn after_restore(&mut self) {
        self.release_expired_reservations();
        let ids: Vec<ConnectorId> = self
            .state
            .connectors
            .iter()
            .filter(|c| c.reservation.is_some())
            .map(|c| c.id)
            .collect();
        for id in ids {
            self.arm_reservation(id);
        }
        self.reschedule_metering();
    }
}
