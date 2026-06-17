import { spawnSync } from "node:child_process";
import blessed from "blessed";
import type { Widgets } from "blessed";
import { Station } from "./station.js";
import type { ChargePointErrorCode, LogEntry, StationState, StopReason } from "./ocpp/types.js";

const LEVEL_STYLE: Record<LogEntry["level"], string> = {
  info: "{blue-fg}info{/blue-fg}",
  success: "{green-fg}ok{/green-fg}",
  warn: "{yellow-fg}warn{/yellow-fg}",
  error: "{red-fg}err{/red-fg}",
  in: "{magenta-fg}in{/magenta-fg}",
  out: "{cyan-fg}out{/cyan-fg}"
};

export class Tui {
  private readonly screen: Widgets.Screen;
  private readonly header: Widgets.BoxElement;
  private readonly stateBox: Widgets.BoxElement;
  private readonly controlsBox: Widgets.BoxElement;
  private readonly logBox: Widgets.Log;
  private readonly logs: LogEntry[] = [];
  private plainLogView?: Widgets.BoxElement;
  private selectedConnectorId: number;
  private stateBoxHeight = 18;

  constructor(private readonly station: Station) {
    this.selectedConnectorId = station.config.connectorId;
    this.screen = blessed.screen({
      smartCSR: true,
      mouse: false,
      title: "kirby-ocpp"
    });

    this.header = blessed.box({
      top: 0,
      left: 0,
      width: "100%",
      height: 3,
      tags: true,
      style: { fg: "white", bg: "blue" }
    });

    this.stateBox = blessed.box({
      label: " Station ",
      top: 3,
      left: 0,
      width: "40%",
      height: this.stateBoxHeight,
      tags: true,
      border: "line",
      padding: { left: 1, right: 1 },
      style: {
        border: { fg: "cyan" }
      }
    });

    this.controlsBox = blessed.box({
      label: " Controls ",
      top: 21,
      left: 0,
      width: "40%",
      bottom: 0,
      tags: true,
      border: "line",
      padding: { left: 1, right: 1 },
      style: {
        border: { fg: "cyan" }
      }
    });

    this.logBox = blessed.log({
      label: " OCPP log ",
      top: 3,
      left: "40%",
      width: "60%",
      bottom: 0,
      tags: true,
      border: "line",
      padding: { left: 1, right: 1 },
      keys: true,
      vi: true,
      mouse: false,
      scrollable: true,
      alwaysScroll: true,
      scrollbar: {
        ch: " ",
        style: { bg: "cyan" }
      },
      style: {
        border: { fg: "cyan" }
      }
    });

    this.screen.append(this.header);
    this.screen.append(this.stateBox);
    this.screen.append(this.controlsBox);
    this.screen.append(this.logBox);

    this.layout();
    this.bindKeys();
    this.bindStation();
    this.renderState(this.station.state);
    this.renderControls();
  }

  start(): void {
    this.screen.render();
    this.station.connect();
  }

  stop(): void {
    this.station.disconnect();
    this.screen.destroy();
  }

