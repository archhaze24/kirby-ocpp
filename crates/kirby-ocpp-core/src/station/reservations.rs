//! Reservation timers and restore (rules: [`super::reservation`]).
//!
//! One `ReservationExpiry(connector)` timer per live reservation, armed with
//! `next_expiry_delay` (never longer than 24 h; an early wake-up re-arms); connector `0` carries
//! the timer of a connector-0 reservation. On wake-up `expire_due` releases what has expired and
//! the released connectors are notified.

use super::actor::{StationActor, TimerKey};
use super::event::LogLevel;
use super::reservation::{expire_due, sync_zero_hold};
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

    /// Ids of everything that carries a reservation: connector `0` first, then the connectors.
    fn reserved_ids(&self) -> Vec<ConnectorId> {
        std::iter::once(&self.state.charge_point)
            .chain(self.state.connectors.iter())
            .filter(|c| c.reservation.is_some())
            .map(|c| c.id)
            .collect()
    }

    /// Releases expired reservations (all connectors) and re-arms the live ones.
    pub(super) fn on_reservation_timer(&mut self, _id: ConnectorId) {
        let released = self.release_expired_reservations();
        for id in self.reserved_ids() {
            self.arm_reservation(id);
        }
        drop(released);
    }

    /// `expire_due` plus notifications; returns the released connectors (`0` for an expired
    /// connector-0 reservation).
    pub(super) fn release_expired_reservations(&mut self) -> Vec<ConnectorId> {
        let now = self.clock.now_utc();
        let mut released = expire_due(&mut self.state.connectors, now);
        if self
            .state
            .charge_point
            .reservation
            .as_ref()
            .is_some_and(|r| r.is_expired(now))
        {
            self.state.charge_point.reservation = None;
            released.push(ConnectorId::STATION);
        }
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

    /// Holds the connector that must stay available for a connector-0 reservation and releases
    /// a hold that is no longer needed (see [`super::reservation::sync_zero_hold`]). Runs after
    /// every event, because any availability, plug or transaction change can move the hold.
    pub(super) fn sync_zero_hold(&mut self) {
        let zero = self.state.charge_point.reservation.clone();
        if zero.is_none() {
            return;
        }
        let now = self.clock.now_utc();
        let changed = sync_zero_hold(&mut self.state.connectors, zero.as_ref(), now);
        if changed.is_empty() {
            return;
        }
        self.mark_dirty();
        for id in changed {
            self.status_changed(id);
        }
    }

    /// The connector-0 reservation that `id_tag` (with its `parent`) consumes by starting a
    /// transaction, if any.
    pub(super) fn zero_reservation_for(
        &self,
        id_tag: &crate::ids::IdTag,
        parent: Option<&crate::ids::IdTag>,
    ) -> Option<crate::ids::ReservationId> {
        let now = self.clock.now_utc();
        self.state
            .charge_point
            .reservation
            .as_ref()
            .filter(|r| !r.is_expired(now) && r.accepts(id_tag, parent))
            .map(|r| r.id)
    }

    /// After the persisted state was restored: expire what ran out while the station was
    /// down, re-time the live reservations, re-arm the meters.
    pub(super) fn after_restore(&mut self) {
        self.release_expired_reservations();
        for id in self.reserved_ids() {
            self.arm_reservation(id);
        }
        self.reschedule_metering();
    }
}
