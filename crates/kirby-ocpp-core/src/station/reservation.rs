//! Reservations (`ReserveNow` / `CancelReservation`) and their expiry.
//!
//! TS reference: `src/station/reservation-rules.ts`,
//! `src/station/reservation-scheduler.ts`, `transaction-rules.ts`, and the
//! reservation handlers in `src/station.ts`.
//!
//! The functions here change [`ConnectorSnapshot`]s in place (the reservation
//! slot and the connector status) and return what the actor must do next: send a
//! StatusNotification, arm or cancel a timer. No I/O and no timers.
//!
//! ## ReserveNow (`reserve_now`)
//!
//! Checks run in this order; the first failure is the answer:
//!
//! 1. connector `0`: `Rejected` when `ReserveConnectorZeroSupported=false`,
//!    otherwise the first connector that is idle, operative and `Available`
//!    (TS `findAvailable`); no such connector -> `Unavailable`;
//! 2. no such connector -> `Unavailable`;
//! 3. `Occupied`: an active transaction, a live reservation with another id, or
//!    a status of `Preparing`, `Charging`, `SuspendedEV`, `SuspendedEVSE`,
//!    `Finishing`; `Reserved` counts too unless it is this reservation;
//! 4. `Faulted`;
//! 5. `Unavailable`: inoperative, or status `Unavailable`;
//! 6. `Rejected`: expiry not in the future;
//! 7. `Rejected`: the same reservation id is live on another connector. This pure function
//!    never moves a reservation; the `ReserveNow` handler first releases the old one on a
//!    trial copy of the connectors (OCPP 1.6 section 5.13: an existing reservation id is
//!    replaced) and keeps the change only if this function accepts, so a refused move leaves
//!    the original reservation alone.
//!
//! Replacement: a request with the reservation id that is already on this
//! connector replaces it (not `Occupied`), which is the TS bug fix.
//!
//! ## Matching a tag (`accepts`)
//!
//! A tag may use a reservation when it is the reserved idTag, or when its
//! parentIdTag (from its `IdTagInfo`) equals the reservation's parentIdTag.
//!
//! ## Expiry
//!
//! [`Reservation::next_expiry_delay`] never returns more than
//! [`MAX_EXPIRY_TIMER`], so a reservation months away cannot overflow a timer.
//! The actor re-arms when the timer fires early and calls [`expire_due`] on
//! wake-up. TS fired reservations beyond ~24.8 days immediately.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{ConnectorId, IdTag, ReservationId};
use crate::ocpp::messages::{ReservationStatus, ReserveNowRequest};
use crate::ocpp::types::{AvailabilityType, ConnectorStatus};
use crate::station::ConnectorSnapshot;

/// Longest single expiry timer. Later expiries are re-armed when it fires.
pub const MAX_EXPIRY_TIMER: Duration = Duration::from_secs(24 * 60 * 60);

/// An active reservation on a connector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub id: ReservationId,
    pub id_tag: IdTag,
    pub parent_id_tag: Option<IdTag>,
    pub expiry: DateTime<Utc>,
}

/// An expiry string that is not a date-time.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("`{0}` is not an RFC 3339 date-time")]
pub struct ExpiryError(pub String);

/// Parses an `expiryDate` from text (scenario files, command line). Wire
/// messages are already typed, so the schema rejects bad dates before this runs.
pub fn parse_expiry(text: &str) -> Result<DateTime<Utc>, ExpiryError> {
    DateTime::parse_from_rfc3339(text)
        .map(|date| date.with_timezone(&Utc))
        .map_err(|_| ExpiryError(text.to_owned()))
}

impl Reservation {
    /// Whether the reservation has expired at `now`.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expiry <= now
    }

    /// Delay until the reservation should be checked again. `None` when it has
    /// already expired (release it now). Otherwise the time left, capped at
    /// [`MAX_EXPIRY_TIMER`]: when the timer fires before the expiry, call this
    /// again and re-arm.
    pub fn next_expiry_delay(&self, now: DateTime<Utc>) -> Option<Duration> {
        if self.is_expired(now) {
            return None;
        }
        let remaining = (self.expiry - now).to_std().unwrap_or(MAX_EXPIRY_TIMER);
        Some(remaining.min(MAX_EXPIRY_TIMER))
    }

    /// Whether `id_tag` (whose parentIdTag is `id_tag_parent`) may use this reservation.
    pub fn accepts(&self, id_tag: &IdTag, id_tag_parent: Option<&IdTag>) -> bool {
        if self.id_tag == *id_tag {
            return true;
        }
        match (&self.parent_id_tag, id_tag_parent) {
            (Some(reserved_parent), Some(parent)) => reserved_parent == parent,
            _ => false,
        }
    }
}

