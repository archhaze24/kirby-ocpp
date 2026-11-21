//! The EVSE and connector model and the local commands that change it.
//!
//! `--connectors N` gives N EVSEs with one connector each. Every level keeps its own
//! `operative` flag (station: `Snapshot2::availability_operative`); the connector status is
//! always derived from the flags, never stored (see [`derive_status`]).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::actor::Station2Actor;
use super::command::Outcome2;
use super::snapshot::{ConnectorSnapshot2, EvseSnapshot};
use super::status::derive_status;
use super::types::Reply2;
use crate::error::StationError;
use crate::ocpp::v2::messages::{ChangeAvailabilityStatus, ConnectorStatus};
use crate::ocpp::v2::types::WireEnum;

/// Availability of one EVSE and its connectors, as persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvseAvail {
    pub evse: u32,
    pub operative: bool,
    /// `(connector id, operative)` for each connector.
    pub connectors: Vec<(u32, bool)>,
    /// A station-level `Inoperative` request waits for a transaction to end. The same value
    /// is written to every EVSE entry; the request applies once the restored transactions
    /// have ended.
    #[serde(default)]
    pub station_inoperative_pending: bool,
    /// An EVSE-level `Inoperative` request waits for the transaction on this EVSE to end
    /// (added in format version 2).
    #[serde(default)]
    pub evse_inoperative_pending: bool,
    /// Connectors with a connector-level `Inoperative` request waiting for the transaction
    /// to end (added in format version 2).
    #[serde(default)]
    pub connectors_inoperative_pending: Vec<u32>,
}

/// One connector.
#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags of one connector; the status is derived from them"
)]
pub(crate) struct Connector {
    pub(super) id: u32,
    pub(super) operative: bool,
    pub(super) ev_connected: bool,
    pub(super) fault: bool,
    /// An `Inoperative` request waits for the transaction on the EVSE to end.
    pub(super) pending_inoperative: bool,
    /// A reservation holds the EVSE (see `reservation`).
    pub(super) reserved: bool,
    /// The status the CSMS was last told about, or would have been told while offline. Only
    /// used to notice a change; the current status is always derived.
    pub(super) accounted: ConnectorStatus,
    /// When `accounted` last changed.
    pub(super) changed_at: DateTime<Utc>,
}

/// One EVSE with its connectors.
#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags of one EVSE; the status is derived from them"
)]
pub(crate) struct Evse {
    pub(super) id: u32,
    pub(super) operative: bool,
    pub(super) connectors: Vec<Connector>,
    /// An EVSE-level `Inoperative` request waits for the transaction to end.
    pub(super) pending_inoperative: bool,
    /// A station-level `Inoperative` request waits for the transaction on this EVSE to end.
    /// The EVSE keeps its status until then.
    pub(super) pending_station_inoperative: bool,
    /// A station-level `Inoperative` request is waiting for another EVSE's transaction and
    /// this EVSE is already unavailable (G04.FR.07). Cleared when the station flag takes over
    /// or `Operative` restores the station.
    pub(super) station_hold: bool,
}

impl Evse {
    /// An operative EVSE with `connectors` idle connectors.
    pub(super) fn new(id: u32, connectors: u32, now: DateTime<Utc>) -> Self {
        Self {
            id,
            operative: true,
            connectors: (1..=connectors)
                .map(|id| Connector {
                    id,
                    operative: true,
                    ev_connected: false,
                    fault: false,
                    pending_inoperative: false,
                    reserved: false,
                    accounted: ConnectorStatus::Available,
                    changed_at: now,
                })
                .collect(),
            pending_inoperative: false,
            pending_station_inoperative: false,
            station_hold: false,
        }
    }

    /// Whether the EVSE itself allows operation: its own flag and no station-level hold.
    pub(super) fn gate(&self) -> bool {
        self.operative && !self.station_hold
    }

    fn connector(&self, id: u32) -> Option<&Connector> {
        self.connectors.iter().find(|c| c.id == id)
    }

