//! Pure connector status rules.
//!
//! TS reference: `src/station/connector-status.ts`. The connector registry itself is
//! part of the actor (`Snapshot::connectors` is the live model: connector `1..=n`;
//! connector 0, the charge point, is `Snapshot::charge_point`).
//!
//! Rules to preserve: `Faulted` stays `Faulted` on `ChangeAvailability`; the
//! `connector_count` only grows; unplugging an idle connector gives `Available` unless
//! the connector is inoperative. Fixes over the TS code: availability is persisted,
//! connector 0 has a status, `StatusNotification` text fields are trimmed to the
//! schema's 50 characters.

use crate::ids::ConnectorId;
use crate::ocpp::types::{AvailabilityType, ChargePointErrorCode, ConnectorStatus};
use crate::station::snapshot::ConnectorSnapshot;

/// Maximum length of `info` and `vendorErrorCode` in `StatusNotification`.
pub const STATUS_TEXT_MAX: usize = 50;

/// The order the "cycle status" action walks through (TS `CONNECTOR_STATUS_SEQUENCE`).
pub const STATUS_SEQUENCE: [ConnectorStatus; 9] = [
    ConnectorStatus::Available,
    ConnectorStatus::Preparing,
    ConnectorStatus::Charging,
    ConnectorStatus::SuspendedEVSE,
    ConnectorStatus::SuspendedEV,
    ConnectorStatus::Finishing,
    ConnectorStatus::Reserved,
    ConnectorStatus::Unavailable,
    ConnectorStatus::Faulted,
];

/// The status after `status` in [`STATUS_SEQUENCE`], wrapping around.
pub fn next_status(status: ConnectorStatus) -> ConnectorStatus {
    let index = STATUS_SEQUENCE
        .iter()
        .position(|candidate| *candidate == status)
        .map_or(0, |index| (index + 1) % STATUS_SEQUENCE.len());
    STATUS_SEQUENCE[index]
}

/// Trims `value` and cuts it to `max` characters; `None` if nothing is left.
pub fn trim_text(value: &str, max: usize) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.chars().take(max).collect())
}

/// Error code to report for an injected fault (`NoError` is not a fault).
pub fn fault_error_code(requested: ChargePointErrorCode) -> ChargePointErrorCode {
    if requested == ChargePointErrorCode::NoError {
        ChargePointErrorCode::OtherError
    } else {
        requested
    }
}

/// Status after a fault is cleared.
pub fn status_after_fault_clear(connector: &ConnectorSnapshot) -> ConnectorStatus {
    if connector.availability == AvailabilityType::Inoperative {
        ConnectorStatus::Unavailable
    } else if !connector.transaction.is_idle() {
        ConnectorStatus::Charging
    } else if connector.ev_connected {
        ConnectorStatus::Preparing
    } else if connector.reservation.is_some() {
        ConnectorStatus::Reserved
    } else {
        ConnectorStatus::Available
    }
}

/// Status after `ChangeAvailability` on a connector without a running transaction.
pub fn status_after_availability_change(
    connector: &ConnectorSnapshot,
    availability: AvailabilityType,
) -> ConnectorStatus {
    if connector.status == ConnectorStatus::Faulted {
        ConnectorStatus::Faulted
    } else if availability == AvailabilityType::Inoperative {
        ConnectorStatus::Unavailable
    } else if connector.status == ConnectorStatus::Finishing {
        ConnectorStatus::Finishing
    } else if connector.reservation.is_some() {
        ConnectorStatus::Reserved
    } else if connector.ev_connected {
        ConnectorStatus::Preparing
    } else {
        ConnectorStatus::Available
    }
}

/// Status after the EV is unplugged from a connector without a running transaction.
pub fn status_after_unplug(connector: &ConnectorSnapshot) -> ConnectorStatus {
    if connector.availability == AvailabilityType::Inoperative
        || connector.status == ConnectorStatus::Unavailable
    {
        ConnectorStatus::Unavailable
    } else if connector.reservation.is_some() {
        ConnectorStatus::Reserved
    } else {
        ConnectorStatus::Available
    }
}

/// Status of connector 0 after its availability changed (a fault is sticky).
pub fn charge_point_status(
    availability: AvailabilityType,
    current: ConnectorStatus,
) -> ConnectorStatus {
    match (current, availability) {
        (ConnectorStatus::Faulted, _) => ConnectorStatus::Faulted,
        (_, AvailabilityType::Inoperative) => ConnectorStatus::Unavailable,
        _ => ConnectorStatus::Available,
    }
}

