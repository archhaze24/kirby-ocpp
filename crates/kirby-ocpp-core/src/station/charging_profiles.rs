//! Charging profile store: `SetChargingProfile`, `ClearChargingProfile`,
//! expiry pruning and the transaction binding of TxProfiles.
//!
//! TS reference: `src/station/charging-profile-registry.ts` (storage side).
//! Evaluation and `GetCompositeSchedule` live in [`super::smart_charging`].
//!
//! # Admission (`set`)
//!
//! Checked in this order; the first failing rule decides the status:
//!
//! 1. The connector must be known (`0` or `1..=connector_count`). Rejected.
//! 2. `ChargePointMaxProfile` only on connector 0. Rejected.
//! 3. `TxProfile` needs an active transaction on the connector, and a
//!    `transactionId` in the profile must equal that transaction's wire id.
//!    Connector 0 never has a transaction. Rejected otherwise.
//! 4. Shape: stack level, periods (see [`super::smart_charging::shape_error`]). Rejected.
//! 5. Rate unit not in `allowed_units`. NotSupported.
//! 6. `stackLevel` above `max_stack_level`, or more than `max_periods` periods. Rejected.
//! 7. The new profile replaces every stored profile with the same id, and every stored
//!    profile with the same (connector, stackLevel, purpose). Replaced profiles do not
//!    count against `max_installed`. Otherwise the store is full. Rejected.
//!
//! A profile whose `validTo` already lies in the past is accepted and pruned lazily,
//! as in TS. Expired profiles are pruned on every `set`, `clear` and composite call.
//!
//! # Transaction binding
//!
//! A TxProfile is stored together with the [`TxId`] it was set for
//! ([`StoredProfile::bound_tx`]). It applies only to that transaction and is removed by
//! [`ChargingProfileStore::on_transaction_ended`]. TS kept profiles without
//! `transactionId` and let them match every later transaction.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::smart_charging::{
    self, CompositeError, CompositeRequest, CompositeSchedule, is_expired, is_known_connector,
    shape_error,
};
use crate::ids::{ConnectorId, TxId};
use crate::ocpp::messages::{
    ChargingProfile, ChargingProfileStatus, ClearChargingProfileRequest, ClearChargingProfileStatus,
};
use crate::ocpp::types::{ChargingProfilePurposeType, ChargingRateUnitType};

/// Station limits that the store enforces (`ChargeProfileMaxStackLevel`,
/// `MaxChargingProfilesInstalled`, `ChargingScheduleMaxPeriods`,
/// `ChargingScheduleAllowedChargingRateUnit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreLimits {
    pub max_stack_level: i32,
    pub max_installed: usize,
    pub max_periods: usize,
    pub allowed_units: Vec<ChargingRateUnitType>,
}

impl Default for StoreLimits {
    /// No practical limits; both rate units allowed. The station configuration supplies real values.
    fn default() -> Self {
        Self {
            max_stack_level: i32::MAX,
            max_installed: usize::MAX,
            max_periods: usize::MAX,
            allowed_units: vec![ChargingRateUnitType::A, ChargingRateUnitType::W],
        }
    }
}

impl StoreLimits {
    pub fn allows_unit(&self, unit: ChargingRateUnitType) -> bool {
        self.allowed_units.contains(&unit)
    }
}

/// Context of a `set` call.
#[derive(Debug, Clone, Copy)]
pub struct ProfileContext<'a> {
    pub now: DateTime<Utc>,
    /// Number of physical connectors (`1..=connector_count` are known).
    pub connector_count: u32,
    /// Transaction active on the connector the profile is set for, if any.
    pub active_tx: Option<TxId>,
    pub limits: &'a StoreLimits,
}

/// A stored profile together with its connector and, for TxProfiles, its transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredProfile {
    pub connector_id: ConnectorId,
    pub profile: ChargingProfile,
    /// Transaction a TxProfile belongs to. Always `None` for the other purposes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_tx: Option<TxId>,
}

/// Persistable store of charging profiles.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChargingProfileStore {
    profiles: Vec<StoredProfile>,
}