    /// Whether anything on this EVSE waits for the end of a transaction.
    fn has_pending(&self) -> bool {
        self.pending_inoperative
            || self.pending_station_inoperative
            || self.connectors.iter().any(|c| c.pending_inoperative)
    }
}

/// What a `ChangeAvailability` addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Station,
    Evse(u32),
    Connector(u32, u32),
}

/// Applies a `ChangeAvailability` to the model. `Err(())` means the EVSE or connector does
/// not exist. `busy(evse)` tells whether a transaction runs on it.
///
/// `Operative` always applies at once and cancels a waiting `Inoperative` of the same scope.
/// `Inoperative` that would change the state during a transaction waits for its end
/// (`Scheduled`). Requesting the state a scope already has is `Accepted` (G03.FR.03).
///
/// A station-level `Inoperative` with transactions running holds every EVSE without one at
/// once; each busy EVSE follows when its own transaction ends and the station flag changes
/// after the last one (G04.FR.06/07).
pub(crate) fn apply_availability(
    evses: &mut [Evse],
    station_operative: &mut bool,
    scope: Scope,
    operative: bool,
    busy: &dyn Fn(u32) -> bool,
) -> Result<ChangeAvailabilityStatus, ()> {
    match scope {
        Scope::Station => {
            if operative {
                for evse in evses.iter_mut() {
                    evse.pending_station_inoperative = false;
                    evse.station_hold = false;
                }
                *station_operative = true;
                return Ok(ChangeAvailabilityStatus::Accepted);
            }
            if !*station_operative {
                return Ok(ChangeAvailabilityStatus::Accepted);
            }
            let mut scheduled = false;
            for evse in evses.iter_mut() {
                if busy(evse.id) {
                    evse.pending_station_inoperative = true;
                    evse.station_hold = false;
                    scheduled = true;
                } else {
                    evse.pending_station_inoperative = false;
                    evse.station_hold = true;
                }
            }
            if scheduled {
                return Ok(ChangeAvailabilityStatus::Scheduled);
            }
            for evse in evses.iter_mut() {
                evse.station_hold = false;
            }
            *station_operative = false;
            Ok(ChangeAvailabilityStatus::Accepted)
        }
        Scope::Evse(id) => {
            let in_tx = busy(id);
            let evse = evses.iter_mut().find(|e| e.id == id).ok_or(())?;
            if operative {
                evse.pending_inoperative = false;
                evse.operative = true;
            } else if !evse.operative {
                // Already inoperative: nothing to change, nothing to wait for.
            } else if in_tx {
                evse.pending_inoperative = true;
                return Ok(ChangeAvailabilityStatus::Scheduled);
            } else {
                evse.operative = false;
            }
            Ok(ChangeAvailabilityStatus::Accepted)
        }
        Scope::Connector(id, connector) => {
            let in_tx = busy(id);
            let evse = evses.iter_mut().find(|e| e.id == id).ok_or(())?;
            let c = evse
                .connectors
                .iter_mut()
                .find(|c| c.id == connector)
                .ok_or(())?;
            if operative {
                c.pending_inoperative = false;
                c.operative = true;
            } else if !c.operative {
                // Already inoperative.
            } else if in_tx {
                c.pending_inoperative = true;
                return Ok(ChangeAvailabilityStatus::Scheduled);
            } else {
                c.operative = false;
            }
            Ok(ChangeAvailabilityStatus::Accepted)
        }
    }
}

/// The transaction on `evse_id` ended: applies what waited for it. A station-level request
/// holds this EVSE at once and sets the station flag once no EVSE waits for it any more.
/// Returns whether any flag changed.
pub(crate) fn release_pending(
    evses: &mut [Evse],
    station_operative: &mut bool,
    evse_id: u32,
) -> bool {
    let Some(evse) = evses.iter_mut().find(|e| e.id == evse_id) else {
        return false;
    };
    if !evse.has_pending() {
        return false;
    }
    if evse.pending_inoperative {
        evse.pending_inoperative = false;
        evse.operative = false;
    }
    for c in &mut evse.connectors {
        if c.pending_inoperative {
            c.pending_inoperative = false;
            c.operative = false;
        }
    }
    if evse.pending_station_inoperative {
        evse.pending_station_inoperative = false;
        evse.station_hold = true;
        if !evses.iter().any(|e| e.pending_station_inoperative) {
            for e in evses.iter_mut() {
                e.station_hold = false;
            }
            *station_operative = false;
        }
    }
    true
}

