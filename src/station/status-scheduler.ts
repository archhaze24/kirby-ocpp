import type { ConnectorStatus } from "../ocpp/types.js";

export class StatusNotificationScheduler {
  private readonly pending = new Map<number, { status: ConnectorStatus; timer: NodeJS.Timeout }>();
  private readonly lastSent = new Map<number, ConnectorStatus>();

  constructor(
    private readonly options: {
      minimumSeconds: () => number;
      currentStatus: (connectorId: number) => ConnectorStatus | undefined;
      send: (connectorId: number, status: ConnectorStatus) => void;
    }
  ) {}

  schedule(connectorId: number, status: ConnectorStatus): boolean {
    const pending = this.pending.get(connectorId);
    if (pending?.status === status) {
      return true;
    }

    if (pending) {
      this.clear(connectorId);
    }

    if (this.lastSent.get(connectorId) === status) {
      return true;
    }

    const minimumSeconds = this.options.minimumSeconds();
    if (minimumSeconds <= 0) {
      return false;
    }

    this.pending.set(
      connectorId,
      {
        status,
        timer: setTimeout(() => {
          this.pending.delete(connectorId);
          this.options.send(connectorId, status);
        }, minimumSeconds * 1000)
      }
    );
    return true;
  }

  markSent(connectorId: number, status: ConnectorStatus): void {
    this.lastSent.set(connectorId, status);
  }

  clear(connectorId: number): void {
    const pending = this.pending.get(connectorId);
    if (!pending) {
      return;
    }

    clearTimeout(pending.timer);
    this.pending.delete(connectorId);
  }

  clearAll(): void {
    for (const connectorId of [...this.pending.keys()]) {
      this.clear(connectorId);
    }
  }

  flush(): void {
    const connectorIds = [...this.pending.keys()];
    this.clearAll();
    for (const connectorId of connectorIds) {
      const status = this.options.currentStatus(connectorId);
      if (status) {
        this.options.send(connectorId, status);
      }
    }
  }
}