/// The reservation of `connector` if it is still live at `now`.
fn live_reservation(connector: &ConnectorSnapshot, now: DateTime<Utc>) -> Option<&Reservation> {
    connector
        .reservation
        .as_ref()
        .filter(|reservation| !reservation.is_expired(now))
}

/// Whether a live reservation on `connector` rules out `id_tag` (a StartTransaction
/// or RemoteStart for it would be refused as "reserved for another idTag").
pub fn reserved_for_other(
    connector: &ConnectorSnapshot,
    id_tag: &IdTag,
    id_tag_parent: Option<&IdTag>,
    now: DateTime<Utc>,
) -> bool {
    live_reservation(connector, now)
        .is_some_and(|reservation| !reservation.accepts(id_tag, id_tag_parent))
}

/// `RemoteStartTransaction` gate: the connector is idle, operative, not
/// `Unavailable`/`Faulted`, and either unreserved (and not `Reserved`) or
/// reserved for this tag. A `Reserved` status without a live reservation blocks.
pub fn can_remote_start(
    connector: Option<&ConnectorSnapshot>,
    id_tag: &IdTag,
    id_tag_parent: Option<&IdTag>,
    now: DateTime<Utc>,
) -> bool {
    let Some(connector) = connector else {
        return false;
    };
    if !remote_start_usable(connector) {
        return false;
    }
    match live_reservation(connector, now) {
        Some(reservation) => reservation.accepts(id_tag, id_tag_parent),
        None => connector.status != ConnectorStatus::Reserved,
    }
}

/// The part of the `RemoteStartTransaction` gate that does not depend on the idTag: the
/// connector is idle, operative and neither `Unavailable` nor `Faulted`.
pub fn remote_start_usable(connector: &ConnectorSnapshot) -> bool {
    connector.transaction.is_idle()
        && connector.availability == AvailabilityType::Operative
        && !matches!(
            connector.status,
            ConnectorStatus::Unavailable | ConnectorStatus::Faulted
        )
}

/// Decides and applies a `ReserveNow`. On `Ok` the connector now holds the
/// reservation and status `Reserved`; the caller sends a StatusNotification.
///
/// Precondition: the caller has run [`expire_due`] for `now` (and notified for
/// the connectors it returned). An expired reservation that is still in its slot
/// counts as occupied, so the request fails closed until it is released. This
/// function never changes state on `Err`.
pub fn reserve_now(
    connectors: &mut [ConnectorSnapshot],
    request: &ReserveNowRequest,
    now: DateTime<Utc>,
    connector_zero_supported: bool,
) -> Result<ConnectorId, ReservationStatus> {
    let index = target_index(connectors, request.connector_id, connector_zero_supported)?;

    let connector = &connectors[index];
    let same_reservation = connector
        .reservation
        .as_ref()
        .is_some_and(|reservation| reservation.id == request.reservation_id);
    if occupied(connector, same_reservation) {
        return Err(ReservationStatus::Occupied);
    }
    if connector.status == ConnectorStatus::Faulted {
        return Err(ReservationStatus::Faulted);
    }
    if connector.availability == AvailabilityType::Inoperative
        || connector.status == ConnectorStatus::Unavailable
    {
        return Err(ReservationStatus::Unavailable);
    }
    if request.expiry_date <= now {
        return Err(ReservationStatus::Rejected);
    }
    let id_used_elsewhere = connectors.iter().enumerate().any(|(i, other)| {
        i != index
            && other
                .reservation
                .as_ref()
                .is_some_and(|reservation| reservation.id == request.reservation_id)
    });
    if id_used_elsewhere {
        return Err(ReservationStatus::Rejected);
    }

    let connector = &mut connectors[index];
    connector.reservation = Some(Reservation {
        id: request.reservation_id,
        id_tag: request.id_tag.clone(),
        parent_id_tag: request.parent_id_tag.clone(),
        expiry: request.expiry_date,
    });
    connector.status = ConnectorStatus::Reserved;
    Ok(connector.id)
}