impl Station2Actor {
    /// Creates the EVSEs from the configuration and fills the snapshot. Called once when the
    /// actor is built, before any state is restored.
    pub(super) fn init_evses(&mut self) {
        let now = self.clock.now_utc();
        self.evses = (1..=self.config.connector_count)
            .map(|id| Evse::new(id, 1, now))
            .collect();
        self.account_all_statuses();
        self.sync_evse_snapshot();
    }

    /// An EVSE by id.
    pub(super) fn evse(&self, id: u32) -> Option<&Evse> {
        self.evses.iter().find(|e| e.id == id)
    }

    /// An EVSE by id, mutable.
    pub(super) fn evse_mut(&mut self, id: u32) -> Option<&mut Evse> {
        self.evses.iter_mut().find(|e| e.id == id)
    }

    /// Whether the EVSE and the station are operative.
    pub(super) fn evse_operative(&self, id: u32) -> bool {
        self.state.availability_operative && self.evse(id).is_some_and(Evse::gate)
    }

    /// Whether the station-level availability is `Operative`.
    pub(super) fn station_operative(&self) -> bool {
        self.state.availability_operative
    }

    /// The `operative` flag of one connector itself.
    #[allow(
        dead_code,
        reason = "no caller yet; the device model reads the EVSE flag only"
    )]
    pub(super) fn connector_operative(&self, evse: u32, connector: u32) -> Option<bool> {
        self.evse(evse)?.connector(connector).map(|c| c.operative)
    }

    /// The derived status of a connector.
    pub(super) fn connector_status(&self, evse: u32, connector: u32) -> Option<ConnectorStatus> {
        let e = self.evse(evse)?;
        let c = e.connector(connector)?;
        Some(derive_status(
            self.state.availability_operative,
            e.gate(),
            c,
        ))
    }

    /// The `ConnectorStatus` wire text of a connector.
    pub(super) fn connector_status_text(&self, evse: u32, connector: u32) -> Option<&'static str> {
        self.connector_status(evse, connector).map(|s| s.as_str())
    }

    /// The availability flags for the state file.
    pub(super) fn availability_snapshot(&self) -> Vec<EvseAvail> {
        let station_pending = self.evses.iter().any(|e| e.pending_station_inoperative);
        self.evses
            .iter()
            .map(|e| EvseAvail {
                evse: e.id,
                operative: e.operative,
                connectors: e.connectors.iter().map(|c| (c.id, c.operative)).collect(),
                station_inoperative_pending: station_pending,
                evse_inoperative_pending: e.pending_inoperative,
                connectors_inoperative_pending: e
                    .connectors
                    .iter()
                    .filter(|c| c.pending_inoperative)
                    .map(|c| c.id)
                    .collect(),
            })
            .collect()
    }

    /// Restores the availability flags from the state file; unknown EVSEs and connectors are
    /// ignored. Statuses are recorded without a notification: the boot sends them all.
    ///
    /// A waiting station-level request is kept as pending on every EVSE; the restore of the
    /// transactions applies it ([`Self::release_restored_requests`]).
    pub(super) fn restore_availability(&mut self, station_operative: bool, saved: &[EvseAvail]) {
        self.state.availability_operative = station_operative;
        let station_pending =
            station_operative && saved.iter().any(|e| e.station_inoperative_pending);
        if station_pending {
            for evse in &mut self.evses {
                evse.pending_station_inoperative = true;
            }
        }
        for entry in saved {
            let Some(evse) = self.evse_mut(entry.evse) else {
                continue;
            };
            evse.operative = entry.operative;
            // A request only waits while the level is still operative.
            evse.pending_inoperative = entry.operative && entry.evse_inoperative_pending;
            for &(id, operative) in &entry.connectors {
                if let Some(c) = evse.connectors.iter_mut().find(|c| c.id == id) {
                    c.operative = operative;
                    c.pending_inoperative =
                        operative && entry.connectors_inoperative_pending.contains(&id);
                }
            }
        }
        self.account_all_statuses();
        self.sync_evse_snapshot();
    }

    /// Mirrors the model into `state.evses`, keeping what other modules own there.
    pub(super) fn sync_evse_snapshot(&mut self) {
        let station = self.state.availability_operative;
        for evse in &self.evses {
            let connectors = evse
                .connectors
                .iter()
                .map(|c| ConnectorSnapshot2 {
                    id: c.id,
                    status: derive_status(station, evse.gate(), c),
                    ev_connected: c.ev_connected,
                    fault: c.fault,
                })
                .collect();
            if let Some(entry) = self.state.evses.iter_mut().find(|e| e.id == evse.id) {
                entry.operative = evse.gate();
                entry.connectors = connectors;
            } else {
                self.state.evses.push(EvseSnapshot {
                    id: evse.id,
                    operative: evse.gate(),
                    connectors,
                    transaction: None,
                    meter_wh: 0,
                    reservation: None,
                    offered_w: None,
                    external_limit: false,
                    transaction_limit: None,
                    limit_reached: None,
                    total_cost: None,
                });
            }
        }
        self.state.evses.sort_by_key(|e| e.id);
    }

    /// Looks up the connectors a local command addresses: one, or all of the EVSE.
    fn target_connectors(
        &self,
        evse: u32,
        connector: Option<u32>,
    ) -> Result<Vec<u32>, StationError> {
        let e = self
            .evse(evse)
            .ok_or_else(|| StationError::InvalidArgument(format!("EVSE {evse} does not exist")))?;
        match connector {
            None => Ok(e.connectors.iter().map(|c| c.id).collect()),
            Some(id) if e.connector(id).is_some() => Ok(vec![id]),
            Some(id) => Err(StationError::InvalidArgument(format!(
                "connector {id} does not exist on EVSE {evse}"
            ))),
        }
    }

    /// Sets a flag on the connectors of `evse` through `set`; the local commands share this.
    fn edit_connectors(&mut self, evse: u32, connectors: &[u32], set: impl Fn(&mut Connector)) {
        if let Some(e) = self.evse_mut(evse) {
            e.connectors
                .iter_mut()
                .filter(|c| connectors.contains(&c.id))
                .for_each(set);
        }
    }

    /// `Command2::PlugIn`.
    pub(super) fn cmd_plug_in(&mut self, evse: u32, reply: Reply2) {
        let result = self.plug(evse, true);
        if result.is_ok() {
            self.on_ev_connected(evse);
        }
        // The requester may have gone away; nothing to do about that.
        let _ = reply.send(result.map(|()| Outcome2::Done));
    }

    /// `Command2::Unplug`.
    pub(super) fn cmd_unplug(&mut self, evse: u32, reply: Reply2) {
        let result = self.plug(evse, false);
        if result.is_ok() {
            self.on_ev_disconnected(evse);
            // After the transaction ended: a request that waited for it is applied by now, so
            // the CSMS gets the final status only.
            self.after_model_change();
        }
        let _ = reply.send(result.map(|()| Outcome2::Done));
    }

    /// Connects or disconnects the EV on the (only) connector of `evse`. Plugging in reports
    /// the status change here; the unplug command reports it after the transaction handling.
    fn plug(&mut self, evse: u32, connected: bool) -> Result<(), StationError> {
        let ids = self.target_connectors(evse, None)?;
        let already = self
            .evse(evse)
            .is_some_and(|e| e.connectors.iter().all(|c| c.ev_connected == connected));
        if already {
            return Err(StationError::InvalidState {
                connector: None,
                reason: if connected {
                    format!("an EV is already connected to EVSE {evse}")
                } else {
                    format!("no EV is connected to EVSE {evse}")
                },
            });
        }
        self.edit_connectors(evse, &ids, |c| c.ev_connected = connected);
        if connected {
            self.after_model_change();
        }
        Ok(())
    }

    /// `Command2::Fault`.
    pub(super) fn cmd_fault(&mut self, evse: u32, connector: Option<u32>, reply: Reply2) {
        let result = self.set_fault(evse, connector, true);
        if result.is_ok() {
            self.on_fault(evse);
        }
        let _ = reply.send(result.map(|()| Outcome2::Done));
    }

    /// `Command2::ClearFault`.
    pub(super) fn cmd_clear_fault(&mut self, evse: u32, connector: Option<u32>, reply: Reply2) {
        let result = self.set_fault(evse, connector, false);
        let _ = reply.send(result.map(|()| Outcome2::Done));
    }

    fn set_fault(
        &mut self,
        evse: u32,
        connector: Option<u32>,
        fault: bool,
    ) -> Result<(), StationError> {
        let ids = self.target_connectors(evse, connector)?;
        self.edit_connectors(evse, &ids, |c| c.fault = fault);
        self.after_model_change();
        Ok(())
    }

    /// Mirrors the model into the snapshot, sends the status changes and schedules a write.
    pub(super) fn after_model_change(&mut self) {
        self.sync_evse_snapshot();
        self.notify_status_changes();
        self.mark_dirty();
    }

    /// The transaction on `evse` ended: applies an availability change that waited for it.
    pub(super) fn on_transaction_ended(&mut self, evse: u32) {
        let mut station = self.state.availability_operative;
        if release_pending(&mut self.evses, &mut station, evse) {
            self.state.availability_operative = station;
            self.after_model_change();
        }
    }

    /// Applies availability requests waiting for transactions that did not resume. The
    /// statuses are recorded without a notification, the boot sends them all.
    pub(super) fn release_restored_requests(&mut self) {
        let ids: Vec<u32> = self.evses.iter().map(|e| e.id).collect();
        let mut station = self.state.availability_operative;
        let mut changed = false;
        for id in ids {
            if !self.evse_busy(id) {
                changed |= release_pending(&mut self.evses, &mut station, id);
            }
        }
        if changed {
            self.state.availability_operative = station;
            self.account_all_statuses();
            self.sync_evse_snapshot();
            self.mark_dirty();
        }
    }

    /// Whether a transaction runs on `evse`.
    pub(super) fn evse_busy(&self, evse: u32) -> bool {
        self.active_tx(evse).is_some()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn evses(n: u32) -> Vec<Evse> {
        let now = Utc.timestamp_opt(0, 0).single().expect("valid time");
        (1..=n).map(|id| Evse::new(id, 1, now)).collect()
    }

    fn apply(
        evses: &mut [Evse],
        station: &mut bool,
        scope: Scope,
        operative: bool,
        busy: &[u32],
    ) -> ChangeAvailabilityStatus {
        apply_availability(evses, station, scope, operative, &|id| busy.contains(&id))
            .expect("target exists")
    }

    #[test]
    fn same_state_is_accepted_without_change() {
        let mut e = evses(1);
        let mut station = true;
        assert_eq!(
            apply(&mut e, &mut station, Scope::Evse(1), true, &[1]),
            ChangeAvailabilityStatus::Accepted
        );
        assert_eq!(
            apply(&mut e, &mut station, Scope::Connector(1, 1), true, &[]),
            ChangeAvailabilityStatus::Accepted
        );
        assert_eq!(
            apply(&mut e, &mut station, Scope::Station, true, &[1]),
            ChangeAvailabilityStatus::Accepted
        );
    }

    #[test]
    fn inoperative_applies_at_once_when_idle() {
        let mut e = evses(2);
        let mut station = true;
        apply(&mut e, &mut station, Scope::Evse(1), false, &[]);
        assert!(!e[0].operative && e[1].operative);
        apply(&mut e, &mut station, Scope::Connector(2, 1), false, &[]);
        assert!(!e[1].connectors[0].operative && e[1].operative);
        apply(&mut e, &mut station, Scope::Station, false, &[]);
        assert!(!station);
    }

    #[test]
    fn inoperative_waits_for_a_transaction() {
        let mut e = evses(2);
        let mut station = true;
        assert_eq!(
            apply(&mut e, &mut station, Scope::Evse(1), false, &[1]),
            ChangeAvailabilityStatus::Scheduled
        );
        assert!(e[0].operative && e[0].pending_inoperative);
        assert_eq!(
            apply(&mut e, &mut station, Scope::Connector(1, 1), false, &[1]),
            ChangeAvailabilityStatus::Scheduled
        );
        assert!(release_pending(&mut e, &mut station, 1));
        assert!(!e[0].operative && !e[0].connectors[0].operative);
        assert!(!e[0].has_pending());
        assert!(!release_pending(&mut e, &mut station, 1));
    }

    #[test]
    fn operative_cancels_a_waiting_request() {
        let mut e = evses(1);
        let mut station = true;
        apply(&mut e, &mut station, Scope::Evse(1), false, &[1]);
        assert_eq!(
            apply(&mut e, &mut station, Scope::Evse(1), true, &[1]),
            ChangeAvailabilityStatus::Accepted
        );
        assert!(!release_pending(&mut e, &mut station, 1));
        assert!(e[0].operative);
    }

    #[test]
    fn station_request_applies_after_the_last_transaction() {
        let mut e = evses(3);
        let mut station = true;
        assert_eq!(
            apply(&mut e, &mut station, Scope::Station, false, &[1, 2]),
            ChangeAvailabilityStatus::Scheduled
        );
        assert!(station);
        // The idle EVSE is held at once, the busy ones are left alone.
        assert!(e[0].gate() && e[1].gate() && !e[2].gate() && e[2].station_hold);
        assert!(e[0].pending_station_inoperative && e[1].pending_station_inoperative);
        assert!(release_pending(&mut e, &mut station, 1));
        assert!(station && e[0].station_hold && !e[1].station_hold);
        assert!(release_pending(&mut e, &mut station, 2));
        assert!(!station);
        assert!(e.iter().all(|evse| !evse.station_hold && evse.gate()));
        // Operative restores the station and keeps EVSE flags independent.
        apply(&mut e, &mut station, Scope::Station, true, &[]);
        assert!(station);
    }

    #[test]
    fn station_request_during_a_transaction_holds_only_the_idle_evses() {
        let mut e = evses(2);
        let mut station = true;
        assert_eq!(
            apply(&mut e, &mut station, Scope::Station, false, &[1]),
            ChangeAvailabilityStatus::Scheduled
        );
        assert!(e[0].gate() && e[0].pending_station_inoperative);
        assert!(!e[1].gate() && !e[1].pending_station_inoperative);
        // Operative before the transaction ends lifts the hold and the wait.
        assert_eq!(
            apply(&mut e, &mut station, Scope::Station, true, &[1]),
            ChangeAvailabilityStatus::Accepted
        );
        assert!(e.iter().all(|evse| evse.gate() && !evse.has_pending()));
        assert!(!release_pending(&mut e, &mut station, 1));
    }

    #[test]
    fn evse_restore_keeps_connector_flag() {
        let mut e = evses(1);
        let mut station = true;
        apply(&mut e, &mut station, Scope::Connector(1, 1), false, &[]);
        apply(&mut e, &mut station, Scope::Evse(1), false, &[]);
        apply(&mut e, &mut station, Scope::Evse(1), true, &[]);
        assert!(e[0].operative && !e[0].connectors[0].operative);
    }

    #[test]
    fn unknown_target_is_an_error() {
        let mut e = evses(1);
        let mut station = true;
        let busy = |_: u32| false;
        assert!(apply_availability(&mut e, &mut station, Scope::Evse(9), false, &busy).is_err());
        assert!(
            apply_availability(&mut e, &mut station, Scope::Connector(1, 2), false, &busy).is_err()
        );
    }
}
