//! Simulated energy meter, MeterValues construction and clock alignment.
//!
//! TS reference: `src/station/metering.ts` (readings, configuration parsing) and
//! `src/station/meter-value-scheduler.ts` (clock alignment).
//!
//! # The meter
//!
//! [`SimulatedMeter`] is deterministic: the same energy yields the same readings.
//! While the connector is `Charging`, energy grows at the nominal rate
//! [`NOMINAL_POWER_W`] (`Wh += 7200 * seconds / 3600`). The other readings are
//! functions of the energy, with the TS formulas (see [`SimulatedMeter::sample`]).
//!
//! Energy is integrated **once per elapsed interval**. [`SimulatedMeter::update`] closes
//! the interval since the last update with the status that held during it, and then records
//! the new status. Two samples at the same instant add nothing, so periodic and
//! clock-aligned sampling cannot double count. Time that passes offline counts too,
//! because the meter does not depend on the connection. A timestamp that goes backwards
//! never reduces the energy.
//!
//! # Power cap
//!
//! [`SimulatedMeter::set_power_limit_w`] caps the simulated power. The cap applies to the
//! integration rate and to the reported `Power.Active.Import` and `Current.Import`.
//! `None` keeps the TS behaviour.

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ocpp::messages::{MeterValue, SampledValue};
use crate::ocpp::types::{
    ConnectorStatus, Location, Measurand, Phase, ReadingContext, UnitOfMeasure, ValueFormat,
};

/// Nominal charging power of the simulated meter, used for energy integration (7.2 kW).
pub const NOMINAL_POWER_W: f64 = 7_200.0;

/// Measurands sampled when the configuration does not name any (TS `DEFAULT_SAMPLED_DATA`).
pub const DEFAULT_SAMPLED_DATA: [Measurand; 6] = [
    Measurand::EnergyActiveImportRegister,
    Measurand::PowerActiveImport,
    Measurand::CurrentImport,
    Measurand::Voltage,
    Measurand::Temperature,
    Measurand::SoC,
];

/// Deterministic simulated energy meter for one connector.
///
/// Only the energy is persisted. Everything else (the last update, the charging state,
/// the power cap) is runtime state: a restored meter has no baseline yet, so downtime is
/// not counted as charging.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulatedMeter {
    energy_wh: f64,
    #[serde(skip)]
    last_update: Option<DateTime<Utc>>,
    #[serde(skip)]
    charging: bool,
    #[serde(skip)]
    power_limit_w: Option<f64>,
}

impl Default for SimulatedMeter {
    fn default() -> Self {
        Self::with_energy_wh(0.0)
    }
}

impl SimulatedMeter {
    /// A meter at `energy_wh` (for example the energy restored from persisted state).
    pub fn with_energy_wh(energy_wh: f64) -> Self {
        Self {
            energy_wh,
            last_update: None,
            charging: false,
            power_limit_w: None,
        }
    }

    /// Accumulated energy in Wh (not rounded).
    pub fn energy_wh(&self) -> f64 {
        self.energy_wh
    }

    /// Current power cap in watts, if any.
    pub fn power_limit_w(&self) -> Option<f64> {
        self.power_limit_w
    }

    /// Sets or clears the power cap (see the module docs).
    pub fn set_power_limit_w(&mut self, limit_w: Option<f64>) {
        self.power_limit_w = limit_w;
    }

    /// Integrates energy up to `now` with the status recorded before, then records `status`.
    ///
    /// Call it on every status change and before every sample. A call with an earlier
    /// `now` only records the status.
    pub fn update(&mut self, now: DateTime<Utc>, status: ConnectorStatus) {
        match self.last_update {
            None => self.last_update = Some(now),
            Some(last) if now > last => {
                if self.charging {
                    let elapsed_s = millis_to_seconds((now - last).num_milliseconds());
                    self.energy_wh += self.rate_w() * elapsed_s / 3600.0;
                }
                self.last_update = Some(now);
            }
            Some(_) => {}
        }
        self.charging = status == ConnectorStatus::Charging;
    }