  private bindKeys(): void {
    this.screen.key("escape", () => this.closePlainLogView());
    this.screen.key(["q", "C-c"], () => {
      if (this.plainLogView) {
        this.closePlainLogView();
        return;
      }

      this.stopAndExit();
    });
    this.screen.key("b", () => this.runStationTask(() => this.station.boot()));
    this.screen.key("h", () => this.runStationTask(() => this.station.heartbeat()));
    this.screen.key("s", () => this.runStationTask(() => this.station.cycleStatus(this.selectedConnectorId)));
    this.screen.key("a", () => this.promptIdTag("Authorize", (idTag) => this.runStationTask(() => this.station.authorize(idTag))));
    this.screen.key("t", () => {
      const connector = this.selectedConnector();
      if (connector?.transactionId) {
        this.promptText("StopTransaction reason", "Local", (reason) =>
          this.runStationTask(() => this.station.stopTransaction(this.selectedConnectorId, readStopReason(reason)))
        );
        return;
      }

      this.promptIdTag("StartTransaction", (idTag) =>
        this.runStationTask(() => this.station.startTransaction(this.selectedConnectorId, idTag))
      );
    });
    this.screen.key("m", () => this.runStationTask(() => this.station.meterValues(this.selectedConnectorId)));
    this.screen.key(["M", "S-m"], () => {
      this.promptText(
        "MeterValues measurands",
        "Energy.Active.Import.Register,Power.Active.Import,Current.Import,Voltage,SoC",
        (sampledData) => this.runStationTask(() => this.station.meterValues(this.selectedConnectorId, 120, sampledData))
      );
    });
    this.screen.key("d", () => {
      this.promptText("DataTransfer JSON", JSON.stringify({ vendorId: this.station.config.vendor, data: "ping" }), (value) => {
        const payload = parseObjectJson(value);
        if (!payload || typeof payload.vendorId !== "string") {
          this.renderLog({
            at: new Date(),
            level: "error",
            message: "DataTransfer JSON must be an object with string vendorId"
          });
          return;
        }

        this.runStationTask(() => this.station.dataTransfer(payload));
      });
    });
    this.screen.key("f", () => {
      const connector = this.selectedConnector();
      if (connector?.status === "Faulted") {
        this.runStationTask(() => this.station.clearFault(this.selectedConnectorId));
        return;
      }

      this.promptText("Fault errorCode", "OtherError", (errorCode) =>
        this.runStationTask(() => this.station.setFault(this.selectedConnectorId, readChargePointErrorCode(errorCode)))
      );
    });
    this.screen.key("p", () => {
      const connector = this.selectedConnector();
      this.runStationTask(() =>
        connector?.evConnected || connector?.status === "Finishing"
          ? this.station.unplug(this.selectedConnectorId)
          : this.station.plugIn(this.selectedConnectorId)
      );
    });
    this.screen.key("r", () => this.station.reconnect());
    this.screen.key(["j", "down"], () => this.scrollLog(1));
    this.screen.key(["k", "up"], () => this.scrollLog(-1));
    this.screen.key(["g", "G", "S-g", "end"], () => this.scrollLogToEnd());
    this.screen.key("l", () => this.openPlainLogView());
    this.screen.key("y", () => this.copyLogsToClipboard());
    this.screen.key(["[", "left"], () => this.selectPreviousConnector());
    this.screen.key(["]", "right"], () => this.selectNextConnector());
    this.screen.key("+", () => {
      this.selectedConnectorId = this.station.addConnector();
      this.renderState(this.station.state);
    });
    this.screen.on("resize", () => {
      this.layout();
      this.renderState(this.station.state);
      this.renderControls();
      this.screen.render();
    });
    this.bindLogNavigationKeys(this.logBox);
  }

  private bindStation(): void {
    this.station.on("state", (state) => this.renderState(state));
    this.station.on("log", (entry) => this.renderLog(entry));
  }

