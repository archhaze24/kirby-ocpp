//! The EVSE and connector model and the local commands that change it.

#![allow(
    dead_code,
    clippy::unused_self,
    reason = "placeholder module; remove this allow when the module is implemented"
)]

use serde::{Deserialize, Serialize};

use super::actor::Station2Actor;
use super::commands::not_implemented;
use super::types::Reply2;

/// Availability of one EVSE and its connectors, as persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvseAvail {
    pub evse: u32,
    pub operative: bool,
    /// `(connector id, operative)` for each connector.
    pub connectors: Vec<(u32, bool)>,
}

/// One EVSE with its connectors.
#[derive(Debug)]
pub(crate) struct Evse {}

impl Station2Actor {
    /// An EVSE by id.
    pub(super) fn evse(&self, _id: u32) -> Option<&Evse> {
        None
    }

    /// An EVSE by id, mutable.
    pub(super) fn evse_mut(&mut self, _id: u32) -> Option<&mut Evse> {
        None
    }

    /// Whether the EVSE and the station are operative.
    pub(super) fn evse_operative(&self, _id: u32) -> bool {
        false
    }

    /// The `ConnectorStatus` wire text of a connector.
    pub(super) fn connector_status_text(
        &self,
        _evse: u32,
        _connector: u32,
    ) -> Option<&'static str> {
        None
    }

    /// `Command2::PlugIn`.
    pub(super) fn cmd_plug_in(&mut self, _evse: u32, reply: Reply2) {
        not_implemented(reply, "plug_in");
    }

    /// `Command2::Unplug`.
    pub(super) fn cmd_unplug(&mut self, _evse: u32, reply: Reply2) {
        not_implemented(reply, "unplug");
    }

    /// `Command2::Fault`.
    pub(super) fn cmd_fault(&mut self, _evse: u32, _connector: Option<u32>, reply: Reply2) {
        not_implemented(reply, "fault");
    }

    /// `Command2::ClearFault`.
    pub(super) fn cmd_clear_fault(&mut self, _evse: u32, _connector: Option<u32>, reply: Reply2) {
        not_implemented(reply, "clear_fault");
    }

    /// The transaction on `evse` ended: applies an availability change that waited for it.
    pub(super) fn on_transaction_ended(&mut self, _evse: u32) {}
}
