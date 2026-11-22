//! A protocol-neutral view of a running station, for code that only watches the link.
//!
//! Headless runners and the scenario runner need to connect a station, wait for its boot and
//! report why reconnecting stopped, whatever OCPP version the station speaks.
//! [`StationLike`] and [`LinkSnapshot`] cover exactly that and are implemented for both the
//! 1.6 and the 2.0.1 handle and snapshot.

use std::future::Future;

use tokio::sync::{broadcast, watch};

use crate::config::StationConfig;
use crate::error::StationError;
use crate::station::{Event, Snapshot, StationHandle};
use crate::station2::{Snapshot2, Station2Handle};
use crate::ws::ConnectionState;

/// The link-related part of a station snapshot.
pub trait LinkSnapshot {
    fn charge_point_id(&self) -> &str;

    /// `true` once the CSMS accepted a `BootNotification`.
    fn booted(&self) -> bool;

    /// Why the automatic reconnect loop ended, if it did.
    fn reconnect_stopped(&self) -> Option<&str>;

    fn connection(&self) -> ConnectionState;

    /// Transaction events queued for delivery (always 0 in 1.6, which has no such queue).
    fn queued_events(&self) -> u32 {
        0
    }
}

impl LinkSnapshot for Snapshot {
    fn charge_point_id(&self) -> &str {
        &self.charge_point_id
    }

    fn booted(&self) -> bool {
        Snapshot::booted(self)
    }

    fn reconnect_stopped(&self) -> Option<&str> {
        self.reconnect_stopped.as_deref()
    }

    fn connection(&self) -> ConnectionState {
        self.connection
    }
}

impl LinkSnapshot for Snapshot2 {
    fn charge_point_id(&self) -> &str {
        &self.charge_point_id
    }

    fn booted(&self) -> bool {
        Snapshot2::booted(self)
    }

    fn queued_events(&self) -> u32 {
        self.queued_events
    }

    fn reconnect_stopped(&self) -> Option<&str> {
        self.reconnect_stopped.as_deref()
    }

    fn connection(&self) -> ConnectionState {
        self.connection
    }
}

/// A handle to a running station of any OCPP version.
pub trait StationLike: Clone + Send + Sync + 'static {
    /// The snapshot type of the station.
    type Snap: LinkSnapshot + Clone + Send + Sync + 'static;

    /// The launch configuration.
    fn config(&self) -> &StationConfig;

    /// A new subscription to station events.
    fn subscribe(&self) -> broadcast::Receiver<Event>;

    /// A receiver that wakes on every snapshot change.
    fn watch_snapshot(&self) -> watch::Receiver<Self::Snap>;

    /// Opens the connection.
    fn connect(&self) -> impl Future<Output = Result<(), StationError>> + Send;

    /// Closes the connection and stops automatic reconnects.
    fn disconnect(&self) -> impl Future<Output = Result<(), StationError>> + Send;

    /// Stops the station.
    fn shutdown(&self) -> impl Future<Output = Result<(), StationError>> + Send;
}

impl StationLike for StationHandle {
    type Snap = Snapshot;

    fn config(&self) -> &StationConfig {
        StationHandle::config(self)
    }

    fn subscribe(&self) -> broadcast::Receiver<Event> {
        StationHandle::subscribe(self)
    }

    fn watch_snapshot(&self) -> watch::Receiver<Snapshot> {
        StationHandle::watch_snapshot(self)
    }

    fn connect(&self) -> impl Future<Output = Result<(), StationError>> + Send {
        StationHandle::connect(self)
    }

    fn disconnect(&self) -> impl Future<Output = Result<(), StationError>> + Send {
        StationHandle::disconnect(self)
    }

    fn shutdown(&self) -> impl Future<Output = Result<(), StationError>> + Send {
        StationHandle::shutdown(self)
    }
}

impl StationLike for Station2Handle {
    type Snap = Snapshot2;

    fn config(&self) -> &StationConfig {
        Station2Handle::config(self)
    }

    fn subscribe(&self) -> broadcast::Receiver<Event> {
        Station2Handle::subscribe(self)
    }

    fn watch_snapshot(&self) -> watch::Receiver<Snapshot2> {
        Station2Handle::watch_snapshot(self)
    }

    fn connect(&self) -> impl Future<Output = Result<(), StationError>> + Send {
        Station2Handle::connect(self)
    }

    fn disconnect(&self) -> impl Future<Output = Result<(), StationError>> + Send {
        Station2Handle::disconnect(self)
    }

    fn shutdown(&self) -> impl Future<Output = Result<(), StationError>> + Send {
        Station2Handle::shutdown(self)
    }
}
