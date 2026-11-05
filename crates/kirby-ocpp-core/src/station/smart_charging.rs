//! Smart charging rules: profile validity, schedule evaluation and the
//! composite schedule (`GetCompositeSchedule`).
//!
//! TS reference: `src/station/smart-charging.ts` and the composite part of
//! `src/station/charging-profile-registry.ts`. The store that owns the
//! profiles lives in [`super::charging_profiles`].
//!
//! # Time model
//!
//! Everything is evaluated at [`DateTime<Utc>`] instants. A composite schedule
//! is piecewise constant on the whole-second grid that starts at its schedule
//! start, so the algorithm only evaluates the instants where something can
//! change (see [`change_instants`]) and rounds those up to the grid.
//!
//! # Precedence (OCPP 1.6 composite schedule)
//!
//! For one instant and one connector:
//!
//! 1. Only profiles in the requested `chargingRateUnit` take part (no A/W
//!    conversion). Without a requested unit, the unit of the highest-priority
//!    valid profile is used (TxProfile, then TxDefaultProfile, then
//!    ChargePointMaxProfile, highest stack level first; `A` if none).
//! 2. Within a purpose the highest stack level that is *active at that
//!    instant* wins. A higher stack that has ended or not started yet falls
//!    through to the next one.
//! 3. A TxProfile bound to the connector's active transaction overrides any
//!    TxDefaultProfile. TxDefaultProfiles of the connector and of connector 0
//!    compete on stack level; the connector's own profile wins a tie.
//! 4. The ChargePointMaxProfile (connector 0 only) caps the result: the limit
//!    is the minimum of the winning TxProfile/TxDefaultProfile and the
//!    ChargePointMaxProfile limit.
//!
//! The TS code took the minimum over all three purposes, so a TxProfile did
//! not override a TxDefaultProfile. The smoke assertions hold under the spec
//! rule as well; the difference is pinned by a unit test.
//!
//! # Gaps and anchors
//!
//! A profile only limits the instants it covers (inside its validity window, after its
//! start, inside its `duration`, per recurrence cycle). Where no profile covers an instant
//! there is no limit: the station is bound only by its own maximum. A schedule period
//! cannot say "no limit", so the composite schedule always spans the whole requested
//! window, starts at the requested time, and shows the station's nominal power (converted
//! to the requested unit) for the uncovered stretches. The request is answered with
//! `Rejected` only if no profile of the requested unit covers any instant of the window.
//!
//! The schedule of an `Absolute` or `Recurring` profile counts from `startSchedule`, else
//! `validFrom`, else the moment the profile was installed. A `Relative` profile counts from
//! the start of the transaction (the composite request time if there is none).
//!
//! # Power
//!
//! [`limit_w_at`] and [`energy_wh`] apply the same precedence to the profiles of both units
//! after converting amperes to watts ([`PowerModel`]); the simulated meter uses them.

use chrono::{DateTime, TimeDelta, Utc};
use thiserror::Error;

use super::charging_profiles::StoredProfile;
use super::metering::PowerModel;
use crate::ids::{ConnectorId, TxId};
use crate::ocpp::messages::{ChargingProfile, ChargingSchedule, ChargingSchedulePeriod};
use crate::ocpp::types::{
    ChargingProfileKindType, ChargingProfilePurposeType, ChargingRateUnitType, RecurrencyKindType,
};

/// Seconds in a day (daily recurrence period).
pub const DAY_SECONDS: i64 = 86_400;
/// Seconds in a week (weekly recurrence period).
pub const WEEK_SECONDS: i64 = 604_800;
/// Longest window a composite schedule covers; a longer `duration` is clamped to it (the
/// answer carries the clamped `duration`).
pub const MAX_COMPOSITE_DURATION_S: i32 = 604_800;
/// Most periods in a composite schedule; the tail beyond it is cut and the answer's
/// `duration` ends where the first cut period would have started.
pub const MAX_COMPOSITE_PERIODS: usize = 1000;

/// Structural problem of a `ChargingProfile`, independent of the station's limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ShapeError {
    #[error("stackLevel must not be negative")]
    NegativeStackLevel,
    #[error("chargingSchedulePeriod must not be empty")]
    NoPeriods,
    #[error("the first chargingSchedulePeriod must start at 0")]
    FirstPeriodNotZero,
    #[error("chargingSchedulePeriod startPeriod values must strictly increase")]
    PeriodsNotIncreasing,
    #[error("limit must be finite and not negative")]
    InvalidLimit,
    #[error("chargingSchedule duration must be positive")]
    NonPositiveDuration,
}

/// Checks the structure of a profile. Returns the first problem found.
pub fn shape_error(profile: &ChargingProfile) -> Option<ShapeError> {
    let schedule = &profile.charging_schedule;
    let periods = &schedule.charging_schedule_period;

    if profile.stack_level < 0 {
        return Some(ShapeError::NegativeStackLevel);
    }
    if schedule.duration.is_some_and(|duration| duration <= 0) {
        return Some(ShapeError::NonPositiveDuration);
    }
    let Some(first) = periods.first() else {
        return Some(ShapeError::NoPeriods);
    };
    if first.start_period != 0 {
        return Some(ShapeError::FirstPeriodNotZero);
    }
    if periods
        .windows(2)
        .any(|pair| pair[1].start_period <= pair[0].start_period)
    {
        return Some(ShapeError::PeriodsNotIncreasing);
    }
    if periods
        .iter()
        .any(|period| !period.limit.is_finite() || period.limit < 0.0)
    {
        return Some(ShapeError::InvalidLimit);
    }
    None
}

/// `true` when `at` lies inside the profile's `validFrom`/`validTo` window (both inclusive).
pub fn is_valid_at(profile: &ChargingProfile, at: DateTime<Utc>) -> bool {
    profile.valid_from.is_none_or(|from| at >= from) && profile.valid_to.is_none_or(|to| at <= to)
}

/// `true` when the profile's `validTo` lies before `at`.
pub fn is_expired(profile: &ChargingProfile, at: DateTime<Utc>) -> bool {
    profile.valid_to.is_some_and(|to| at > to)
}

/// Length of one recurrence cycle in milliseconds; `None` for non-recurring profiles.
///
/// A recurring profile without `recurrencyKind` repeats daily (TS behaviour).
fn recurrence_period_ms(profile: &ChargingProfile) -> Option<i64> {
    if profile.charging_profile_kind != ChargingProfileKindType::Recurring {
        return None;
    }
    let seconds = match profile.recurrency_kind {
        Some(RecurrencyKindType::Weekly) => WEEK_SECONDS,
        Some(RecurrencyKindType::Daily) | None => DAY_SECONDS,
    };
    Some(seconds * 1000)
}

/// The instant that elapsed time of a profile's schedule is counted from.
///
/// * `Absolute` and `Recurring`: `startSchedule`, else `validFrom`, else `installed_at`
///   (the moment the profile was installed), else `fallback_start`.
/// * `Relative`: the transaction start when known, else `fallback_start`
///   (`startSchedule` is ignored, as in TS).
pub fn schedule_base(
    profile: &ChargingProfile,
    relative_start: Option<DateTime<Utc>>,
    installed_at: Option<DateTime<Utc>>,
    fallback_start: DateTime<Utc>,
) -> DateTime<Utc> {
    match profile.charging_profile_kind {
        ChargingProfileKindType::Relative => relative_start.unwrap_or(fallback_start),
        ChargingProfileKindType::Absolute | ChargingProfileKindType::Recurring => profile
            .charging_schedule
            .start_schedule
            .or(profile.valid_from)
            .or(installed_at)
            .unwrap_or(fallback_start),
    }
}

