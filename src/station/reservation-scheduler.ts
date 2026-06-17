export class ReservationScheduler {
  private readonly timers = new Map<number, NodeJS.Timeout>();

  constructor(
    private readonly options: {
      expiryDate: (connectorId: number) => string | undefined;
      expire: (connectorId: number) => void;
    }
  ) {}

  schedule(connectorId: number): void {
    this.clear(connectorId);

    const expiryDate = this.options.expiryDate(connectorId);
    if (!expiryDate) {
      return;
    }

    const delay = Date.parse(expiryDate) - Date.now();
    if (delay <= 0) {
      this.options.expire(connectorId);
      return;
    }

    this.timers.set(connectorId, setTimeout(() => this.options.expire(connectorId), delay));
  }

  clear(connectorId: number): void {
    const timer = this.timers.get(connectorId);
    if (!timer) {
      return;
    }

    clearTimeout(timer);
    this.timers.delete(connectorId);
  }

  clearAll(): void {
    for (const connectorId of [...this.timers.keys()]) {
      this.clear(connectorId);
    }
  }
}