  private renderState(state: StationState): void {
    const connector = this.selectedConnector(state);
    const transaction = connector?.transactionId ? String(connector.transactionId) : "-";
    const reservation = connector?.reservationId ? String(connector.reservationId) : "-";
    const reservationTag = connector?.reservationIdTag ?? "-";
    const connectorStatus = connector?.status ?? "-";
    const availability = connector?.availability ?? "-";
    const errorCode = connector?.errorCode ?? "-";
    const ev = connector?.evConnected ? "connected" : "-";
    const meterWh = connector?.meterWh ?? 0;
    const lastHeartbeat = state.lastHeartbeatAt ? state.lastHeartbeatAt.toLocaleTimeString() : "-";
    const contentWidth = this.leftPanelContentWidth();
    const connectorLines = state.connectors.map((item) => {
      const marker = item.id === this.selectedConnectorId ? ">" : " ";
      const tx = item.transactionId ? ` tx:${item.transactionId}` : "";
      const reserve = item.reservationId ? ` res:${item.reservationId}` : "";
      const evConnected = item.evConnected ? " ev" : "";
      const fault = item.status === "Faulted" ? ` ${item.errorCode}` : "";
      return truncateText(`${marker} #${item.id} ${item.status}${fault}${evConnected} ${Math.round(item.meterWh)}Wh${tx}${reserve}`, contentWidth);
    });
    const baseLines = [
      `Connection: ${state.connected ? "{green-fg}connected{/green-fg}" : "{red-fg}offline{/red-fg}"}`,
      `Boot:       ${state.booted ? "{green-fg}accepted{/green-fg}" : state.registrationStatus}`,
      `Selected:   #${this.selectedConnectorId} ${connectorStatus}`,
      `Error:      ${errorCode}`,
      `Available:  ${availability}`,
      `EV:         ${ev}  Txn: ${transaction}`,
      `Reserve:    ${reservation}  idTag: ${truncateText(reservationTag, 14)}`,
      `Meter:      ${Math.round(meterWh)} Wh`,
      `LocalList:  ${state.localListVersion}`,
      `Heartbeat:  ${lastHeartbeat}`,
      "",
      `{bold}Connectors (${state.connectors.length}){/bold}`
    ];
    const visibleConnectors = this.visibleConnectorLines(connectorLines, baseLines.length);

    this.header.setContent(
      ` {bold}kirby-ocpp{/bold}  ${this.station.config.chargePointId} -> ${this.station.config.centralSystemUrl}`
    );

    this.stateBox.setContent(
      [
        ...baseLines,
        ...visibleConnectors
      ].join("\n")
    );

    this.screen.render();
  }

  private renderControls(): void {
    this.controlsBox.setContent(
      [
        "{bold}b{/bold} BootNotification",
        "{bold}h{/bold} Heartbeat",
        "{bold}s{/bold} Cycle StatusNotification",
        "{bold}a{/bold} Authorize",
        "{bold}t{/bold} Start/StopTransaction",
        "{bold}m{/bold} MeterValues +120 Wh",
        "{bold}M{/bold} MeterValues custom",
        "{bold}d{/bold} DataTransfer",
        "{bold}f{/bold} Fault/Clear fault",
        "{bold}p{/bold} Plug/Unplug EV",
        "{bold}j/k{/bold} Scroll, {bold}G/g{/bold} bottom",
        "{bold}l{/bold} Plain log, {bold}y{/bold} copy",
        "{bold}[ ]{/bold} Select, {bold}+{/bold} add",
        "{bold}r{/bold} Reconnect, {bold}q{/bold} quit"
      ].join("\n")
    );
  }

  private layout(): void {
    const rows = this.screenRows();
    const controlsHeight = rows >= 34 ? 16 : 13;
    this.stateBoxHeight = Math.max(16, rows - 3 - controlsHeight);

    this.stateBox.top = 3;
    this.stateBox.height = this.stateBoxHeight;
    this.controlsBox.top = 3 + this.stateBoxHeight;
    this.controlsBox.bottom = 0;
  }

  private visibleConnectorLines(connectorLines: string[], usedRows: number): string[] {
    const maxLines = Math.max(1, this.stateContentRows() - usedRows);
    const selectedIndex = Math.max(
      0,
      this.station.state.connectors.findIndex((connector) => connector.id === this.selectedConnectorId)
    );
    const maxStart = Math.max(0, connectorLines.length - maxLines);
    const start = Math.min(Math.max(0, selectedIndex - maxLines + 1), maxStart);

    return connectorLines.slice(start, start + maxLines);
  }

  private stateContentRows(): number {
    return Math.max(1, this.stateBoxHeight - 2);
  }

  private screenRows(): number {
    return typeof this.screen.height === "number" ? this.screen.height : 40;
  }

  private leftPanelContentWidth(): number {
    const columns = typeof this.screen.width === "number" ? this.screen.width : 100;
    return Math.max(24, Math.floor(columns * 0.4) - 4);
  }

