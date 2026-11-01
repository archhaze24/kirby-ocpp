//! Simulated power and energy, MeterValues construction and clock alignment.
//!
//! TS reference: `src/station/metering.ts` (readings, configuration parsing) and
//! `src/station/meter-value-scheduler.ts` (clock alignment). The power model differs from TS
//! on purpose (OCPP 1.6 behaviour wins): see below.
//!
//! # Power model
//!
//! A charging connector draws `min(nominal power, charging limit)` watts. The limit comes
//! from the stored charging profiles (see [`super::smart_charging::limit_w_at`]); an
//! ampere limit is converted with the configured voltage and phase count
//! ([`PowerModel`]). Without a limit the connector draws the nominal power. Energy is the
//! integral of that power over elapsed time
//! ([`super::smart_charging::energy_wh`]), so a limit change mid-session shows up in the
//! register, and every reported measurand is derived from the same numbers:
//!
//! * `Power.Active.Import` = the power, `Current.Import` = power / (voltage x phases),
//!   `Voltage` = the configured voltage;
//! * `Power.Offered` / `Current.Offered` = the limit in force (`min(nominal, limit)`), also
//!   while idle;
//! * `Energy.Active.Import.Register` = the register, `Energy.Active.Import.Interval` = energy
//!   since the previous sample;
//! * temperature and SoC are decorative functions of the energy.
//!
//! The integration state lives in the station actor (`power.rs`); this module is pure.

use chrono::{DateTime, TimeDelta, Utc};
use thiserror::Error;

use crate::config::StationConfig;
use crate::ocpp::messages::{MeterValue, SampledValue};
use crate::ocpp::types::{Location, Measurand, Phase, ReadingContext, UnitOfMeasure, ValueFormat};

/// Measurands sampled when the configuration does not name any (TS `DEFAULT_SAMPLED_DATA`).
pub const DEFAULT_SAMPLED_DATA: [Measurand; 6] = [
    Measurand::EnergyActiveImportRegister,
    Measurand::PowerActiveImport,
    Measurand::CurrentImport,
    Measurand::Voltage,
    Measurand::Temperature,
    Measurand::SoC,
];

/// Electrical parameters of the station: nominal power, voltage and phases.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PowerModel {
    /// Nominal power of one connector in watts: the simulated power never exceeds it.
    pub nominal_w: f64,
    /// Voltage per phase in volts.
    pub voltage_v: f64,
    /// Phases the station charges with (at least 1).
    pub phases: u8,
}

impl PowerModel {
    pub fn from_config(config: &StationConfig) -> Self {
        Self {
            nominal_w: f64::from(config.nominal_power_w),
            voltage_v: f64::from(config.voltage_v),
            phases: config.phases.max(1),
        }
    }

    /// Phases used for a schedule period: `numberPhases`, or all of the station's when
    /// absent, and never more than the station has.
    pub fn period_phases(&self, number_phases: Option<i32>) -> f64 {
        let station = i32::from(self.phases);
        f64::from(number_phases.map_or(station, |n| n.clamp(1, station)))
    }

    /// Power of `amps` per phase on `number_phases` phases.
    pub fn amps_to_watts(&self, amps: f64, number_phases: Option<i32>) -> f64 {
        amps * self.voltage_v * self.period_phases(number_phases)
    }

    /// Current per phase that `watts` need on all of the station's phases.
    pub fn watts_to_amps(&self, watts: f64) -> f64 {
        watts / (self.voltage_v * f64::from(self.phases))
    }

    /// The power a connector can draw under `limit_w` (`None` = no limit).
    pub fn effective_w(&self, limit_w: Option<f64>) -> f64 {
        limit_w.map_or(self.nominal_w, |limit| limit.clamp(0.0, self.nominal_w))
    }
}

/// Energy `power_w` delivers in `elapsed`.
pub fn energy_wh_for(power_w: f64, elapsed: std::time::Duration) -> f64 {
    power_w * elapsed.as_secs_f64() / 3600.0
}

/// What the meter of one connector (or the station) shows at one instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeterReading {
    /// The energy register in Wh.
    pub energy_wh: f64,
    /// `true` while the connector is `Charging`.
    pub charging: bool,
    /// Power drawn now in watts (0 unless charging).
    pub power_w: f64,
    /// Power the station offers now in watts (the limit in force).
    pub offered_w: f64,
    /// Energy since the previous sample in Wh.
    pub interval_wh: f64,
}

/// Builds one MeterValue with the requested measurands from `reading`.
pub fn meter_value(
    model: &PowerModel,
    reading: &MeterReading,
    at: DateTime<Utc>,
    measurands: &[Measurand],
    context: ReadingContext,
) -> MeterValue {
    let readings = compute_readings(model, reading);
    MeterValue {
        timestamp: at,
        sampled_value: measurands
            .iter()
            .map(|measurand| sampled_value(&readings, context, *measurand))
            .collect(),
    }
}

