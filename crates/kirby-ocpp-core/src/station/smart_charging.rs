//! Charging profiles and composite schedules.
//!
//! TS reference: `src/station/charging-profile-registry.ts`, `src/station/smart-charging.ts`.
//!
//! Will hold the profile registry (`SetChargingProfile`, `ClearChargingProfile`,
//! `GetCompositeSchedule`). Preserve: `ChargePointMaxProfile` only on connector 0;
//! `TxProfile` needs an active transaction (+ matching id if given); recurring
//! daily/weekly profiles via modulo; relative profiles from the transaction start;
//! unit filter, no A/W conversion; merge adjacent identical windows. Fix: same
//! stackLevel+purpose replaces; TxProfile without transactionId must not match
//! later transactions; spec precedence (TxProfile overrides TxDefault, only
//! ChargePointMax caps); return `NotSupported` where appropriate; enforce the
//! ChargeProfileMaxStackLevel/MaxPeriods/MaxInstalled limits.