  private renderLog(entry: LogEntry): void {
    this.logs.push(entry);
    if (this.logs.length > 500) {
      this.logs.shift();
    }

    this.logBox.log(this.formatLogEntryForTui(entry));

    if (entry.details) {
      this.logBox.log(`  {gray-fg}${entry.details}{/gray-fg}`);
    }

    this.refreshPlainLogView();

    this.screen.render();
  }

  private stopAndExit(): void {
    this.stop();
    process.exit(0);
  }

  private promptIdTag(label: string, callback: (idTag: string) => void): void {
    this.promptText(label, "", callback);
  }

  private promptText(label: string, initialValue: string, callback: (value: string) => void): void {
    const prompt = blessed.prompt({
      parent: this.screen,
      top: "center",
      left: "center",
      width: 54,
      height: 8,
      border: "line",
      label: ` ${label} `,
      tags: true,
      keys: true,
      vi: true,
      style: {
        border: { fg: "cyan" }
      }
    });

    prompt.input(label, initialValue, (_error, value) => {
      prompt.destroy();
      this.screen.render();

      const result = value?.trim();
      if (result) {
        callback(result);
      }
    });
  }

  private runStationTask(task: () => Promise<unknown>): void {
    task().catch((error) => {
      const message = error instanceof Error ? error.message : String(error);
      this.renderLog({
        at: new Date(),
        level: "error",
        message
      });
    });
  }

  private scrollLog(offset: number): void {
    const target = this.plainLogView ?? this.logBox;
    target.scroll(offset);
    this.screen.render();
  }

  private scrollLogToEnd(): void {
    const target = this.plainLogView ?? this.logBox;
    target.setScrollPerc(100);
    this.screen.render();
  }

  private openPlainLogView(): void {
    if (this.plainLogView) {
      this.plainLogView.focus();
      return;
    }

    this.plainLogView = blessed.box({
      top: 0,
      left: 0,
      width: "100%",
      height: "100%",
      tags: false,
      keys: true,
      vi: true,
      mouse: false,
      scrollable: true,
      alwaysScroll: false,
      content: this.plainLogContent(),
      style: {
        fg: "white",
        bg: "black"
      },
      scrollbar: {
        ch: " ",
        style: { bg: "cyan" }
      }
    });
    this.bindLogNavigationKeys(this.plainLogView);

    this.screen.append(this.plainLogView);
    this.plainLogView.setFront();
    this.plainLogView.focus();
    this.plainLogView.setScrollPerc(100);
    this.screen.render();
  }

  private closePlainLogView(): void {
    if (!this.plainLogView) {
      return;
    }

    this.plainLogView.destroy();
    this.plainLogView = undefined;
    this.screen.render();
  }

  private refreshPlainLogView(): void {
    if (!this.plainLogView) {
      return;
    }

    const scrollPercent = this.plainLogView.getScrollPerc();
    this.plainLogView.setContent(this.plainLogContent());
    if (scrollPercent >= 99) {
      this.plainLogView.setScrollPerc(100);
    }
  }

  private copyLogsToClipboard(): void {
    const text = this.logs.map((entry) => this.formatLogEntryForClipboard(entry)).join("\n");
    if (!text) {
      this.renderLog({
        at: new Date(),
        level: "warn",
        message: "Log is empty"
      });
      return;
    }

    const copied = copyToClipboard(text);
    this.renderLog({
      at: new Date(),
      level: copied ? "success" : "error",
      message: copied ? `Copied ${this.logs.length} log entries to clipboard` : "Could not copy logs to clipboard"
    });
  }

  private formatLogEntryForTui(entry: LogEntry): string {
    const time = entry.at.toLocaleTimeString();
    const level = LEVEL_STYLE[entry.level];
    return `${time} ${level} ${entry.message}`;
  }

  private formatLogEntryForClipboard(entry: LogEntry): string {
    const line = `${entry.at.toISOString()} ${entry.level.toUpperCase()} ${entry.message}`;
    return entry.details ? `${line}\n  ${entry.details}` : line;
  }

