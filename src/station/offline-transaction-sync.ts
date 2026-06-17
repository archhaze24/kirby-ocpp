import type { CallResponse, ConnectorStatus, StopReason } from "../ocpp/types.js";
import { isAcceptedIdTagInfo, type IdTagInfo } from "./authorization.js";
import type { ChargingProfileRegistry } from "./charging-profile-registry.js";
import type { ConnectorRegistry } from "./connector-registry.js";
import { readNumber } from "./payload.js";
import { statusAfterStopTransaction } from "./transaction-rules.js";

export interface OfflineTransactionSyncOptions {
  connectors: ConnectorRegistry;
  chargingProfiles: ChargingProfileRegistry;
  send: (action: string, payload: Record<string, unknown>) => Promise<CallResponse>;
  readIdTagInfo: (value: unknown) => IdTagInfo;
  rememberAuthorization: (idTag: string, idTagInfo: IdTagInfo) => void;
  save: () => void;
  log: (level: "success" | "warn", message: string) => void;
  statusNotification: (connectorId: number, status: ConnectorStatus) => Promise<void>;
}

export class OfflineTransactionSync {
  constructor(private readonly options: OfflineTransactionSyncOptions) {}

  async syncPendingTransactions(): Promise<void> {
    for (const connector of this.options.connectors.all()) {
      if (!connector.pendingStartTransaction) {
        if (connector.pendingStopTransaction && connector.transactionId && connector.transactionId > 0) {
          await this.deliverPendingStopTransaction(connector.id);
        }
        continue;
      }

      await this.deliverPendingStartTransaction(connector.id);
    }
  }

  private async deliverPendingStartTransaction(connectorId: number): Promise<void> {
    const connector = this.options.connectors.get(connectorId);
    const pendingStart = connector.pendingStartTransaction;
    if (!pendingStart) {
      return;
    }

    const response = await this.options.send("StartTransaction", pendingStart as unknown as Record<string, unknown>);
    const idTagInfo = this.options.readIdTagInfo(response.payload.idTagInfo);
    this.options.rememberAuthorization(pendingStart.idTag, idTagInfo);

    if (!isAcceptedIdTagInfo(idTagInfo)) {
      this.options.log("warn", `Offline StartTransaction rejected during sync: ${idTagInfo.status} (${pendingStart.idTag})`);
      this.options.connectors.patch(connectorId, {
        transactionId: undefined,
        transactionStartedAt: undefined,
        pendingStartTransaction: undefined,
        pendingStopTransaction: undefined,
        stopTransactionAtWh: undefined,
        status: connector.evConnected ? "Finishing" : connector.availability === "Inoperative" ? "Unavailable" : "Available"
      });
      this.options.save();
      return;
    }

    const transactionId = readNumber(response.payload.transactionId, Date.now());
    const oldTransactionId = connector.transactionId;
    this.options.connectors.patch(connectorId, {
      transactionId,
      pendingStartTransaction: undefined
    });
    if (oldTransactionId && oldTransactionId !== transactionId) {
      this.options.chargingProfiles.clearForTransaction(oldTransactionId);
    }
    this.options.save();
    this.options.log("success", `Offline transaction on connector ${connectorId} synced as transaction ${transactionId}`);

    if (this.options.connectors.get(connectorId).pendingStopTransaction) {
      await this.deliverPendingStopTransaction(connectorId);
    }
  }

  private async deliverPendingStopTransaction(connectorId: number): Promise<void> {
    const connector = this.options.connectors.get(connectorId);
    if (!connector.pendingStopTransaction || !connector.transactionId) {
      return;
    }

    const payload: Record<string, unknown> = {
      ...connector.pendingStopTransaction,
      transactionId: connector.transactionId
    };
    await this.options.send("StopTransaction", payload);
    const { evConnected, status } = statusAfterStopTransaction(
      connector.pendingStopTransaction.reason,
      connector.evConnected,
      connector.availability
    );
    const transactionId = connector.transactionId;
    this.options.connectors.patch(connectorId, {
      transactionId: undefined,
      transactionStartedAt: undefined,
      stopTransactionAtWh: undefined,
      pendingStartTransaction: undefined,
      pendingStopTransaction: undefined,
      evConnected,
      status
    });
    if (this.options.chargingProfiles.clearForTransaction(transactionId)) {
      this.options.save();
    }
    this.options.save();
    await this.options.statusNotification(connectorId, status);
  }
}

export function localTransactionId(connectorId: number): number {
  return -Math.abs(Date.now() * 100 + connectorId);
}

export function pendingStopPayload(options: {
  meterStop: number;
  timestamp: string;
  reason: StopReason;
  idTag?: string;
  transactionData: Record<string, unknown>[];
}): Record<string, unknown> {
  const payload: Record<string, unknown> = {
    meterStop: options.meterStop,
    timestamp: options.timestamp,
    reason: options.reason,
    transactionData: options.transactionData
  };

  if (options.idTag) {
    payload.idTag = options.idTag;
  }

  return payload;
}
