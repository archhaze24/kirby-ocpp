import type { ConnectorState, StopReason } from "../ocpp/types.js";
import type {
  LocalAuthorizationEntry,
  PersistedConnectorTransaction,
  PersistedReservation,
  PersistedStationState
} from "../station-store.js";
import type { ConfigurationKey } from "./configuration.js";
import type { ConfigurationRegistry } from "./configuration-registry.js";
import type { ConnectorRegistry } from "./connector-registry.js";
import { DEFAULT_SAMPLED_DATA, type MeteringConfigurationKey } from "./metering.js";
import {
  readChargingProfileEntry,
  readLegacyChargingProfileEntry,
  type ChargingProfileEntry
} from "./smart-charging.js";

export function buildPersistedStationState(options: {
  chargePointId: string;
  connectorId: number;
  connectors: ConnectorState[];
  localListVersion: number;
  localAuthorizationList: Map<string, Record<string, unknown> | undefined>;
  authorizationCache: Map<string, Record<string, unknown> | undefined>;
  chargingProfiles: ChargingProfileEntry[];
  configuration: Map<string, ConfigurationKey>;
}): PersistedStationState {
  const primaryConnector = options.connectors.find((connector) => connector.id === options.connectorId) ?? options.connectors[0];

  return {
    version: 1,
    chargePointId: options.chargePointId,
    connectorCount: options.connectors.length,
    connectorMeterValues: serializeConnectorMeterValues(options.connectors),
    connectorReservations: serializeConnectorReservations(options.connectors),
    connectorTransactions: serializeConnectorTransactions(options.connectors),
    localListVersion: options.localListVersion,
    localAuthorizationList: serializeAuthorizationEntries(options.localAuthorizationList),
    authorizationCache: serializeAuthorizationEntries(options.authorizationCache),
    chargingProfiles: options.chargingProfiles,
    configurationValues: serializeMutableConfiguration(options.configuration),
    meterWh: primaryConnector?.meterWh ?? 0
  };
}

export function restorePersistedStationState(options: {
  persisted: PersistedStationState;
  connectorId: number;
  connectors: ConnectorRegistry;
  configuration: ConfigurationRegistry;
  localAuthorizationList: Map<string, Record<string, unknown> | undefined>;
  authorizationCache: Map<string, Record<string, unknown> | undefined>;
  scheduleReservation: (connectorId: number) => void;
}): { localListVersion: number; chargingProfiles: ChargingProfileEntry[] } {
  const { persisted } = options;
  const persistedConnectorCount = Math.max(persisted.connectorCount ?? 1, options.connectorId);
  options.connectors.ensureCount(persistedConnectorCount);
  options.configuration.setValue("NumberOfConnectors", String(options.connectors.size));

  const chargingProfiles = persisted.chargingProfiles
    .map((entry) => readChargingProfileEntry(entry) ?? readLegacyChargingProfileEntry(entry, options.connectorId))
    .filter(isDefined);

  for (const [connectorId, meterWh] of Object.entries(persisted.connectorMeterValues ?? {})) {
    const numericConnectorId = Number.parseInt(connectorId, 10);
    if (Number.isInteger(numericConnectorId) && options.connectors.has(numericConnectorId)) {
      options.connectors.patch(numericConnectorId, { meterWh });
    }
  }

  for (const [connectorId, reservation] of Object.entries(persisted.connectorReservations ?? {})) {
    const numericConnectorId = Number.parseInt(connectorId, 10);
    if (!Number.isInteger(numericConnectorId) || !options.connectors.has(numericConnectorId)) {
      continue;
    }

    if (Date.parse(reservation.expiryDate) <= Date.now()) {
      continue;
    }

    options.connectors.patch(numericConnectorId, {
      reservationId: reservation.reservationId,
      reservationIdTag: reservation.idTag,
      reservationParentIdTag: reservation.parentIdTag,
      reservationExpiryDate: reservation.expiryDate,
      status: "Reserved"
    });
    options.scheduleReservation(numericConnectorId);
  }

  for (const [connectorId, transaction] of Object.entries(persisted.connectorTransactions ?? {})) {
    const numericConnectorId = Number.parseInt(connectorId, 10);
    if (!Number.isInteger(numericConnectorId) || !options.connectors.has(numericConnectorId)) {
      continue;
    }

    options.connectors.patch(numericConnectorId, {
      transactionId: transaction.transactionId,
      transactionStartedAt: transaction.transactionStartedAt,
      lastIdTag: transaction.lastIdTag,
      evConnected: transaction.evConnected,
      status: readPersistedConnectorStatus(transaction.status),
      stopTransactionAtWh: transaction.stopTransactionAtWh,
      pendingStartTransaction: readPersistedPendingStart(transaction.pendingStartTransaction),
      pendingStopTransaction: readPersistedPendingStop(transaction.pendingStopTransaction)
    });
  }

  if (Object.keys(persisted.connectorMeterValues ?? {}).length === 0 && options.connectors.has(options.connectorId)) {
    options.connectors.patch(options.connectorId, { meterWh: persisted.meterWh });
  }

  options.localAuthorizationList.clear();
  for (const entry of persisted.localAuthorizationList) {
    options.localAuthorizationList.set(entry.idTag, entry.idTagInfo);
  }

  options.authorizationCache.clear();
  for (const entry of persisted.authorizationCache ?? []) {
    options.authorizationCache.set(entry.idTag, entry.idTagInfo);
  }

  for (const [key, value] of Object.entries(persisted.configurationValues)) {
    options.configuration.setValue(key, restoredConfigurationValue(key, value));
  }

  return { localListVersion: persisted.localListVersion, chargingProfiles };
}