/// The schedule period in force for `profile` at `at`, if any.
///
/// `base` comes from [`schedule_base`]. Returns `None` when the profile is outside its
/// validity window, before its base, or past its schedule `duration` (per recurrence
/// cycle for recurring profiles). Before its base an entry is not active yet, so a future
/// `startSchedule` does not start early.
pub fn period_at(
    profile: &ChargingProfile,
    at: DateTime<Utc>,
    base: DateTime<Utc>,
) -> Option<&ChargingSchedulePeriod> {
    if !is_valid_at(profile, at) || at < base {
        return None;
    }
    let mut elapsed_ms = (at - base).num_milliseconds();
    if let Some(cycle_ms) = recurrence_period_ms(profile) {
        elapsed_ms = elapsed_ms.rem_euclid(cycle_ms);
    }
    let schedule: &ChargingSchedule = &profile.charging_schedule;
    if schedule
        .duration
        .is_some_and(|duration| elapsed_ms >= i64::from(duration) * 1000)
    {
        return None;
    }
    schedule
        .charging_schedule_period
        .iter()
        .filter(|period| i64::from(period.start_period) * 1000 <= elapsed_ms)
        .max_by_key(|period| period.start_period)
}

/// Instants in `[from, until]` where the output of `profile` may change.
///
/// The result is a superset: the caller evaluates the state around each instant,
/// so duplicates and irrelevant instants are harmless.
pub fn change_instants(
    profile: &ChargingProfile,
    base: DateTime<Utc>,
    from: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Vec<DateTime<Utc>> {
    let mut candidates: Vec<Option<DateTime<Utc>>> =
        vec![profile.valid_from, profile.valid_to, Some(base)];
    let schedule = &profile.charging_schedule;

    if let Some(cycle_ms) = recurrence_period_ms(profile) {
        let first_cycle = (from - base).num_milliseconds().div_euclid(cycle_ms) - 1;
        let last_cycle = (until - base).num_milliseconds().div_euclid(cycle_ms) + 1;
        for cycle in first_cycle..=last_cycle {
            let Some(cycle_start) = offset_ms(base, cycle.saturating_mul(cycle_ms)) else {
                continue;
            };
            candidates.extend(
                schedule
                    .charging_schedule_period
                    .iter()
                    .map(|period| offset_ms(cycle_start, i64::from(period.start_period) * 1000)),
            );
            candidates.push(
                schedule
                    .duration
                    .and_then(|duration| offset_ms(cycle_start, i64::from(duration) * 1000)),
            );
        }
    } else {
        candidates.extend(
            schedule
                .charging_schedule_period
                .iter()
                .map(|period| offset_ms(base, i64::from(period.start_period) * 1000)),
        );
        candidates.push(
            schedule
                .duration
                .and_then(|duration| offset_ms(base, i64::from(duration) * 1000)),
        );
    }

    candidates
        .into_iter()
        .flatten()
        .filter(|instant| *instant >= from && *instant <= until)
        .collect()
}

fn offset_ms(instant: DateTime<Utc>, ms: i64) -> Option<DateTime<Utc>> {
    instant.checked_add_signed(TimeDelta::try_milliseconds(ms)?)
}

/// What a composite schedule is requested for.
#[derive(Debug, Clone, PartialEq)]
pub struct CompositeRequest {
    pub connector: ConnectorId,
    /// Requested length in seconds (`GetCompositeSchedule.duration`).
    pub duration_s: i32,
    /// Requested unit; `None` means "the unit the charge point prefers".
    pub unit: Option<ChargingRateUnitType>,
    /// Current time. The schedule start is this instant truncated to whole seconds.
    pub now: DateTime<Utc>,
    /// Number of physical connectors (`1..=connector_count` are known; `0` is the station).
    pub connector_count: u32,
    /// Transaction active on `connector`, if any.
    pub active_tx: Option<TxId>,
    /// Start of `active_tx`: the base of `Relative` profiles.
    pub tx_started_at: Option<DateTime<Utc>>,
    /// The station's electrical parameters: its nominal power fills stretches no profile covers.
    pub model: PowerModel,
}

/// A composite schedule as returned by `GetCompositeSchedule.conf`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompositeSchedule {
    pub connector_id: ConnectorId,
    /// The requested `now` (truncated), or a later covered instant (see module docs).
    pub schedule_start: DateTime<Utc>,
    pub charging_schedule: ChargingSchedule,
}

/// Why no composite schedule was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CompositeError {
    #[error("connector {0} is unknown")]
    UnknownConnector(ConnectorId),
    #[error("the requested duration must be positive")]
    NonPositiveDuration,
    /// No profile in the requested unit covers any instant of the window.
    #[error("no charging profile applies in the requested window")]
    NoSchedule,
}

/// The limit in force at one instant after combining the winning profiles.
#[derive(Debug, Clone, Copy)]
struct Resolved {
    limit: f64,
    number_phases: Option<i32>,
    min_charging_rate: Option<f64>,
}

/// A profile in force at the evaluated instant, with its period.
#[derive(Clone, Copy)]
struct Candidate<'a> {
    entry: &'a StoredProfile,
    period: &'a ChargingSchedulePeriod,
    /// The period's limit in the comparison unit.
    limit: f64,
    /// Ranking inside its purpose: stack level, then "connector-specific" as the tie break.
    rank: (i32, bool),
}

/// Computes the composite schedule for `req` from the stored profiles.
///
/// `entries` must come from a store that already pruned expired profiles
/// (see [`super::charging_profiles::ChargingProfileStore::composite_schedule`]).
pub fn composite_schedule(
    entries: &[StoredProfile],
    req: &CompositeRequest,
) -> Result<CompositeSchedule, CompositeError> {
    if !is_known_connector(req.connector, req.connector_count) {
        return Err(CompositeError::UnknownConnector(req.connector));
    }
    if req.duration_s <= 0 {
        return Err(CompositeError::NonPositiveDuration);
    }

    let duration_s = req.duration_s.min(MAX_COMPOSITE_DURATION_S);
    let start = truncate_to_second(req.now);
    let unit = req
        .unit
        .unwrap_or_else(|| default_unit(entries, req, start));
    let window_end = start + TimeDelta::seconds(i64::from(duration_s));

    // Profiles of this unit that can affect the connector: its own and connector 0's.
    let usable: Vec<(&StoredProfile, DateTime<Utc>)> = entries
        .iter()
        .filter(|entry| entry.profile.charging_schedule.charging_rate_unit == unit)
        .filter(|entry| entry.connector_id == req.connector || entry.connector_id.is_station())
        .map(|entry| {
            (
                entry,
                schedule_base(&entry.profile, req.tx_started_at, entry.installed_at, start),
            )
        })
        .collect();
    if usable.is_empty() {
        return Err(CompositeError::NoSchedule);
    }

    // Every whole second where the state may change, relative to `start`.
    let window_ms = i64::from(duration_s) * 1000;
    let mut offsets: Vec<i64> = vec![0];
    for &(entry, base) in &usable {
        for instant in change_instants(&entry.profile, base, start, window_end) {
            let ms = (instant - start).num_milliseconds();
            // The first whole second at or after the change, and the first one strictly after it.
            offsets.push((ms + 999).div_euclid(1000));
            offsets.push(ms.div_euclid(1000) + 1);
        }
    }
    offsets.retain(|offset| *offset >= 0 && *offset * 1000 < window_ms);
    offsets.sort_unstable();
    offsets.dedup();

    let points: Vec<(i64, Option<Resolved>)> = offsets
        .into_iter()
        .map(|offset| {
            let at = start + TimeDelta::seconds(offset);
            (
                offset,
                resolve_at(&usable, req.connector, req.active_tx, at, &|_, p| p.limit),
            )
        })
        .collect();
    if points.iter().all(|(_, resolved)| resolved.is_none()) {
        return Err(CompositeError::NoSchedule);
    }

    // Stretches no profile covers are limited by the station only.
    let station_limit = match unit {
        ChargingRateUnitType::W => req.model.nominal_w,
        ChargingRateUnitType::A => round_tenth(req.model.watts_to_amps(req.model.nominal_w)),
    };
    let mut periods: Vec<ChargingSchedulePeriod> = Vec::new();
    let mut min_charging_rate: Option<f64> = None;
    for (offset, resolved) in &points {
        let (limit, number_phases) = match resolved {
            Some(resolved) => {
                min_charging_rate = max_option(min_charging_rate, resolved.min_charging_rate);
                (resolved.limit, resolved.number_phases)
            }
            None => (station_limit, None),
        };
        if let Some(last) = periods.last()
            && last.limit.to_bits() == limit.to_bits()
            && last.number_phases == number_phases
        {
            continue;
        }
        periods.push(ChargingSchedulePeriod {
            start_period: i32::try_from(*offset).unwrap_or(i32::MAX),
            limit,
            number_phases,
        });
    }

    let mut duration_s = duration_s;
    if periods.len() > MAX_COMPOSITE_PERIODS {
        if let Some(first_cut) = periods.get(MAX_COMPOSITE_PERIODS) {
            duration_s = first_cut.start_period.min(duration_s);
        }
        periods.truncate(MAX_COMPOSITE_PERIODS);
    }

    Ok(CompositeSchedule {
        connector_id: req.connector,
        schedule_start: start,
        charging_schedule: ChargingSchedule {
            duration: Some(duration_s),
            start_schedule: Some(start),
            charging_rate_unit: unit,
            charging_schedule_period: periods,
            min_charging_rate,
        },
    })
}

