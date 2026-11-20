//! Reservations (H01 `ReserveNow`, H02 `CancelReservation`, H03 use, H04 expiry).
//!
//! A reservation holds an EVSE (or, without an EVSE id, any one EVSE) for an `idToken` and
//! optionally its `groupIdToken` until it expires. At most one reservation exists per EVSE,
//! and at most as many reservations without an EVSE as the station has EVSEs.
//!
//! * `ReserveNow` is answered `Accepted` when an EVSE can be held (H01.FR.03/04/06):
//!   a reservation with a known id replaces that one (H01.FR.02). Otherwise the answer follows
//!   the state of the targeted EVSEs: `Faulted`, `Unavailable` or `Occupied` (an EVSE that is
//!   reserved, in use, or needed for another reservation; H01.FR.11/12/14), and `Rejected` for
//!   a disabled `ReservationCtrlr`, an unknown EVSE, an unknown connector type or an expiry in
//!   the past. All connectors of the station are `cType2`.
//! * While a reservation holds an EVSE, its connectors report `Reserved` (H01.FR.20/23). An
//!   EV that is plugged in makes the connector `Occupied` again. Reservations without an EVSE
//!   report nothing until as many reservations as free EVSEs exist; then every free EVSE is
//!   `Reserved`.
//! * Only the reserving token, or one with the reserved group, starts on a held EVSE (H03).
//!   Using the reservation consumes it: the first transaction event of that start carries the
//!   `reservationId` (H01.FR.15). Another token gets the status `NotAtThisLocation`, or a
//!   rejected remote start.
//! * A reservation ends with `ReservationStatusUpdate` `Expired` at its expiry (H04.FR.01) and
//!   `Removed` when its EVSE becomes `Faulted` or `Unavailable` (H01.FR.16/17). Both are only
//!   reported while the station is registered: an end that happens offline is reported after
//!   the next accepted boot or reconnect, together with the statuses.
//! * `ReservationCtrlr.Enabled` set to `false` keeps the entries but ignores them: nothing is
//!   held and `ReserveNow` is `Rejected`. They still expire.
//!
//! Expiry uses the station clock. One timer waits for the earliest expiry; every change of a
//! connector status also checks the list.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::actor::{Station2Actor, TimerKey2};
use super::evse::Evse;
use super::types::{IdTokenRef, Origin2};
use crate::ocpp::responder::Responder;
use crate::ocpp::v2::messages::{
    CancelReservation, CancelReservationRequest, CancelReservationResponse,
    CancelReservationStatus, ReservationStatusUpdate, ReservationStatusUpdateRequest,
    ReservationUpdateStatus, ReserveNow, ReserveNowRequest, ReserveNowResponse, ReserveNowStatus,
};
use crate::ocpp::v2::types::{StatusInfo, WireEnum};
use crate::station::LogLevel;

/// `ReservationCtrlr` as named in the device model.
pub(super) const COMPONENT: &str = "ReservationCtrlr";

/// The type of every connector of the station (`Connector.ConnectorType`).
const CONNECTOR_TYPE: &str = "cType2";

/// A reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub id: i32,
    pub expires_at: DateTime<Utc>,
    /// The reserved EVSE; `None` holds any one EVSE.
    pub evse: Option<u32>,
    pub connector_type: Option<String>,
    pub id_token: IdTokenRef,
    /// The text of the `groupIdToken`.
    pub group_id_token: Option<String>,
}

impl Reservation {
    /// Whether a token (with the group the station knows for it) may use the reservation.
    fn matches(&self, token: &IdTokenRef, group: Option<&str>) -> bool {
        self.id_token == *token
            || self
                .group_id_token
                .as_deref()
                .is_some_and(|reserved| group == Some(reserved))
    }
}

/// The reservations as persisted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReservationPersist {
    pub reservations: Vec<Reservation>,
}

/// The reservations held at run time.
#[derive(Debug, Default)]
pub(crate) struct ReservationState {
    list: Vec<Reservation>,
    /// Reservations used by an authorization whose first transaction event is still to be
    /// built, by EVSE.
    claims: BTreeMap<u32, Reservation>,
}

