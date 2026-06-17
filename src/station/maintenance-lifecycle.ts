import type { DiagnosticsStatus, FirmwareStatus } from "../ocpp/types.js";
import { readNumber, readString } from "./payload.js";

export type DiagnosticsOutcome = "success" | "uploadFailure";
export type FirmwareOutcome = "success" | "downloadFailure" | "installationFailure";

interface ScheduledStatus<TStatus extends string> {
  status: TStatus;
  delayMs: number;
}

export class MaintenanceLifecycle {
  private diagnosticsTimers: NodeJS.Timeout[] = [];
  private firmwareTimers: NodeJS.Timeout[] = [];
  private diagnosticsOutcome: DiagnosticsOutcome = "success";
  private firmwareOutcome: FirmwareOutcome = "success";

  constructor(
    private readonly options: {
      run: (task: () => Promise<void>) => void;
      diagnosticsStatus: (status: DiagnosticsStatus) => Promise<void>;
      firmwareStatus: (status: FirmwareStatus) => Promise<void>;
      log: (level: "info" | "warn", message: string) => void;
    }
  ) {}

  setDiagnosticsOutcome(outcome: DiagnosticsOutcome): void {
    this.diagnosticsOutcome = outcome;
  }

  setFirmwareOutcome(outcome: FirmwareOutcome): void {
    this.firmwareOutcome = outcome;
  }

  startDiagnostics(payload: Record<string, unknown>): string {
    this.clearDiagnostics();
    const location = readString(payload.location, "");
    const fileName = diagnosticsFileName(location);
    const retries = boundedRetries(readNumber(payload.retries, 0));
    const retryInterval = readNumber(payload.retryInterval, 0);
    const startDelay = retryInterval > 0 ? Math.min(retryInterval * 1000, 1000) : 100;
    const stepDelay = retryInterval > 0 ? Math.min(retryInterval * 1000, 1000) : 400;
    const statuses = diagnosticsStatuses(this.diagnosticsOutcome, retries, startDelay, stepDelay);

    this.options.log("info", `Diagnostics upload scheduled to ${location}`);
    this.diagnosticsTimers = this.schedule(statuses, this.options.diagnosticsStatus);
    return fileName;
  }

  startFirmware(payload: Record<string, unknown>): void {
    this.clearFirmware();
    const location = readString(payload.location, "");
    const retrieveDate = readString(payload.retrieveDate, "");
    const retries = boundedRetries(readNumber(payload.retries, 0));
    const retryInterval = readNumber(payload.retryInterval, 0);
    const startDelay = Math.min(Math.max(Date.parse(retrieveDate) - Date.now(), 100), 1000);
    const stepDelay = retryInterval > 0 ? Math.min(retryInterval * 1000, 1000) : 400;
    const statuses = firmwareStatuses(this.firmwareOutcome, retries, startDelay, stepDelay);

    this.options.log("info", `Firmware update scheduled from ${location}`);
    this.firmwareTimers = this.schedule(statuses, this.options.firmwareStatus);
  }

  clearAll(): void {
    this.clearDiagnostics();
    this.clearFirmware();
  }

  private clearDiagnostics(): void {
    this.clear(this.diagnosticsTimers);
    this.diagnosticsTimers = [];
  }

  private clearFirmware(): void {
    this.clear(this.firmwareTimers);
    this.firmwareTimers = [];
  }

  private schedule<TStatus extends string>(
    statuses: ScheduledStatus<TStatus>[],
    send: (status: TStatus) => Promise<void>
  ): NodeJS.Timeout[] {
    return statuses.map(({ status, delayMs }) =>
      setTimeout(() => {
        this.options.run(() => send(status));
      }, delayMs)
    );
  }

  private clear(timers: NodeJS.Timeout[]): void {
    for (const timer of timers) {
      clearTimeout(timer);
    }
  }
}

function diagnosticsFileName(location: string): string {
  try {
    const url = new URL(location);
    const leaf = url.pathname.split("/").filter(Boolean).at(-1);
    return leaf || "kirby-ocpp-diagnostics.log";
  } catch {
    return "kirby-ocpp-diagnostics.log";
  }
}

function diagnosticsStatuses(
  outcome: DiagnosticsOutcome,
  retries: number,
  startDelay: number,
  stepDelay: number
): ScheduledStatus<DiagnosticsStatus>[] {
  if (outcome === "uploadFailure") {
    return retryingStatuses("Uploading", "UploadFailed", retries, startDelay, stepDelay);
  }

  return [
    ...Array.from({ length: retries + 1 }, (_, index) => ({ status: "Uploading" as const, delayMs: startDelay + stepDelay * index })),
    { status: "Uploaded", delayMs: startDelay + stepDelay * (retries + 1) }
  ];
}

function firmwareStatuses(
  outcome: FirmwareOutcome,
  retries: number,
  startDelay: number,
  stepDelay: number
): ScheduledStatus<FirmwareStatus>[] {
  if (outcome === "downloadFailure") {
    return retryingStatuses("Downloading", "DownloadFailed", retries, startDelay, stepDelay);
  }

  if (outcome === "installationFailure") {
    return [
      { status: "Downloading", delayMs: startDelay },
      { status: "Downloaded", delayMs: startDelay + stepDelay },
      ...retryingStatuses("Installing", "InstallationFailed", retries, startDelay + stepDelay * 2, stepDelay)
    ];
  }

  return [
    { status: "Downloading", delayMs: startDelay },
    { status: "Downloaded", delayMs: startDelay + stepDelay },
    { status: "Installing", delayMs: startDelay + stepDelay * 2 },
    { status: "Installed", delayMs: startDelay + stepDelay * 3 }
  ];
}

function retryingStatuses<TStatus extends string>(
  activeStatus: TStatus,
  failureStatus: TStatus,
  retries: number,
  startDelay: number,
  stepDelay: number
): ScheduledStatus<TStatus>[] {
  return [
    ...Array.from({ length: retries + 1 }, (_, index) => ({ status: activeStatus, delayMs: startDelay + stepDelay * index })),
    { status: failureStatus, delayMs: startDelay + stepDelay * (retries + 1) }
  ];
}

function boundedRetries(value: number): number {
  return Number.isInteger(value) && value > 0 ? Math.min(value, 5) : 0;
}