fn round_tenth(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// The limit of `period` of `entry` in the unit the caller compares in.
type LimitOf<'a> = dyn Fn(&StoredProfile, &ChargingSchedulePeriod) -> f64 + 'a;

fn resolve_at(
    usable: &[(&StoredProfile, DateTime<Utc>)],
    connector: ConnectorId,
    active_tx: Option<TxId>,
    at: DateTime<Utc>,
    limit_of: &LimitOf<'_>,
) -> Option<Resolved> {
    let mut tx_best: Option<Candidate<'_>> = None;
    let mut default_best: Option<Candidate<'_>> = None;
    let mut cap_best: Option<Candidate<'_>> = None;

    for &(entry, base) in usable {
        let Some(period) = period_at(&entry.profile, at, base) else {
            continue;
        };
        let candidate = Candidate {
            entry,
            period,
            limit: limit_of(entry, period),
            rank: (entry.profile.stack_level, entry.connector_id == connector),
        };
        match entry.profile.charging_profile_purpose {
            ChargingProfilePurposeType::TxProfile => {
                let bound = active_tx.is_some() && entry.bound_tx == active_tx;
                if entry.connector_id == connector && bound {
                    tx_best = Some(keep_higher(tx_best, candidate));
                }
            }
            ChargingProfilePurposeType::TxDefaultProfile => {
                default_best = Some(keep_higher(default_best, candidate));
            }
            ChargingProfilePurposeType::ChargePointMaxProfile => {
                if entry.connector_id.is_station() {
                    cap_best = Some(keep_higher(cap_best, candidate));
                }
            }
        }
    }

    let selected = tx_best.or(default_best);
    let min_charging_rate = selected
        .iter()
        .chain(cap_best.iter())
        .fold(None, |acc, candidate| {
            max_option(
                acc,
                candidate.entry.profile.charging_schedule.min_charging_rate,
            )
        });

    let (limit, number_phases) = match (selected, cap_best) {
        (None, None) => return None,
        (Some(only), None) | (None, Some(only)) => (only.limit, only.period.number_phases),
        (Some(profile), Some(cap)) => match profile.limit.partial_cmp(&cap.limit) {
            Some(std::cmp::Ordering::Less) => (profile.limit, profile.period.number_phases),
            Some(std::cmp::Ordering::Greater) => (cap.limit, cap.period.number_phases),
            _ => (
                profile.limit,
                min_option(profile.period.number_phases, cap.period.number_phases),
            ),
        },
    };

    Some(Resolved {
        limit,
        number_phases,
        min_charging_rate,
    })
}

/// The higher-ranked of `best` and `candidate`; the earlier one wins a tie.
fn keep_higher<'a>(best: Option<Candidate<'a>>, candidate: Candidate<'a>) -> Candidate<'a> {
    match best {
        Some(best) if best.rank >= candidate.rank => best,
        _ => candidate,
    }
}

/// Unit of the highest-priority profile valid at `start`; `A` when none is.
fn default_unit(
    entries: &[StoredProfile],
    req: &CompositeRequest,
    start: DateTime<Utc>,
) -> ChargingRateUnitType {
    let priority = |entry: &StoredProfile| -> Option<(u8, i32)> {
        let class = match entry.profile.charging_profile_purpose {
            ChargingProfilePurposeType::TxProfile => {
                let bound = req.active_tx.is_some() && entry.bound_tx == req.active_tx;
                if entry.connector_id != req.connector || !bound {
                    return None;
                }
                2
            }
            ChargingProfilePurposeType::TxDefaultProfile => {
                if entry.connector_id != req.connector && !entry.connector_id.is_station() {
                    return None;
                }
                1
            }
            ChargingProfilePurposeType::ChargePointMaxProfile => {
                if !entry.connector_id.is_station() {
                    return None;
                }
                0
            }
        };
        is_valid_at(&entry.profile, start).then_some((class, entry.profile.stack_level))
    };

    entries
        .iter()
        .filter_map(|entry| priority(entry).map(|key| (key, entry)))
        .max_by_key(|(key, _)| *key)
        .map_or(ChargingRateUnitType::A, |(_, entry)| {
            entry.profile.charging_schedule.charging_rate_unit
        })
}

/// What the power of one connector is evaluated for.
#[derive(Debug, Clone, Copy)]
pub struct PowerRequest {
    pub connector: ConnectorId,
    /// Transaction active on `connector`, if any.
    pub active_tx: Option<TxId>,
    /// Start of `active_tx`: the base of `Relative` profiles.
    pub tx_started_at: Option<DateTime<Utc>>,
    pub model: PowerModel,
    /// Connectors drawing power right now (at least 1). A `ChargePointMaxProfile` limits the
    /// whole station, so each of them gets an equal share of it.
    pub charging_connectors: u32,
}

/// Every profile that can affect the connector, with its schedule base.
fn power_profiles<'a>(
    entries: &'a [StoredProfile],
    req: &PowerRequest,
    fallback_start: DateTime<Utc>,
) -> Vec<(&'a StoredProfile, DateTime<Utc>)> {
    entries
        .iter()
        .filter(|entry| entry.connector_id == req.connector || entry.connector_id.is_station())
        .map(|entry| {
            (
                entry,
                schedule_base(
                    &entry.profile,
                    req.tx_started_at,
                    entry.installed_at,
                    fallback_start,
                ),
            )
        })
        .collect()
}

fn limit_w_resolved(
    usable: &[(&StoredProfile, DateTime<Utc>)],
    req: &PowerRequest,
    at: DateTime<Utc>,
) -> Option<f64> {
    let share = f64::from(req.charging_connectors.max(1));
    let limit_of = |entry: &StoredProfile, period: &ChargingSchedulePeriod| {
        let watts = match entry.profile.charging_schedule.charging_rate_unit {
            ChargingRateUnitType::W => period.limit,
            ChargingRateUnitType::A => req.model.amps_to_watts(period.limit, period.number_phases),
        };
        if entry.profile.charging_profile_purpose
            == ChargingProfilePurposeType::ChargePointMaxProfile
        {
            watts / share
        } else {
            watts
        }
    };
    resolve_at(usable, req.connector, req.active_tx, at, &limit_of).map(|r| r.limit)
}