/// What a `ReserveNow` sees on one EVSE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvseState {
    Available,
    Reserved,
    Occupied,
    Faulted,
    Unavailable,
}

/// The reservations that count at one moment.
struct Situation {
    /// EVSEs held by a reservation.
    held: BTreeSet<u32>,
    /// Reservations that hold any one EVSE.
    unspecified: usize,
}

fn status_info(reason: &str) -> StatusInfo {
    StatusInfo {
        reason_code: reason.to_owned(),
        additional_info: None,
        custom_data: None,
    }
}

impl Station2Actor {
    /// `ReservationCtrlr.Enabled`.
    fn reservations_enabled(&self) -> bool {
        self.dm_enabled(COMPONENT)
    }

    /// The reservations in force: none while disabled, none past their expiry. A reservation
    /// with the id `except` is left out (it is being replaced).
    fn situation(&self, except: Option<i32>) -> Situation {
        let mut situation = Situation {
            held: BTreeSet::new(),
            unspecified: 0,
        };
        if !self.reservations_enabled() {
            return situation;
        }
        let now = self.clock.now_utc();
        for reservation in &self.reservations.list {
            if reservation.expires_at <= now || Some(reservation.id) == except {
                continue;
            }
            match reservation.evse {
                Some(evse) => {
                    situation.held.insert(evse);
                }
                None => situation.unspecified += 1,
            }
        }
        situation
    }

    fn evse_state(&self, evse: &Evse, held: &BTreeSet<u32>) -> EvseState {
        let operative = self.state.availability_operative && evse.gate();
        if evse.connectors.iter().any(|c| c.fault) {
            EvseState::Faulted
        } else if !operative || evse.connectors.iter().any(|c| !c.operative) {
            EvseState::Unavailable
        } else if evse.connectors.iter().any(|c| c.ev_connected)
            || self.active_tx(evse.id).is_some()
            || self.authorized_token(evse.id).is_some()
        {
            EvseState::Occupied
        } else if held.contains(&evse.id) {
            EvseState::Reserved
        } else {
            EvseState::Available
        }
    }

    /// The EVSEs nobody holds and nobody uses.
    fn free_evses(&self, situation: &Situation) -> Vec<u32> {
        self.evses
            .iter()
            .filter(|evse| self.evse_state(evse, &situation.held) == EvseState::Available)
            .map(|evse| evse.id)
            .collect()
    }

    // ---- H03: use -----------------------------------------------------------------------

    /// Whether the reservations keep `token` (with its group, when known) from starting on
    /// `evse`.
    pub(super) fn reservation_blocks(
        &self,
        evse: u32,
        token: &IdTokenRef,
        group: Option<&str>,
    ) -> bool {
        if !self.reservations_enabled() {
            return false;
        }
        let now = self.clock.now_utc();
        let live: Vec<&Reservation> = self
            .reservations
            .list
            .iter()
            .filter(|reservation| reservation.expires_at > now)
            .collect();
        if let Some(held) = live
            .iter()
            .find(|reservation| reservation.evse == Some(evse))
        {
            return !held.matches(token, group);
        }
        // Without a hold on this EVSE, the free EVSEs must stay enough for the reservations
        // that are not this token's.
        let others = live
            .iter()
            .filter(|reservation| reservation.evse.is_none() && !reservation.matches(token, group))
            .count();
        if others == 0 {
            return false;
        }
        let free = self.free_evses(&self.situation(None));
        free.contains(&evse) && free.len() <= others
    }

