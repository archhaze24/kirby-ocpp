import {
  applicableChargingProfiles,
  canStoreChargingProfile,
  limitAt,
  matchesChargingProfileFilter,
  readChargingProfile,
  readChargingRateUnit,
  type ChargingProfileEntry
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
    const before = this.profiles.length;
    const remaining = this.profiles.filter((entry) => !matchesChargingProfileFilter(entry, payload));
    this.profiles.splice(0, this.profiles.length, ...remaining);
    return before - this.profiles.length > 0 ? "Accepted" : "Unknown";
  }

  compositeSchedulePayload(payload: Record<string, unknown>, transactionId: number | undefined): Record<string, unknown> | undefined {
    const connectorId = readNumber(payload.connectorId, -1);
    const duration = readNumber(payload.duration, 0);
    const requestedUnit = readChargingRateUnit(payload.chargingRateUnit);

    if (duration <= 0) {
      return undefined;
    }

    const scheduleStart = new Date();
    const activeProfiles = applicableChargingProfiles(this.profiles, connectorId, transactionId, scheduleStart);
    const unit = requestedUnit ?? activeProfiles[0]?.profile.chargingSchedule.chargingRateUnit ?? "A";
    const limits = activeProfiles
      .filter((entry) => entry.profile.chargingSchedule.chargingRateUnit === unit)
      .map((entry) => limitAt(entry.profile, scheduleStart))
      .filter((limit): limit is number => typeof limit === "number" && Number.isFinite(limit));

    if (limits.length === 0) {
      return undefined;
    }

    return {
      status: "Accepted",
      connectorId,
      scheduleStart: scheduleStart.toISOString(),
      chargingSchedule: {
        duration,
        startSchedule: scheduleStart.toISOString(),
        chargingRateUnit: unit,
        chargingSchedulePeriod: [{ startPeriod: 0, limit: Math.min(...limits) }]
      }
    };
  }
}