    /// Advances the meter to `at` and builds one MeterValue with the requested measurands.
    ///
    /// The readings follow the TS formulas, with `cycle = round(Wh / 120) % 8`:
    /// `Power.Active.Import = 6800 + cycle * 90 W`, `Voltage = 228 + cycle % 6 V`,
    /// `Current.Import = power / voltage A`, `Temperature = 29 + cycle + min(8, Wh / 3000) °C`,
    /// `SoC = clamp(18 + Wh / 450, 18, 96) %`. When not charging: 230 V, 0 W, 0 A, 25 °C, SoC 0.
    pub fn sample(
        &mut self,
        at: DateTime<Utc>,
        status: ConnectorStatus,
        measurands: &[Measurand],
        context: ReadingContext,
    ) -> MeterValue {
        self.update(at, status);
        let readings = compute_readings(self.energy_wh, self.charging, self.power_limit_w);
        MeterValue {
            timestamp: at,
            sampled_value: measurands
                .iter()
                .map(|measurand| sampled_value(&readings, context, *measurand))
                .collect(),
        }
    }

    fn rate_w(&self) -> f64 {
        self.power_limit_w
            .map_or(NOMINAL_POWER_W, |limit| limit.min(NOMINAL_POWER_W))
    }
}

/// Builds the `transactionData` of a StopTransaction (TS `buildStopTransactionData`).
///
/// `Transaction.End` with `sampled_data` (StopTxnSampledData) comes first, then
/// `Sample.Clock` with `aligned_data` (StopTxnAlignedData), sampled at `at`, the
/// clock-aligned instant. An empty list produces no MeterValue. TS always sent both, so an
/// empty list produced a MeterValue with an empty `sampledValue`, which the schema rejects.
pub fn stop_transaction_data(
    meter: &mut SimulatedMeter,
    at: DateTime<Utc>,
    status: ConnectorStatus,
    sampled_data: &[Measurand],
    aligned_data: &[Measurand],
) -> Vec<MeterValue> {
    let mut data = Vec::with_capacity(2);
    if !sampled_data.is_empty() {
        data.push(meter.sample(at, status, sampled_data, ReadingContext::TransactionEnd));
    }
    if !aligned_data.is_empty() {
        data.push(meter.sample(at, status, aligned_data, ReadingContext::SampleClock));
    }
    data
}

/// A configured list of measurands is invalid.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SampledDataError {
    #[error("the measurand list is empty")]
    Empty,
    #[error("unknown measurand(s): {}", .0.join(", "))]
    Unknown(Vec<String>),
}

/// Parses a comma-separated measurand list the way `ChangeConfiguration` must validate it:
/// blank items are ignored, at least one item must remain, and every item must be a known measurand.
pub fn parse_sampled_data(value: &str) -> Result<Vec<Measurand>, SampledDataError> {
    let items: Vec<&str> = split_items(value).collect();
    if items.is_empty() {
        return Err(SampledDataError::Empty);
    }
    let mut measurands = Vec::with_capacity(items.len());
    let mut unknown = Vec::new();
    for item in items {
        match item.parse::<Measurand>() {
            Ok(measurand) => measurands.push(measurand),
            Err(_) => unknown.push(item.to_owned()),
        }
    }
    if unknown.is_empty() {
        Ok(measurands)
    } else {
        Err(SampledDataError::Unknown(unknown))
    }
}

/// The measurands to sample for a configuration value: unknown items are dropped, and
/// [`DEFAULT_SAMPLED_DATA`] is used when nothing valid remains (TS `readConfiguredMeasurands`).
pub fn configured_measurands(value: Option<&str>) -> Vec<Measurand> {
    let measurands: Vec<Measurand> = split_items(value.unwrap_or(""))
        .filter_map(|item| item.parse::<Measurand>().ok())
        .collect();
    if measurands.is_empty() {
        DEFAULT_SAMPLED_DATA.to_vec()
    } else {
        measurands
    }
}