    /// Consumes the reservation `token` holds for `evse`, if there is one, ahead of the
    /// authorization that starts there.
    pub(super) fn reservation_claim(&mut self, evse: u32, token: &IdTokenRef, group: Option<&str>) {
        if !self.reservations_enabled() {
            return;
        }
        let now = self.clock.now_utc();
        let usable = |reservation: &&Reservation| {
            reservation.expires_at > now
                && reservation.matches(token, group)
                && reservation.evse.is_none_or(|held| held == evse)
        };
        // The EVSE's own reservation first, then the exact token before the group.
        let position = self
            .reservations
            .list
            .iter()
            .position(|r| usable(&r) && r.evse == Some(evse))
            .or_else(|| {
                self.reservations
                    .list
                    .iter()
                    .position(|r| usable(&r) && r.id_token == *token)
            })
            .or_else(|| self.reservations.list.iter().position(|r| usable(&r)));
        let Some(position) = position else {
            return;
        };
        let used = self.reservations.list.remove(position);
        self.log(
            LogLevel::Info,
            format!("reservation {} used on EVSE {evse}", used.id),
        );
        self.reservations.claims.insert(evse, used);
        self.mark_dirty();
        self.notify_status_changes();
    }

    /// The authorization that claimed a reservation on `evse` failed: the reservation stands.
    pub(super) fn reservation_release_claim(&mut self, evse: u32) {
        if let Some(reservation) = self.reservations.claims.remove(&evse) {
            self.reservations.list.push(reservation);
            self.mark_dirty();
            self.notify_status_changes();
        }
    }

    /// The authorization on `evse` ended without a transaction event: the claim is void.
    pub(super) fn reservation_forget_claim(&mut self, evse: u32) {
        self.reservations.claims.remove(&evse);
    }

    /// The id for the next transaction event on `evse`, once.
    pub(super) fn take_reservation_claim(&mut self, evse: u32) -> Option<i32> {
        self.reservations
            .claims
            .remove(&evse)
            .map(|reservation| reservation.id)
    }

    // ---- H04: the end ---------------------------------------------------------------------

    /// Ends the reservations that expired or lost their EVSE, then brings the connector
    /// flags and the timer up to date. Called before every status comparison.
    pub(super) fn reservation_review(&mut self) {
        let now = self.clock.now_utc();
        let reportable = self.link.conn.is_some()
            && self
                .may_send("ReservationStatusUpdate", Origin2::Station)
                .is_ok();
        let mut ended: Vec<(i32, ReservationUpdateStatus)> = Vec::new();
        if reportable {
            let enabled = self.reservations_enabled();
            let none = BTreeSet::new();
            ended = self
                .reservations
                .list
                .iter()
                .filter_map(|reservation| {
                    if reservation.expires_at <= now {
                        return Some((reservation.id, ReservationUpdateStatus::Expired));
                    }
                    let evse = self.evse(reservation.evse?)?;
                    let lost = matches!(
                        self.evse_state(evse, &none),
                        EvseState::Faulted | EvseState::Unavailable
                    );
                    (enabled && lost).then_some((reservation.id, ReservationUpdateStatus::Removed))
                })
                .collect();
            self.reservations
                .list
                .retain(|reservation| !ended.iter().any(|(id, _)| *id == reservation.id));
        }
        for (id, status) in &ended {
            self.report_reservation_end(*id, *status);
        }
        let flags_changed = self.reservation_sync_flags();
        if !ended.is_empty() {
            self.mark_dirty();
        }
        if flags_changed || !ended.is_empty() {
            self.sync_evse_snapshot();
        }
        self.reservation_arm();
    }

    fn report_reservation_end(&mut self, id: i32, status: ReservationUpdateStatus) {
        self.log(
            LogLevel::Info,
            format!("reservation {id} ended: {}", status.as_str()),
        );
        let request = ReservationStatusUpdateRequest {
            reservation_id: id,
            reservation_update_status: status,
            custom_data: None,
        };
        self.call::<ReservationStatusUpdate>(Origin2::Station, &request, move |actor, result| {
            if let Err(error) = result {
                actor.log(
                    LogLevel::Warn,
                    format!("ReservationStatusUpdate for {id} failed: {error}"),
                );
            }
        });
    }

    /// Arms the timer for the next expiry that is still ahead.
    fn reservation_arm(&mut self) {
        let now = self.clock.now_utc();
        let next = self
            .reservations
            .list
            .iter()
            .map(|reservation| reservation.expires_at)
            .filter(|at| *at > now)
            .min();
        match next {
            Some(at) => {
                let delay = (at - now).to_std().unwrap_or(Duration::ZERO);
                self.timers.set(TimerKey2::ReservationExpiry, delay);
            }
            None => {
                self.timers.cancel(&TimerKey2::ReservationExpiry);
            }
        }
    }

