//! Connector registry and connector status rules.
//!
//! TS reference: `src/station/connector-registry.ts`, `src/station/connector-status.ts`,
//! the plug/unplug/fault/availability flows in `src/station.ts`.
//!
//! Will hold the actor-owned `ConnectorRegistry` (connector `1..=n`; connector 0
//! is the charge point, not an entry) and the pure status rules
//! (`nextConnectorStatus`, `statusAfterStopTransaction`, `statusAfterFaultClear`).
//! Rules to preserve: Finishing if the EV is still connected after a stop,
//! Available otherwise, Unavailable when Inoperative; reason `EVDisconnected`
//! forces `ev_connected = false`; `Faulted` stays `Faulted` on ChangeAvailability;
//! `connector_count` only grows. Fixes: persist availability; send StatusNotification
//! for connector 0; MinimumStatusDuration without delaying every change and with the
//! change time as timestamp. The read model exposed to UIs is
//! [`crate::station::ConnectorSnapshot`].
