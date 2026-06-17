export type ChargingProfilePurpose = "ChargePointMaxProfile" | "TxDefaultProfile" | "TxProfile";
export type ChargingProfileKind = "Absolute" | "Recurring" | "Relative";
export type ChargingRateUnit = "A" | "W";

export interface ChargingSchedulePeriod {
  startPeriod: number;
  limit: number;
  numberPhases?: number;
}

export interface ChargingSchedule {
  duration?: number;
  startSchedule?: string;
  chargingRateUnit: ChargingRateUnit;
  chargingSchedulePeriod: ChargingSchedulePeriod[];
  minChargingRate?: number;
}

export interface ChargingProfile {
  chargingProfileId: number;
  transactionId?: number;
  stackLevel: number;
  chargingProfilePurpose: ChargingProfilePurpose;
  chargingProfileKind: ChargingProfileKind;
  recurrencyKind?: "Daily" | "Weekly";
  validFrom?: string;
  validTo?: string;
  chargingSchedule: ChargingSchedule;
}

export interface ChargingProfileEntry {
  connectorId: number;
  profile: ChargingProfile;
}

const PURPOSE_PRIORITY: ChargingProfilePurpose[] = ["TxProfile", "TxDefaultProfile", "ChargePointMaxProfile"];

export function readChargingProfile(value: unknown): ChargingProfile | undefined {
  const payload = readObject(value);
  const chargingSchedule = readChargingSchedule(payload.chargingSchedule);
  const chargingProfilePurpose = readChargingProfilePurpose(payload.chargingProfilePurpose);
  const chargingProfileKind = readChargingProfileKind(payload.chargingProfileKind);

  if (!chargingSchedule || !chargingProfilePurpose || !chargingProfileKind) {
    return undefined;
  }

  const profile: ChargingProfile = {
    chargingProfileId: readNumber(payload.chargingProfileId, -1),
    stackLevel: readNumber(payload.stackLevel, -1),
    chargingProfilePurpose,
    chargingProfileKind,
    chargingSchedule
  };

  const transactionId = readOptionalNumber(payload.transactionId);
  const recurrencyKind = readRecurrencyKind(payload.recurrencyKind);
  const validFrom = readOptionalString(payload.validFrom);
  const validTo = readOptionalString(payload.validTo);

  if (transactionId !== undefined) {
    profile.transactionId = transactionId;
  }
  if (recurrencyKind) {
    profile.recurrencyKind = recurrencyKind;
  }
  if (validFrom) {
    profile.validFrom = validFrom;
  }
  if (validTo) {
    profile.validTo = validTo;
  }

  return profile.chargingProfileId >= 0 && profile.stackLevel >= 0 ? profile : undefined;
}

export function readChargingProfileEntry(value: unknown): ChargingProfileEntry | undefined {
  const payload = readObject(value);
  const connectorId = readOptionalNumber(payload.connectorId);
  const profile = readChargingProfile(payload.profile);

  if (connectorId === undefined || !profile) {
    return undefined;
  }

  return { connectorId, profile };
}

export function readLegacyChargingProfileEntry(
  value: unknown,
  defaultConnectorId: number
): ChargingProfileEntry | undefined {
  const profile = readChargingProfile(value);
  return profile ? { connectorId: defaultConnectorId, profile } : undefined;
}

export function readChargingRateUnit(value: unknown): ChargingRateUnit | undefined {
  return value === "A" || value === "W" ? value : undefined;
}

export function canStoreChargingProfile(
  connectorId: number,
  profile: ChargingProfile,
  options: {
    hasConnector: (connectorId: number) => boolean;
    transactionIdForConnector: (connectorId: number) => number | undefined;
  }
): boolean {
  if (!Number.isInteger(connectorId) || connectorId < 0) {
    return false;
  }

  if (profile.chargingProfilePurpose === "ChargePointMaxProfile") {
    return connectorId === 0;
  }

  if (connectorId !== 0 && !options.hasConnector(connectorId)) {
    return false;
  }

  if (profile.chargingProfilePurpose === "TxProfile") {
    const transactionId = connectorId > 0 ? options.transactionIdForConnector(connectorId) : undefined;
    if (!transactionId) {
      return false;
    }

    return profile.transactionId === undefined || profile.transactionId === transactionId;
  }

  return true;
}

