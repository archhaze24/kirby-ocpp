import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

export interface PersistedStationState {
  version: 1;
  chargePointId: string;
  connectorCount?: number;
  connectorMeterValues?: Record<string, number>;
  connectorReservations?: Record<string, PersistedReservation>;
  localListVersion: number;
  localAuthorizationList: LocalAuthorizationEntry[];
  authorizationCache?: LocalAuthorizationEntry[];
  chargingProfiles: unknown[];
  configurationValues: Record<string, string>;
  meterWh: number;
}

export interface LocalAuthorizationEntry {
  idTag: string;
  idTagInfo?: Record<string, unknown>;
}

export interface PersistedReservation {
  reservationId: number;
  idTag: string;
  parentIdTag?: string;
  expiryDate: string;
}

export class StationStore {
  private readonly filePath: string;

  constructor(
    private readonly chargePointId: string,
    stateDirectory = process.env.KIRBY_OCPP_STATE_DIR ?? join(homedir(), ".kirby-ocpp", "stations")
  ) {
    this.filePath = join(stateDirectory, `${safeFileName(chargePointId)}.json`);
  }

  load(): PersistedStationState | undefined {
    if (!existsSync(this.filePath)) {
      return undefined;
    }

    const parsed = JSON.parse(readFileSync(this.filePath, "utf8")) as Partial<PersistedStationState>;
    if (parsed.version !== 1 || parsed.chargePointId !== this.chargePointId) {
      return undefined;
    }

    return {
      version: 1,
      chargePointId: this.chargePointId,
      connectorCount: readInteger(parsed.connectorCount, 1),
      connectorMeterValues: isRecord(parsed.connectorMeterValues) ? readNumberRecord(parsed.connectorMeterValues) : {},
      connectorReservations: isRecord(parsed.connectorReservations) ? readReservationRecord(parsed.connectorReservations) : {},
      localListVersion: readInteger(parsed.localListVersion, 0),
      localAuthorizationList: Array.isArray(parsed.localAuthorizationList) ? parsed.localAuthorizationList : [],
      authorizationCache: Array.isArray(parsed.authorizationCache) ? parsed.authorizationCache : [],
      chargingProfiles: Array.isArray(parsed.chargingProfiles) ? parsed.chargingProfiles : [],
      configurationValues: isRecord(parsed.configurationValues) ? readStringRecord(parsed.configurationValues) : {},
      meterWh: readNumber(parsed.meterWh, 0)
    };
  }

  save(state: PersistedStationState): void {
    mkdirSync(dirname(this.filePath), { recursive: true });
    writeFileSync(this.filePath, `${JSON.stringify(state, null, 2)}\n`, "utf8");
  }
}

function safeFileName(value: string): string {
  return Buffer.from(value).toString("base64url");
}

function readInteger(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isInteger(value) ? value : fallback;
}

function readNumber(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function readStringRecord(value: Record<string, unknown>): Record<string, string> {
  return Object.fromEntries(Object.entries(value).filter((entry): entry is [string, string] => typeof entry[1] === "string"));
}

function readNumberRecord(value: Record<string, unknown>): Record<string, number> {
  return Object.fromEntries(Object.entries(value).filter((entry): entry is [string, number] => typeof entry[1] === "number"));
}

function readReservationRecord(value: Record<string, unknown>): Record<string, PersistedReservation> {
  const entries = Object.entries(value).flatMap(([connectorId, reservation]) => {
    if (!isRecord(reservation)) {
      return [];
    }

    const reservationId = reservation.reservationId;
    const idTag = reservation.idTag;
    const expiryDate = reservation.expiryDate;
    if (typeof reservationId !== "number" || typeof idTag !== "string" || typeof expiryDate !== "string") {
      return [];
    }

    const parsed: PersistedReservation = { reservationId, idTag, expiryDate };
    if (typeof reservation.parentIdTag === "string") {
      parsed.parentIdTag = reservation.parentIdTag;
    }

    return [[connectorId, parsed] as const];
  });

  return Object.fromEntries(entries);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