impl ChargingProfileStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    pub fn profiles(&self) -> &[StoredProfile] {
        &self.profiles
    }

    /// `SetChargingProfile`: validates and stores `profile` on `connector`.
    pub fn set(
        &mut self,
        connector: ConnectorId,
        profile: ChargingProfile,
        ctx: &ProfileContext<'_>,
    ) -> ChargingProfileStatus {
        self.prune_expired(ctx.now);

        let bound_tx = match admit(connector, &profile, ctx) {
            Ok(bound_tx) => bound_tx,
            Err(status) => return status,
        };

        let replaces = |stored: &StoredProfile| {
            stored.profile.charging_profile_id == profile.charging_profile_id
                || (stored.connector_id == connector
                    && stored.profile.stack_level == profile.stack_level
                    && stored.profile.charging_profile_purpose == profile.charging_profile_purpose)
        };
        let remaining = self
            .profiles
            .iter()
            .filter(|stored| !replaces(stored))
            .count();
        if remaining >= ctx.limits.max_installed {
            return ChargingProfileStatus::Rejected;
        }

        self.profiles.retain(|stored| !replaces(stored));
        self.profiles.push(StoredProfile {
            connector_id: connector,
            profile,
            bound_tx,
        });
        ChargingProfileStatus::Accepted
    }

    /// `ClearChargingProfile`. An empty filter clears everything; the criteria are
    /// conjunctive. Expired profiles are pruned first, so an expired id answers `Unknown`.
    pub fn clear(
        &mut self,
        filter: &ClearChargingProfileRequest,
        now: DateTime<Utc>,
    ) -> ClearChargingProfileStatus {
        self.prune_expired(now);
        let before = self.profiles.len();
        self.profiles
            .retain(|stored| !matches_filter(stored, filter));
        if self.profiles.len() < before {
            ClearChargingProfileStatus::Accepted
        } else {
            ClearChargingProfileStatus::Unknown
        }
    }

    /// Removes profiles whose `validTo` has passed. Returns how many were removed.
    pub fn prune_expired(&mut self, now: DateTime<Utc>) -> usize {
        let before = self.profiles.len();
        self.profiles
            .retain(|stored| !is_expired(&stored.profile, now));
        before - self.profiles.len()
    }

    /// Removes the TxProfiles bound to `tx`. Call when the transaction stops, and
    /// with the old id when a transaction id changes (TS behaviour). Returns how many were removed.
    pub fn on_transaction_ended(&mut self, tx: TxId) -> usize {
        let before = self.profiles.len();
        self.profiles.retain(|stored| {
            !(stored.profile.charging_profile_purpose == ChargingProfilePurposeType::TxProfile
                && stored.bound_tx == Some(tx))
        });
        before - self.profiles.len()
    }

    /// `GetCompositeSchedule` over the stored profiles. Prunes expired profiles at `req.now`
    /// first; compare [`Self::len`] before and after to know whether the store changed.
    pub fn composite_schedule(
        &mut self,
        req: &CompositeRequest,
    ) -> Result<CompositeSchedule, CompositeError> {
        self.prune_expired(req.now);
        smart_charging::composite_schedule(&self.profiles, req)
    }
}

/// Checks `profile` against the rules in the module docs. Returns the bound transaction
/// for a TxProfile, or the status to answer with.
fn admit(
    connector: ConnectorId,
    profile: &ChargingProfile,
    ctx: &ProfileContext<'_>,
) -> Result<Option<TxId>, ChargingProfileStatus> {
    if !is_known_connector(connector, ctx.connector_count) {
        return Err(ChargingProfileStatus::Rejected);
    }

    let bound_tx = match profile.charging_profile_purpose {
        ChargingProfilePurposeType::ChargePointMaxProfile => {
            if !connector.is_station() {
                return Err(ChargingProfileStatus::Rejected);
            }
            None
        }
        ChargingProfilePurposeType::TxProfile => {
            if connector.is_station() {
                return Err(ChargingProfileStatus::Rejected);
            }
            let tx = ctx.active_tx.ok_or(ChargingProfileStatus::Rejected)?;
            if profile
                .transaction_id
                .is_some_and(|id| tx.wire() != Some(id))
            {
                return Err(ChargingProfileStatus::Rejected);
            }
            Some(tx)
        }
        ChargingProfilePurposeType::TxDefaultProfile => None,
    };

    if shape_error(profile).is_some() {
        return Err(ChargingProfileStatus::Rejected);
    }
    if !ctx
        .limits
        .allows_unit(profile.charging_schedule.charging_rate_unit)
    {
        return Err(ChargingProfileStatus::NotSupported);
    }
    if profile.stack_level > ctx.limits.max_stack_level
        || profile.charging_schedule.charging_schedule_period.len() > ctx.limits.max_periods
    {
        return Err(ChargingProfileStatus::Rejected);
    }

    Ok(bound_tx)
}