/// `CancelReservation`: releases the reservation with this id. Returns the
/// connector that was released, or `None` (CancelReservation `Rejected`).
pub fn cancel_reservation(
    connectors: &mut [ConnectorSnapshot],
    id: ReservationId,
) -> Option<ConnectorId> {
    let index = connectors.iter().position(|connector| {
        connector
            .reservation
            .as_ref()
            .is_some_and(|reservation| reservation.id == id)
    })?;
    clear_reservation(&mut connectors[index]);
    Some(connectors[index].id)
}

/// Releases every reservation that has expired at `now`, for the timers and for
/// restore-time pruning. Returns the connectors released.
pub fn expire_due(connectors: &mut [ConnectorSnapshot], now: DateTime<Utc>) -> Vec<ConnectorId> {
    let mut released = Vec::new();
    for connector in connectors.iter_mut() {
        let expired = connector
            .reservation
            .as_ref()
            .is_some_and(|reservation| reservation.is_expired(now));
        if expired {
            clear_reservation(connector);
            released.push(connector.id);
        }
    }
    released
}

/// Empties the reservation slot of `connector`. A `Reserved` status moves to the
/// status the connector would have without the reservation (TS
/// `statusAfterReservationClear`); other statuses are left alone.
pub fn clear_reservation(connector: &mut ConnectorSnapshot) -> Option<Reservation> {
    let released = connector.reservation.take()?;
    if connector.status == ConnectorStatus::Reserved {
        connector.status = status_without_reservation(connector);
    }
    Some(released)
}

fn target_index(
    connectors: &[ConnectorSnapshot],
    requested: ConnectorId,
    connector_zero_supported: bool,
) -> Result<usize, ReservationStatus> {
    if requested.is_station() {
        if !connector_zero_supported {
            return Err(ReservationStatus::Rejected);
        }
        return connectors
            .iter()
            .position(|connector| {
                connector.transaction.is_idle()
                    && connector.availability == AvailabilityType::Operative
                    && connector.status == ConnectorStatus::Available
            })
            .ok_or(ReservationStatus::Unavailable);
    }
    connectors
        .iter()
        .position(|connector| connector.id == requested)
        .ok_or(ReservationStatus::Unavailable)
}

fn occupied(connector: &ConnectorSnapshot, same_reservation: bool) -> bool {
    if !connector.transaction.is_idle() {
        return true;
    }
    if connector.reservation.is_some() && !same_reservation {
        return true;
    }
    match connector.status {
        ConnectorStatus::Preparing
        | ConnectorStatus::Charging
        | ConnectorStatus::SuspendedEV
        | ConnectorStatus::SuspendedEVSE
        | ConnectorStatus::Finishing => true,
        ConnectorStatus::Reserved => !same_reservation,
        _ => false,
    }
}

