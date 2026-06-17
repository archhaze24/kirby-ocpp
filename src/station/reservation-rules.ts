import type { ConnectorState } from "../ocpp/types.js";

export type ReserveNowStatus = "Accepted" | "Faulted" | "Occupied" | "Rejected" | "Unavailable";

export function reserveNowRejection(connector: ConnectorState | undefined, expiryDate: string): Exclude<ReserveNowStatus, "Accepted"> | undefined {
  if (!connector) {
    return "Unavailable";
  }

  if (connector.transactionId || connector.reservationId || connector.status === "Reserved") {
    return "Occupied";
  }

  if (connector.status === "Faulted") {
    return "Faulted";
  }

  if (connector.availability === "Inoperative" || connector.status === "Unavailable") {
    return "Unavailable";
  }

  if (!expiryDate || Date.parse(expiryDate) <= Date.now()) {
    return "Rejected";
  }

  return undefined;
}
