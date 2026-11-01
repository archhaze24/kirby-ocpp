//! OCPP configuration keys (`GetConfiguration` / `ChangeConfiguration`).
//!
//! TS reference: `src/station/configuration.ts`, `configuration-registry.ts`,
//! `configuration-commands.ts`.
//!
//! About 38 typed keys (bool, non-negative int, positive int, sampled-data list,
//! phase-rotation list); read-only identity keys; inert keys that are stored but
//! have no effect. Fixes: keys are case-insensitive per spec; add the missing
//! smart-charging keys (ChargeProfileMaxStackLevel, ChargingScheduleAllowedChargingRateUnit,
//! ChargingScheduleMaxPeriods, MaxChargingProfilesInstalled, ConnectorSwitch3to1PhaseSupported,
//! ReserveConnectorZeroSupported) and enforce the limits.