/// TS `statusAfterReservationClear`.
fn status_without_reservation(connector: &ConnectorSnapshot) -> ConnectorStatus {
    if connector.availability == AvailabilityType::Inoperative
        || connector.status == ConnectorStatus::Unavailable
    {
        return ConnectorStatus::Unavailable;
    }
    if connector.status == ConnectorStatus::Faulted {
        return ConnectorStatus::Faulted;
    }
    if !connector.transaction.is_idle() {
        return ConnectorStatus::Charging;
    }
    if connector.ev_connected {
        return ConnectorStatus::Preparing;
    }
    ConnectorStatus::Available
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;
    use crate::ids::TxId;
    use crate::station::TransactionState;
    use crate::station::transaction::{ActiveTransaction, PendingStart};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
    }

    fn future() -> DateTime<Utc> {
        now() + Duration::hours(2)
    }

    fn tag(name: &str) -> IdTag {
        IdTag::new(name).unwrap()
    }

    fn connectors(count: u32) -> Vec<ConnectorSnapshot> {
        (1..=count)
            .map(|id| ConnectorSnapshot::new(ConnectorId::new(id)))
            .collect()
    }

    fn request(
        connector: u32,
        reservation: i32,
        id_tag: &str,
        expiry: DateTime<Utc>,
    ) -> ReserveNowRequest {
        ReserveNowRequest {
            connector_id: ConnectorId::new(connector),
            expiry_date: expiry,
            id_tag: tag(id_tag),
            parent_id_tag: None,
            reservation_id: ReservationId::new(reservation),
        }
    }

    fn reserve(
        list: &mut [ConnectorSnapshot],
        req: &ReserveNowRequest,
    ) -> Result<ConnectorId, ReservationStatus> {
        reserve_now(list, req, now(), true)
    }

    fn connector(list: &mut [ConnectorSnapshot], id: u32) -> &mut ConnectorSnapshot {
        list.iter_mut()
            .find(|c| c.id == ConnectorId::new(id))
            .unwrap()
    }

    fn reservation(
        id: i32,
        id_tag: &str,
        parent: Option<&str>,
        expiry: DateTime<Utc>,
    ) -> Reservation {
        Reservation {
            id: ReservationId::new(id),
            id_tag: tag(id_tag),
            parent_id_tag: parent.map(tag),
            expiry,
        }
    }

    // ---- ReserveNow, ported from the negative-csms scenario ---------------------

    #[test]
    fn expired_reservation_is_rejected() {
        let mut list = connectors(2);
        let req = request(1, 401, "TAG1", now() - Duration::minutes(1));
        assert_eq!(reserve(&mut list, &req), Err(ReservationStatus::Rejected));
        assert!(connector(&mut list, 1).reservation.is_none());
    }

    #[test]
    fn unknown_connector_is_unavailable() {
        let mut list = connectors(2);
        assert_eq!(
            reserve(&mut list, &request(404, 402, "TAG1", future())),
            Err(ReservationStatus::Unavailable)
        );
    }

    #[test]
    fn second_reservation_on_a_reserved_connector_is_occupied_then_cancel_frees_it() {
        let mut list = connectors(2);
        assert_eq!(
            reserve(&mut list, &request(2, 403, "TAG1", future())),
            Ok(ConnectorId::new(2))
        );
        assert_eq!(connector(&mut list, 2).status, ConnectorStatus::Reserved);
        assert_eq!(
            reserve(&mut list, &request(2, 404, "TAG2", future())),
            Err(ReservationStatus::Occupied)
        );
        assert_eq!(
            cancel_reservation(&mut list, ReservationId::new(403)),
            Some(ConnectorId::new(2))
        );
        assert_eq!(connector(&mut list, 2).status, ConnectorStatus::Available);
        assert_eq!(cancel_reservation(&mut list, ReservationId::new(404)), None);
    }

    #[test]
    fn unavailable_then_faulted_connectors_answer_their_own_status() {
        let mut list = connectors(2);
        connector(&mut list, 2).availability = AvailabilityType::Inoperative;
        connector(&mut list, 2).status = ConnectorStatus::Unavailable;
        assert_eq!(
            reserve(&mut list, &request(2, 405, "TAG1", future())),
            Err(ReservationStatus::Unavailable)
        );

        let mut list = connectors(2);
        connector(&mut list, 2).status = ConnectorStatus::Faulted;
        assert_eq!(
            reserve(&mut list, &request(2, 406, "TAG1", future())),
            Err(ReservationStatus::Faulted)
        );
    }

    #[test]
    fn occupied_checks_come_before_fault_and_availability() {
        let mut list = connectors(1);
        connector(&mut list, 1).status = ConnectorStatus::Faulted;
        connector(&mut list, 1).transaction = TransactionState::Idle;
        connector(&mut list, 1).reservation = Some(reservation(1, "OTHER", None, future()));
        assert_eq!(
            reserve(&mut list, &request(1, 2, "TAG1", future())),
            Err(ReservationStatus::Occupied)
        );
    }

    #[test]
    fn busy_statuses_are_occupied() {
        for status in [
            ConnectorStatus::Preparing,
            ConnectorStatus::Charging,
            ConnectorStatus::SuspendedEV,
            ConnectorStatus::SuspendedEVSE,
            ConnectorStatus::Finishing,
        ] {
            let mut list = connectors(1);
            connector(&mut list, 1).status = status;
            assert_eq!(
                reserve(&mut list, &request(1, 9, "TAG1", future())),
                Err(ReservationStatus::Occupied),
                "{status:?}"
            );
        }
    }

    #[test]
    fn active_transaction_is_occupied() {
        let mut list = connectors(1);
        connector(&mut list, 1).transaction = TransactionState::Active(ActiveTransaction {
            tx_id: TxId::Csms(5),
            id_tag: tag("DRIVER"),
            started_at: now(),
            meter_start_wh: 0,
        });
        assert_eq!(
            reserve(&mut list, &request(1, 9, "TAG1", future())),
            Err(ReservationStatus::Occupied)
        );
    }

    #[test]
    fn pending_start_is_occupied_too() {
        let mut list = connectors(1);
        connector(&mut list, 1).transaction = TransactionState::PendingStart(PendingStart {
            tx_id: TxId::Local(1),
            id_tag: tag("DRIVER"),
            started_at: now(),
            meter_start_wh: 0,
            reservation_id: None,
        });
        assert_eq!(
            reserve(&mut list, &request(1, 9, "TAG1", future())),
            Err(ReservationStatus::Occupied)
        );
    }

    #[test]
    fn same_reservation_id_on_the_same_connector_replaces_it() {
        let mut list = connectors(1);
        reserve(&mut list, &request(1, 7, "TAG1", future())).unwrap();
        let later = future() + Duration::hours(1);
        assert_eq!(
            reserve(&mut list, &request(1, 7, "TAG2", later)),
            Ok(ConnectorId::new(1))
        );
        let stored = connector(&mut list, 1).reservation.clone().unwrap();
        assert_eq!(stored.id_tag, tag("TAG2"));
        assert_eq!(stored.expiry, later);
        assert_eq!(connector(&mut list, 1).status, ConnectorStatus::Reserved);
    }

    #[test]
    fn same_reservation_id_on_another_connector_is_rejected() {
        let mut list = connectors(2);
        reserve(&mut list, &request(1, 7, "TAG1", future())).unwrap();
        assert_eq!(
            reserve(&mut list, &request(2, 7, "TAG1", future())),
            Err(ReservationStatus::Rejected)
        );
    }

    #[test]
    fn expired_reservation_does_not_block_once_released_by_expire_due() {
        let mut list = connectors(1);
        connector(&mut list, 1).reservation =
            Some(reservation(1, "OLD", None, now() - Duration::minutes(5)));
        connector(&mut list, 1).status = ConnectorStatus::Reserved;
        assert_eq!(
            reserve(&mut list, &request(1, 2, "NEW", future())),
            Err(ReservationStatus::Occupied),
            "an unreleased expired reservation fails closed"
        );
        assert!(
            connector(&mut list, 1).reservation.is_some(),
            "a rejected request changes nothing"
        );
        assert_eq!(expire_due(&mut list, now()), [ConnectorId::new(1)]);
        assert_eq!(connector(&mut list, 1).status, ConnectorStatus::Available);
        assert_eq!(
            reserve(&mut list, &request(1, 2, "NEW", future())),
            Ok(ConnectorId::new(1))
        );
        assert_eq!(
            connector(&mut list, 1).reservation.as_ref().unwrap().id,
            ReservationId::new(2)
        );
    }

    #[test]
    fn connector_zero_picks_the_first_available_connector() {
        let mut list = connectors(3);
        connector(&mut list, 1).status = ConnectorStatus::Charging;
        assert_eq!(
            reserve_now(&mut list, &request(0, 11, "TAG1", future()), now(), true),
            Ok(ConnectorId::new(2))
        );
    }

    #[test]
    fn connector_zero_without_a_free_connector_is_unavailable() {
        let mut list = connectors(1);
        connector(&mut list, 1).status = ConnectorStatus::Preparing;
        assert_eq!(
            reserve_now(&mut list, &request(0, 11, "TAG1", future()), now(), true),
            Err(ReservationStatus::Unavailable)
        );
    }

    #[test]
    fn connector_zero_is_rejected_when_not_supported() {
        let mut list = connectors(2);
        assert_eq!(
            reserve_now(&mut list, &request(0, 11, "TAG1", future()), now(), false),
            Err(ReservationStatus::Rejected)
        );
        assert!(list.iter().all(|c| c.reservation.is_none()));
    }

    // ---- matching, remote start, start gating -----------------------------------

    #[test]
    fn reserved_tag_and_parent_tag_matching() {
        let r = reservation(1, "TAG1", Some("PARENT"), future());
        assert!(r.accepts(&tag("TAG1"), None), "the reserved tag itself");
        assert!(r.accepts(&tag("TAG2"), Some(&tag("PARENT"))), "same parent");
        assert!(!r.accepts(&tag("TAG2"), None), "no parent known");
        assert!(
            !r.accepts(&tag("TAG2"), Some(&tag("OTHER"))),
            "other parent"
        );
        assert!(
            !r.accepts(&tag("PARENT"), None),
            "the parent itself is not the reserved tag"
        );
        let no_parent = reservation(2, "TAG1", None, future());
        assert!(!no_parent.accepts(&tag("TAG2"), Some(&tag("PARENT"))));
    }

    #[test]
    fn start_is_blocked_only_for_a_live_reservation_of_another_tag() {
        let mut list = connectors(1);
        connector(&mut list, 1).reservation = Some(reservation(1, "TAG1", Some("P"), future()));
        let c = &list[0];
        assert!(reserved_for_other(c, &tag("TAG9"), None, now()));
        assert!(!reserved_for_other(c, &tag("TAG1"), None, now()));
        assert!(!reserved_for_other(c, &tag("TAG2"), Some(&tag("P")), now()));
        assert!(!reserved_for_other(
            c,
            &tag("TAG9"),
            None,
            future() + Duration::seconds(1)
        ));
    }

    #[test]
    fn remote_start_allows_the_reserved_tag_on_a_reserved_connector() {
        let mut list = connectors(1);
        reserve(&mut list, &request(1, 1, "TAG1", future())).unwrap();
        let c = Some(&list[0]);
        assert!(can_remote_start(c, &tag("TAG1"), None, now()));
        assert!(!can_remote_start(
            c,
            &tag("TAG2"),
            Some(&tag("PARENT")),
            now()
        ));
        assert!(!can_remote_start(c, &tag("TAG2"), None, now()));
    }

    #[test]
    fn remote_start_with_matching_parent_on_a_reserved_connector() {
        let mut list = connectors(1);
        reserve(
            &mut list,
            &ReserveNowRequest {
                parent_id_tag: Some(tag("PARENT")),
                ..request(1, 1, "TAG1", future())
            },
        )
        .unwrap();
        let c = Some(&list[0]);
        assert!(can_remote_start(
            c,
            &tag("TAG2"),
            Some(&tag("PARENT")),
            now()
        ));
        assert!(!can_remote_start(
            c,
            &tag("TAG2"),
            Some(&tag("OTHER")),
            now()
        ));
    }

    #[test]
    fn remote_start_gates() {
        let idle = ConnectorSnapshot::new(ConnectorId::new(1));
        assert!(can_remote_start(Some(&idle), &tag("TAG1"), None, now()));
        assert!(!can_remote_start(None, &tag("TAG1"), None, now()));

        let mut inoperative = idle.clone();
        inoperative.availability = AvailabilityType::Inoperative;
        assert!(!can_remote_start(
            Some(&inoperative),
            &tag("TAG1"),
            None,
            now()
        ));

        let mut faulted = idle.clone();
        faulted.status = ConnectorStatus::Faulted;
        assert!(!can_remote_start(Some(&faulted), &tag("TAG1"), None, now()));

        let mut busy = idle.clone();
        busy.transaction = TransactionState::Active(ActiveTransaction {
            tx_id: TxId::Csms(1),
            id_tag: tag("DRIVER"),
            started_at: now(),
            meter_start_wh: 0,
        });
        assert!(!can_remote_start(Some(&busy), &tag("TAG1"), None, now()));

        // Reserved status with no live reservation blocks everyone.
        let mut orphan = idle;
        orphan.status = ConnectorStatus::Reserved;
        assert!(!can_remote_start(Some(&orphan), &tag("TAG1"), None, now()));
    }

    #[test]
    fn remote_start_is_refused_for_a_different_live_reservation_even_when_available() {
        let mut list = connectors(1);
        connector(&mut list, 1).reservation = Some(reservation(1, "TAG1", None, future()));
        assert!(!can_remote_start(Some(&list[0]), &tag("TAG2"), None, now()));
    }

    // ---- expiry -----------------------------------------------------------------

    #[test]
    fn expiry_delay_is_clamped_for_far_future_dates() {
        let far = reservation(1, "TAG1", None, now() + Duration::days(100));
        assert_eq!(far.next_expiry_delay(now()), Some(MAX_EXPIRY_TIMER));
        let near = reservation(2, "TAG1", None, now() + Duration::minutes(3));
        assert_eq!(
            near.next_expiry_delay(now()),
            Some(std::time::Duration::from_secs(180))
        );
    }

    #[test]
    fn expiry_delay_is_none_once_expired() {
        let exact = reservation(1, "TAG1", None, now());
        assert_eq!(exact.next_expiry_delay(now()), None);
        let past = reservation(2, "TAG1", None, now() - Duration::hours(1));
        assert_eq!(past.next_expiry_delay(now()), None);
        assert!(exact.is_expired(now()));
    }

    #[test]
    fn far_future_expiry_does_not_release_early() {
        let mut list = connectors(1);
        reserve(
            &mut list,
            &request(1, 1, "TAG1", now() + Duration::days(100)),
        )
        .unwrap();
        assert_eq!(expire_due(&mut list, now() + Duration::days(1)), Vec::new());
        assert!(connector(&mut list, 1).reservation.is_some());
        assert_eq!(
            expire_due(&mut list, now() + Duration::days(101)),
            [ConnectorId::new(1)]
        );
        assert_eq!(connector(&mut list, 1).status, ConnectorStatus::Available);
    }

    #[test]
    fn restore_pruning_releases_only_expired_reservations() {
        let mut list = connectors(3);
        connector(&mut list, 1).reservation =
            Some(reservation(1, "OLD", None, now() - Duration::hours(1)));
        connector(&mut list, 1).status = ConnectorStatus::Reserved;
        connector(&mut list, 2).reservation = Some(reservation(2, "LIVE", None, future()));
        connector(&mut list, 2).status = ConnectorStatus::Reserved;

        assert_eq!(expire_due(&mut list, now()), [ConnectorId::new(1)]);
        assert!(connector(&mut list, 1).reservation.is_none());
        assert_eq!(connector(&mut list, 1).status, ConnectorStatus::Available);
        assert!(connector(&mut list, 2).reservation.is_some());
        assert_eq!(connector(&mut list, 2).status, ConnectorStatus::Reserved);
    }

    #[test]
    fn clearing_a_reservation_restores_the_status_it_hid() {
        let mut inoperative = ConnectorSnapshot::new(ConnectorId::new(1));
        inoperative.availability = AvailabilityType::Inoperative;
        inoperative.status = ConnectorStatus::Reserved;
        inoperative.reservation = Some(reservation(1, "TAG1", None, future()));
        clear_reservation(&mut inoperative);
        assert_eq!(inoperative.status, ConnectorStatus::Unavailable);

        let mut plugged = ConnectorSnapshot::new(ConnectorId::new(1));
        plugged.status = ConnectorStatus::Reserved;
        plugged.ev_connected = true;
        plugged.reservation = Some(reservation(1, "TAG1", None, future()));
        clear_reservation(&mut plugged);
        assert_eq!(plugged.status, ConnectorStatus::Preparing);

        // A status that is not Reserved is left alone.
        let mut faulted = ConnectorSnapshot::new(ConnectorId::new(1));
        faulted.status = ConnectorStatus::Faulted;
        faulted.reservation = Some(reservation(1, "TAG1", None, future()));
        clear_reservation(&mut faulted);
        assert_eq!(faulted.status, ConnectorStatus::Faulted);
        assert!(faulted.reservation.is_none());
    }

    #[test]
    fn clear_without_reservation_is_a_no_op() {
        let mut idle = ConnectorSnapshot::new(ConnectorId::new(1));
        assert!(clear_reservation(&mut idle).is_none());
        assert_eq!(idle.status, ConnectorStatus::Available);
    }

    #[test]
    fn parse_expiry_accepts_rfc3339_and_rejects_garbage() {
        let parsed = parse_expiry("2026-06-01T14:00:00+02:00").unwrap();
        assert_eq!(parsed, Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap());
        assert_eq!(
            parse_expiry("not a date"),
            Err(ExpiryError("not a date".to_owned()))
        );
        assert!(parse_expiry("").is_err());
    }

    #[test]
    fn reservation_roundtrips_through_json() {
        let r = reservation(42, "TAG1", Some("PARENT"), future());
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Reservation>(&json).unwrap(), r);
    }
}
