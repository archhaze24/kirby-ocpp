//! Reservations (`ReserveNow` / `CancelReservation`) and their expiry timers.
//!
//! TS reference: `src/station/reservation-rules.ts`,
//! `src/station/reservation-scheduler.ts`, the reservation handlers in `src/station.ts`.
//!
//! Rejection order to preserve for `ReserveNow`: no such connector
//! (`Unavailable`) -> `Occupied` -> `Faulted` -> `Unavailable` -> bad/past
//! expiry (`Rejected`). Fix on the way: same reservation id on the same
//! connector replaces instead of `Occupied`; `parentIdTag` counts as a match;
//! expiry timers beyond ~24.8 days must not fire immediately (clamp / re-arm).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{IdTag, ReservationId};

/// An active reservation on a connector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub id: ReservationId,
    pub id_tag: IdTag,
    pub parent_id_tag: Option<IdTag>,
    pub expiry: DateTime<Utc>,
}