function restoredConfigurationValue(key: string, value: string): string {
  if (isMeteringConfigurationKey(key) && value === "Energy.Active.Import.Register") {
    return DEFAULT_SAMPLED_DATA.join(",");
  }

  return value;
}

function isMeteringConfigurationKey(key: string): key is MeteringConfigurationKey {
  return key === "MeterValuesAlignedData" ||
    key === "MeterValuesSampledData" ||
    key === "StopTxnAlignedData" ||
    key === "StopTxnSampledData";
}

function serializeAuthorizationEntries(
  entries: Map<string, Record<string, unknown> | undefined>
): LocalAuthorizationEntry[] {
  return [...entries.entries()].map(([idTag, idTagInfo]) => {
    if (!idTagInfo) {
      return { idTag };
    }

    return { idTag, idTagInfo };
  });
}

function serializeMutableConfiguration(configuration: Map<string, ConfigurationKey>): Record<string, string> {
  return Object.fromEntries(
    [...configuration.entries()].filter(([, value]) => !value.readonly).map(([key, value]) => [key, value.value])
  );
}

function serializeConnectorMeterValues(connectors: ConnectorState[]): Record<string, number> {
  return Object.fromEntries(connectors.map((connector) => [String(connector.id), connector.meterWh]));
}

function serializeConnectorReservations(connectors: ConnectorState[]): Record<string, PersistedReservation> {
  return Object.fromEntries(
    connectors
      .filter((connector) => connector.reservationId && connector.reservationIdTag && connector.reservationExpiryDate)
      .map((connector) => {
        const reservation: PersistedReservation = {
          reservationId: connector.reservationId ?? 0,
          idTag: connector.reservationIdTag ?? "",
          expiryDate: connector.reservationExpiryDate ?? ""
        };
        if (connector.reservationParentIdTag) {
          reservation.parentIdTag = connector.reservationParentIdTag;
        }
        return [String(connector.id), reservation];
      })
  );
}

function serializeConnectorTransactions(connectors: ConnectorState[]): Record<string, PersistedConnectorTransaction> {
  return Object.fromEntries(
    connectors
      .filter((connector) => connector.transactionId || connector.pendingStartTransaction || connector.pendingStopTransaction)
      .map((connector) => [
        String(connector.id),
        {
          transactionId: connector.transactionId ?? 0,
          transactionStartedAt: connector.transactionStartedAt,
          lastIdTag: connector.lastIdTag,
          evConnected: connector.evConnected,
          status: connector.status,
          stopTransactionAtWh: connector.stopTransactionAtWh,
          pendingStartTransaction: connector.pendingStartTransaction as unknown as Record<string, unknown> | undefined,
          pendingStopTransaction: connector.pendingStopTransaction as unknown as Record<string, unknown> | undefined
        }
      ])
  );
}

function readPersistedConnectorStatus(value: string): ConnectorState["status"] {
  return value === "Available" ||
    value === "Preparing" ||
    value === "Charging" ||
    value === "SuspendedEVSE" ||
    value === "SuspendedEV" ||
    value === "Finishing" ||
    value === "Reserved" ||
    value === "Unavailable" ||
    value === "Faulted"
    ? value
    : "Charging";
}

function readPersistedPendingStart(value: Record<string, unknown> | undefined): ConnectorState["pendingStartTransaction"] {
  if (!value) {
    return undefined;
  }

  const connectorId = typeof value.connectorId === "number" ? value.connectorId : undefined;
  const idTag = typeof value.idTag === "string" ? value.idTag : undefined;
  const meterStart = typeof value.meterStart === "number" ? value.meterStart : undefined;
  const timestamp = typeof value.timestamp === "string" ? value.timestamp : undefined;
  if (connectorId === undefined || idTag === undefined || meterStart === undefined || timestamp === undefined) {
    return undefined;
  }

  const pendingStart = { connectorId, idTag, meterStart, timestamp };
  if (typeof value.reservationId === "number") {
    return { ...pendingStart, reservationId: value.reservationId };
  }
  return pendingStart;
}

function readPersistedPendingStop(value: Record<string, unknown> | undefined): ConnectorState["pendingStopTransaction"] {
  if (!value) {
    return undefined;
  }

  const meterStop = typeof value.meterStop === "number" ? value.meterStop : undefined;
  const timestamp = typeof value.timestamp === "string" ? value.timestamp : undefined;
  const reason = typeof value.reason === "string" ? value.reason : undefined;
  const transactionData = Array.isArray(value.transactionData) ? value.transactionData.filter(isRecord) : [];
  if (meterStop === undefined || timestamp === undefined || !isStopReason(reason)) {
    return undefined;
  }

  const pendingStop = { meterStop, timestamp, reason, transactionData };
  if (typeof value.idTag === "string") {
    return { ...pendingStop, idTag: value.idTag };
  }
  return pendingStop;
}

function isStopReason(value: unknown): value is StopReason {
  return value === "EmergencyStop" ||
    value === "EVDisconnected" ||
    value === "HardReset" ||
    value === "Local" ||
    value === "Other" ||
    value === "PowerLoss" ||
    value === "Reboot" ||
    value === "Remote" ||
    value === "SoftReset" ||
    value === "UnlockCommand" ||
    value === "DeAuthorized";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function isDefined<T>(value: T | undefined): value is T {
  return value !== undefined;
}