/// The charging limit in watts in force at `at`, or `None` when no profile covers `at`
/// (the station is then limited only by its nominal power). Profiles in amperes are
/// converted with the station's voltage and phases.
pub fn limit_w_at(entries: &[StoredProfile], req: &PowerRequest, at: DateTime<Utc>) -> Option<f64> {
    limit_w_resolved(&power_profiles(entries, req, at), req, at)
}

/// Energy in Wh a charging connector takes in `[from, to]`: the integral of
/// `min(nominal power, limit)`, summed piecewise over the stretches where the limit is constant.
pub fn energy_wh(
    entries: &[StoredProfile],
    req: &PowerRequest,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> f64 {
    if to <= from {
        return 0.0;
    }
    let usable = power_profiles(entries, req, from);
    let mut cuts = vec![from, to];
    for &(entry, base) in &usable {
        cuts.extend(change_instants(&entry.profile, base, from, to));
    }
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .map(|pair| {
            let span = pair[1] - pair[0];
            let middle = pair[0] + span / 2;
            let power = req
                .model
                .effective_w(limit_w_resolved(&usable, req, middle));
            power * span.as_seconds_f64() / 3600.0
        })
        .sum()
}

/// Connector `0` is the station; `1..=count` are the physical connectors.
pub fn is_known_connector(connector: ConnectorId, connector_count: u32) -> bool {
    connector.is_station() || (1..=connector_count).contains(&connector.get())
}

/// Drops the sub-second part of `instant` (towards the past).
pub fn truncate_to_second(instant: DateTime<Utc>) -> DateTime<Utc> {
    let millis = instant.timestamp_subsec_millis();
    instant - TimeDelta::milliseconds(i64::from(millis))
}

fn max_option(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(a), Some(b)) => Some(if a >= b { a } else { b }),
        (a, None) => a,
        (None, b) => b,
    }
}