/// Builds the `transactionData` of a StopTransaction (TS `buildStopTransactionData`).
///
/// `Transaction.End` with `sampled_data` (StopTxnSampledData) comes first, then
/// `Sample.Clock` with `aligned_data` (StopTxnAlignedData), both at `at` from the same
/// `reading`. An empty list produces no MeterValue. TS always sent both, so an empty list
/// produced a MeterValue with an empty `sampledValue`, which the schema rejects.
pub fn stop_transaction_data(
    model: &PowerModel,
    reading: &MeterReading,
    at: DateTime<Utc>,
    sampled_data: &[Measurand],
    aligned_data: &[Measurand],
) -> Vec<MeterValue> {
    let mut data = Vec::with_capacity(2);
    if !sampled_data.is_empty() {
        data.push(meter_value(
            model,
            reading,
            at,
            sampled_data,
            ReadingContext::TransactionEnd,
        ));
    }
    if !aligned_data.is_empty() {
        data.push(meter_value(
            model,
            reading,
            at,
            aligned_data,
            ReadingContext::SampleClock,
        ));
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

/// Readings derived from a [`MeterReading`] and the [`PowerModel`].
#[derive(Debug, Clone, Copy, PartialEq)]
struct Readings {
    charging: bool,
    energy_wh: f64,
    interval_wh: f64,
    power_w: f64,
    offered_w: f64,
    voltage_v: f64,
    current_a: f64,
    offered_a: f64,
    temperature_c: i64,
    soc_percent: i64,
}

fn compute_readings(model: &PowerModel, reading: &MeterReading) -> Readings {
    let energy_wh = reading.energy_wh;
    let charging = reading.charging;
    let cycle = round_half_up(energy_wh / 120.0).abs() % 8;
    let power_w = if charging { reading.power_w } else { 0.0 };
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
        interval_wh: reading.interval_wh,
        power_w,
        offered_w: reading.offered_w,
        voltage_v: model.voltage_v,
        current_a: model.watts_to_amps(power_w),
        offered_a: model.watts_to_amps(reading.offered_w),
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
        Measurand::EnergyActiveImportInterval => (
            round_half_up(readings.interval_wh).to_string(),
            Some(UnitOfMeasure::Wh),
            outlet,
            None,
        ),
        Measurand::PowerActiveImport => (
            format_decimal(readings.power_w),
            Some(UnitOfMeasure::W),
            outlet,
            None,
        ),
        Measurand::PowerActiveExport => (zero(), Some(UnitOfMeasure::W), outlet, None),
        Measurand::PowerOffered => (
            format_decimal(readings.offered_w),
            Some(UnitOfMeasure::W),
            outlet,
            None,
        ),
        Measurand::PowerReactiveImport | Measurand::PowerReactiveExport => {
            (zero(), Some(UnitOfMeasure::Var), outlet, None)
        }
        Measurand::PowerFactor => (when_charging("1"), None, outlet, None),
        Measurand::CurrentImport => (
            format_decimal(readings.current_a),
            Some(UnitOfMeasure::A),
            outlet,
            Some(Phase::L1),
        ),
        Measurand::CurrentExport => (zero(), Some(UnitOfMeasure::A), outlet, Some(Phase::L1)),
        Measurand::CurrentOffered => (
            format_decimal(readings.offered_a),
            Some(UnitOfMeasure::A),
            outlet,
            Some(Phase::L1),
        ),
        Measurand::Voltage => (
            format_decimal(readings.voltage_v),
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
fn format_decimal(value: f64) -> String {
    let fixed = format!("{value:.1}");
    fixed.strip_suffix(".0").unwrap_or(&fixed).to_owned()
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

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    /// 2026-11-01T12:00:00Z plus `seconds`.
    fn t(seconds: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap() + TimeDelta::seconds(seconds)
    }

    fn model() -> PowerModel {
        PowerModel {
            nominal_w: 7200.0,
            voltage_v: 230.0,
            phases: 1,
        }
    }

    fn charging(energy_wh: f64, power_w: f64) -> MeterReading {
        MeterReading {
            energy_wh,
            charging: true,
            power_w,
            offered_w: power_w,
            interval_wh: 12.0,
        }
    }

    fn idle(energy_wh: f64) -> MeterReading {
        MeterReading {
            energy_wh,
            charging: false,
            power_w: 0.0,
            offered_w: 7200.0,
            interval_wh: 0.0,
        }
    }

    fn sample(model: &PowerModel, reading: &MeterReading, m: &[Measurand]) -> MeterValue {
        meter_value(model, reading, t(0), m, ReadingContext::SamplePeriodic)
    }

    fn sampled(mv: &MeterValue, measurand: Measurand) -> &SampledValue {
        mv.sampled_value
            .iter()
            .find(|value| value.measurand == Some(measurand))
            .expect("measurand was requested")
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
    fn watts_amps_conversion_uses_voltage_and_phases() {
        let one = model();
        let three = PowerModel { phases: 3, ..one };
        assert!((one.amps_to_watts(16.0, None) - 3680.0).abs() < 1e-9);
        assert!((three.amps_to_watts(16.0, None) - 11_040.0).abs() < 1e-9);
        // numberPhases narrows the station's phases but never widens them.
        assert!((three.amps_to_watts(16.0, Some(1)) - 3680.0).abs() < 1e-9);
        assert!((one.amps_to_watts(16.0, Some(3)) - 3680.0).abs() < 1e-9);
        assert!((three.watts_to_amps(11_040.0) - 16.0).abs() < 1e-9);
    }

    #[test]
    fn effective_power_is_the_smaller_of_nominal_and_limit() {
        let m = model();
        assert!((m.effective_w(None) - 7200.0).abs() < 1e-9);
        assert!((m.effective_w(Some(3000.0)) - 3000.0).abs() < 1e-9);
        assert!((m.effective_w(Some(99_000.0)) - 7200.0).abs() < 1e-9);
        assert!(m.effective_w(Some(-5.0)).abs() < 1e-9);
    }

    #[test]
    fn energy_is_power_times_time() {
        let wh = energy_wh_for(3600.0, std::time::Duration::from_secs(30));
        assert!((wh - 30.0).abs() < 1e-9);
        assert!((energy_wh_for(7200.0, std::time::Duration::from_secs(60)) - 120.0).abs() < 1e-9);
    }

    #[test]
    fn readings_are_derived_from_the_same_numbers() {
        // 3680 W on 230 V single phase = 16 A; offered equals the limit in force.
        let mv = sample(&model(), &charging(1440.0, 3680.0), Measurand::ALL);
        assert_eq!(sampled(&mv, Measurand::PowerActiveImport).value, "3680");
        assert_eq!(sampled(&mv, Measurand::CurrentImport).value, "16");
        assert_eq!(sampled(&mv, Measurand::Voltage).value, "230");
        assert_eq!(sampled(&mv, Measurand::PowerOffered).value, "3680");
        assert_eq!(sampled(&mv, Measurand::CurrentOffered).value, "16");
        assert_eq!(
            sampled(&mv, Measurand::EnergyActiveImportRegister).value,
            "1440"
        );
        assert_eq!(
            sampled(&mv, Measurand::EnergyActiveImportInterval).value,
            "12"
        );
    }

    #[test]
    fn three_phase_current_is_per_phase() {
        let m = PowerModel {
            phases: 3,
            nominal_w: 11_040.0,
            ..model()
        };
        let mv = sample(&m, &charging(0.0, 11_040.0), &[Measurand::CurrentImport]);
        assert_eq!(sampled(&mv, Measurand::CurrentImport).value, "16");
    }

    #[test]
    fn golden_charging_sample_shape() {
        let mv = sample(&model(), &charging(1440.0, 7200.0), &DEFAULT_SAMPLED_DATA);
        assert_eq!(mv.timestamp, t(0));
        assert_eq!(mv.sampled_value.len(), 6);
        assert_eq!(sampled(&mv, Measurand::CurrentImport).value, "31.3");
        // cycle = round(1440/120) % 8 = 4: temperature 29 + 4, SoC 18 + 3.
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
            sampled(&mv, Measurand::Temperature).location,
            Some(Location::Body)
        );
        assert_eq!(sampled(&mv, Measurand::SoC).location, Some(Location::EV));
    }

    #[test]
    fn idle_readings_draw_nothing_but_keep_the_offer() {
        let mv = sample(&model(), &idle(1440.0), Measurand::ALL);
        assert_eq!(
            sampled(&mv, Measurand::EnergyActiveImportRegister).value,
            "1440"
        );
        assert_eq!(sampled(&mv, Measurand::PowerActiveImport).value, "0");
        assert_eq!(sampled(&mv, Measurand::CurrentImport).value, "0");
        assert_eq!(sampled(&mv, Measurand::Voltage).value, "230");
        assert_eq!(sampled(&mv, Measurand::PowerOffered).value, "7200");
        assert_eq!(sampled(&mv, Measurand::Temperature).value, "25");
        assert_eq!(sampled(&mv, Measurand::SoC).value, "0");
    }

    #[test]
    fn every_measurand_has_a_sample() {
        let mv = sample(&model(), &charging(1000.0, 7200.0), Measurand::ALL);
        assert_eq!(mv.sampled_value.len(), Measurand::ALL.len());
        assert_eq!(sampled(&mv, Measurand::PowerFactor).value, "1");
        assert_eq!(sampled(&mv, Measurand::PowerFactor).unit, None);
        assert_eq!(sampled(&mv, Measurand::Frequency).value, "50");
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
    fn stop_transaction_data_orders_transaction_end_before_clock() {
        let data = stop_transaction_data(
            &model(),
            &charging(120.0, 7200.0),
            t(60),
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
    }

    #[test]
    fn stop_transaction_data_skips_empty_lists() {
        let only_end =
            stop_transaction_data(&model(), &idle(0.0), t(0), &[Measurand::Voltage], &[]);
        assert_eq!(only_end.len(), 1);
        assert_eq!(
            only_end[0].sampled_value[0].context,
            Some(ReadingContext::TransactionEnd)
        );
        assert_eq!(
            stop_transaction_data(&model(), &idle(0.0), t(0), &[], &[]),
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
}
