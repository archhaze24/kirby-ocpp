//! The `StatusNotification` discipline.
//!
//! A connector's status follows from its flags ([`derive_status`]). Each change is sent as a
//! typed `StatusNotification` through `Origin2::Station`, without debounce (2.0.1 has no
//! `MinimumStatusDuration`). A change while the link is down is only recorded; the link code
//! sends it after the reconnect through [`Station2Actor::send_all_statuses`] (B04.FR.01/02).

use chrono::{DateTime, Utc};

use super::actor::Station2Actor;
use super::command::Outcome2;
use super::evse::Connector;
use super::types::{Origin2, Reply2};
use crate::error::StationError;
use crate::ocpp::v2::messages::{ConnectorStatus, StatusNotification, StatusNotificationRequest};
use crate::station::LogLevel;
use crate::ws::ConnectionState;

/// The status of a connector: `Faulted` if a fault is set, `Unavailable` if the station, the
/// EVSE or the connector is inoperative, `Occupied` with an EV connected, else `Available`.
/// `Reserved` is never derived (no reservations).
pub(super) fn derive_status(
    station_operative: bool,
    evse_operative: bool,
    connector: &Connector,
) -> ConnectorStatus {
    if connector.fault {
        ConnectorStatus::Faulted
    } else if !(station_operative && evse_operative && connector.operative) {
        ConnectorStatus::Unavailable
    } else if connector.ev_connected {
        ConnectorStatus::Occupied
    } else {
        ConnectorStatus::Available
    }
}

impl Station2Actor {
    /// Records the current status of every connector as already accounted for, without
    /// sending anything.
    pub(super) fn account_all_statuses(&mut self) {
        let station = self.state.availability_operative;
        for evse in &mut self.evses {
            let evse_operative = evse.gate();
            for c in &mut evse.connectors {
                c.accounted = derive_status(station, evse_operative, c);
            }
        }
    }

    /// Finds the connectors whose status differs from the accounted one, updates the record
    /// and sends one notification per changed connector. Connectors that did not change
    /// stay quiet (G01.FR.08).
    pub(super) fn notify_status_changes(&mut self) {
        let now = self.clock.now_utc();
        let station = self.state.availability_operative;
        let mut changed = Vec::new();
        for evse in &mut self.evses {
            let evse_operative = evse.gate();
            for c in &mut evse.connectors {
                let current = derive_status(station, evse_operative, c);
                if current != c.accounted {
                    c.accounted = current;
                    c.changed_at = now;
                    changed.push((evse.id, c.id));
                }
            }
        }
        for (evse, connector) in changed {
            self.send_status(evse, connector, Origin2::Station);
        }
    }

    /// The request that reports the current status of a connector.
    fn status_request(&self, evse: u32, connector: u32) -> Option<StatusNotificationRequest> {
        let status = self.connector_status(evse, connector)?;
        Some(StatusNotificationRequest {
            timestamp: self.clock.now_utc(),
            connector_status: status,
            evse_id: i32::try_from(evse).ok()?,
            connector_id: i32::try_from(connector).ok()?,
            custom_data: None,
        })
    }

    /// Sends the current status of one connector. Nothing is sent while the link is down.
    pub(super) fn send_status(&mut self, evse: u32, connector: u32, origin: Origin2) {
        if self.state.connection != ConnectionState::Connected {
            return;
        }
        let Some(request) = self.status_request(evse, connector) else {
            return;
        };
        self.call::<StatusNotification>(origin, &request, move |actor, result| {
            if let Err(error) = result {
                actor.log(
                    LogLevel::Warn,
                    format!("StatusNotification for {evse}/{connector} failed: {error}"),
                );
            }
        });
    }

    /// Sends the status of every connector, or only of those that changed at or after the
    /// given time.
    pub(super) fn send_all_statuses(&mut self, only_changed_since: Option<DateTime<Utc>>) {
        let due: Vec<(u32, u32)> = self
            .evses
            .iter()
            .flat_map(|e| e.connectors.iter().map(move |c| (e.id, c)))
            .filter(|(_, c)| only_changed_since.is_none_or(|since| c.changed_at >= since))
            .map(|(evse, c)| (evse, c.id))
            .collect();
        for (evse, connector) in due {
            self.send_status(evse, connector, Origin2::Station);
        }
    }

    /// `Command2::StatusNotification`: re-sends the current state of one connector.
    pub(super) fn cmd_status_notification(&mut self, evse: u32, connector: u32, reply: Reply2) {
        let Some(request) = self.status_request(evse, connector) else {
            let error = StationError::InvalidArgument(format!(
                "connector {connector} does not exist on EVSE {evse}"
            ));
            // The requester may have gone away; nothing to do about that.
            let _ = reply.send(Err(error));
            return;
        };
        self.call::<StatusNotification>(Origin2::Station, &request, move |_, result| {
            let _ = reply.send(result.map(|_| Outcome2::Done).map_err(StationError::from));
        });
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::station2::evse::Evse;

    fn connector(f: impl FnOnce(&mut Connector)) -> Connector {
        let now = Utc.timestamp_opt(0, 0).single().expect("valid time");
        let mut evse = Evse::new(1, 1, now);
        let mut c = evse.connectors.remove(0);
        f(&mut c);
        c
    }

    #[test]
    fn idle_connector_is_available() {
        assert_eq!(
            derive_status(true, true, &connector(|_| {})),
            ConnectorStatus::Available
        );
    }

    #[test]
    fn connected_ev_makes_it_occupied() {
        let c = connector(|c| c.ev_connected = true);
        assert_eq!(derive_status(true, true, &c), ConnectorStatus::Occupied);
    }

    #[test]
    fn any_inoperative_level_makes_it_unavailable() {
        let c = connector(|c| c.ev_connected = true);
        assert_eq!(derive_status(false, true, &c), ConnectorStatus::Unavailable);
        assert_eq!(derive_status(true, false, &c), ConnectorStatus::Unavailable);
        let c = connector(|c| c.operative = false);
        assert_eq!(derive_status(true, true, &c), ConnectorStatus::Unavailable);
    }

    #[test]
    fn fault_wins_over_everything() {
        let c = connector(|c| {
            c.fault = true;
            c.operative = false;
            c.ev_connected = true;
        });
        assert_eq!(derive_status(false, false, &c), ConnectorStatus::Faulted);
    }
}
