import { clockAlignedDelayMs, periodicMeterDeltaWh } from "./metering.js";

export class MeterValueScheduler {
  private readonly periodicTimers = new Map<number, NodeJS.Timeout>();
  private clockAlignedTimer?: NodeJS.Timeout;

  constructor(
    private readonly options: {
      sampleIntervalSeconds: () => number;
      clockAlignedIntervalSeconds: () => number;
      isConnected: () => boolean;
      activeConnectorIds: () => number[];
      isPeriodicConnectorActive: (connectorId: number) => boolean;
      sendPeriodic: (connectorId: number, deltaWh: number) => Promise<void>;
      sendClockAligned: (connectorIds: number[], deltaWh: number) => Promise<void>;
      runSafely: (task: () => Promise<void>) => void;
    }
  ) {}

  startPeriodic(connectorId: number): void {
    this.stopPeriodic(connectorId);

    const intervalSeconds = this.options.sampleIntervalSeconds();
    if (intervalSeconds <= 0) {
      return;
    }

    const timer = setInterval(() => {
      this.options.runSafely(async () => {
        if (!this.options.isConnected()) {
          return;
        }

        if (!this.options.isPeriodicConnectorActive(connectorId)) {
          this.stopPeriodic(connectorId);
          return;
        }

        await this.options.sendPeriodic(connectorId, periodicMeterDeltaWh(intervalSeconds));
      });
    }, intervalSeconds * 1000);

    this.periodicTimers.set(connectorId, timer);
  }

  stopPeriodic(connectorId: number): void {
    const timer = this.periodicTimers.get(connectorId);
    if (!timer) {
      return;
    }

    clearInterval(timer);
    this.periodicTimers.delete(connectorId);
  }

  stopAllPeriodic(): void {
    for (const connectorId of [...this.periodicTimers.keys()]) {
      this.stopPeriodic(connectorId);
    }
  }

  reschedulePeriodic(connectorIds: number[]): void {
    for (const connectorId of connectorIds) {
      if (this.options.isPeriodicConnectorActive(connectorId)) {
        this.startPeriodic(connectorId);
        continue;
      }

      this.stopPeriodic(connectorId);
    }
  }

  startClockAligned(): void {
    this.stopClockAligned();

    const intervalSeconds = this.options.clockAlignedIntervalSeconds();
    if (intervalSeconds <= 0 || !this.options.isConnected()) {
      return;
    }

    this.clockAlignedTimer = setTimeout(() => {
      this.clockAlignedTimer = undefined;
      this.options.runSafely(async () => {
        try {
          await this.options.sendClockAligned(this.options.activeConnectorIds(), periodicMeterDeltaWh(intervalSeconds));
        } finally {
          if (this.options.isConnected()) {
            this.startClockAligned();
          }
        }
      });
    }, clockAlignedDelayMs(intervalSeconds));
  }

  stopClockAligned(): void {
    if (!this.clockAlignedTimer) {
      return;
    }

    clearTimeout(this.clockAlignedTimer);
    this.clockAlignedTimer = undefined;
  }
}