    /// The timer of the next expiry fired.
    pub(super) fn on_reservation_timer(&mut self) {
        self.notify_status_changes();
    }

    /// Marks the connectors of held EVSEs as reserved. Returns whether a flag changed.
    pub(super) fn reservation_sync_flags(&mut self) -> bool {
        let situation = self.situation(None);
        let free = self.free_evses(&situation);
        // A reservation for any EVSE shows once the free EVSEs are all needed for those.
        let saturated = situation.unspecified > 0 && situation.unspecified >= free.len();
        let mut changed = false;
        for evse in &mut self.evses {
            let hold = situation.held.contains(&evse.id) || (saturated && free.contains(&evse.id));
            for connector in &mut evse.connectors {
                if connector.reserved != hold {
                    connector.reserved = hold;
                    changed = true;
                }
            }
        }
        changed
    }

    // ---- persistence ----------------------------------------------------------------------

    /// The reservations for the state file.
    pub(super) fn reservations_persist(&self) -> ReservationPersist {
        ReservationPersist {
            reservations: self.reservations.list.clone(),
        }
    }

    /// Takes back the persisted reservations at start: one per known EVSE and at most as many
    /// without an EVSE as the station has EVSEs.
    pub(super) fn reservations_restore(&mut self, persisted: ReservationPersist) {
        let evse_count = self.evses.len();
        let mut held = BTreeSet::new();
        let mut ids = BTreeSet::new();
        let mut unspecified = 0;
        let mut list = Vec::new();
        for reservation in persisted.reservations {
            if !ids.insert(reservation.id) {
                continue;
            }
            if let Some(evse) = reservation.evse {
                if self.evse(evse).is_none() || !held.insert(evse) {
                    continue;
                }
            } else if unspecified >= evse_count {
                continue;
            } else {
                unspecified += 1;
            }
            list.push(reservation);
        }
        self.reservations.list = list;
        self.account_all_statuses();
        self.reservation_arm();
    }
}

// ---- CALL handlers ---------------------------------------------------------------------

impl Station2Actor {
    /// The answer to a `ReserveNow` and the reason for a refusal.
    fn reserve_decision(&self, request: &ReserveNowRequest) -> (ReserveNowStatus, &'static str) {
        if !self.reservations_enabled() {
            return (ReserveNowStatus::Rejected, "ReservationDisabled");
        }
        if request.expiry_date_time <= self.clock.now_utc() {
            return (ReserveNowStatus::Rejected, "AlreadyExpired");
        }
        if request
            .connector_type
            .as_deref()
            .is_some_and(|kind| kind != CONNECTOR_TYPE)
        {
            return (ReserveNowStatus::Rejected, "NoSuchConnectorType");
        }
        let situation = self.situation(Some(request.id));
        let free = self.free_evses(&situation);
        let Some(requested) = request.evse_id else {
            // H01.FR.04/18: any EVSE, as long as a free one remains for each reservation.
            if free.len() > situation.unspecified {
                return (ReserveNowStatus::Accepted, "");
            }
            let states: Vec<EvseState> = self
                .evses
                .iter()
                .map(|evse| self.evse_state(evse, &situation.held))
                .collect();
            return if states.is_empty() {
                (ReserveNowStatus::Rejected, "NoEvse")
            } else if states.iter().any(|state| {
                matches!(
                    state,
                    EvseState::Available | EvseState::Reserved | EvseState::Occupied
                )
            }) {
                (ReserveNowStatus::Occupied, "Occupied")
            } else if states.contains(&EvseState::Unavailable) {
                (ReserveNowStatus::Unavailable, "Unavailable")
            } else {
                (ReserveNowStatus::Faulted, "Faulted")
            };
        };
        let Some(evse) = u32::try_from(requested)
            .ok()
            .filter(|id| *id > 0)
            .and_then(|id| self.evse(id))
        else {
            return (ReserveNowStatus::Rejected, "UnknownEvse");
        };
        match self.evse_state(evse, &situation.held) {
            // The EVSE may only go if the free ones still cover the unspecified reservations.
            EvseState::Available if free.len() > situation.unspecified => {
                (ReserveNowStatus::Accepted, "")
            }
            EvseState::Available | EvseState::Reserved | EvseState::Occupied => {
                (ReserveNowStatus::Occupied, "Occupied")
            }
            EvseState::Faulted => (ReserveNowStatus::Faulted, "Faulted"),
            EvseState::Unavailable => (ReserveNowStatus::Unavailable, "Unavailable"),
        }
    }
}