fn min_option(left: Option<i32>, right: Option<i32>) -> Option<i32> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use proptest::prelude::*;

    use super::*;
    use crate::ocpp::messages::{
        ChargingProfileStatus, ClearChargingProfileRequest, ClearChargingProfileStatus,
    };
    use crate::station::charging_profiles::test_util::{
        at, entry, period, profile, recurring, t0, tx_entry, with_unit,
    };
    use crate::station::charging_profiles::{ChargingProfileStore, ProfileContext, StoreLimits};

    const CONNECTOR_COUNT: u32 = 2;

    fn request(connector: u32, duration_s: i32) -> CompositeRequest {
        CompositeRequest {
            connector: ConnectorId::new(connector),
            duration_s,
            unit: None,
            now: t0(),
            connector_count: CONNECTOR_COUNT,
            active_tx: None,
            tx_started_at: None,
            model: model(),
        }
    }

    /// 7.2 kW, 230 V, single phase (the station defaults).
    fn model() -> PowerModel {
        PowerModel {
            nominal_w: 7200.0,
            voltage_v: 230.0,
            phases: 1,
        }
    }

    fn set(
        store: &mut ChargingProfileStore,
        connector: u32,
        profile: ChargingProfile,
        active_tx: Option<TxId>,
    ) -> ChargingProfileStatus {
        let limits = StoreLimits::default();
        store.set(
            ConnectorId::new(connector),
            profile,
            &ProfileContext {
                now: t0(),
                connector_count: CONNECTOR_COUNT,
                active_tx,
                limits: &limits,
            },
        )
    }

    fn accepted(store: &mut ChargingProfileStore, connector: u32, profile: ChargingProfile) {
        assert_eq!(
            set(store, connector, profile, None),
            ChargingProfileStatus::Accepted
        );
    }

    fn composite(store: &mut ChargingProfileStore, req: &CompositeRequest) -> CompositeSchedule {
        store.composite_schedule(req).expect("a composite schedule")
    }

    fn limits_of(schedule: &CompositeSchedule) -> Vec<f64> {
        schedule
            .charging_schedule
            .charging_schedule_period
            .iter()
            .map(|period| period.limit)
            .collect()
    }

    fn starts_of(schedule: &CompositeSchedule) -> Vec<i32> {
        schedule
            .charging_schedule
            .charging_schedule_period
            .iter()
            .map(|period| period.start_period)
            .collect()
    }

    fn cp_max(id: i32, unit: ChargingRateUnitType, limit: f64) -> ChargingProfile {
        with_unit(
            profile(
                id,
                1,
                ChargingProfilePurposeType::ChargePointMaxProfile,
                vec![period(0, limit)],
            ),
            unit,
        )
    }

    fn tx_default(id: i32, stack: i32, periods: Vec<ChargingSchedulePeriod>) -> ChargingProfile {
        profile(
            id,
            stack,
            ChargingProfilePurposeType::TxDefaultProfile,
            periods,
        )
    }

    fn assert_limits(schedule: &CompositeSchedule, expected: &[f64]) {
        assert_eq!(limits_of(schedule), expected);
    }

    // --- Ports of the smoke scenario (`scripts/smoke/scenarios/smart-charging.mjs`) ---

    #[test]
    fn smoke_composite_priority_uses_the_charge_point_cap() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 0, cp_max(2, ChargingRateUnitType::A, 20.0));
        accepted(&mut store, 1, tx_default(3, 2, vec![period(0, 32.0)]));
        // Expired and pruned by the composite call (smoke step 'expiredProfileResponse').
        let mut expired = tx_default(4, 100, vec![period(0, 1.0)]);
        expired.valid_to = Some(t0() - TimeDelta::seconds(60));
        assert_eq!(
            set(&mut store, 1, expired, None),
            ChargingProfileStatus::Accepted
        );
        assert_eq!(store.len(), 3);

        let schedule = composite(&mut store, &request(1, 60));
        assert_limits(&schedule, &[20.0]);
        assert_eq!(schedule.schedule_start, t0());
        assert_eq!(store.len(), 2, "the expired profile was pruned");
    }

    #[test]
    fn smoke_composite_with_unavailable_unit_is_rejected() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 0, cp_max(2, ChargingRateUnitType::A, 20.0));
        let mut req = request(1, 60);
        req.unit = Some(ChargingRateUnitType::W);
        assert_eq!(
            store.composite_schedule(&req),
            Err(CompositeError::NoSchedule)
        );
    }

    #[test]
    fn smoke_composite_unit_filter_ignores_higher_stack_watt_profile() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 0, cp_max(2, ChargingRateUnitType::A, 20.0));
        accepted(&mut store, 0, {
            let mut watt = cp_max(11, ChargingRateUnitType::W, 7000.0);
            watt.stack_level = 2;
            watt
        });

        let mut amps = request(1, 60);
        amps.unit = Some(ChargingRateUnitType::A);
        assert_limits(&composite(&mut store, &amps), &[20.0]);

        let mut watts = request(1, 60);
        watts.unit = Some(ChargingRateUnitType::W);
        assert_limits(&composite(&mut store, &watts), &[7000.0]);
    }

    #[test]
    fn smoke_composite_zero_duration_is_rejected() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 1, tx_default(1, 1, vec![period(0, 8.0)]));
        assert_eq!(
            store.composite_schedule(&request(1, 0)),
            Err(CompositeError::NonPositiveDuration)
        );
    }

    #[test]
    fn smoke_composite_unknown_connector_is_rejected() {
        let mut store = ChargingProfileStore::new();
        assert_eq!(
            store.composite_schedule(&request(99, 60)),
            Err(CompositeError::UnknownConnector(ConnectorId::new(99)))
        );
    }

    #[test]
    fn smoke_multi_period_absolute_profile() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 0, cp_max(2, ChargingRateUnitType::A, 20.0));
        accepted(
            &mut store,
            1,
            tx_default(6, 3, vec![period(0, 18.0), period(30, 12.0)]),
        );
        let schedule = composite(&mut store, &request(1, 90));
        assert_limits(&schedule, &[18.0, 12.0]);
        assert_eq!(starts_of(&schedule), vec![0, 30]);
        assert_eq!(schedule.charging_schedule.duration, Some(90));
    }

    #[test]
    fn smoke_daily_recurring_profile() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 0, cp_max(2, ChargingRateUnitType::A, 20.0));
        let start = t0() - TimeDelta::seconds(86_400 - 20);
        accepted(
            &mut store,
            1,
            recurring(
                7,
                3,
                ChargingProfilePurposeType::TxDefaultProfile,
                RecurrencyKindType::Daily,
                start,
                vec![period(0, 14.0), period(86_390, 11.0)],
            ),
        );
        let schedule = composite(&mut store, &request(1, 30));
        assert_limits(&schedule, &[14.0, 11.0, 14.0]);
        assert_eq!(starts_of(&schedule), vec![0, 10, 20]);
    }

    #[test]
    fn smoke_weekly_recurring_profile() {
        let mut store = ChargingProfileStore::new();
        let start = t0() - TimeDelta::seconds(604_800 - 20);
        accepted(
            &mut store,
            1,
            recurring(
                10,
                3,
                ChargingProfilePurposeType::TxDefaultProfile,
                RecurrencyKindType::Weekly,
                start,
                vec![period(0, 15.0), period(604_790, 13.0)],
            ),
        );
        let schedule = composite(&mut store, &request(1, 30));
        assert_limits(&schedule, &[15.0, 13.0, 15.0]);
    }

    #[test]
    fn smoke_and_ts_long_daily_recurring_profile_covers_all_cycles() {
        // TS test: `long daily recurring composite schedules include transitions across all requested cycles`.
        let mut store = ChargingProfileStore::new();
        let start = t0() - TimeDelta::seconds(86_400 - 10);
        accepted(
            &mut store,
            1,
            recurring(
                12,
                3,
                ChargingProfilePurposeType::TxDefaultProfile,
                RecurrencyKindType::Daily,
                start,
                vec![period(0, 17.0), period(86_395, 12.0)],
            ),
        );
        let schedule = composite(&mut store, &request(1, 2 * 24 * 60 * 60 + 20));
        assert_limits(&schedule, &[17.0, 12.0, 17.0, 12.0, 17.0, 12.0, 17.0]);
        assert_eq!(
            schedule.charging_schedule.duration,
            Some(2 * 24 * 60 * 60 + 20)
        );
    }

    #[test]
    fn a_huge_duration_is_clamped_to_seven_days() {
        let mut store = ChargingProfileStore::new();
        for id in 0..10 {
            accepted(
                &mut store,
                1,
                recurring(
                    20 + id,
                    3,
                    ChargingProfilePurposeType::TxDefaultProfile,
                    RecurrencyKindType::Daily,
                    t0(),
                    vec![period(0, 17.0), period(43_200, 12.0)],
                ),
            );
        }
        let schedule = composite(&mut store, &request(1, i32::MAX));
        let periods = &schedule.charging_schedule.charging_schedule_period;
        assert_eq!(
            schedule.charging_schedule.duration,
            Some(MAX_COMPOSITE_DURATION_S)
        );
        assert!(periods.len() <= 2 * 7 + 1, "{} periods", periods.len());
        assert!(
            periods
                .iter()
                .all(|p| p.start_period < MAX_COMPOSITE_DURATION_S)
        );
    }

    #[test]
    fn the_number_of_periods_is_capped_and_the_duration_follows() {
        let mut store = ChargingProfileStore::new();
        // 2000 alternating limits per day: ~14 000 changes in a week.
        let periods: Vec<_> = (0..2000)
            .map(|i| period(i * 40, if i % 2 == 0 { 16.0 } else { 8.0 }))
            .collect();
        accepted(
            &mut store,
            1,
            recurring(
                30,
                3,
                ChargingProfilePurposeType::TxDefaultProfile,
                RecurrencyKindType::Daily,
                t0(),
                periods,
            ),
        );
        let schedule = composite(&mut store, &request(1, i32::MAX));
        let out = &schedule.charging_schedule.charging_schedule_period;
        assert_eq!(out.len(), MAX_COMPOSITE_PERIODS);
        let duration = schedule.charging_schedule.duration.unwrap();
        assert!(duration > out.last().unwrap().start_period);
        assert!(duration < MAX_COMPOSITE_DURATION_S);
    }

    #[test]
    fn smoke_future_valid_from_does_not_affect_the_current_composite() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 0, cp_max(2, ChargingRateUnitType::A, 20.0));
        let mut future = tx_default(9, 99, vec![period(0, 1.0)]);
        future.valid_from = Some(at(60));
        accepted(&mut store, 1, future);
        assert_limits(&composite(&mut store, &request(1, 60)), &[20.0]);
    }

    #[test]
    fn smoke_valid_from_activation_inside_the_window() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 0, cp_max(2, ChargingRateUnitType::A, 20.0));
        accepted(&mut store, 1, tx_default(3, 2, vec![period(0, 32.0)]));
        let mut activating = tx_default(12, 3, vec![period(0, 10.0)]);
        activating.valid_from = Some(at(20));
        accepted(&mut store, 1, activating);

        let schedule = composite(&mut store, &request(1, 40));
        assert_limits(&schedule, &[20.0, 10.0]);
        assert_eq!(starts_of(&schedule), vec![0, 20]);
    }

    #[test]
    fn smoke_number_phases_and_min_charging_rate_propagate() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 0, cp_max(2, ChargingRateUnitType::A, 20.0));
        let mut phases = tx_default(13, 3, vec![period(0, 18.0)]);
        phases.charging_schedule.min_charging_rate = Some(6.0);
        phases.charging_schedule.charging_schedule_period[0].number_phases = Some(3);
        accepted(&mut store, 1, phases);

        let schedule = composite(&mut store, &request(1, 60));
        assert_limits(&schedule, &[18.0]);
        assert_eq!(
            schedule.charging_schedule.charging_schedule_period[0].number_phases,
            Some(3)
        );
        assert_eq!(schedule.charging_schedule.min_charging_rate, Some(6.0));
    }

    #[test]
    fn smoke_same_stack_latest_profile_wins_and_replaces_the_earlier_one() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 1, tx_default(14, 3, vec![period(0, 17.0)]));
        accepted(&mut store, 1, tx_default(15, 3, vec![period(0, 15.0)]));
        assert_limits(&composite(&mut store, &request(1, 60)), &[15.0]);
        // Difference from the TS smoke assertion: TS kept both profiles, so clearing id 14
        // answered Accepted. The spec replacement rule removes id 14, so it answers Unknown.
        let clear_14 = store.clear(
            &ClearChargingProfileRequest {
                id: Some(14),
                ..Default::default()
            },
            t0(),
        );
        assert_eq!(clear_14, ClearChargingProfileStatus::Unknown);
    }

    #[test]
    fn smoke_connector_filtered_clear_leaves_the_other_connector() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 0, cp_max(2, ChargingRateUnitType::A, 20.0));
        accepted(&mut store, 1, tx_default(3, 2, vec![period(0, 32.0)]));
        accepted(&mut store, 1, tx_default(16, 4, vec![period(0, 19.0)]));
        accepted(&mut store, 2, tx_default(17, 4, vec![period(0, 18.0)]));

        let clear = store.clear(
            &ClearChargingProfileRequest {
                connector_id: Some(ConnectorId::new(1)),
                charging_profile_purpose: Some(ChargingProfilePurposeType::TxDefaultProfile),
                stack_level: Some(4),
                ..Default::default()
            },
            t0(),
        );
        assert_eq!(clear, ClearChargingProfileStatus::Accepted);
        assert_limits(&composite(&mut store, &request(1, 60)), &[20.0]);
        assert_limits(&composite(&mut store, &request(2, 60)), &[18.0]);
    }

    // --- TS composite tests (`tests/station-smart-charging.test.mjs`) ---

    #[test]
    fn ts_expired_profile_is_pruned_during_composite() {
        let mut store = ChargingProfileStore::new();
        let mut expired = tx_default(3, 1, vec![period(0, 8.0)]);
        expired.valid_to = Some(t0() - TimeDelta::seconds(1));
        accepted(&mut store, 1, expired);
        assert_eq!(
            store.composite_schedule(&request(1, 60)),
            Err(CompositeError::NoSchedule)
        );
        assert!(store.is_empty());
    }

    // --- Behaviours specific to this port ---

    #[test]
    fn spec_precedence_tx_profile_overrides_tx_default_where_ts_took_the_minimum() {
        // TS: min(TxProfile 16, TxDefault 10, CPMax 20) = 10.
        // Spec: TxProfile overrides TxDefault, then CPMax caps: 16.
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 0, cp_max(2, ChargingRateUnitType::A, 20.0));
        accepted(&mut store, 1, tx_default(3, 5, vec![period(0, 10.0)]));
        let tx_profile = profile(
            4,
            1,
            ChargingProfilePurposeType::TxProfile,
            vec![period(0, 16.0)],
        );
        assert_eq!(
            set(&mut store, 1, tx_profile, Some(TxId::Csms(7))),
            ChargingProfileStatus::Accepted
        );

        let mut req = request(1, 60);
        req.active_tx = Some(TxId::Csms(7));
        assert_limits(&composite(&mut store, &req), &[16.0]);
    }

    #[test]
    fn tx_profile_applies_only_to_its_own_transaction() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 1, tx_default(3, 1, vec![period(0, 10.0)]));
        let tx_profile = profile(
            4,
            1,
            ChargingProfilePurposeType::TxProfile,
            vec![period(0, 6.0)],
        );
        assert_eq!(
            set(&mut store, 1, tx_profile, Some(TxId::Csms(5))),
            ChargingProfileStatus::Accepted
        );

        let mut other_tx = request(1, 60);
        other_tx.active_tx = Some(TxId::Csms(6));
        assert_limits(&composite(&mut store, &other_tx), &[10.0]);

        let mut same_tx = request(1, 60);
        same_tx.active_tx = Some(TxId::Csms(5));
        assert_limits(&composite(&mut store, &same_tx), &[6.0]);

        store.on_transaction_ended(TxId::Csms(5));
        assert_limits(&composite(&mut store, &same_tx), &[10.0]);
    }

    #[test]
    fn relative_tx_profile_counts_from_the_transaction_start() {
        let mut relative = profile(
            5,
            1,
            ChargingProfilePurposeType::TxProfile,
            vec![period(0, 10.0), period(30, 6.0)],
        );
        relative.charging_profile_kind = ChargingProfileKindType::Relative;
        let entries = vec![tx_entry(1, relative, TxId::Csms(7))];

        let mut req = request(1, 30);
        req.active_tx = Some(TxId::Csms(7));
        req.tx_started_at = Some(t0() - TimeDelta::seconds(20));
        let schedule = composite_schedule(&entries, &req).expect("a composite schedule");
        // 20 s into the transaction already: 10 A, then 6 A once elapsed reaches 30 s (offset 10).
        assert_limits(&schedule, &[10.0, 6.0]);
        assert_eq!(starts_of(&schedule), vec![0, 10]);
    }

    #[test]
    fn relative_profile_without_a_transaction_counts_from_the_schedule_start() {
        let mut store = ChargingProfileStore::new();
        let mut relative = tx_default(6, 1, vec![period(0, 10.0), period(30, 6.0)]);
        relative.charging_profile_kind = ChargingProfileKindType::Relative;
        accepted(&mut store, 1, relative);
        let schedule = composite(&mut store, &request(1, 60));
        assert_limits(&schedule, &[10.0, 6.0]);
        assert_eq!(starts_of(&schedule), vec![0, 30]);
    }

    #[test]
    fn connector_zero_composite_uses_station_level_profiles_only() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 0, cp_max(2, ChargingRateUnitType::A, 20.0));
        accepted(&mut store, 1, tx_default(3, 2, vec![period(0, 32.0)]));
        accepted(&mut store, 0, tx_default(8, 2, vec![period(0, 10.0)]));
        assert_limits(&composite(&mut store, &request(0, 60)), &[10.0]);
    }

    #[test]
    fn connector_specific_default_wins_a_stack_tie() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 0, tx_default(8, 2, vec![period(0, 10.0)]));
        accepted(&mut store, 1, tx_default(9, 2, vec![period(0, 12.0)]));
        assert_limits(&composite(&mut store, &request(1, 60)), &[12.0]);
    }

    /// The station default (7.2 kW at 230 V, one phase) in amperes, as the composite shows it.
    const STATION_A: f64 = 31.3;

    #[test]
    fn leading_gap_shows_the_station_limit_and_keeps_the_requested_window() {
        let mut store = ChargingProfileStore::new();
        let mut later = tx_default(1, 1, vec![period(0, 8.0)]);
        later.valid_from = Some(at(30));
        accepted(&mut store, 1, later);

        let schedule = composite(&mut store, &request(1, 60));
        assert_eq!(schedule.schedule_start, at(0));
        assert_eq!(schedule.charging_schedule.duration, Some(60));
        assert_limits(&schedule, &[STATION_A, 8.0]);
        assert_eq!(starts_of(&schedule), vec![0, 30]);
    }

    #[test]
    fn trailing_gap_returns_to_the_station_limit() {
        let mut store = ChargingProfileStore::new();
        let mut ends = tx_default(1, 1, vec![period(0, 8.0)]);
        ends.valid_to = Some(at(20));
        accepted(&mut store, 1, ends);

        let schedule = composite(&mut store, &request(1, 60));
        // validTo is inclusive: second 20 is still covered, second 21 is not.
        assert_eq!(schedule.charging_schedule.duration, Some(60));
        assert_limits(&schedule, &[8.0, STATION_A]);
        assert_eq!(starts_of(&schedule), vec![0, 21]);
    }

    #[test]
    fn covered_run_after_a_gap_is_kept() {
        let mut store = ChargingProfileStore::new();
        let mut first = tx_default(1, 1, vec![period(0, 8.0)]);
        first.valid_to = Some(at(9));
        accepted(&mut store, 1, first);
        let mut second = tx_default(2, 2, vec![period(0, 6.0)]);
        second.valid_from = Some(at(20));
        accepted(&mut store, 1, second);

        let schedule = composite(&mut store, &request(1, 60));
        assert_eq!(schedule.charging_schedule.duration, Some(60));
        assert_limits(&schedule, &[8.0, STATION_A, 6.0]);
        assert_eq!(starts_of(&schedule), vec![0, 10, 20]);
    }

    #[test]
    fn gap_in_watts_uses_the_nominal_power() {
        let mut store = ChargingProfileStore::new();
        let mut later = with_unit(
            tx_default(1, 1, vec![period(0, 3000.0)]),
            ChargingRateUnitType::W,
        );
        later.valid_from = Some(at(30));
        accepted(&mut store, 1, later);
        let mut req = request(1, 60);
        req.unit = Some(ChargingRateUnitType::W);
        let schedule = composite(&mut store, &req);
        assert_limits(&schedule, &[7200.0, 3000.0]);
    }

    #[test]
    fn no_covering_profile_at_all_is_still_no_schedule() {
        let mut store = ChargingProfileStore::new();
        let mut later = tx_default(1, 1, vec![period(0, 8.0)]);
        later.valid_from = Some(at(500));
        accepted(&mut store, 1, later);
        assert_eq!(
            store.composite_schedule(&request(1, 60)),
            Err(CompositeError::NoSchedule)
        );
    }

    #[test]
    fn recurring_profile_that_starts_later_leaves_a_leading_gap() {
        let mut store = ChargingProfileStore::new();
        accepted(
            &mut store,
            1,
            recurring(
                1,
                1,
                ChargingProfilePurposeType::TxDefaultProfile,
                RecurrencyKindType::Daily,
                at(10),
                vec![period(0, 8.0)],
            ),
        );
        let schedule = composite(&mut store, &request(1, 30));
        assert_eq!(schedule.schedule_start, at(0));
        assert_eq!(schedule.charging_schedule.duration, Some(30));
        assert_limits(&schedule, &[STATION_A, 8.0]);
        assert_eq!(starts_of(&schedule), vec![0, 10]);
    }

    #[test]
    fn absolute_profile_without_a_start_counts_from_its_install_time() {
        // Installed at t0 with periods 0 s: 16 A, 30 s: 8 A and no startSchedule/validFrom.
        let mut store = ChargingProfileStore::new();
        accepted(
            &mut store,
            1,
            tx_default(1, 1, vec![period(0, 16.0), period(30, 8.0)]),
        );
        // A request 20 s later still sees the schedule from the install time: 10 s of 16 A,
        // then 8 A (anchoring at the request time would give 16 A for 30 s).
        let mut req = request(1, 60);
        req.now = at(20);
        let schedule = composite(&mut store, &req);
        assert_eq!(schedule.schedule_start, at(20));
        assert_limits(&schedule, &[16.0, 8.0]);
        assert_eq!(starts_of(&schedule), vec![0, 10]);
    }

    fn power_request(model: PowerModel, charging_connectors: u32) -> PowerRequest {
        PowerRequest {
            connector: ConnectorId::new(1),
            active_tx: None,
            tx_started_at: None,
            model,
            charging_connectors,
        }
    }

    fn entry_at(
        connector: u32,
        profile: ChargingProfile,
        installed: DateTime<Utc>,
    ) -> StoredProfile {
        let mut stored = entry(connector, profile);
        stored.installed_at = Some(installed);
        stored
    }

    #[test]
    fn power_without_profiles_is_unlimited_and_energy_is_nominal() {
        let req = power_request(model(), 1);
        assert_eq!(limit_w_at(&[], &req, at(0)), None);
        // 7200 W for 60 s = 120 Wh.
        assert!((energy_wh(&[], &req, at(0), at(60)) - 120.0).abs() < 1e-9);
        assert!(energy_wh(&[], &req, at(60), at(0)).abs() < 1e-9);
    }

    #[test]
    fn amps_are_converted_and_capped_by_the_nominal_power() {
        let req = power_request(model(), 1);
        let entries = [entry_at(1, tx_default(1, 1, vec![period(0, 16.0)]), at(0))];
        // 16 A x 230 V = 3680 W.
        assert_eq!(limit_w_at(&entries, &req, at(5)), Some(3680.0));
        assert!((energy_wh(&entries, &req, at(0), at(3600)) - 3680.0).abs() < 1e-6);
        // A limit above the nominal power does not raise the power.
        let big = [entry_at(1, tx_default(1, 1, vec![period(0, 63.0)]), at(0))];
        assert!((energy_wh(&big, &req, at(0), at(3600)) - 7200.0).abs() < 1e-6);
    }

    #[test]
    fn three_phase_amps_use_all_phases() {
        let three = PowerModel {
            phases: 3,
            nominal_w: 22_000.0,
            ..model()
        };
        let req = power_request(three, 1);
        let entries = [entry_at(1, tx_default(1, 1, vec![period(0, 16.0)]), at(0))];
        assert_eq!(limit_w_at(&entries, &req, at(0)), Some(11_040.0));
    }

    #[test]
    fn energy_is_integrated_piecewise_over_period_boundaries() {
        let req = power_request(model(), 1);
        // 3000 W for the first 30 s, then 1000 W, anchored at t0 (installed).
        let w = |p: ChargingProfile| with_unit(p, ChargingRateUnitType::W);
        let entries = [entry_at(
            1,
            w(tx_default(
                1,
                1,
                vec![period(0, 3000.0), period(30, 1000.0)],
            )),
            at(0),
        )];
        // [0, 60]: 3000*30/3600 + 1000*30/3600 = 25 + 8.333.. Wh.
        let wh = energy_wh(&entries, &req, at(0), at(60));
        assert!((wh - (25.0 + 1000.0 * 30.0 / 3600.0)).abs() < 1e-9, "{wh}");
        // A span that starts inside the second period.
        let wh = energy_wh(&entries, &req, at(45), at(60));
        assert!((wh - 1000.0 * 15.0 / 3600.0).abs() < 1e-9, "{wh}");
    }

    #[test]
    fn a_stretch_without_a_covering_profile_runs_at_nominal_power() {
        let req = power_request(model(), 1);
        let mut profile = with_unit(
            tx_default(1, 1, vec![period(0, 1000.0)]),
            ChargingRateUnitType::W,
        );
        profile.valid_to = Some(at(30));
        let entries = [entry_at(1, profile, at(0))];
        // 30 s at 1000 W, then 30 s at 7200 W.
        let wh = energy_wh(&entries, &req, at(0), at(60));
        assert!(
            (wh - (1000.0 * 30.0 + 7200.0 * 30.0) / 3600.0).abs() < 1e-9,
            "{wh}"
        );
    }

    #[test]
    fn charge_point_max_is_shared_between_charging_connectors() {
        let entries = [entry_at(
            0,
            cp_max(1, ChargingRateUnitType::W, 6000.0),
            at(0),
        )];
        let one = power_request(model(), 1);
        let two = power_request(model(), 2);
        assert_eq!(limit_w_at(&entries, &one, at(1)), Some(6000.0));
        assert_eq!(limit_w_at(&entries, &two, at(1)), Some(3000.0));
        // The connector's own lower limit still wins.
        let own = [
            entries[0].clone(),
            entry_at(
                1,
                with_unit(
                    tx_default(2, 1, vec![period(0, 2000.0)]),
                    ChargingRateUnitType::W,
                ),
                at(0),
            ),
        ];
        assert_eq!(limit_w_at(&own, &two, at(1)), Some(2000.0));
    }

    #[test]
    fn mixed_units_are_compared_in_watts() {
        let req = power_request(model(), 1);
        // TxDefault 16 A = 3680 W under a 3000 W cap: the cap wins; under a 5000 W cap, 16 A.
        let tight = [
            entry_at(0, cp_max(1, ChargingRateUnitType::W, 3000.0), at(0)),
            entry_at(1, tx_default(2, 1, vec![period(0, 16.0)]), at(0)),
        ];
        assert_eq!(limit_w_at(&tight, &req, at(1)), Some(3000.0));
        let loose = [
            entry_at(0, cp_max(1, ChargingRateUnitType::W, 5000.0), at(0)),
            entry_at(1, tx_default(2, 1, vec![period(0, 16.0)]), at(0)),
        ];
        assert_eq!(limit_w_at(&loose, &req, at(1)), Some(3680.0));
    }

    #[test]
    fn default_unit_follows_the_highest_priority_profile() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 0, cp_max(2, ChargingRateUnitType::W, 7000.0));
        let unit = composite(&mut store, &request(1, 60))
            .charging_schedule
            .charging_rate_unit;
        assert_eq!(unit, ChargingRateUnitType::W);

        // A valid TxDefaultProfile outranks the ChargePointMaxProfile.
        accepted(&mut store, 1, tx_default(3, 1, vec![period(0, 16.0)]));
        let unit = composite(&mut store, &request(1, 60))
            .charging_schedule
            .charging_rate_unit;
        assert_eq!(unit, ChargingRateUnitType::A);
    }

    #[test]
    fn schedule_start_is_truncated_to_whole_seconds() {
        let mut store = ChargingProfileStore::new();
        accepted(&mut store, 1, tx_default(3, 1, vec![period(0, 16.0)]));
        let mut req = request(1, 60);
        req.now = t0() + TimeDelta::milliseconds(250);
        assert_eq!(composite(&mut store, &req).schedule_start, t0());
    }

    #[test]
    fn composite_request_rejects_unknown_connector_before_duration() {
        let profiles = vec![entry(1, tx_default(1, 1, vec![period(0, 8.0)]))];
        let mut req = request(9, 0);
        assert_eq!(
            composite_schedule(&profiles, &req),
            Err(CompositeError::UnknownConnector(ConnectorId::new(9)))
        );
        req.connector = ConnectorId::new(1);
        assert_eq!(
            composite_schedule(&profiles, &req),
            Err(CompositeError::NonPositiveDuration)
        );
    }

    // --- Property tests ---

    /// Limit of the composite at `offset` seconds from its start, if any period covers it.
    fn limit_at_offset(schedule: &CompositeSchedule, offset: i64) -> Option<f64> {
        let duration = i64::from(schedule.charging_schedule.duration.unwrap_or(0));
        if offset >= duration {
            return None;
        }
        schedule
            .charging_schedule
            .charging_schedule_period
            .iter()
            .filter(|period| i64::from(period.start_period) <= offset)
            .max_by_key(|period| period.start_period)
            .map(|period| period.limit)
    }

    fn arb_periods() -> impl Strategy<Value = Vec<ChargingSchedulePeriod>> {
        (
            prop::collection::vec(1i32..3000, 0..4),
            prop::collection::vec(0u32..40, 4..5),
        )
            .prop_map(|(mut starts, limits)| {
                starts.push(0);
                starts.sort_unstable();
                starts.dedup();
                starts
                    .iter()
                    .zip(limits)
                    .map(|(start, limit)| period(*start, f64::from(limit)))
                    .collect()
            })
    }

    fn arb_entry() -> impl Strategy<Value = StoredProfile> {
        (
            0usize..3,
            1i32..=3,
            0usize..4,
            0usize..2,
            prop::option::of(-200i64..4000),
            prop::option::of(-200i64..4000),
            prop::option::of(-200i64..4000),
            prop::option::of(1i32..4000),
            arb_periods(),
        )
            .prop_map(
                |(purpose, stack, kind, unit, valid_from, valid_to, start, duration, periods)| {
                    let purpose = match purpose {
                        0 => ChargingProfilePurposeType::TxDefaultProfile,
                        1 => ChargingProfilePurposeType::TxProfile,
                        _ => ChargingProfilePurposeType::ChargePointMaxProfile,
                    };
                    let unit = if unit == 0 {
                        ChargingRateUnitType::A
                    } else {
                        ChargingRateUnitType::W
                    };
                    let connector = if purpose == ChargingProfilePurposeType::ChargePointMaxProfile
                    {
                        ConnectorId::STATION
                    } else {
                        ConnectorId::new(1)
                    };
                    let mut profile = profile(7, stack, purpose, periods);
                    profile.charging_schedule.charging_rate_unit = unit;
                    profile.charging_schedule.duration = duration;
                    profile.valid_from = valid_from.map(at);
                    profile.valid_to = valid_to.map(at);
                    profile.charging_schedule.start_schedule = start.map(at);
                    match kind {
                        0 => {}
                        1 => {
                            profile.charging_profile_kind = ChargingProfileKindType::Relative;
                        }
                        2 => {
                            profile.charging_profile_kind = ChargingProfileKindType::Recurring;
                            profile.recurrency_kind = Some(RecurrencyKindType::Daily);
                        }
                        _ => {
                            profile.charging_profile_kind = ChargingProfileKindType::Recurring;
                            profile.recurrency_kind = Some(RecurrencyKindType::Weekly);
                        }
                    }
                    let bound_tx =
                        (purpose == ChargingProfilePurposeType::TxProfile).then_some(TxId::Csms(1));
                    StoredProfile {
                        connector_id: connector,
                        profile,
                        bound_tx,
                        installed_at: None,
                    }
                },
            )
    }

    proptest! {
        /// With a ChargePointMaxProfile that always applies, the composite schedule is
        /// always produced, covers the whole window, and never exceeds the cap.
        #[test]
        fn composite_never_exceeds_charge_point_max(
            cap in 1u32..=40,
            extras in prop::collection::vec(arb_entry(), 0..6),
            duration in 1i32..3000,
            millis in 0i64..1000,
        ) {
            let cap = f64::from(cap);
            let mut entries = vec![entry(0, profile(1, 1, ChargingProfilePurposeType::ChargePointMaxProfile, vec![period(0, cap)]))];
            entries.extend(
                extras
                    .into_iter()
                    .filter(|stored| stored.profile.charging_profile_purpose != ChargingProfilePurposeType::ChargePointMaxProfile)
                    .map(|mut stored| {
                        stored.connector_id = ConnectorId::new(1);
                        stored.profile.charging_schedule.charging_rate_unit = ChargingRateUnitType::A;
                        stored.bound_tx = (stored.profile.charging_profile_purpose
                            == ChargingProfilePurposeType::TxProfile)
                            .then_some(TxId::Csms(1));
                        stored
                    }),
            );
            let mut req = request(1, duration);
            req.now = t0() + TimeDelta::milliseconds(millis);
            req.active_tx = Some(TxId::Csms(1));

            let schedule = composite_schedule(&entries, &req).expect("the cap always applies");
            prop_assert_eq!(schedule.schedule_start, truncate_to_second(req.now));
            prop_assert_eq!(schedule.charging_schedule.duration, Some(duration));
            for offset in 0..i64::from(duration) {
                let limit = limit_at_offset(&schedule, offset);
                prop_assert!(limit.is_some(), "no gap while the cap applies (offset {offset})");
                prop_assert!(limit.unwrap_or(f64::MAX) <= cap, "offset {offset}: {limit:?} exceeds {cap}");
            }
        }

        /// Any composite that is produced is well formed.
        #[test]
        fn composite_output_is_well_formed(
            entries in prop::collection::vec(arb_entry(), 0..6),
            duration in 1i32..3000,
            millis in 0i64..1000,
            unit in 0usize..3,
            with_tx in any::<bool>(),
        ) {
            let mut req = request(1, duration);
            req.now = t0() + TimeDelta::milliseconds(millis);
            req.unit = match unit {
                0 => None,
                1 => Some(ChargingRateUnitType::A),
                _ => Some(ChargingRateUnitType::W),
            };
            req.active_tx = with_tx.then_some(TxId::Csms(1));

            if let Ok(schedule) = composite_schedule(&entries, &req) {
                let charging = &schedule.charging_schedule;
                let periods = &charging.charging_schedule_period;
                prop_assert!(!periods.is_empty());
                prop_assert_eq!(periods[0].start_period, 0);
                prop_assert!(periods.windows(2).all(|pair| pair[0].start_period < pair[1].start_period));
                let duration_out = charging.duration.unwrap_or(0);
                prop_assert!(duration_out >= 1 && duration_out <= duration);
                prop_assert!(periods.iter().all(|p| p.start_period < duration_out));
                prop_assert!(periods.iter().all(|p| p.limit.is_finite() && p.limit >= 0.0));
                prop_assert!(schedule.schedule_start >= truncate_to_second(req.now));
                prop_assert!(charging.min_charging_rate.is_none_or(|min| min >= 0.0));
                if let Some(unit) = req.unit {
                    prop_assert_eq!(charging.charging_rate_unit, unit);
                }
                // Merged: neighbours never carry the same limit and phases.
                let merged = periods.windows(2).all(|pair| {
                    pair[0].limit.to_bits() != pair[1].limit.to_bits()
                        || pair[0].number_phases != pair[1].number_phases
                });
                prop_assert!(merged, "adjacent periods must be merged: {periods:?}");
            }
        }
    }
}
