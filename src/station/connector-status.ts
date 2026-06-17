import type { AvailabilityType, ChargePointErrorCode, ConnectorState, ConnectorStatus } from "../ocpp/types.js";

export const CONNECTOR_STATUS_SEQUENCE: ConnectorStatus[] = [
  "Available",
  "Preparing",
  "Charging",
  "SuspendedEVSE",
  "SuspendedEV",
  "Finishing",
  "Reserved",
  "Unavailable",
  "Faulted"
];

export function nextConnectorStatus(status: ConnectorStatus): ConnectorStatus {
  const currentIndex = CONNECTOR_STATUS_SEQUENCE.indexOf(status);
  return CONNECTOR_STATUS_SEQUENCE[(currentIndex + 1) % CONNECTOR_STATUS_SEQUENCE.length] ?? "Available";
}

export function faultErrorCode(connector: ConnectorState): ChargePointErrorCode {
  return connector.errorCode === "NoError" ? "OtherError" : connector.errorCode;
}

export function statusAfterReservationClear(connector: ConnectorState): ConnectorStatus {
  if (connector.availability === "Inoperative" || connector.status === "Unavailable") {
    return "Unavailable";
  }

  if (connector.status === "Faulted") {
    return "Faulted";
  }

  if (connector.transactionId) {
    return "Charging";
  }

  if (connector.evConnected) {
    return "Preparing";
  }

  return "Available";
}

export function statusAfterFaultClear(connector: ConnectorState): ConnectorStatus {
  if (connector.availability === "Inoperative") {
    return "Unavailable";
  }

  if (connector.transactionId) {
    return "Charging";
  }

  if (connector.evConnected) {
    return "Preparing";
  }

  if (connector.reservationId) {
    return "Reserved";
  }

  return "Available";
}

export function statusAfterAvailabilityChange(connector: ConnectorState, availability: AvailabilityType): ConnectorStatus {
  if (connector.status === "Faulted") {
    return "Faulted";
  }

  if (availability === "Inoperative") {
    return "Unavailable";
  }

  if (connector.status === "Finishing") {
    return "Finishing";
  }

  if (connector.reservationId) {
    return "Reserved";
  }

  if (connector.evConnected) {
    return "Preparing";
  }

  return "Available";
}
