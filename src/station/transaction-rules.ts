import type { AvailabilityType, ConnectorState, ConnectorStatus, StopReason } from "../ocpp/types.js";

export function startTransactionRejection(connector: ConnectorState, idTag: string): string | undefined {
  if (connector.transactionId) {
    return `Connector ${connector.id} already has transaction ${connector.transactionId}`;
  }

  if (connector.availability === "Inoperative" || connector.status === "Unavailable") {
    return `Connector ${connector.id} is unavailable`;
  }

  if (connector.status === "Faulted") {
    return `Connector ${connector.id} is faulted`;
  }

  if (connector.status === "Finishing") {
    return `Connector ${connector.id} is finishing; unplug before starting a new transaction`;
  }

  if (connector.reservationId && connector.reservationIdTag !== idTag) {
    return `Connector ${connector.id} is reserved for another idTag`;
  }

  return undefined;
}

export function canRemoteStart(connector: ConnectorState | undefined): boolean {
  return Boolean(connector) &&
    !connector?.transactionId &&
    connector?.availability === "Operative" &&
    connector.status !== "Unavailable" &&
    connector.status !== "Faulted" &&
    connector.status !== "Reserved";
}

export function statusAfterStopTransaction(reason: StopReason, evConnected: boolean, availability: AvailabilityType): {
  evConnected: boolean;
  status: ConnectorStatus;
} {
  const stillConnected = reason !== "EVDisconnected" && evConnected;
  const availableStatus: ConnectorStatus = availability === "Inoperative" ? "Unavailable" : "Available";
  return {
    evConnected: stillConnected,
    status: stillConnected ? "Finishing" : availableStatus
  };
}