fn split_items(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

/// Start of the next clock-aligned instant: the next multiple of `interval_s` since the
/// Unix epoch, strictly after `now`. Returns the delay from `now`, or `None` when the
/// interval is `0` (disabled).
///
/// Example: with `interval_s = 60`, `12:00:00.000` gives 60 s and `12:00:30.000` gives 30 s.
pub fn clock_aligned_delay(now: DateTime<Utc>, interval_s: u32) -> Option<TimeDelta> {
    if interval_s == 0 {
        return None;
    }
    let period_ms = i64::from(interval_s) * 1000;
    let remainder = now.timestamp_millis().rem_euclid(period_ms);
    let delay_ms = if remainder == 0 {
        period_ms
    } else {
        period_ms - remainder
    };
    Some(TimeDelta::milliseconds(delay_ms))
}

/// The instant of the next clock-aligned sample (see [`clock_aligned_delay`]).
pub fn next_clock_aligned(now: DateTime<Utc>, interval_s: u32) -> Option<DateTime<Utc>> {
    clock_aligned_delay(now, interval_s).and_then(|delay| now.checked_add_signed(delay))
}

/// Energy the meter integrates in `interval_s` seconds at the nominal rate (TS
/// `periodicMeterDeltaWh`): `7200 * interval / 3600 = 2 * interval` Wh.
pub fn periodic_meter_delta_wh(interval_s: u32) -> u64 {
    u64::from(interval_s) * 2
}

/// Readings of the simulated meter at one instant.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Readings {
    charging: bool,
    energy_wh: f64,
    power_w: f64,
    voltage_v: i64,
    current_a: f64,
    temperature_c: i64,
    soc_percent: i64,
}

fn compute_readings(energy_wh: f64, charging: bool, limit_w: Option<f64>) -> Readings {
    let cycle = round_half_up(energy_wh / 120.0).abs() % 8;
    let voltage_v = if charging { 228 + cycle % 6 } else { 230 };
    let mut power_w = if charging {
        as_f64(6800 + cycle * 90)
    } else {
        0.0
    };
    if let Some(limit) = limit_w.filter(|_| charging) {
        power_w = power_w.min(limit);
    }
    let current_a = if charging {
        power_w / as_f64(voltage_v)
    } else {
        0.0
    };
    let temperature_c = if charging {
        29 + cycle + floor_i64(energy_wh / 3000.0).min(8)
    } else {
        25
    };
    let soc_percent = if charging {
        (18 + floor_i64(energy_wh / 450.0)).clamp(18, 96)
    } else {
        0
    };
    Readings {
        charging,
        energy_wh,
        power_w,
        voltage_v,
        current_a,
        temperature_c,
        soc_percent,
    }
}

/// One SampledValue of `measurand`, with the TS unit, location and phase.
fn sampled_value(
    readings: &Readings,
    context: ReadingContext,
    measurand: Measurand,
) -> SampledValue {
    let charging = readings.charging;
    let when_charging = |on: &str| {
        if charging {
            on.to_owned()
        } else {
            "0".to_owned()
        }
    };
    let outlet = Some(Location::Outlet);
    let zero = || "0".to_owned();

    let (value, unit, location, phase) = match measurand {
        Measurand::EnergyActiveImportRegister => (
            round_half_up(readings.energy_wh).to_string(),
            Some(UnitOfMeasure::Wh),
            outlet,
            None,
        ),
        Measurand::EnergyActiveExportRegister | Measurand::EnergyActiveExportInterval => {
            (zero(), Some(UnitOfMeasure::Wh), outlet, None)
        }
        Measurand::EnergyReactiveImportRegister
        | Measurand::EnergyReactiveExportRegister
        | Measurand::EnergyReactiveImportInterval
        | Measurand::EnergyReactiveExportInterval => {
            (zero(), Some(UnitOfMeasure::Varh), outlet, None)
        }
        Measurand::EnergyActiveImportInterval => {
            (when_charging("120"), Some(UnitOfMeasure::Wh), outlet, None)
        }
        Measurand::PowerActiveImport => (
            // Shortest decimal form: `7160` for 7160.0, as JS `String(number)`.
            readings.power_w.to_string(),
            Some(UnitOfMeasure::W),
            outlet,
            None,
        ),
        Measurand::PowerActiveExport => (zero(), Some(UnitOfMeasure::W), outlet, None),
        Measurand::PowerOffered => ("7400".to_owned(), Some(UnitOfMeasure::W), outlet, None),
        Measurand::PowerReactiveImport | Measurand::PowerReactiveExport => {
            (zero(), Some(UnitOfMeasure::Var), outlet, None)
        }
        Measurand::PowerFactor => (when_charging("1"), None, outlet, None),
        Measurand::CurrentImport => (
            format_current(readings.current_a),
            Some(UnitOfMeasure::A),
            outlet,
            Some(Phase::L1),
        ),
        Measurand::CurrentExport => (zero(), Some(UnitOfMeasure::A), outlet, Some(Phase::L1)),
        Measurand::CurrentOffered => (
            "32".to_owned(),
            Some(UnitOfMeasure::A),
            outlet,
            Some(Phase::L1),
        ),
        Measurand::Voltage => (
            readings.voltage_v.to_string(),
            Some(UnitOfMeasure::V),
            outlet,
            Some(Phase::L1N),
        ),
        Measurand::Frequency => ("50".to_owned(), None, outlet, None),
        Measurand::Temperature => (
            readings.temperature_c.to_string(),
            Some(UnitOfMeasure::Celsius),
            Some(Location::Body),
            None,
        ),
        Measurand::SoC => (
            readings.soc_percent.to_string(),
            Some(UnitOfMeasure::Percent),
            Some(Location::EV),
            None,
        ),
        Measurand::Rpm => (zero(), None, Some(Location::Body), None),
    };

    SampledValue {
        value,
        context: Some(context),
        format: Some(ValueFormat::Raw),
        measurand: Some(measurand),
        phase,
        location,
        unit,
    }
}