pub(super) fn reserve_now(
    actor: &mut Station2Actor,
    responder: Responder<ReserveNow>,
    request: ReserveNowRequest,
) {
    let (status, reason) = actor.reserve_decision(&request);
    actor.log(
        LogLevel::Info,
        format!(
            "ReserveNow {}: {}",
            request.id,
            if reason.is_empty() {
                "Accepted"
            } else {
                reason
            }
        ),
    );
    actor.respond(
        responder,
        ReserveNowResponse {
            status,
            status_info: (status != ReserveNowStatus::Accepted).then(|| status_info(reason)),
            custom_data: None,
        },
    );
    if status != ReserveNowStatus::Accepted {
        return;
    }
    let ReserveNowRequest {
        id,
        expiry_date_time,
        connector_type,
        id_token,
        evse_id,
        group_id_token,
        ..
    } = request;
    actor
        .reservations
        .list
        .retain(|reservation| reservation.id != id);
    actor.reservations.list.push(Reservation {
        id,
        expires_at: expiry_date_time,
        evse: evse_id.and_then(|evse| u32::try_from(evse).ok()),
        connector_type,
        id_token: IdTokenRef {
            id_token: id_token.id_token,
            kind: id_token.kind,
        },
        group_id_token: group_id_token.map(|group| group.id_token),
    });
    actor.flush_now();
    actor.notify_status_changes();
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "handlers share the signature `fn(&mut Station2Actor, Responder<A>, A::Req)`"
)]
pub(super) fn cancel_reservation(
    actor: &mut Station2Actor,
    responder: Responder<CancelReservation>,
    request: CancelReservationRequest,
) {
    let position = actor
        .reservations
        .list
        .iter()
        .position(|reservation| reservation.id == request.reservation_id);
    let Some(position) = position else {
        actor.log(
            LogLevel::Info,
            format!(
                "CancelReservation {}: no such reservation",
                request.reservation_id
            ),
        );
        actor.respond(
            responder,
            CancelReservationResponse {
                status: CancelReservationStatus::Rejected,
                status_info: Some(status_info("UnknownReservation")),
                custom_data: None,
            },
        );
        return;
    };
    actor.reservations.list.remove(position);
    actor.log(
        LogLevel::Info,
        format!("CancelReservation {}: Accepted", request.reservation_id),
    );
    actor.respond(
        responder,
        CancelReservationResponse {
            status: CancelReservationStatus::Accepted,
            status_info: None,
            custom_data: None,
        },
    );
    actor.flush_now();
    actor.notify_status_changes();
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::ocpp::v2::types::IdTokenType;

    fn token(id: &str) -> IdTokenRef {
        IdTokenRef {
            id_token: id.to_owned(),
            kind: IdTokenType::Iso14443,
        }
    }

    fn reservation(group: Option<&str>) -> Reservation {
        Reservation {
            id: 1,
            expires_at: Utc.timestamp_opt(1_000, 0).single().expect("valid time"),
            evse: None,
            connector_type: None,
            id_token: token("A"),
            group_id_token: group.map(str::to_owned),
        }
    }

    #[test]
    fn the_token_or_its_group_matches() {
        let plain = reservation(None);
        assert!(plain.matches(&token("A"), None));
        assert!(!plain.matches(&token("B"), Some("G")));
        let grouped = reservation(Some("G"));
        assert!(grouped.matches(&token("B"), Some("G")));
        assert!(!grouped.matches(&token("B"), Some("H")));
        assert!(!grouped.matches(&token("B"), None));
    }
}