export function matchesChargingProfileFilter(entry: ChargingProfileEntry, payload: Record<string, unknown>): boolean {
  const hasFilter =
    "id" in payload || "connectorId" in payload || "chargingProfilePurpose" in payload || "stackLevel" in payload;

  if (!hasFilter) {
    return true;
  }

  const id = readOptionalNumber(payload.id);
  const connectorId = readOptionalNumber(payload.connectorId);
  const purpose = readChargingProfilePurpose(payload.chargingProfilePurpose);
  const stackLevel = readOptionalNumber(payload.stackLevel);

  return (
    (id === undefined || entry.profile.chargingProfileId === id) &&
    (connectorId === undefined || entry.connectorId === connectorId) &&
    (purpose === undefined || entry.profile.chargingProfilePurpose === purpose) &&
    (stackLevel === undefined || entry.profile.stackLevel === stackLevel)
  );
}

export function applicableChargingProfiles(
  entries: ChargingProfileEntry[],
  connectorId: number,
  transactionId: number | undefined,
  at: Date
): ChargingProfileEntry[] {
  return entries
    .filter((entry) => entry.connectorId === 0 || entry.connectorId === connectorId)
    .filter((entry) => isChargingProfileActive(entry.profile, at))
    .filter((entry) => {
      if (entry.profile.chargingProfilePurpose !== "TxProfile") {
        return true;
      }

      return Boolean(transactionId) && (entry.profile.transactionId === undefined || entry.profile.transactionId === transactionId);
    })
    .sort((left, right) => right.profile.stackLevel - left.profile.stackLevel);
}

export function effectiveChargingProfiles(
  entries: ChargingProfileEntry[],
  connectorId: number,
  transactionId: number | undefined,
  at: Date,
  chargingRateUnit?: ChargingRateUnit
): ChargingProfileEntry[] {
  const applicable = applicableChargingProfiles(entries, connectorId, transactionId, at).filter(
    (entry) => chargingRateUnit === undefined || entry.profile.chargingSchedule.chargingRateUnit === chargingRateUnit
  );
  return PURPOSE_PRIORITY.flatMap((purpose) => {
    const candidates = applicable.filter((entry) => entry.profile.chargingProfilePurpose === purpose);
    if (candidates.length === 0) {
      return [];
    }

    return candidates.reduce((best, entry) => (entry.profile.stackLevel >= best.profile.stackLevel ? entry : best));
  });
}

export function limitAt(profile: ChargingProfile, at: Date, relativeStart?: Date, defaultStart?: Date): number | undefined {
  return periodAt(profile, at, relativeStart, defaultStart)?.limit;
}

export function periodAt(
  profile: ChargingProfile,
  at: Date,
  relativeStart?: Date,
  defaultStart?: Date
): ChargingSchedulePeriod | undefined {
  if (!isChargingProfileActive(profile, at)) {
    return undefined;
  }

  const schedule = profile.chargingSchedule;
  const elapsedSeconds = scheduleElapsedSeconds(profile, at, relativeStart, defaultStart);

  if (schedule.duration !== undefined && elapsedSeconds >= schedule.duration) {
    return undefined;
  }

  let activePeriod: ChargingSchedulePeriod | undefined;
  for (const period of schedule.chargingSchedulePeriod) {
    if (period.startPeriod <= elapsedSeconds) {
      activePeriod = period;
    }
  }

  return activePeriod;
}

export function limitTransitionOffsets(
  profile: ChargingProfile,
  scheduleStart: Date,
  duration: number,
  relativeStart?: Date
): number[] {
  const offsets = new Set<number>();
  const elapsedSeconds = scheduleElapsedSeconds(profile, scheduleStart, relativeStart, scheduleStart);
  const schedule = profile.chargingSchedule;

  offsets.add(0);

  if (profile.validTo) {
    addOffset(offsets, Math.ceil((Date.parse(profile.validTo) - scheduleStart.getTime()) / 1000), duration);
  }
  if (profile.validFrom) {
    addOffset(offsets, Math.ceil((Date.parse(profile.validFrom) - scheduleStart.getTime()) / 1000), duration);
  }

  if (profile.chargingProfileKind === "Recurring") {
    const recurrenceSeconds = profile.recurrencyKind === "Weekly" ? 7 * 24 * 60 * 60 : 24 * 60 * 60;
    for (let cycle = 0; cycle <= elapsedSeconds + duration + recurrenceSeconds; cycle += recurrenceSeconds) {
      addScheduleOffsets(offsets, schedule, duration, elapsedSeconds, cycle);
    }
    return [...offsets].sort((left, right) => left - right);
  }

  addScheduleOffsets(offsets, schedule, duration, elapsedSeconds, 0);
  return [...offsets].sort((left, right) => left - right);
}

export function isChargingProfileExpired(profile: ChargingProfile, at: Date): boolean {
  const validTo = profile.validTo ? Date.parse(profile.validTo) : undefined;
  return validTo !== undefined && !Number.isNaN(validTo) && at.getTime() > validTo;
}

