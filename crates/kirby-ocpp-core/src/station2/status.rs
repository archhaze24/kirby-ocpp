//! The `StatusNotification` discipline.

#![allow(
    dead_code,
    clippy::unused_self,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use chrono::{DateTime, Utc};

use super::actor::Station2Actor;
use super::commands::not_implemented;
use super::types::{Origin2, Reply2};

impl Station2Actor {
    /// Sends the current status of one connector.
    pub(super) fn send_status(&mut self, _evse: u32, _connector: u32, _origin: Origin2) {}

    /// Sends the status of every connector, or only of those that changed since the given
    /// time.
    pub(super) fn send_all_statuses(&mut self, _only_changed_since: Option<DateTime<Utc>>) {}

    /// `Command2::StatusNotification`.
    pub(super) fn cmd_status_notification(&mut self, _evse: u32, _connector: u32, reply: Reply2) {
        not_implemented(reply, "status_notification");
    }
}
