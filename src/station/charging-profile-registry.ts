import {
  canStoreChargingProfile,
  effectiveChargingProfiles,
  isChargingProfileExpired,
  limitTransitionOffsets,
  matchesChargingProfileFilter,
  periodAt,
  readChargingProfile,
  readChargingRateUnit,
  type ChargingProfileEntry,
  type ChargingSchedulePeriod
} from "./smart-charging.js";
import { readNumber } from "./payload.js";

export class ChargingProfileRegistry {
  private readonly profiles: ChargingProfileEntry[] = [];

  get length(): number {
    return this.profiles.length;
  }

  entries(): ChargingProfileEntry[] {
    return [...this.profiles];
  }

  replace(entries: ChargingProfileEntry[]): void {
    this.profiles.splice(0, this.profiles.length, ...entries);
  }

  set(
    connectorId: number,
    profilePayload: unknown,
    options: {
      hasConnector: (id: number) => boolean;
      transactionIdForConnector: (id: number) => number | undefined;
    }
  ): "Accepted" | "Rejected" {
    const profile = readChargingProfile(profilePayload);

    if (!profile || !canStoreChargingProfile(connectorId, profile, options)) {
      return "Rejected";
    }

    const existingIndex = this.profiles.findIndex((entry) => entry.profile.chargingProfileId === profile.chargingProfileId);
    if (existingIndex >= 0) {
      this.profiles.splice(existingIndex, 1);
    }
    this.profiles.push({ connectorId, profile });
    return "Accepted";
  }

  clear(payload: Record<string, unknown>): "Accepted" | "Unknown" {
    this.pruneExpired();
    const before = this.profiles.length;
    const remaining = this.profiles.filter((entry) => !matchesChargingProfileFilter(entry, payload));
    this.profiles.splice(0, this.profiles.length, ...remaining);
    return before - this.profiles.length > 0 ? "Accepted" : "Unknown";
  }

  clearForTransaction(transactionId: number): boolean {
    const before = this.profiles.length;
    const remaining = this.profiles.filter(
      (entry) => entry.profile.chargingProfilePurpose !== "TxProfile" || entry.profile.transactionId !== transactionId
    );
    this.profiles.splice(0, this.profiles.length, ...remaining);
    return this.profiles.length !== before;
  }

  pruneExpired(at = new Date()): boolean {
    const before = this.profiles.length;
    const remaining = this.profiles.filter((entry) => !isChargingProfileExpired(entry.profile, at));
    this.profiles.splice(0, this.profiles.length, ...remaining);
    return this.profiles.length !== before;
  }

  compositeSchedulePayload(
    payload: Record<string, unknown>,
    transactionId: number | undefined,
    transactionStartedAt?: string
  ): { payload: Record<string, unknown>; prunedExpired: boolean } | undefined {
    const prunedExpired = this.pruneExpired();
    const connectorId = readNumber(payload.connectorId, -1);
    const duration = readNumber(payload.duration, 0);
    const requestedUnit = readChargingRateUnit(payload.chargingRateUnit);

    if (duration <= 0) {
      return undefined;
    }

    const scheduleStart = new Date();
    const defaultUnit = requestedUnit ?? effectiveChargingProfiles(this.profiles, connectorId, transactionId, scheduleStart)[0]?.profile.chargingSchedule.chargingRateUnit ?? "A";
    const unit = defaultUnit;
    const relativeStart = transactionStartedAt ? new Date(transactionStartedAt) : undefined;
    const composite = compositeSchedule(this.profiles, connectorId, transactionId, unit, scheduleStart, duration, relativeStart);

    if (composite.chargingSchedulePeriod.length === 0) {
      return undefined;
    }

    const chargingSchedule: Record<string, unknown> = {
      duration,
      startSchedule: scheduleStart.toISOString(),
      chargingRateUnit: unit,
      chargingSchedulePeriod: composite.chargingSchedulePeriod
    };
    if (composite.minChargingRate !== undefined) {
      chargingSchedule.minChargingRate = composite.minChargingRate;
    }

    return {
      prunedExpired,
      payload: {
        status: "Accepted",
        connectorId,
        scheduleStart: scheduleStart.toISOString(),
        chargingSchedule
      }
    };
  }
}

function compositeSchedule(
  entries: ChargingProfileEntry[],
  connectorId: number,
  transactionId: number | undefined,
  unit: "A" | "W",
  scheduleStart: Date,
  duration: number,
  relativeStart?: Date
): { chargingSchedulePeriod: { startPeriod: number; limit: number; numberPhases?: number }[]; minChargingRate?: number } {
  const offsets = new Set<number>([0]);
  const unitEntries = entries.filter((entry) => entry.profile.chargingSchedule.chargingRateUnit === unit);
  for (const entry of unitEntries) {
    for (const offset of limitTransitionOffsets(entry.profile, scheduleStart, duration, relativeStart)) {
      offsets.add(offset);
    }
  }

  let minChargingRate: number | undefined;
  const chargingSchedulePeriod = [...offsets]
    .sort((left, right) => left - right)
    .map((offset) => {
      const at = new Date(scheduleStart.getTime() + offset * 1000);
      const activeProfiles = effectiveChargingProfiles(unitEntries, connectorId, transactionId, at, unit);
      const activePeriods = activeProfiles
        .map((entry) => ({ entry, period: periodAt(entry.profile, at, relativeStart, scheduleStart) }))
        .filter((item): item is { entry: ChargingProfileEntry; period: ChargingSchedulePeriod } => item.period !== undefined);

      for (const { entry } of activePeriods) {
        const profileMin = entry.profile.chargingSchedule.minChargingRate;
        if (profileMin !== undefined) {
          minChargingRate = minChargingRate === undefined ? profileMin : Math.max(minChargingRate, profileMin);
        }
      }

      if (activePeriods.length === 0) {
        return undefined;
      }

      const limit = Math.min(...activePeriods.map(({ period }) => period.limit));
      const limitingPhases = activePeriods
        .filter(({ period }) => period.limit === limit)
        .map(({ period }) => period.numberPhases)
        .filter((numberPhases): numberPhases is number => typeof numberPhases === "number" && Number.isFinite(numberPhases));
      const period: { startPeriod: number; limit: number; numberPhases?: number } = { startPeriod: offset, limit };
      if (limitingPhases.length > 0) {
        period.numberPhases = Math.min(...limitingPhases);
      }
      return period;
    })
    .filter((period): period is { startPeriod: number; limit: number; numberPhases?: number } => period !== undefined);

  return {
    chargingSchedulePeriod: chargingSchedulePeriod.filter(
      (period, index) =>
        index === 0 ||
        period.limit !== chargingSchedulePeriod[index - 1]?.limit ||
        period.numberPhases !== chargingSchedulePeriod[index - 1]?.numberPhases
    ),
    minChargingRate
  };
}
