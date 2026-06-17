import type { ConnectorState } from "../ocpp/types.js";

export class ConnectorRegistry {
  private readonly connectors = new Map<number, ConnectorState>();
  private onChange?: () => void;

  constructor(
    private readonly primaryConnectorId: number,
    count: number
  ) {
    const connectorCount = Math.max(validConnectorCount(count), primaryConnectorId);
    for (let connectorId = 1; connectorId <= connectorCount; connectorId += 1) {
      this.connectors.set(connectorId, createConnector(connectorId));
    }
  }

  setOnChange(onChange: () => void): void {
    this.onChange = onChange;
  }

  get size(): number {
    return this.connectors.size;
  }

  has(id: number): boolean {
    return this.connectors.has(id);
  }

  get(id: number): ConnectorState {
    const connector = this.connectors.get(id);
    if (!connector) {
      throw new Error(`Connector ${id} does not exist`);
    }

    return connector;
  }

  find(id: number): ConnectorState | undefined {
    return this.connectors.get(id);
  }

  all(): ConnectorState[] {
    return [...this.connectors.values()].sort((left, right) => left.id - right.id);
  }

  add(): ConnectorState {
    const connector = createConnector(this.nextId());
    this.connectors.set(connector.id, connector);
    this.notifyChange();
    return connector;
  }

  ensureCount(count: number): void {
    const initialSize = this.connectors.size;
    while (this.connectors.size < count) {
      const connector = createConnector(this.nextId());
      this.connectors.set(connector.id, connector);
    }

    if (this.connectors.size !== initialSize) {
      this.notifyChange();
    }
  }

  patch(id: number, patch: Partial<ConnectorState>): void {
    Object.assign(this.get(id), patch);
    this.notifyChange();
  }

  snapshot(): { connectors: ConnectorState[]; primary?: ConnectorState } {
    const connectors = this.all().map((connector) => ({ ...connector }));
    const primary = this.connectors.get(this.primaryConnectorId) ?? connectors[0];
    return { connectors, primary };
  }

  findAvailable(): ConnectorState | undefined {
    return this.all().find(
      (connector) =>
        !connector.transactionId &&
        connector.availability === "Operative" &&
        connector.status === "Available"
    );
  }

  findByTransaction(transactionId: number): ConnectorState | undefined {
    return this.all().find((connector) => connector.transactionId === transactionId);
  }

  findByReservation(reservationId: number): ConnectorState | undefined {
    return this.all().find((connector) => connector.reservationId === reservationId);
  }

  private nextId(): number {
    return Math.max(0, ...this.connectors.keys()) + 1;
  }

  private notifyChange(): void {
    this.onChange?.();
  }
}

function validConnectorCount(count: number): number {
  return Number.isInteger(count) && count > 0 ? count : 1;
}

function createConnector(id: number): ConnectorState {
  return {
    id,
    status: "Available",
    availability: "Operative",
    errorCode: "NoError",
    evConnected: false,
    meterWh: 0
  };
}