fn matches_filter(stored: &StoredProfile, filter: &ClearChargingProfileRequest) -> bool {
    filter
        .id
        .is_none_or(|id| stored.profile.charging_profile_id == id)
        && filter
            .connector_id
            .is_none_or(|connector| stored.connector_id == connector)
        && filter
            .charging_profile_purpose
            .is_none_or(|purpose| stored.profile.charging_profile_purpose == purpose)
        && filter
            .stack_level
            .is_none_or(|stack| stored.profile.stack_level == stack)
}

/// Test fixtures shared with [`super::smart_charging`] tests.
#[cfg(test)]
pub(crate) mod test_util {
    use chrono::{DateTime, TimeZone, Utc};

    use super::StoredProfile;
    use crate::ids::{ConnectorId, TxId};
    use crate::ocpp::messages::{
        ChargingProfile as Profile, ChargingSchedule, ChargingSchedulePeriod,
    };
    use crate::ocpp::types::{
        ChargingProfileKindType, ChargingProfilePurposeType, ChargingRateUnitType,
        RecurrencyKindType,
    };

    /// Fixed test clock: 2026-11-01T12:00:00Z.
    pub(crate) fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap()
    }

    pub(crate) fn at(seconds: i64) -> DateTime<Utc> {
        t0() + chrono::TimeDelta::seconds(seconds)
    }

    pub(crate) fn period(start: i32, limit: f64) -> ChargingSchedulePeriod {
        ChargingSchedulePeriod {
            start_period: start,
            limit,
            number_phases: None,
        }
    }

    /// An absolute profile with the given periods, in amperes, without validity window.
    pub(crate) fn profile(
        id: i32,
        stack: i32,
        purpose: ChargingProfilePurposeType,
        periods: Vec<ChargingSchedulePeriod>,
    ) -> Profile {
        Profile {
            charging_profile_id: id,
            transaction_id: None,
            stack_level: stack,
            charging_profile_purpose: purpose,
            charging_profile_kind: ChargingProfileKindType::Absolute,
            recurrency_kind: None,
            valid_from: None,
            valid_to: None,
            charging_schedule: ChargingSchedule {
                duration: None,
                start_schedule: None,
                charging_rate_unit: ChargingRateUnitType::A,
                charging_schedule_period: periods,
                min_charging_rate: None,
            },
        }
    }

    pub(crate) fn recurring(
        id: i32,
        stack: i32,
        purpose: ChargingProfilePurposeType,
        recurrency: RecurrencyKindType,
        start_schedule: DateTime<Utc>,
        periods: Vec<ChargingSchedulePeriod>,
    ) -> Profile {
        let mut profile = profile(id, stack, purpose, periods);
        profile.charging_profile_kind = ChargingProfileKindType::Recurring;
        profile.recurrency_kind = Some(recurrency);
        profile.charging_schedule.start_schedule = Some(start_schedule);
        profile
    }

    pub(crate) fn with_unit(mut profile: Profile, unit: ChargingRateUnitType) -> Profile {
        profile.charging_schedule.charging_rate_unit = unit;
        profile
    }

    /// A TxProfile entry bound to `tx`, stored on `connector`.
    pub(crate) fn tx_entry(connector: u32, profile: Profile, tx: TxId) -> StoredProfile {
        StoredProfile {
            connector_id: ConnectorId::new(connector),
            profile,
            bound_tx: Some(tx),
        }
    }

    /// A plain entry on `connector` (no transaction binding).
    pub(crate) fn entry(connector: u32, profile: Profile) -> StoredProfile {
        StoredProfile {
            connector_id: ConnectorId::new(connector),
            profile,
            bound_tx: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::test_util::{at, period, profile, t0};
    use super::*;
    use crate::ocpp::messages::ClearChargingProfileRequest;

    fn limits() -> StoreLimits {
        StoreLimits::default()
    }

    fn ctx(limits: &StoreLimits, active_tx: Option<TxId>) -> ProfileContext<'_> {
        ProfileContext {
            now: t0(),
            connector_count: 2,
            active_tx,
            limits,
        }
    }

    fn tx_default(id: i32, stack: i32, limit: f64) -> ChargingProfile {
        profile(
            id,
            stack,
            ChargingProfilePurposeType::TxDefaultProfile,
            vec![period(0, limit)],
        )
    }

    fn tx_profile(id: i32, stack: i32, limit: f64) -> ChargingProfile {
        profile(
            id,
            stack,
            ChargingProfilePurposeType::TxProfile,
            vec![period(0, limit)],
        )
    }

    fn cp_max(id: i32, limit: f64) -> ChargingProfile {
        profile(
            id,
            1,
            ChargingProfilePurposeType::ChargePointMaxProfile,
            vec![period(0, limit)],
        )
    }

    #[test]
    fn unknown_connector_is_rejected() {
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        let status = store.set(
            ConnectorId::new(99),
            tx_default(991, 1, 8.0),
            &ctx(&limits, None),
        );
        assert_eq!(status, ChargingProfileStatus::Rejected);
        assert!(store.is_empty());
    }

    #[test]
    fn charge_point_max_only_on_connector_zero() {
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        assert_eq!(
            store.set(ConnectorId::new(1), cp_max(5, 20.0), &ctx(&limits, None)),
            ChargingProfileStatus::Rejected
        );
        assert_eq!(
            store.set(ConnectorId::STATION, cp_max(5, 20.0), &ctx(&limits, None)),
            ChargingProfileStatus::Accepted
        );
    }

    #[test]
    fn tx_profile_needs_an_active_transaction() {
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        // Smoke: TxProfile without transaction on connector 1 is Rejected.
        assert_eq!(
            store.set(
                ConnectorId::new(1),
                tx_profile(992, 1, 8.0),
                &ctx(&limits, None)
            ),
            ChargingProfileStatus::Rejected
        );
        // Connector 0 never has a transaction.
        assert_eq!(
            store.set(
                ConnectorId::STATION,
                tx_profile(993, 1, 8.0),
                &ctx(&limits, Some(TxId::Csms(5)))
            ),
            ChargingProfileStatus::Rejected
        );
    }

    #[test]
    fn tx_profile_with_wrong_transaction_id_is_rejected() {
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        let mut profile = tx_profile(1, 1, 8.0);
        profile.transaction_id = Some(6);
        assert_eq!(
            store.set(
                ConnectorId::new(1),
                profile.clone(),
                &ctx(&limits, Some(TxId::Csms(5)))
            ),
            ChargingProfileStatus::Rejected
        );
        // A local (not yet synced) transaction cannot match an explicit CSMS id.
        assert_eq!(
            store.set(
                ConnectorId::new(1),
                profile.clone(),
                &ctx(&limits, Some(TxId::Local(7)))
            ),
            ChargingProfileStatus::Rejected
        );
        assert_eq!(
            store.set(
                ConnectorId::new(1),
                profile,
                &ctx(&limits, Some(TxId::Csms(6)))
            ),
            ChargingProfileStatus::Accepted
        );
    }

    #[test]
    fn tx_profile_without_transaction_id_is_bound_to_its_transaction() {
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        assert_eq!(
            store.set(
                ConnectorId::new(1),
                tx_profile(2, 1, 8.0),
                &ctx(&limits, Some(TxId::Csms(5)))
            ),
            ChargingProfileStatus::Accepted
        );
        assert_eq!(store.profiles()[0].bound_tx, Some(TxId::Csms(5)));
        assert_eq!(store.on_transaction_ended(TxId::Csms(6)), 0);
        assert_eq!(store.on_transaction_ended(TxId::Csms(5)), 1);
        assert!(store.is_empty());
    }

    #[test]
    fn non_tx_profiles_are_never_bound() {
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        store.set(
            ConnectorId::new(1),
            tx_default(3, 1, 8.0),
            &ctx(&limits, Some(TxId::Csms(5))),
        );
        assert_eq!(store.profiles()[0].bound_tx, None);
        // Ending a transaction does not remove TxDefault profiles.
        assert_eq!(store.on_transaction_ended(TxId::Csms(5)), 0);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn same_stack_and_purpose_replaces_the_previous_profile() {
        // Spec: one profile per connector, stack level and purpose.
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        store.set(
            ConnectorId::new(1),
            tx_default(14, 3, 17.0),
            &ctx(&limits, None),
        );
        store.set(
            ConnectorId::new(1),
            tx_default(15, 3, 15.0),
            &ctx(&limits, None),
        );
        assert_eq!(store.len(), 1);
        assert_eq!(store.profiles()[0].profile.charging_profile_id, 15);
    }

    #[test]
    fn same_id_replaces_even_on_another_connector() {
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        store.set(
            ConnectorId::new(1),
            tx_default(20, 1, 8.0),
            &ctx(&limits, None),
        );
        store.set(
            ConnectorId::new(2),
            tx_default(20, 2, 9.0),
            &ctx(&limits, None),
        );
        assert_eq!(store.len(), 1);
        assert_eq!(store.profiles()[0].connector_id, ConnectorId::new(2));
    }

    #[test]
    fn different_stack_levels_coexist() {
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        store.set(
            ConnectorId::new(1),
            tx_default(1, 1, 8.0),
            &ctx(&limits, None),
        );
        store.set(
            ConnectorId::new(1),
            tx_default(2, 2, 9.0),
            &ctx(&limits, None),
        );
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn expired_profile_is_accepted_then_pruned_by_clear() {
        // Smoke: an expired TxDefaultProfile is Accepted (pruning happens later).
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        let mut expired = tx_default(4, 100, 1.0);
        expired.valid_to = Some(t0() - TimeDelta::seconds(60));
        assert_eq!(
            store.set(ConnectorId::new(1), expired, &ctx(&limits, None)),
            ChargingProfileStatus::Accepted
        );
        assert_eq!(store.len(), 1);
        // Smoke: clearing the expired id answers Unknown and the store is pruned.
        let status = store.clear(
            &ClearChargingProfileRequest {
                id: Some(4),
                ..Default::default()
            },
            t0(),
        );
        assert_eq!(status, ClearChargingProfileStatus::Unknown);
        assert!(store.is_empty());
    }

    #[test]
    fn expired_profiles_are_pruned_on_set() {
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        let mut expired = tx_default(4, 1, 1.0);
        expired.valid_to = Some(t0() - TimeDelta::seconds(1));
        store.set(ConnectorId::new(1), expired, &ctx(&limits, None));
        store.set(
            ConnectorId::new(1),
            tx_default(5, 2, 8.0),
            &ctx(&limits, None),
        );
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn clear_by_id_connector_purpose_and_stack_are_conjunctive() {
        // Smoke: clear connector 1 / TxDefault / stack 4 leaves connector 2 alone.
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        store.set(
            ConnectorId::new(1),
            tx_default(16, 4, 19.0),
            &ctx(&limits, None),
        );
        store.set(
            ConnectorId::new(2),
            tx_default(17, 4, 18.0),
            &ctx(&limits, None),
        );
        let filter = ClearChargingProfileRequest {
            connector_id: Some(ConnectorId::new(1)),
            charging_profile_purpose: Some(ChargingProfilePurposeType::TxDefaultProfile),
            stack_level: Some(4),
            ..Default::default()
        };
        assert_eq!(
            store.clear(&filter, t0()),
            ClearChargingProfileStatus::Accepted
        );
        assert_eq!(store.len(), 1);
        assert_eq!(store.profiles()[0].profile.charging_profile_id, 17);
        // Filter that matches nothing answers Unknown.
        assert_eq!(
            store.clear(
                &ClearChargingProfileRequest {
                    stack_level: Some(9),
                    ..Default::default()
                },
                t0()
            ),
            ClearChargingProfileStatus::Unknown
        );
    }

    #[test]
    fn empty_clear_filter_clears_everything() {
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        store.set(
            ConnectorId::new(1),
            tx_default(1, 1, 8.0),
            &ctx(&limits, None),
        );
        store.set(ConnectorId::STATION, cp_max(2, 20.0), &ctx(&limits, None));
        assert_eq!(
            store.clear(&ClearChargingProfileRequest::default(), t0()),
            ClearChargingProfileStatus::Accepted
        );
        assert!(store.is_empty());
    }

    #[test]
    fn unit_not_in_allowed_list_is_not_supported() {
        let limits = StoreLimits {
            allowed_units: vec![ChargingRateUnitType::A],
            ..StoreLimits::default()
        };
        let mut store = ChargingProfileStore::new();
        let watt = super::test_util::with_unit(tx_default(1, 1, 7000.0), ChargingRateUnitType::W);
        assert_eq!(
            store.set(ConnectorId::new(1), watt, &ctx(&limits, None)),
            ChargingProfileStatus::NotSupported
        );
    }

    #[test]
    fn stack_level_limit_is_enforced() {
        let limits = StoreLimits {
            max_stack_level: 5,
            ..StoreLimits::default()
        };
        let mut store = ChargingProfileStore::new();
        assert_eq!(
            store.set(
                ConnectorId::new(1),
                tx_default(1, 6, 8.0),
                &ctx(&limits, None)
            ),
            ChargingProfileStatus::Rejected
        );
        assert_eq!(
            store.set(
                ConnectorId::new(1),
                tx_default(2, 5, 8.0),
                &ctx(&limits, None)
            ),
            ChargingProfileStatus::Accepted
        );
    }

    #[test]
    fn period_limit_is_enforced() {
        let limits = StoreLimits {
            max_periods: 1,
            ..StoreLimits::default()
        };
        let mut store = ChargingProfileStore::new();
        let two_periods = profile(
            1,
            1,
            ChargingProfilePurposeType::TxDefaultProfile,
            vec![period(0, 8.0), period(30, 6.0)],
        );
        assert_eq!(
            store.set(ConnectorId::new(1), two_periods, &ctx(&limits, None)),
            ChargingProfileStatus::Rejected
        );
    }

    #[test]
    fn malformed_schedules_are_rejected() {
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        let not_zero = profile(
            1,
            1,
            ChargingProfilePurposeType::TxDefaultProfile,
            vec![period(5, 8.0)],
        );
        let empty = profile(2, 1, ChargingProfilePurposeType::TxDefaultProfile, vec![]);
        let unordered = profile(
            3,
            1,
            ChargingProfilePurposeType::TxDefaultProfile,
            vec![period(0, 8.0), period(0, 6.0)],
        );
        for bad in [not_zero, empty, unordered] {
            assert_eq!(
                store.set(ConnectorId::new(1), bad, &ctx(&limits, None)),
                ChargingProfileStatus::Rejected
            );
        }
        assert!(store.is_empty());
    }

    #[test]
    fn max_installed_counts_after_replacement() {
        let limits = StoreLimits {
            max_installed: 2,
            ..StoreLimits::default()
        };
        let mut store = ChargingProfileStore::new();
        assert_eq!(
            store.set(
                ConnectorId::new(1),
                tx_default(1, 1, 8.0),
                &ctx(&limits, None)
            ),
            ChargingProfileStatus::Accepted
        );
        assert_eq!(
            store.set(
                ConnectorId::new(1),
                tx_default(2, 2, 8.0),
                &ctx(&limits, None)
            ),
            ChargingProfileStatus::Accepted
        );
        // The store is full, but this set replaces profile 1 (same id), so it fits.
        assert_eq!(
            store.set(
                ConnectorId::new(1),
                tx_default(1, 1, 9.0),
                &ctx(&limits, None)
            ),
            ChargingProfileStatus::Accepted
        );
        // A genuinely new profile does not fit.
        assert_eq!(
            store.set(
                ConnectorId::new(1),
                tx_default(3, 3, 8.0),
                &ctx(&limits, None)
            ),
            ChargingProfileStatus::Rejected
        );
        // Replacing by slot (connector 1, stack 2) also fits.
        assert_eq!(
            store.set(
                ConnectorId::new(1),
                tx_default(4, 2, 7.0),
                &ctx(&limits, None)
            ),
            ChargingProfileStatus::Accepted
        );
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn expired_profiles_free_capacity() {
        let limits = StoreLimits {
            max_installed: 1,
            ..StoreLimits::default()
        };
        let mut store = ChargingProfileStore::new();
        let mut expired = tx_default(1, 1, 8.0);
        expired.valid_to = Some(t0() - TimeDelta::seconds(1));
        store.set(ConnectorId::new(1), expired, &ctx(&limits, None));
        assert_eq!(
            store.set(
                ConnectorId::new(1),
                tx_default(2, 2, 8.0),
                &ctx(&limits, None)
            ),
            ChargingProfileStatus::Accepted
        );
    }

    #[test]
    fn future_valid_from_is_stored_and_pruned_only_by_validity() {
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        let mut future = tx_default(9, 99, 1.0);
        future.valid_from = Some(at(60));
        assert_eq!(
            store.set(ConnectorId::new(1), future, &ctx(&limits, None)),
            ChargingProfileStatus::Accepted
        );
        assert_eq!(store.prune_expired(at(3600)), 0);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn store_roundtrips_through_serde_with_bindings() {
        let limits = limits();
        let mut store = ChargingProfileStore::new();
        store.set(
            ConnectorId::new(1),
            tx_default(1, 1, 8.0),
            &ctx(&limits, None),
        );
        store.set(
            ConnectorId::new(1),
            tx_profile(2, 2, 6.0),
            &ctx(&limits, Some(TxId::Local(9))),
        );
        let json = serde_json::to_string(&store).unwrap();
        let back: ChargingProfileStore = serde_json::from_str(&json).unwrap();
        assert_eq!(back, store);
        assert_eq!(back.profiles()[1].bound_tx, Some(TxId::Local(9)));
    }
}