/// A fresh connector `id`, as a freshly started station has it.
pub fn new_connector(id: u32) -> ConnectorSnapshot {
    ConnectorSnapshot::new(ConnectorId::new(id))
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;
    use crate::ids::{IdTag, ReservationId};
    use crate::station::reservation::Reservation;

    fn connector() -> ConnectorSnapshot {
        new_connector(1)
    }

    fn reserved() -> ConnectorSnapshot {
        let mut c = connector();
        c.reservation = Some(Reservation {
            id: ReservationId::new(1),
            id_tag: IdTag::new("T").unwrap(),
            parent_id_tag: None,
            expiry: DateTime::UNIX_EPOCH,
        });
        c
    }

    #[test]
    fn cycle_walks_the_ts_order_and_wraps() {
        let mut status = ConnectorStatus::Available;
        let mut seen = vec![status];
        for _ in 0..9 {
            status = next_status(status);
            seen.push(status);
        }
        assert_eq!(&seen[..9], &STATUS_SEQUENCE);
        assert_eq!(seen[9], ConnectorStatus::Available);
    }

    #[test]
    fn trim_text_trims_cuts_and_drops_blank() {
        assert_eq!(trim_text("  hi  ", 50), Some("hi".into()));
        assert_eq!(trim_text("   ", 50), None);
        assert_eq!(trim_text(&"é".repeat(80), 50).unwrap().chars().count(), 50);
    }

    #[test]
    fn no_error_is_not_a_fault_code() {
        assert_eq!(
            fault_error_code(ChargePointErrorCode::NoError),
            ChargePointErrorCode::OtherError
        );
        assert_eq!(
            fault_error_code(ChargePointErrorCode::GroundFailure),
            ChargePointErrorCode::GroundFailure
        );
    }

    #[test]
    fn availability_change_keeps_faulted_and_finishing() {
        let mut c = connector();
        c.status = ConnectorStatus::Faulted;
        assert_eq!(
            status_after_availability_change(&c, AvailabilityType::Operative),
            ConnectorStatus::Faulted
        );
        c.status = ConnectorStatus::Finishing;
        assert_eq!(
            status_after_availability_change(&c, AvailabilityType::Operative),
            ConnectorStatus::Finishing
        );
        assert_eq!(
            status_after_availability_change(&c, AvailabilityType::Inoperative),
            ConnectorStatus::Unavailable
        );
    }

    #[test]
    fn availability_change_prefers_reserved_then_preparing() {
        assert_eq!(
            status_after_availability_change(&reserved(), AvailabilityType::Operative),
            ConnectorStatus::Reserved
        );
        let mut c = connector();
        c.ev_connected = true;
        assert_eq!(
            status_after_availability_change(&c, AvailabilityType::Operative),
            ConnectorStatus::Preparing
        );
        assert_eq!(
            status_after_availability_change(&connector(), AvailabilityType::Operative),
            ConnectorStatus::Available
        );
    }

    #[test]
    fn fault_clear_resolves_to_the_underlying_state() {
        assert_eq!(
            status_after_fault_clear(&connector()),
            ConnectorStatus::Available
        );
        let mut c = connector();
        c.ev_connected = true;
        assert_eq!(status_after_fault_clear(&c), ConnectorStatus::Preparing);
        c.availability = AvailabilityType::Inoperative;
        assert_eq!(status_after_fault_clear(&c), ConnectorStatus::Unavailable);
        assert_eq!(
            status_after_fault_clear(&reserved()),
            ConnectorStatus::Reserved
        );
    }

    #[test]
    fn unplug_gives_available_unless_inoperative_or_reserved() {
        assert_eq!(
            status_after_unplug(&connector()),
            ConnectorStatus::Available
        );
        let mut c = connector();
        c.availability = AvailabilityType::Inoperative;
        assert_eq!(status_after_unplug(&c), ConnectorStatus::Unavailable);
        assert_eq!(status_after_unplug(&reserved()), ConnectorStatus::Reserved);
    }

    #[test]
    fn charge_point_status_follows_availability_and_keeps_faults() {
        assert_eq!(
            charge_point_status(AvailabilityType::Inoperative, ConnectorStatus::Available),
            ConnectorStatus::Unavailable
        );
        assert_eq!(
            charge_point_status(AvailabilityType::Operative, ConnectorStatus::Unavailable),
            ConnectorStatus::Available
        );
        assert_eq!(
            charge_point_status(AvailabilityType::Operative, ConnectorStatus::Faulted),
            ConnectorStatus::Faulted
        );
    }
}