  private plainLogContent(): string {
    const lines = this.logs.map((entry) => this.formatLogEntryForClipboard(entry));
    return [
      "OCPP log - j/k scroll, G bottom, y copy, Esc close",
      "",
      ...lines
    ].join("\n");
  }

  private bindLogNavigationKeys(widget: Widgets.BoxElement | Widgets.Log): void {
    widget.key(["g", "G", "S-g", "end"], () => this.scrollLogToEnd());
  }

  private selectedConnector(state = this.station.state) {
    return state.connectors.find((connector) => connector.id === this.selectedConnectorId) ?? state.connectors[0];
  }

  private selectPreviousConnector(): void {
    const ids = this.station.state.connectors.map((connector) => connector.id);
    const currentIndex = ids.indexOf(this.selectedConnectorId);
    this.selectedConnectorId = ids[Math.max(0, currentIndex - 1)] ?? this.selectedConnectorId;
    this.renderState(this.station.state);
  }

  private selectNextConnector(): void {
    const ids = this.station.state.connectors.map((connector) => connector.id);
    const currentIndex = ids.indexOf(this.selectedConnectorId);
    this.selectedConnectorId = ids[Math.min(ids.length - 1, currentIndex + 1)] ?? this.selectedConnectorId;
    this.renderState(this.station.state);
  }
}

function copyToClipboard(text: string): boolean {
  const command = clipboardCommand();
  if (!command) {
    return false;
  }

  const result = spawnSync(command.command, command.args, {
    input: text,
    stdio: ["pipe", "ignore", "ignore"]
  });

  return result.status === 0;
}

function clipboardCommand(): { command: string; args: string[] } | undefined {
  if (process.platform === "darwin") {
    return { command: "pbcopy", args: [] };
  }

  if (process.platform === "win32") {
    return { command: "clip", args: [] };
  }

  for (const candidate of [
    { command: "wl-copy", args: [] },
    { command: "xclip", args: ["-selection", "clipboard"] },
    { command: "xsel", args: ["--clipboard", "--input"] }
  ]) {
    const result = spawnSync(candidate.command, ["--version"], { stdio: "ignore" });
    if (result.status === 0) {
      return candidate;
    }
  }

  return undefined;
}

function readStopReason(value: string): StopReason {
  const normalized = value.trim();
  return isStopReason(normalized) ? normalized : "Local";
}

function parseObjectJson(value: string): Record<string, unknown> | undefined {
  try {
    const parsed = JSON.parse(value);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed as Record<string, unknown> : undefined;
  } catch {
    return undefined;
  }
}

function truncateText(value: string, maxLength: number): string {
  if (value.length <= maxLength) {
    return value;
  }

  return `${value.slice(0, Math.max(0, maxLength - 3))}...`;
}

function isStopReason(value: string): value is StopReason {
  return [
    "EmergencyStop",
    "EVDisconnected",
    "HardReset",
    "Local",
    "Other",
    "PowerLoss",
    "Reboot",
    "Remote",
    "SoftReset",
    "UnlockCommand",
    "DeAuthorized"
  ].includes(value);
}

function readChargePointErrorCode(value: string): ChargePointErrorCode {
  const normalized = value.trim();
  return isChargePointErrorCode(normalized) && normalized !== "NoError" ? normalized : "OtherError";
}

function isChargePointErrorCode(value: string): value is ChargePointErrorCode {
  return [
    "ConnectorLockFailure",
    "EVCommunicationError",
    "GroundFailure",
    "HighTemperature",
    "InternalError",
    "LocalListConflict",
    "NoError",
    "OtherError",
    "OverCurrentFailure",
    "PowerMeterFailure",
    "PowerSwitchFailure",
    "ReaderFailure",
    "ResetFailure",
    "UnderVoltage",
    "OverVoltage",
    "WeakSignal"
  ].includes(value);
}