/// `toFixed(1)` with a trailing `.0` removed (TS `formatDecimal(value, 1)`).
fn format_current(value: f64) -> String {
    let fixed = format!("{value:.1}");
    fixed.strip_suffix(".0").unwrap_or(&fixed).to_owned()
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the readings are small integers, far below 2^53"
)]
fn as_f64(value: i64) -> f64 {
    value as f64
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "value is floored first; energies are far below i64::MAX"
)]
fn floor_i64(value: f64) -> i64 {
    value.floor() as i64
}

/// JavaScript `Math.round` for the non-negative values used here (ties go up).
#[allow(
    clippy::cast_possible_truncation,
    reason = "value is floored before the cast; energies are far below i64::MAX"
)]
fn round_half_up(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}

#[allow(
    clippy::cast_precision_loss,
    reason = "millisecond counts for meter intervals are far below 2^53"
)]
fn millis_to_seconds(millis: i64) -> f64 {
    millis as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use proptest::prelude::*;

    use super::*;

    /// 2026-11-01T12:00:00Z plus `seconds`.
    fn t(seconds: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap() + TimeDelta::seconds(seconds)
    }

    fn sampled(mv: &MeterValue, measurand: Measurand) -> &SampledValue {
        mv.sampled_value
            .iter()
            .find(|value| value.measurand == Some(measurand))
            .expect("measurand was requested")
    }

    fn assert_wh(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected} Wh, got {actual} Wh"
        );
    }

    #[test]
    fn default_sampled_data_matches_ts() {
        assert_eq!(
            DEFAULT_SAMPLED_DATA.to_vec(),
            vec![
                Measurand::EnergyActiveImportRegister,
                Measurand::PowerActiveImport,
                Measurand::CurrentImport,
                Measurand::Voltage,
                Measurand::Temperature,
                Measurand::SoC,
            ]
        );
    }

    #[test]
    fn golden_charging_readings_at_1440_wh() {
        // cycle = round(1440/120) % 8 = 4 -> voltage 232, power 6800 + 360 = 7160 W,
        // current 7160 / 232 = 30.86 A, temperature 29 + 4 + min(8, 0) = 33 °C, SoC 18 + 3 = 21 %.
        let mut meter = SimulatedMeter::with_energy_wh(1440.0);
        let mv = meter.sample(
            t(0),
            ConnectorStatus::Charging,
            &DEFAULT_SAMPLED_DATA,
            ReadingContext::SamplePeriodic,
        );
        assert_eq!(mv.timestamp, t(0));
        assert_eq!(mv.sampled_value.len(), 6);
        assert_eq!(
            sampled(&mv, Measurand::EnergyActiveImportRegister).value,
            "1440"
        );
        assert_eq!(sampled(&mv, Measurand::PowerActiveImport).value, "7160");
        assert_eq!(sampled(&mv, Measurand::CurrentImport).value, "30.9");
        assert_eq!(sampled(&mv, Measurand::Voltage).value, "232");
        assert_eq!(sampled(&mv, Measurand::Temperature).value, "33");
        assert_eq!(sampled(&mv, Measurand::SoC).value, "21");

        let power = sampled(&mv, Measurand::PowerActiveImport);
        assert_eq!(power.unit, Some(UnitOfMeasure::W));
        assert_eq!(power.location, Some(Location::Outlet));
        assert_eq!(power.phase, None);
        assert_eq!(power.format, Some(ValueFormat::Raw));
        assert_eq!(power.context, Some(ReadingContext::SamplePeriodic));
        assert_eq!(
            sampled(&mv, Measurand::CurrentImport).phase,
            Some(Phase::L1)
        );
        assert_eq!(sampled(&mv, Measurand::Voltage).phase, Some(Phase::L1N));
        assert_eq!(
            sampled(&mv, Measurand::Temperature).unit,
            Some(UnitOfMeasure::Celsius)
        );
        assert_eq!(
            sampled(&mv, Measurand::Temperature).location,
            Some(Location::Body)
        );
        assert_eq!(
            sampled(&mv, Measurand::SoC).unit,
            Some(UnitOfMeasure::Percent)
        );
        assert_eq!(sampled(&mv, Measurand::SoC).location, Some(Location::EV));
    }

    #[test]
    fn readings_when_not_charging_are_idle_values() {
        let mut meter = SimulatedMeter::with_energy_wh(1440.0);
        let mv = meter.sample(
            t(0),
            ConnectorStatus::Available,
            &DEFAULT_SAMPLED_DATA,
            ReadingContext::SamplePeriodic,
        );
        assert_eq!(
            sampled(&mv, Measurand::EnergyActiveImportRegister).value,
            "1440"
        );
        assert_eq!(sampled(&mv, Measurand::PowerActiveImport).value, "0");
        assert_eq!(sampled(&mv, Measurand::CurrentImport).value, "0");
        assert_eq!(sampled(&mv, Measurand::Voltage).value, "230");
        assert_eq!(sampled(&mv, Measurand::Temperature).value, "25");
        assert_eq!(sampled(&mv, Measurand::SoC).value, "0");
    }

    #[test]
    fn every_measurand_has_a_ts_shaped_sample() {
        let mut meter = SimulatedMeter::with_energy_wh(1000.0);
        let mv = meter.sample(
            t(0),
            ConnectorStatus::Charging,
            Measurand::ALL,
            ReadingContext::SamplePeriodic,
        );
        assert_eq!(mv.sampled_value.len(), Measurand::ALL.len());
        assert_eq!(
            sampled(&mv, Measurand::EnergyActiveImportInterval).value,
            "120"
        );
        assert_eq!(sampled(&mv, Measurand::PowerFactor).value, "1");
        assert_eq!(sampled(&mv, Measurand::PowerFactor).unit, None);
        assert_eq!(sampled(&mv, Measurand::CurrentOffered).value, "32");
        assert_eq!(sampled(&mv, Measurand::PowerOffered).value, "7400");
        assert_eq!(sampled(&mv, Measurand::Frequency).value, "50");
        assert_eq!(sampled(&mv, Measurand::Frequency).unit, None);
        assert_eq!(sampled(&mv, Measurand::Rpm).location, Some(Location::Body));
        assert_eq!(
            sampled(&mv, Measurand::EnergyReactiveImportRegister).unit,
            Some(UnitOfMeasure::Varh)
        );
        assert_eq!(
            sampled(&mv, Measurand::PowerReactiveExport).unit,
            Some(UnitOfMeasure::Var)
        );
    }

    #[test]
    fn energy_integrates_nominal_rate_once_per_elapsed_time() {
        let mut meter = SimulatedMeter::default();
        meter.update(t(0), ConnectorStatus::Charging);
        meter.sample(
            t(60),
            ConnectorStatus::Charging,
            &DEFAULT_SAMPLED_DATA,
            ReadingContext::SamplePeriodic,
        );
        assert_wh(meter.energy_wh(), 120.0);
        // A second sampler at the same instant (clock-aligned) adds nothing.
        meter.sample(
            t(60),
            ConnectorStatus::Charging,
            &DEFAULT_SAMPLED_DATA,
            ReadingContext::SampleClock,
        );
        assert_wh(meter.energy_wh(), 120.0);
        meter.sample(
            t(90),
            ConnectorStatus::Charging,
            &DEFAULT_SAMPLED_DATA,
            ReadingContext::TransactionEnd,
        );
        assert_wh(meter.energy_wh(), 180.0);
    }

    #[test]
    fn sixty_seconds_equal_the_periodic_delta() {
        let mut meter = SimulatedMeter::default();
        meter.update(t(0), ConnectorStatus::Charging);
        meter.update(t(60), ConnectorStatus::Charging);
        assert_wh(meter.energy_wh(), 120.0);
        assert_eq!(periodic_meter_delta_wh(60), 120);
    }

    #[test]
    fn nothing_is_counted_before_charging_starts() {
        let mut meter = SimulatedMeter::default();
        meter.update(t(0), ConnectorStatus::Available);
        meter.update(t(30), ConnectorStatus::Charging);
        assert_wh(meter.energy_wh(), 0.0);
        meter.update(t(60), ConnectorStatus::Charging);
        assert_wh(meter.energy_wh(), 60.0);
    }

    #[test]
    fn leaving_charging_closes_the_interval() {
        let mut meter = SimulatedMeter::default();
        meter.update(t(0), ConnectorStatus::Charging);
        meter.update(t(10), ConnectorStatus::Finishing);
        assert_wh(meter.energy_wh(), 20.0);
        meter.update(t(100), ConnectorStatus::Finishing);
        assert_wh(meter.energy_wh(), 20.0);
    }

    #[test]
    fn time_passing_offline_is_integrated_at_the_next_sample() {
        // The meter does not depend on the connection: no sample is needed in between.
        let mut meter = SimulatedMeter::default();
        meter.update(t(0), ConnectorStatus::Charging);
        meter.sample(
            t(600),
            ConnectorStatus::Charging,
            &DEFAULT_SAMPLED_DATA,
            ReadingContext::SamplePeriodic,
        );
        assert_wh(meter.energy_wh(), 1200.0);
    }

    #[test]
    fn restored_meter_does_not_count_downtime() {
        let mut meter = SimulatedMeter::with_energy_wh(1000.0);
        meter.update(t(0), ConnectorStatus::Charging);
        meter.update(t(60), ConnectorStatus::Charging);
        assert_wh(meter.energy_wh(), 1120.0);
    }

    #[test]
    fn backwards_timestamps_never_reduce_energy() {
        let mut meter = SimulatedMeter::default();
        meter.update(t(0), ConnectorStatus::Charging);
        meter.update(t(60), ConnectorStatus::Charging);
        meter.update(t(30), ConnectorStatus::Charging);
        assert_wh(meter.energy_wh(), 120.0);
    }

    #[test]
    fn power_cap_limits_the_rate_and_the_readings() {
        let mut meter = SimulatedMeter::default();
        meter.set_power_limit_w(Some(3600.0));
        meter.update(t(0), ConnectorStatus::Charging);
        let mv = meter.sample(
            t(60),
            ConnectorStatus::Charging,
            &[Measurand::PowerActiveImport, Measurand::CurrentImport],
            ReadingContext::SamplePeriodic,
        );
        // 3600 W for 60 s = 60 Wh. Cycle at 60 Wh: round(0.5) = 1, so voltage 229 V.
        assert_wh(meter.energy_wh(), 60.0);
        assert_eq!(sampled(&mv, Measurand::PowerActiveImport).value, "3600");
        assert_eq!(sampled(&mv, Measurand::CurrentImport).value, "15.7");
    }

    #[test]
    fn stop_transaction_data_orders_transaction_end_before_clock() {
        let mut meter = SimulatedMeter::default();
        meter.update(t(0), ConnectorStatus::Charging);
        let data = stop_transaction_data(
            &mut meter,
            t(60),
            ConnectorStatus::Charging,
            &[Measurand::EnergyActiveImportRegister],
            &[Measurand::PowerActiveImport],
        );
        assert_eq!(data.len(), 2);
        assert_eq!(
            data[0].sampled_value[0].context,
            Some(ReadingContext::TransactionEnd)
        );
        assert_eq!(
            data[1].sampled_value[0].context,
            Some(ReadingContext::SampleClock)
        );
        assert_wh(meter.energy_wh(), 120.0);
    }

    #[test]
    fn stop_transaction_data_skips_empty_lists() {
        let mut meter = SimulatedMeter::default();
        let only_end = stop_transaction_data(
            &mut meter,
            t(0),
            ConnectorStatus::Available,
            &[Measurand::Voltage],
            &[],
        );
        assert_eq!(only_end.len(), 1);
        assert_eq!(
            only_end[0].sampled_value[0].context,
            Some(ReadingContext::TransactionEnd)
        );
        assert_eq!(
            stop_transaction_data(&mut meter, t(0), ConnectorStatus::Available, &[], &[]),
            Vec::<MeterValue>::new()
        );
    }

    #[test]
    fn sampled_data_parsing_and_validation() {
        assert_eq!(
            parse_sampled_data("Energy.Active.Import.Register, Voltage").unwrap(),
            vec![Measurand::EnergyActiveImportRegister, Measurand::Voltage]
        );
        assert_eq!(
            parse_sampled_data("a,,b"),
            Err(SampledDataError::Unknown(vec!["a".into(), "b".into()]))
        );
        assert_eq!(parse_sampled_data(" , "), Err(SampledDataError::Empty));
        assert_eq!(
            parse_sampled_data("Voltage,Bogus"),
            Err(SampledDataError::Unknown(vec!["Bogus".into()]))
        );
    }

    #[test]
    fn configured_measurands_drop_unknown_items_and_fall_back_to_default() {
        assert_eq!(
            configured_measurands(Some("Bogus")),
            DEFAULT_SAMPLED_DATA.to_vec()
        );
        assert_eq!(configured_measurands(None), DEFAULT_SAMPLED_DATA.to_vec());
        assert_eq!(
            configured_measurands(Some("")),
            DEFAULT_SAMPLED_DATA.to_vec()
        );
        assert_eq!(
            configured_measurands(Some("Voltage,Bogus")),
            vec![Measurand::Voltage]
        );
    }

    #[test]
    fn clock_alignment_targets_epoch_multiples() {
        // 12:00:00 is a whole minute, so a 60 s interval waits a full interval.
        assert_eq!(clock_aligned_delay(t(0), 60), Some(TimeDelta::seconds(60)));
        assert_eq!(clock_aligned_delay(t(30), 60), Some(TimeDelta::seconds(30)));
        assert_eq!(
            clock_aligned_delay(t(0) + TimeDelta::milliseconds(59_999), 60),
            Some(TimeDelta::milliseconds(1))
        );
        assert_eq!(next_clock_aligned(t(30), 60), Some(t(60)));
        assert_eq!(clock_aligned_delay(t(0), 0), None);
        assert_eq!(next_clock_aligned(t(0), 0), None);
    }

    proptest! {
        #[test]
        fn energy_never_decreases(
            steps in prop::collection::vec((-5_000i64..20_000, any::<bool>(), any::<bool>()), 1..40),
            limit in prop::option::of(0.0f64..10_000.0),
        ) {
            let mut meter = SimulatedMeter::default();
            meter.set_power_limit_w(limit);
            let mut clock_ms = 0i64;
            let mut previous = meter.energy_wh();
            for (delta_ms, charging, sample) in steps {
                clock_ms += delta_ms;
                let at = t(0) + TimeDelta::milliseconds(clock_ms);
                let status = if charging {
                    ConnectorStatus::Charging
                } else {
                    ConnectorStatus::Available
                };
                if sample {
                    meter.sample(at, status, &DEFAULT_SAMPLED_DATA, ReadingContext::SamplePeriodic);
                } else {
                    meter.update(at, status);
                }
                prop_assert!(meter.energy_wh() >= previous);
                previous = meter.energy_wh();
            }
        }
    }
}
