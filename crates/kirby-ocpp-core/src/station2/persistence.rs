//! Wiring of the state store: restore at start, debounced writes, flush on shutdown.

#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use super::actor::Station2Actor;
use crate::store::StoreError;

/// The state file of the station and the write bookkeeping.
#[derive(Debug, Default)]
pub(crate) struct Persistence2 {}

impl Station2Actor {
    /// The persisted state changed: schedules a write.
    pub(super) fn mark_dirty(&mut self) {}

    /// Writes the state now.
    pub(super) fn flush_now(&mut self) {}

    /// The debounce delay of the state file write elapsed.
    pub(super) fn on_persist_flush(&mut self) {}

    /// A state file write finished.
    pub(super) fn on_store_saved(&mut self, _result: Result<(), StoreError>) {}
}