function readChargingSchedule(value: unknown): ChargingSchedule | undefined {
  const payload = readObject(value);
  const chargingRateUnit = readChargingRateUnit(payload.chargingRateUnit);
  const periods = Array.isArray(payload.chargingSchedulePeriod)
    ? payload.chargingSchedulePeriod.map((period) => readChargingSchedulePeriod(period)).filter(isDefined)
    : [];

  if (!chargingRateUnit || periods.length === 0) {
    return undefined;
  }

  const schedule: ChargingSchedule = {
    chargingRateUnit,
    chargingSchedulePeriod: periods.sort((left, right) => left.startPeriod - right.startPeriod)
  };
  const duration = readOptionalNumber(payload.duration);
  const startSchedule = readOptionalString(payload.startSchedule);
  const minChargingRate = readOptionalNumber(payload.minChargingRate);

  if (duration !== undefined) {
    schedule.duration = duration;
  }
  if (startSchedule) {
    schedule.startSchedule = startSchedule;
  }
  if (minChargingRate !== undefined) {
    schedule.minChargingRate = minChargingRate;
  }

  return schedule;
}

function readChargingSchedulePeriod(value: unknown): ChargingSchedulePeriod | undefined {
  const payload = readObject(value);
  const startPeriod = readOptionalNumber(payload.startPeriod);
  const limit = readOptionalNumber(payload.limit);

  if (startPeriod === undefined || limit === undefined) {
    return undefined;
  }

  const period: ChargingSchedulePeriod = { startPeriod, limit };
  const numberPhases = readOptionalNumber(payload.numberPhases);
  if (numberPhases !== undefined) {
    period.numberPhases = numberPhases;
  }

  return period;
}

function readChargingProfilePurpose(value: unknown): ChargingProfilePurpose | undefined {
  return value === "ChargePointMaxProfile" || value === "TxDefaultProfile" || value === "TxProfile" ? value : undefined;
}

function readChargingProfileKind(value: unknown): ChargingProfileKind | undefined {
  return value === "Absolute" || value === "Recurring" || value === "Relative" ? value : undefined;
}

function readRecurrencyKind(value: unknown): "Daily" | "Weekly" | undefined {
  return value === "Daily" || value === "Weekly" ? value : undefined;
}

function isChargingProfileActive(profile: ChargingProfile, at: Date): boolean {
  const validFrom = profile.validFrom ? Date.parse(profile.validFrom) : undefined;
  const validTo = profile.validTo ? Date.parse(profile.validTo) : undefined;
  const time = at.getTime();

  return (
    (validFrom === undefined || Number.isNaN(validFrom) || time >= validFrom) &&
    (validTo === undefined || Number.isNaN(validTo) || time <= validTo)
  );
}

function addScheduleOffsets(
  offsets: Set<number>,
  schedule: ChargingSchedule,
  duration: number,
  elapsedSeconds: number,
  elapsedBase: number
): void {
  for (const period of schedule.chargingSchedulePeriod) {
    addOffset(offsets, elapsedBase + period.startPeriod - elapsedSeconds, duration);
  }

  if (schedule.duration !== undefined) {
    addOffset(offsets, elapsedBase + schedule.duration - elapsedSeconds, duration);
  }
}

function addOffset(offsets: Set<number>, offset: number, duration: number): void {
  if (Number.isInteger(offset) && offset > 0 && offset < duration) {
    offsets.add(offset);
  }
}

function scheduleElapsedSeconds(profile: ChargingProfile, at: Date, relativeStart?: Date, defaultStart?: Date): number {
  if (profile.chargingProfileKind === "Relative") {
    return Math.max(0, Math.floor((at.getTime() - (relativeStart?.getTime() ?? at.getTime())) / 1000));
  }

  const start = profile.chargingSchedule.startSchedule
    ? Date.parse(profile.chargingSchedule.startSchedule)
    : profile.validFrom
      ? Date.parse(profile.validFrom)
      : defaultStart?.getTime() ?? at.getTime();

  const rawElapsed = Math.max(0, Math.floor((at.getTime() - (Number.isNaN(start) ? at.getTime() : start)) / 1000));

  if (profile.chargingProfileKind !== "Recurring") {
    return rawElapsed;
  }

  const periodSeconds = profile.recurrencyKind === "Weekly" ? 7 * 24 * 60 * 60 : 24 * 60 * 60;
  return rawElapsed % periodSeconds;
}

function readObject(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function readNumber(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function readOptionalNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function readOptionalString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function isDefined<T>(value: T | undefined): value is T {
  return value !== undefined;
}
