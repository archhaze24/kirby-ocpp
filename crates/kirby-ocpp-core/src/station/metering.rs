//! Simulated energy meter and MeterValues scheduling.
//!
//! TS reference: `src/station/metering.ts`, `src/station/meter-value-scheduler.ts`.
//!
//! Deterministic fake meter to preserve: 7.2 kW constant, `Wh += 7200 * interval / 3600`,
//! voltage `228 + cycle % 6`, current = power / voltage, default measurands
//! Energy.Active.Import.Register, Power.Active.Import, Current.Import, Voltage,
//! Temperature, SoC. Clock-aligned sampling aligns to epoch multiples of the interval
//! (via [`crate::Clock`]). StopTransaction data = `Transaction.End`
//! (`StopTxnSampledData`) + `Sample.Clock` (`StopTxnAlignedData`). Fixes: periodic and
//! clock-aligned samples must not both add energy; aligned data also outside
//! transactions; the meter keeps advancing while offline.
