import { spawnSync } from "node:child_process";
import blessed from "blessed";
import type { Widgets } from "blessed";
import { Station } from "./station.js";
import type { ChargePointErrorCode, ConnectorState, LogEntry, StationState, StopReason } from "./ocpp/types.js";

type TabId = "station" | "connector" | "transaction" | "maintenance" | "data" | "logs" | "scenarios";
type LogFilter = "all" | "calls" | "errors" | "station" | "csms";

interface ActionItem {
  key: string;
  label: string;
  description: string;
  run: () => void;
  disabled?: () => boolean;
}

interface PaletteEntry {
  tab: TabId;
  action: ActionItem;
  actionIndex: number;
}

interface FormField {
  name: string;
  label: string;
  initialValue: string;
}

const TABS: { id: TabId; label: string }[] = [
  { id: "station", label: "Station" },
  { id: "connector", label: "Connector" },
  { id: "transaction", label: "Transaction" },
  { id: "maintenance", label: "Maintenance" },
  { id: "data", label: "Data" },
  { id: "logs", label: "Logs" },
  { id: "scenarios", label: "Scenarios" }
];

const LEVEL_STYLE: Record<LogEntry["level"], string> = {
  info: "{blue-fg}info{/blue-fg}",
  success: "{green-fg}ok{/green-fg}",
  warn: "{yellow-fg}warn{/yellow-fg}",
  error: "{red-fg}err{/red-fg}",
  in: "{magenta-fg}in{/magenta-fg}",
  out: "{cyan-fg}out{/cyan-fg}"
};

const DEFAULT_MEASURANDS = "Energy.Active.Import.Register,Power.Active.Import,Current.Import,Voltage,Temperature,SoC";
const THEME = {
  panelBorder: "cyan",
  headerBg: "black",
  headerFg: "white",
  footerBg: "black",
  footerFg: "white",
  selectedBg: "white",
  selectedFg: "black"
} as const;

export class Tui {
  private readonly screen: Widgets.Screen;
  private readonly header: Widgets.BoxElement;
  private readonly connectorList: Widgets.ListElement;
  private readonly detailBox: Widgets.BoxElement;
  private readonly actionList: Widgets.ListElement;
  private readonly logBox: Widgets.Log;
  private readonly footer: Widgets.BoxElement;
  private readonly logs: LogEntry[] = [];
  private activeTab: TabId = "station";
  private logFilter: LogFilter = "all";
  private actions: ActionItem[] = [];
  private modal?: Widgets.BoxElement;
  private plainLogView?: Widgets.BoxElement;
  private selectedConnectorId: number;

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
      style: { fg: THEME.headerFg, bg: THEME.headerBg }
    });

    this.connectorList = blessed.list({
      label: " Connectors ",
      top: 3,
      left: 0,
      width: "25%",
      bottom: 3,
      keys: true,
      vi: false,
      tags: true,
      border: "line",
      padding: { left: 1, right: 1 },
      style: {
        selected: { bg: THEME.selectedBg, fg: THEME.selectedFg },
        border: { fg: THEME.panelBorder }
      }
    });

    this.detailBox = blessed.box({
      label: " Details ",
      top: 3,
      left: "25%",
      width: "37%",
      height: "48%",
      tags: true,
      border: "line",
      padding: { left: 1, right: 1 },
      style: {
        border: { fg: THEME.panelBorder }
      }
    });

    this.actionList = blessed.list({
      label: " Actions ",
      top: "51%",
      left: "25%",
      width: "37%",
      bottom: 3,
      keys: true,
      vi: false,
      tags: true,
      border: "line",
      padding: { left: 1, right: 1 },
      style: {
        selected: { bg: THEME.selectedBg, fg: THEME.selectedFg },
        border: { fg: THEME.panelBorder }
      }
    });

    this.logBox = blessed.log({
      label: " OCPP log [all] ",
      top: 3,
      left: "62%",
      width: "38%",
      bottom: 3,
      tags: true,
      keys: true,
      vi: true,
      mouse: false,
      scrollable: true,
      alwaysScroll: true,
      border: "line",
      padding: { left: 1, right: 1 },
      scrollbar: {
        ch: " ",
        style: { bg: THEME.panelBorder }
      },
      style: {
        border: { fg: THEME.panelBorder }
      }
    });

    this.footer = blessed.box({
      bottom: 0,
      left: 0,
      width: "100%",
      height: 3,
      tags: true,
      style: { fg: THEME.footerFg, bg: THEME.footerBg }
    });

    this.screen.append(this.header);
    this.screen.append(this.connectorList);
    this.screen.append(this.detailBox);
    this.screen.append(this.actionList);
    this.screen.append(this.logBox);
    this.screen.append(this.footer);

    this.layout();
    this.bindKeys();
    this.bindStation();
    this.render();
    this.actionList.focus();
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
    this.screen.key("escape", () => {
      if (this.closeModal()) {
        return;
      }
      this.closePlainLogView();
    });
    this.screen.key("q", () => {
      if (this.modal) {
        return;
      }
      if (this.closePlainLogView()) {
        return;
      }

      this.stopAndExit();
    });
    this.screen.key("C-c", () => {
      if (this.closeModal() || this.closePlainLogView()) {
        return;
      }

      this.stopAndExit();
    });
    this.screen.key(["tab"], () => this.runWithoutOverlay(() => this.nextTab()));
    this.screen.key(["S-tab"], () => this.runWithoutOverlay(() => this.previousTab()));
    this.screen.key(["1"], () => this.runWithoutOverlay(() => this.setTab("station")));
    this.screen.key(["2"], () => this.runWithoutOverlay(() => this.setTab("connector")));
    this.screen.key(["3"], () => this.runWithoutOverlay(() => this.setTab("transaction")));
    this.screen.key(["4"], () => this.runWithoutOverlay(() => this.setTab("maintenance")));
    this.screen.key(["5"], () => this.runWithoutOverlay(() => this.setTab("data")));
    this.screen.key(["6"], () => this.runWithoutOverlay(() => this.setTab("logs")));
    this.screen.key(["7"], () => this.runWithoutOverlay(() => this.setTab("scenarios")));
    this.screen.key(["enter"], () => this.runWithoutOverlay(() => this.runSelectedAction()));
    this.screen.key(["[", "left"], () => this.runWithoutOverlay(() => this.selectPreviousConnector()));
    this.screen.key(["]", "right"], () => this.runWithoutOverlay(() => this.selectNextConnector()));
    this.screen.key("+", () => this.runWithoutOverlay(() => this.addConnector()));
    this.screen.key(["j"], () => this.runWithoutModal(() => this.scrollLog(1)));
    this.screen.key(["k"], () => this.runWithoutModal(() => this.scrollLog(-1)));
    this.screen.key(["g", "G", "S-g", "end"], () => this.runWithoutModal(() => this.scrollLogToEnd()));
    this.screen.key("?", () => this.runWithoutOverlay(() => this.openHelp()));
    this.screen.key("l", () => this.runWithoutOverlay(() => this.openPlainLogView()));
    this.screen.key("y", () => this.runWithoutModal(() => this.copyLogsToClipboard()));
    this.screen.key(["F", "S-f"], () => this.runWithoutOverlay(() => this.cycleLogFilter()));
    this.screen.key("r", () => this.runWithoutOverlay(() => this.station.reconnect()));
    this.bindActionAccelerators();
    this.screen.key("/", () => this.runWithoutOverlay(() => this.openCommandPalette()));
    this.actionList.on("select item", () => {
      this.renderFooter();
      this.screen.render();
    });
    this.screen.on("resize", () => {
      this.layout();
      this.render();
    });
    this.bindLogNavigationKeys(this.logBox);
  }

  private bindStation(): void {
    this.station.on("state", () => this.render());
    this.station.on("log", (entry) => this.renderLog(entry));
  }

  private layout(): void {
    const columns = this.screenColumns();
    const rows = this.screenRows();
    const compact = columns < 110;

    if (compact) {
      const upperHeight = Math.max(7, Math.floor((rows - 6) * 0.42));
      const actionHeight = Math.max(5, Math.floor((rows - 6) * 0.28));
      const actionTop = 3 + upperHeight;
      const logTop = actionTop + actionHeight;

      this.connectorList.top = 3;
      this.connectorList.left = 0;
      this.connectorList.width = "34%";
      this.connectorList.height = upperHeight;
      deleteRuntimeProperty(this.connectorList, "bottom");
      this.detailBox.top = 3;
      this.detailBox.left = "34%";
      this.detailBox.width = "66%";
      this.detailBox.height = upperHeight;
      deleteRuntimeProperty(this.detailBox, "bottom");
      this.actionList.top = actionTop;
      this.actionList.left = 0;
      this.actionList.width = "100%";
      this.actionList.height = actionHeight;
      deleteRuntimeProperty(this.actionList, "bottom");
      this.logBox.left = 0;
      this.logBox.width = "100%";
      this.logBox.top = logTop;
      this.logBox.bottom = 3;
      deleteRuntimeProperty(this.logBox, "height");
      return;
    }

    this.connectorList.top = 3;
    this.connectorList.left = 0;
    this.connectorList.width = "25%";
    deleteRuntimeProperty(this.connectorList, "height");
    this.connectorList.bottom = 3;
    this.detailBox.top = 3;
    this.detailBox.left = "25%";
    this.detailBox.width = "37%";
    this.detailBox.height = "48%";
    deleteRuntimeProperty(this.detailBox, "bottom");
    this.actionList.left = "25%";
    this.actionList.width = "37%";
    this.actionList.top = "51%";
    deleteRuntimeProperty(this.actionList, "height");
    this.actionList.bottom = 3;
    this.logBox.left = "62%";
    this.logBox.width = "38%";
    this.logBox.top = 3;
    deleteRuntimeProperty(this.logBox, "height");
    this.logBox.bottom = 3;
  }

  private render(): void {
    this.ensureSelectedConnector();
    this.renderHeader();
    this.renderConnectors();
    this.renderDetails();
    this.renderActions();
    this.renderFooter();
    this.screen.render();
  }

  private renderHeader(): void {
    const state = this.station.state;
    const connection = state.connected ? "{green-fg}connected{/green-fg}" : "{red-fg}offline{/red-fg}";
    const boot = state.booted ? "{green-fg}accepted{/green-fg}" : `{yellow-fg}${state.registrationStatus}{/yellow-fg}`;
    const compact = this.screenColumns() < 110;
    const tabLine = TABS.map((tab, index) => {
      const label = compact ? `${index + 1}:${tab.label.slice(0, 4)}` : `${index + 1}:${tab.label}`;
      return tab.id === this.activeTab ? `{black-fg}{white-bg}${label}{/white-bg}{/black-fg}` : `{gray-fg}${label}{/gray-fg}`;
    }).join("  ");

    this.header.setContent(
      [
        ` {bold}kirby-ocpp{/bold}  ${this.station.config.chargePointId} -> ${this.station.config.centralSystemUrl}`,
        ` ${connection}  boot=${boot}  selected=#${this.selectedConnectorId}  ${tabLine}`
      ].join("\n")
    );
  }

  private renderConnectors(): void {
    const rows = this.connectorContentRows();
    const contentWidth = Math.max(20, this.connectorContentWidth());
    const items = this.station.state.connectors.map((connector) =>
      truncateText(this.connectorLine(connector, contentWidth), contentWidth)
    );
    const selectedIndex = Math.max(0, this.station.state.connectors.findIndex((connector) => connector.id === this.selectedConnectorId));
    const hasHeader = contentWidth >= 36 && rows > 1;
    const visibleRows = Math.max(hasHeader ? rows - 1 : rows, 1);
    const start = clamp(selectedIndex - visibleRows + 1, 0, Math.max(0, items.length - visibleRows));
    const visibleItems = items.slice(start, start + visibleRows);
    this.connectorList.setLabel(" Connectors ");
    this.connectorList.setItems(hasHeader ? [this.connectorHeaderLine(contentWidth), ...visibleItems] : visibleItems);
    const maxSelectedIndex = Math.max(0, visibleItems.length - 1 + (hasHeader ? 1 : 0));
    this.connectorList.select(clamp(selectedIndex - start + (hasHeader ? 1 : 0), 0, maxSelectedIndex));
  }

  private connectorHeaderLine(width: number): string {
    return truncateText(["id ".padEnd(3), "status".padEnd(8), "ev", "tx".padEnd(6), "meter".padStart(7)].join(" | "), width);
  }

  private connectorLine(connector: ConnectorState, width: number): string {
    const status = connector.status === "Faulted" ? shortErrorCode(connector.errorCode) : shortConnectorStatus(connector.status);
    const ev = connector.evConnected ? "EV" : "--";
    const tx = connector.transactionId ? String(connector.transactionId) : "-";
    const meter = `${Math.round(connector.meterWh)}Wh`;
    const flags = [
      connector.pendingStartTransaction || connector.pendingStopTransaction ? "sync" : "",
      connector.reservationId ? `res:${connector.reservationId}` : ""
    ].filter(Boolean);

    if (width < 31) {
      return [`#${connector.id}`, status, ev, meter, tx !== "-" ? `tx:${tx}` : "", ...flags].filter(Boolean).join(" ");
    }

    const base = [
      `#${String(connector.id).padEnd(2)}`,
      truncateText(status, 8).padEnd(8),
      ev.padEnd(2),
      truncateText(tx, 6).padEnd(6),
      meter.padStart(7)
    ].join(" | ");
    return flags.length > 0 ? `${base} ${flags.join(" ")}` : base;
  }

  private renderDetails(): void {
    const state = this.station.state;
    const connector = this.selectedConnector(state);
    const lastHeartbeat = state.lastHeartbeatAt ? state.lastHeartbeatAt.toLocaleTimeString() : "-";
    const contentWidth = this.centerContentWidth();
    const lines = [
      "{bold}Station{/bold}",
      row("Connection", state.connected ? "{green-fg}{bold}connected{/bold}{/green-fg}" : "{red-fg}{bold}offline{/bold}{/red-fg}"),
      row("Boot", state.booted ? "{green-fg}accepted{/green-fg}" : state.registrationStatus),
      row("Local list", String(state.localListVersion)),
      row("Heartbeat", lastHeartbeat),
      row("Diag/FW", `${state.diagnosticsStatus ?? "-"}/${state.firmwareStatus ?? "-"}`),
      "",
      `{bold}Connector #${connector?.id ?? "-"}{/bold}`,
      row("Status", connector?.status ?? "-"),
      row("Availability", connector?.availability ?? "-"),
      row("EV", connector?.evConnected ? "connected" : "-"),
      row("Transaction", connector?.transactionId ? String(connector.transactionId) : "-"),
      row("Pending sync", connector?.pendingStartTransaction || connector?.pendingStopTransaction ? "yes" : "-"),
      row("Reservation", connector?.reservationId ? String(connector.reservationId) : "-"),
      row("Reserve idTag", connector?.reservationIdTag ?? "-"),
      row("Meter", `${Math.round(connector?.meterWh ?? 0)} Wh`),
      row("Error", connector?.errorCode ?? "-")
    ];

    this.detailBox.setLabel(` ${this.tabLabel(this.activeTab)} / Details `);
    this.detailBox.setContent(lines.map((line) => truncateTaggedLine(line, contentWidth)).join("\n"));
  }

  private renderActions(): void {
    this.actions = this.actionsForTab();
    const width = this.centerContentWidth();
    const items = this.actions.map((action) => {
      const prefix = action.disabled?.() ? "{gray-fg}" : "";
      const suffix = action.disabled?.() ? "{/gray-fg}" : "";
      return `${prefix}${action.key.padEnd(2)} ${truncateText(action.label, Math.max(12, width - 6))}${suffix}`;
    });

    this.actionList.setLabel(` ${this.tabLabel(this.activeTab)} actions `);
    this.actionList.setItems(items);
    const selected = this.selectedActionIndex();
    this.actionList.select(Math.min(selected, Math.max(0, items.length - 1)));

    const action = this.actions[this.selectedActionIndex()];
    const description = action ? action.description : "Choose an action and press Enter.";
    this.footer.setContent(this.footerContent(description));
  }

  private renderFooter(): void {
    const action = this.actions[this.selectedActionIndex()];
    this.footer.setContent(this.footerContent(action?.description ?? "Choose an action and press Enter."));
  }

  private footerContent(description: string): string {
    return [
      ` ${truncateText(description, Math.max(30, this.screenColumns() - 2))}`,
      " Enter run  / commands  Up/Down action  1-7 tabs  [/] connector  + add  j/k log  F filter  l plain  y copy  q quit"
    ].join("\n");
  }

  private actionsForTab(tab: TabId = this.activeTab): ActionItem[] {
    switch (tab) {
      case "station":
        return [
          action("b", "BootNotification", "Register/re-register this charge point.", () => this.runStationTask(() => this.station.boot())),
          action("h", "Heartbeat", "Send Heartbeat and update Central System time.", () => this.runStationTask(() => this.station.heartbeat())),
          action("r", "Reconnect", "Close and reopen the WebSocket connection.", () => this.station.reconnect()),
          action("x", "Disconnect", "Disconnect without exiting the emulator.", () => this.station.disconnect(), () => !this.station.state.connected),
          action("c", "Connect", "Connect after a manual disconnect.", () => this.station.connect(), () => this.station.state.connected),
          action("+", "Add connector", "Add a connector to this station.", () => this.addConnector())
        ];
      case "connector":
        return [
          action("p", "Plug / unplug EV", "Toggle EV plug state on the selected connector.", () => this.togglePlug()),
          action("s", "Cycle status", "Cycle StatusNotification status on the selected connector.", () =>
            this.runStationTask(() => this.station.cycleStatus(this.selectedConnectorId))
          ),
          action("n", "Send StatusNotification", "Send current selected connector status now.", () =>
            this.runStationTask(() => this.station.statusNotification(this.selectedConnectorId, this.selectedConnector()?.status, true))
          ),
          action("f", "Fault / clear fault", "Inject or clear a fault on the selected connector.", () => this.faultFlow()),
          action("]", "Next connector", "Select next connector.", () => this.selectNextConnector()),
          action("[", "Previous connector", "Select previous connector.", () => this.selectPreviousConnector())
        ];
      case "transaction":
        return [
          action("a", "Authorize", "Prompt for idTag and send Authorize.", () =>
            this.promptText("Authorize idTag", "", (idTag) => this.runStationTask(() => this.station.authorize(idTag)))
          ),
          action("t", "Start transaction", "Prompt for connector and idTag, then start a transaction.", () => this.startTransactionFlow()),
          action("e", "Stop transaction", "Choose StopTransaction reason and stop selected connector.", () => this.stopTransactionFlow(), () =>
            !this.selectedConnector()?.transactionId
          ),
          action("m", "MeterValues +120 Wh", "Send MeterValues for selected connector.", () =>
            this.runStationTask(() => this.station.meterValues(this.selectedConnectorId))
          ),
          action("M", "MeterValues custom", "Edit connector, Wh delta, and measurands.", () => this.customMeterValuesFlow())
        ];
      case "maintenance":
        return [
          action("d", "Diagnostics outcome", "Set simulated diagnostics upload outcome.", () =>
            this.promptChoice("Diagnostics outcome", ["success", "uploadFailure"], (value) => {
              this.station.setDiagnosticsOutcome(readDiagnosticsOutcome(value));
              this.render();
            })
          ),
          action("u", "Firmware outcome", "Set simulated firmware download/install outcome.", () =>
            this.promptChoice("Firmware outcome", ["success", "downloadFailure", "installationFailure"], (value) => {
              this.station.setFirmwareOutcome(readFirmwareOutcome(value));
              this.render();
            })
          ),
          action("D", "DiagnosticsStatusNotification", "Send DiagnosticsStatusNotification with selected status.", () =>
            this.promptChoice("Diagnostics status", ["Idle", "Uploading", "Uploaded", "UploadFailed"], (value) =>
              this.runStationTask(() => this.station.diagnosticsStatusNotification(readDiagnosticsStatus(value)))
            )
          ),
          action("U", "FirmwareStatusNotification", "Send FirmwareStatusNotification with selected status.", () =>
            this.promptChoice(
              "Firmware status",
              ["Idle", "Downloading", "Downloaded", "Installing", "Installed", "DownloadFailed", "InstallationFailed"],
              (value) => this.runStationTask(() => this.station.firmwareStatusNotification(readFirmwareStatus(value)))
            )
          )
        ];
      case "data":
        return [
          action("d", "DataTransfer form", "Edit vendorId, messageId, and data.", () => this.dataTransferFlow()),
          action("v", "Vendor ping", "Send a simple vendor DataTransfer ping.", () =>
            this.runStationTask(() => this.station.dataTransfer({ vendorId: this.station.config.vendor, data: "ping" }))
          )
        ];
      case "logs":
        return [
          action("l", "Open plain log", "Open full-screen plain log view for clean selection.", () => this.openPlainLogView()),
          action("y", "Copy logs", "Copy current log buffer to clipboard.", () => this.copyLogsToClipboard()),
          action("F", `Filter: ${this.logFilter}`, "Cycle log filter: all, calls, errors, station, CSMS-in.", () => this.cycleLogFilter()),
          action("G", "Jump to bottom", "Scroll log view to the latest entry.", () => this.scrollLogToEnd()),
          action("c", "Clear log buffer", "Clear in-memory TUI log buffer.", () => this.clearLogs())
        ];
      case "scenarios":
        return [
          action("o", "Charge session", "Plug EV, Authorize, StartTransaction, and send MeterValues.", () => this.chargeSessionScenario(), () =>
            Boolean(this.selectedConnector()?.transactionId)
          ),
          action("w", "Finish session", "Send MeterValues, StopTransaction, and unplug EV.", () => this.finishSessionScenario(), () =>
            !this.selectedConnector()?.transactionId
          ),
          action("O", "Offline start/stop sync", "Disconnect, run local start/stop, then reconnect for sync.", () => this.offlineSyncScenario()),
          action("i", "Fault recovery", "Inject a fault on the selected connector, then clear it.", () => this.faultRecoveryScenario())
        ];
    }
  }

  private runSelectedAction(): void {
    if (this.modal || this.plainLogView) {
      return;
    }

    const action = this.actions[this.selectedActionIndex()];
    if (!action) {
      return;
    }
    this.runAction(action);
  }

  private bindActionAccelerators(): void {
    this.screen.key(
      [
        "a",
        "b",
        "c",
        "d",
        "D",
        "S-d",
        "e",
        "f",
        "h",
        "i",
        "m",
        "M",
        "S-m",
        "n",
        "o",
        "O",
        "S-o",
        "p",
        "s",
        "t",
        "u",
        "U",
        "S-u",
        "v",
        "w",
        "x"
      ],
      (_ch, key) => this.runActionAccelerator(key.full ?? key.name)
    );
  }

  private runActionAccelerator(keyName: string): void {
    if (this.modal || this.plainLogView) {
      return;
    }

    const key = normalizeActionKey(keyName);
    const currentAction = this.actions.find((item) => item.key === key);
    if (currentAction) {
      this.runAction(currentAction);
      return;
    }

    const tab = legacyActionTab(key);
    if (!tab) {
      return;
    }

    const action = this.actionsForTab(tab).find((item) => item.key === key);
    if (!action) {
      return;
    }

    this.setTab(tab);
    const index = this.actions.findIndex((item) => item.key === key);
    if (index >= 0) {
      this.actionList.select(index);
    }
    this.runAction(action);
  }

  private runAction(action: ActionItem): void {
    if (action.disabled?.()) {
      this.renderSyntheticLog("warn", `${action.label} is not available right now`);
      return;
    }

    action.run();
  }

  private runWithoutOverlay(callback: () => void): void {
    if (this.modal || this.plainLogView) {
      return;
    }

    callback();
  }

  private runWithoutModal(callback: () => void): void {
    if (this.modal) {
      return;
    }

    callback();
  }

  private openCommandPalette(): void {
    if (this.modal || this.plainLogView) {
      return;
    }

    const width = Math.min(Math.max(54, this.screenColumns() - 10), 96);
    const height = Math.min(18, Math.max(10, this.screenRows() - 6));
    const box = blessed.box({
      top: "center",
      left: "center",
      width,
      height,
      border: "line",
      label: " Commands ",
      tags: true,
      keys: true,
      style: {
        border: { fg: THEME.panelBorder }
      }
    });
    const input = blessed.box({
      parent: box,
      top: 1,
      left: 2,
      right: 2,
      height: 3,
      border: "line",
      keys: true,
      tags: true,
      style: {
        border: { fg: THEME.panelBorder }
      }
    });
    const list = blessed.list({
      parent: box,
      top: 4,
      left: 2,
      right: 2,
      bottom: 2,
      border: "line",
      tags: true,
      keys: true,
      vi: false,
      items: [],
      style: {
        selected: { bg: THEME.selectedBg, fg: THEME.selectedFg },
        border: { fg: THEME.panelBorder }
      }
    });
    const help = blessed.box({
      parent: box,
      bottom: 0,
      left: 2,
      right: 2,
      height: 1,
      tags: true,
      content: "{gray-fg}Type to filter  Up/Down select  Enter run  Esc cancel{/gray-fg}"
    });

    let entries: PaletteEntry[] = [];
    let selectedIndex = 0;
    let closed = false;
    let query = "";

    const move = (offset: number) => {
      if (entries.length === 0) {
        return;
      }

      selectedIndex = clamp(selectedIndex + offset, 0, entries.length - 1);
      list.select(selectedIndex);
      this.screen.render();
    };
    const renderSearch = () => {
      input.setContent(`${truncateText(query, Math.max(1, inputContentWidth(input) - 1))}{inverse} {/inverse}`);
    };
    const updateResults = () => {
      if (closed) {
        return;
      }

      const trimmedQuery = query.trim();
      renderSearch();
      entries = this.paletteEntries(trimmedQuery);
      selectedIndex = clamp(selectedIndex, 0, Math.max(0, entries.length - 1));
      box.setLabel(` Commands${trimmedQuery ? `: ${truncateText(trimmedQuery, 28)}` : ""} `);
      if (entries.length === 0) {
        list.setItems(["{gray-fg}No matching commands{/gray-fg}"]);
        list.select(0);
        this.screen.render();
        return;
      }

      list.setItems(entries.map((entry) => this.paletteEntryLine(entry)));
      list.select(selectedIndex);
      this.screen.render();
    };
    const close = (entry?: PaletteEntry) => {
      closed = true;
      box.destroy();
      this.modal = undefined;
      this.actionList.focus();
      this.screen.render();
      if (!entry) {
        return;
      }

      this.setTab(entry.tab);
      this.actionList.select(entry.actionIndex);
      this.runAction(entry.action);
    };
    const handleSearchKey = (ch: string | undefined, key: { name?: string; ctrl?: boolean; meta?: boolean }) => {
      if (closed) {
        return;
      }

      if (key.name === "enter") {
        close(entries[selectedIndex]);
        return;
      }
      if (key.name === "escape" || (key.ctrl && key.name === "c")) {
        close();
        return;
      }
      if (key.name === "up") {
        move(-1);
        return;
      }
      if (key.name === "down" || key.name === "tab") {
        move(1);
        return;
      }
      if (key.name === "backspace") {
        query = query.slice(0, -1);
        selectedIndex = 0;
        updateResults();
        return;
      }
      if (key.name === "delete") {
        query = "";
        selectedIndex = 0;
        updateResults();
        return;
      }
      if (ch && !key.ctrl && !key.meta && isPrintableInput(ch)) {
        query += ch;
        selectedIndex = 0;
        updateResults();
      }
    };

    this.modal = box;
    input.on("keypress", handleSearchKey);
    box.key(["up"], () => move(-1));
    box.key(["down"], () => move(1));
    box.key(["escape", "C-c"], () => close());
    box.key("enter", () => close(entries[selectedIndex]));
    box.append(help);
    this.screen.append(box);
    box.setFront();
    input.focus();
    updateResults();
    this.screen.render();
  }

  private openHelp(): void {
    const width = Math.min(92, Math.max(58, this.screenColumns() - 8));
    const height = Math.min(24, Math.max(16, this.screenRows() - 4));
    const box = blessed.box({
      top: "center",
      left: "center",
      width,
      height,
      border: "line",
      label: " Help ",
      tags: true,
      keys: true,
      scrollable: true,
      alwaysScroll: false,
      padding: { left: 2, right: 2 },
      style: {
        border: { fg: THEME.panelBorder }
      },
      scrollbar: {
        ch: " ",
        style: { bg: THEME.panelBorder }
      }
    });

    box.setContent(
      [
        "{bold}Navigation{/bold}",
        "  1-7 tabs     Tab/S-Tab tab cycle     Up/Down action     Enter run",
        "  [/] connector     + add connector     / command search     ? help",
        "",
        "{bold}Logs{/bold}",
        "  j/k scroll     G bottom     F filter     l plain log     y copy",
        "",
        "{bold}Forms{/bold}",
        "  Tab/Up/Down field     Left/Right cursor     Home/End jump",
        "  Backspace/Delete edit     Ctrl+U clear     Enter submit     Esc cancel",
        "",
        "{bold}Station{/bold}",
        `  id=${this.station.config.chargePointId}`,
        `  url=${this.station.config.centralSystemUrl}`,
        "",
        "{gray-fg}Esc or q closes this help.{/gray-fg}"
      ].join("\n")
    );

    const close = () => {
      box.destroy();
      this.modal = undefined;
      this.actionList.focus();
      this.screen.render();
    };

    this.modal = box;
    box.key(["escape", "q", "C-c"], close);
    this.screen.append(box);
    box.setFront();
    box.focus();
    this.screen.render();
  }

  private paletteEntryLine(entry: PaletteEntry): string {
    const tab = this.tabLabel(entry.tab).padEnd(11);
    const key = entry.action.key.padEnd(2);
    const state = entry.action.disabled?.() ? "{gray-fg}" : "";
    const endState = entry.action.disabled?.() ? "{/gray-fg}" : "";
    return `${state}${tab} ${key} ${entry.action.label}${endState}`;
  }

  private paletteEntries(query: string): PaletteEntry[] {
    const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    const entries = TABS.flatMap((tab) =>
      this.actionsForTab(tab.id).map((action, actionIndex) => ({ tab: tab.id, action, actionIndex }))
    );

    if (terms.length === 0) {
      return entries;
    }

    return entries
      .map((entry, originalIndex) => ({
        entry,
        originalIndex,
        score: this.paletteEntryScore(entry, terms)
      }))
      .filter((item) => item.score < Number.POSITIVE_INFINITY)
      .sort((left, right) => left.score - right.score || left.originalIndex - right.originalIndex)
      .map((item) => item.entry);
  }

  private paletteEntryScore(entry: PaletteEntry, terms: string[]): number {
    let score = 0;
    for (const term of terms) {
      const termScore = Math.min(
        weightedMatchScore(entry.action.label, term, 0),
        weightedMatchScore(entry.action.key, term, 20),
        weightedMatchScore(entry.action.description, term, 80),
        weightedMatchScore(this.tabLabel(entry.tab), term, 160)
      );
      if (termScore === Number.POSITIVE_INFINITY) {
        return Number.POSITIVE_INFINITY;
      }

      score += termScore;
    }

    return score + (entry.action.disabled?.() ? 30 : 0);
  }

  private selectedActionIndex(): number {
    const selected = (this.actionList as Widgets.ListElement & { selected?: number }).selected ?? 0;
    return clamp(selected, 0, Math.max(0, this.actions.length - 1));
  }

  private setTab(tab: TabId): void {
    this.activeTab = tab;
    this.actionList.select(0);
    this.render();
  }

  private nextTab(): void {
    const index = TABS.findIndex((tab) => tab.id === this.activeTab);
    this.setTab(TABS[(index + 1) % TABS.length]?.id ?? "station");
  }

  private previousTab(): void {
    const index = TABS.findIndex((tab) => tab.id === this.activeTab);
    this.setTab(TABS[(index - 1 + TABS.length) % TABS.length]?.id ?? "station");
  }

  private tabLabel(tab: TabId): string {
    return TABS.find((item) => item.id === tab)?.label ?? "Station";
  }

  private togglePlug(): void {
    const connector = this.selectedConnector();
    this.runStationTask(() =>
      connector?.evConnected || connector?.status === "Finishing"
        ? this.station.unplug(this.selectedConnectorId)
        : this.station.plugIn(this.selectedConnectorId)
    );
  }

  private stopTransactionFlow(): void {
    this.promptChoice("StopTransaction reason", STOP_REASONS, (reason) =>
      this.runStationTask(() => this.station.stopTransaction(this.selectedConnectorId, readStopReason(reason), this.selectedConnector()?.lastIdTag))
    );
  }

  private startTransactionFlow(): void {
    this.promptForm(
      "StartTransaction",
      [
        { name: "connectorId", label: "connectorId", initialValue: String(this.selectedConnectorId) },
        { name: "idTag", label: "idTag", initialValue: "" }
      ],
      (values) => {
        const connectorId = this.readFormConnectorId(values.connectorId);
        const idTag = values.idTag.trim();
        if (connectorId === undefined || !idTag) {
          this.renderSyntheticLog("error", "StartTransaction requires valid connectorId and idTag");
          return;
        }

        this.runStationTask(() => this.station.startTransaction(connectorId, idTag));
      }
    );
  }

  private customMeterValuesFlow(): void {
    this.promptForm(
      "MeterValues",
      [
        { name: "connectorId", label: "connectorId", initialValue: String(this.selectedConnectorId) },
        { name: "deltaWh", label: "delta Wh", initialValue: "120" },
        { name: "measurands", label: "measurands", initialValue: DEFAULT_MEASURANDS }
      ],
      (values) => {
        const connectorId = this.readFormConnectorId(values.connectorId);
        const deltaWh = Number.parseFloat(values.deltaWh);
        if (connectorId === undefined || !Number.isFinite(deltaWh)) {
          this.renderSyntheticLog("error", "MeterValues requires valid connectorId and numeric deltaWh");
          return;
        }

        this.runStationTask(() => this.station.meterValues(connectorId, deltaWh, values.measurands.trim()));
      }
    );
  }

  private dataTransferFlow(): void {
    this.promptForm(
      "DataTransfer",
      [
        { name: "vendorId", label: "vendorId", initialValue: this.station.config.vendor },
        { name: "messageId", label: "messageId", initialValue: "" },
        { name: "data", label: "data JSON/string", initialValue: JSON.stringify({ ping: true }) }
      ],
      (values) => {
        const vendorId = values.vendorId.trim();
        if (!vendorId) {
          this.renderSyntheticLog("error", "DataTransfer requires vendorId");
          return;
        }

        const payload: Record<string, unknown> = { vendorId };
        const messageId = values.messageId.trim();
        const data = values.data.trim();
        if (messageId) {
          payload.messageId = messageId;
        }
        if (data) {
          payload.data = parseJsonOrString(data);
        }

        this.runStationTask(() => this.station.dataTransfer(payload));
      }
    );
  }

  private readFormConnectorId(value: string): number | undefined {
    const connectorId = Number.parseInt(value.trim(), 10);
    if (!Number.isInteger(connectorId) || !this.station.state.connectors.some((connector) => connector.id === connectorId)) {
      this.renderSyntheticLog("error", `Unknown connectorId: ${value}`);
      return undefined;
    }

    return connectorId;
  }

  private faultFlow(): void {
    const connector = this.selectedConnector();
    if (connector?.status === "Faulted") {
      this.runStationTask(() => this.station.clearFault(this.selectedConnectorId));
      return;
    }

    this.promptChoice("Fault errorCode", CHARGE_POINT_ERROR_CODES.filter((code) => code !== "NoError"), (errorCode) =>
      this.runStationTask(() => this.station.setFault(this.selectedConnectorId, readChargePointErrorCode(errorCode)))
    );
  }

  private chargeSessionScenario(): void {
    const connectorId = this.selectedConnectorId;
    this.promptText(`Charge session idTag (#${connectorId})`, "", (idTag) =>
      this.runStationTask(async () => {
        this.renderSyntheticLog("info", `Scenario charge session started on connector ${connectorId}`);
        await this.station.plugIn(connectorId);
        await this.station.authorize(idTag);
        const transactionId = await this.station.startTransaction(connectorId, idTag);
        if (transactionId) {
          await this.station.meterValues(connectorId);
        }
      })
    );
  }

  private finishSessionScenario(): void {
    const connectorId = this.selectedConnectorId;
    const idTag = this.selectedConnector()?.lastIdTag;
    this.promptChoice("Finish reason", STOP_REASONS, (reason) =>
      this.runStationTask(async () => {
        this.renderSyntheticLog("info", `Scenario finish session started on connector ${connectorId}`);
        await this.station.meterValues(connectorId);
        await this.station.stopTransaction(connectorId, readStopReason(reason), idTag);
        await this.station.unplug(connectorId);
      })
    );
  }

  private offlineSyncScenario(): void {
    const connectorId = this.selectedConnectorId;
    this.promptText(`Offline sync idTag (#${connectorId})`, "", (idTag) =>
      this.runStationTask(async () => {
        this.renderSyntheticLog("info", `Scenario offline sync started on connector ${connectorId}`);
        this.station.disconnect();
        await this.station.plugIn(connectorId);
        const transactionId = await this.station.startTransaction(connectorId, idTag);
        if (transactionId) {
          await this.station.meterValues(connectorId);
          await this.station.stopTransaction(connectorId, "Local", idTag);
        }
        this.station.reconnect();
      })
    );
  }

  private faultRecoveryScenario(): void {
    const connectorId = this.selectedConnectorId;
    this.promptChoice("Fault recovery errorCode", CHARGE_POINT_ERROR_CODES.filter((code) => code !== "NoError"), (errorCode) =>
      this.runStationTask(async () => {
        this.renderSyntheticLog("info", `Scenario fault recovery started on connector ${connectorId}`);
        await this.station.setFault(connectorId, readChargePointErrorCode(errorCode));
        await this.station.clearFault(connectorId);
      })
    );
  }

  private addConnector(): void {
    this.selectedConnectorId = this.station.addConnector();
    this.render();
  }

  private selectPreviousConnector(): void {
    const ids = this.station.state.connectors.map((connector) => connector.id);
    const currentIndex = ids.indexOf(this.selectedConnectorId);
    this.selectedConnectorId = ids[Math.max(0, currentIndex - 1)] ?? this.selectedConnectorId;
    this.render();
  }

  private selectNextConnector(): void {
    const ids = this.station.state.connectors.map((connector) => connector.id);
    const currentIndex = ids.indexOf(this.selectedConnectorId);
    this.selectedConnectorId = ids[Math.min(ids.length - 1, currentIndex + 1)] ?? this.selectedConnectorId;
    this.render();
  }

  private ensureSelectedConnector(): void {
    if (this.station.state.connectors.some((connector) => connector.id === this.selectedConnectorId)) {
      return;
    }

    this.selectedConnectorId = this.station.state.connectors[0]?.id ?? this.station.config.connectorId;
  }

  private selectedConnector(state = this.station.state): ConnectorState | undefined {
    return state.connectors.find((connector) => connector.id === this.selectedConnectorId) ?? state.connectors[0];
  }

  private promptText(label: string, initialValue: string, callback: (value: string) => void): void {
    this.promptForm(label, [{ name: "value", label, initialValue }], (values) => {
      const value = values.value.trim();
      if (value) {
        callback(value);
      }
    });
  }

  private promptForm(label: string, fields: readonly FormField[], callback: (values: Record<string, string>) => void): void {
    const width = Math.min(86, Math.max(54, this.screenColumns() - 8));
    const height = Math.min(this.screenRows() - 2, Math.max(10, fields.length * 4 + 4));
    const box = blessed.box({
      top: "center",
      left: "center",
      width,
      height,
      border: "line",
      label: ` ${label} `,
      tags: true,
      keys: true,
      style: {
        border: { fg: THEME.panelBorder }
      }
    });
    const values = fields.map((field) => field.initialValue);
    const cursors = values.map((value) => value.length);
    const inputBoxes = fields.map((field, index) =>
      blessed.box({
        parent: box,
        top: 1 + index * 4,
        left: 2,
        right: 2,
        height: 3,
        border: "line",
        label: ` ${field.label} `,
        keys: true,
        tags: true,
        style: {
          border: { fg: THEME.panelBorder }
        }
      })
    );
    const help = blessed.box({
      parent: box,
      bottom: 0,
      left: 2,
      right: 2,
      height: 1,
      tags: true,
      content: "{gray-fg}Tab/Up/Down field  Left/Right cursor  C-u clear  Enter submit  Esc cancel{/gray-fg}"
    });

    let focusedIndex = 0;
    let closed = false;
    const renderInput = (index: number) => {
      const input = inputBoxes[index];
      if (!input) {
        return;
      }

      const width = inputContentWidth(input);
      const value = values[index] ?? "";
      const cursor = clamp(cursors[index] ?? value.length, 0, value.length);
      input.setContent(renderFieldValue(value, cursor, width, focusedIndex === index));
      input.style.border = { fg: focusedIndex === index ? "yellow" : THEME.panelBorder };
    };
    const renderInputs = () => {
      inputBoxes.forEach((_input, index) => renderInput(index));
    };
    const focusInput = (index: number) => {
      focusedIndex = clamp(index, 0, inputBoxes.length - 1);
      renderInputs();
      inputBoxes[focusedIndex]?.focus();
      this.screen.render();
    };
    const close = (submit: boolean) => {
      if (closed) {
        return;
      }

      closed = true;
      const submittedValues = Object.fromEntries(fields.map((field, index) => [field.name, values[index]?.trim() ?? ""]));
      box.destroy();
      this.modal = undefined;
      this.actionList.focus();
      this.screen.render();

      if (submit) {
        callback(submittedValues);
      }
    };
    const handleKey = (index: number, ch: string | undefined, key: { name?: string; ctrl?: boolean; shift?: boolean; meta?: boolean }) => {
      if (closed) {
        return;
      }

      if (key.name === "enter") {
        close(true);
        return;
      }
      if (key.name === "escape" || (key.ctrl && key.name === "c")) {
        close(false);
        return;
      }
      if (key.name === "tab" || key.name === "down") {
        focusInput(index + 1);
        return;
      }
      if (key.name === "up" || (key.shift && key.name === "tab")) {
        focusInput(index - 1);
        return;
      }
      if (key.name === "backspace") {
        const cursor = cursors[index] ?? 0;
        if (cursor > 0) {
          values[index] = `${(values[index] ?? "").slice(0, cursor - 1)}${(values[index] ?? "").slice(cursor)}`;
          cursors[index] = cursor - 1;
        }
        renderInput(index);
        this.screen.render();
        return;
      }
      if (key.name === "delete") {
        const value = values[index] ?? "";
        const cursor = cursors[index] ?? 0;
        if (cursor < value.length) {
          values[index] = `${value.slice(0, cursor)}${value.slice(cursor + 1)}`;
        }
        renderInput(index);
        this.screen.render();
        return;
      }
      if (key.ctrl && key.name === "u") {
        values[index] = "";
        cursors[index] = 0;
        renderInput(index);
        this.screen.render();
        return;
      }
      if (key.name === "left") {
        cursors[index] = clamp((cursors[index] ?? 0) - 1, 0, (values[index] ?? "").length);
        renderInput(index);
        this.screen.render();
        return;
      }
      if (key.name === "right") {
        cursors[index] = clamp((cursors[index] ?? 0) + 1, 0, (values[index] ?? "").length);
        renderInput(index);
        this.screen.render();
        return;
      }
      if (key.name === "home") {
        cursors[index] = 0;
        renderInput(index);
        this.screen.render();
        return;
      }
      if (key.name === "end") {
        cursors[index] = (values[index] ?? "").length;
        renderInput(index);
        this.screen.render();
        return;
      }
      if (ch && !key.ctrl && !key.meta && isPrintableInput(ch)) {
        const value = values[index] ?? "";
        const cursor = cursors[index] ?? value.length;
        values[index] = `${value.slice(0, cursor)}${ch}${value.slice(cursor)}`;
        cursors[index] = cursor + ch.length;
        renderInput(index);
        this.screen.render();
      }
    };

    this.modal = box;
    for (const [index, input] of inputBoxes.entries()) {
      input.on("keypress", (ch, key) => handleKey(index, ch, key));
    }
    box.key(["escape", "C-c"], () => close(false));
    box.key(["tab"], () => focusInput(focusedIndex + 1));
    box.key(["S-tab"], () => focusInput(focusedIndex - 1));
    box.append(help);
    this.screen.append(box);
    box.setFront();
    renderInputs();
    focusInput(0);
  }

  private promptChoice(label: string, choices: readonly string[], callback: (value: string) => void): void {
    const width = Math.max(36, Math.min(72, Math.max(...choices.map((choice) => choice.length)) + 8));
    const list = blessed.list({
      parent: this.screen,
      top: "center",
      left: "center",
      width,
      height: Math.min(choices.length + 4, 16),
      border: "line",
      label: ` ${label} `,
      tags: true,
      keys: true,
      vi: true,
      items: choices.map((choice) => ` ${choice}`),
      style: {
        selected: { bg: THEME.selectedBg, fg: THEME.selectedFg },
        border: { fg: THEME.panelBorder }
      }
    });

    this.modal = list;
    const close = (value?: string) => {
      list.destroy();
      this.modal = undefined;
      this.actionList.focus();
      this.screen.render();
      if (value) {
        callback(value);
      }
    };
    list.key(["escape", "q"], () => close());
    list.key("enter", () => close(choices[(list as Widgets.ListElement & { selected?: number }).selected ?? 0]));
    list.focus();
    this.screen.render();
  }

  private closeModal(): boolean {
    if (!this.modal) {
      return false;
    }

    this.modal.destroy();
    this.modal = undefined;
    this.actionList.focus();
    this.screen.render();
    return true;
  }

  private runStationTask(task: () => Promise<unknown>): void {
    task().catch((error) => {
      const message = error instanceof Error ? error.message : String(error);
      this.renderSyntheticLog("error", message);
    });
  }

  private renderLog(entry: LogEntry): void {
    this.logs.push(entry);
    if (this.logs.length > 800) {
      this.logs.shift();
    }

    if (this.matchesLogFilter(entry)) {
      this.appendLogEntry(entry);
    }

    this.refreshPlainLogView();
    this.screen.render();
  }

  private renderSyntheticLog(level: LogEntry["level"], message: string): void {
    this.renderLog({ at: new Date(), level, message });
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

  private closePlainLogView(): boolean {
    if (!this.plainLogView) {
      return false;
    }

    this.plainLogView.destroy();
    this.plainLogView = undefined;
    this.actionList.focus();
    this.screen.render();
    return true;
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
    const filtered = this.filteredLogs();
    const text = filtered.map((entry) => this.formatLogEntryForClipboard(entry)).join("\n");
    if (!text) {
      this.renderSyntheticLog("warn", "Log is empty");
      return;
    }

    const copied = copyToClipboard(text);
    this.renderSyntheticLog(
      copied ? "success" : "error",
      copied ? `Copied ${filtered.length} filtered log entries to clipboard` : "Could not copy logs to clipboard"
    );
  }

  private clearLogs(): void {
    this.logs.splice(0, this.logs.length);
    this.logBox.setContent("");
    this.refreshPlainLogView();
    this.renderSyntheticLog("info", "Log buffer cleared");
  }

  private cycleLogFilter(): void {
    const filters: LogFilter[] = ["all", "calls", "errors", "station", "csms"];
    const current = filters.indexOf(this.logFilter);
    this.logFilter = filters[(current + 1) % filters.length] ?? "all";
    this.renderLogBuffer();
    this.render();
    this.renderSyntheticLog("info", `Log filter: ${this.logFilter}`);
  }

  private renderLogBuffer(): void {
    this.logBox.setLabel(` OCPP log [${this.logFilter}] `);
    this.logBox.setContent("");
    for (const entry of this.filteredLogs()) {
      this.appendLogEntry(entry);
    }
    this.scrollLogToEnd();
  }

  private appendLogEntry(entry: LogEntry): void {
    this.logBox.log(this.formatLogEntryForTui(entry));
    if (entry.details) {
      this.logBox.log(`  {gray-fg}${entry.details}{/gray-fg}`);
    }
  }

  private filteredLogs(): LogEntry[] {
    return this.logs.filter((entry) => this.matchesLogFilter(entry));
  }

  private matchesLogFilter(entry: LogEntry): boolean {
    switch (this.logFilter) {
      case "all":
        return true;
      case "calls":
        return entry.level === "in" || entry.level === "out";
      case "errors":
        return entry.level === "warn" || entry.level === "error";
      case "station":
        return entry.level === "info" || entry.level === "success" || entry.level === "warn" || entry.level === "error";
      case "csms":
        return entry.level === "in";
    }
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
    const lines = this.filteredLogs().map((entry) => this.formatLogEntryForClipboard(entry));
    return [
      `OCPP log [${this.logFilter}] - j/k scroll, G bottom, y copy, F filter, Esc close`,
      "",
      ...lines
    ].join("\n");
  }

  private bindLogNavigationKeys(widget: Widgets.BoxElement | Widgets.Log): void {
    widget.key(["g", "G", "S-g", "end"], () => this.scrollLogToEnd());
    widget.key(["j", "down"], () => this.scrollLog(1));
    widget.key(["k", "up"], () => this.scrollLog(-1));
    widget.key(["y"], () => this.copyLogsToClipboard());
  }

  private stopAndExit(): void {
    this.stop();
    process.exit(0);
  }

  private screenColumns(): number {
    return typeof this.screen.width === "number" ? this.screen.width : 120;
  }

  private screenRows(): number {
    return typeof this.screen.height === "number" ? this.screen.height : 40;
  }

  private connectorContentWidth(): number {
    return Math.max(18, Math.floor(this.screenColumns() * (this.screenColumns() < 110 ? 0.32 : 0.25)) - 4);
  }

  private centerContentWidth(): number {
    return Math.max(28, Math.floor(this.screenColumns() * (this.screenColumns() < 110 ? 0.68 : 0.37)) - 4);
  }

  private connectorContentRows(): number {
    const height = (this.connectorList as Widgets.ListElement & { height?: unknown }).height;
    const rows = typeof height === "number" ? height : this.screenRows() - 6;
    return Math.max(1, rows - 2);
  }
}

function action(
  key: string,
  label: string,
  description: string,
  run: () => void,
  disabled?: () => boolean
): ActionItem {
  return { key, label, description, run, disabled };
}

function row(label: string, value: string): string {
  return `${label.padEnd(13)} ${value}`;
}

function shortConnectorStatus(status: ConnectorState["status"]): string {
  switch (status) {
    case "Available":
      return "Avail";
    case "Preparing":
      return "Prep";
    case "Charging":
      return "Charge";
    case "SuspendedEV":
      return "SuspEV";
    case "SuspendedEVSE":
      return "SuspSE";
    case "Finishing":
      return "Finish";
    case "Reserved":
      return "Reserv";
    case "Unavailable":
      return "Unavail";
    case "Faulted":
      return "Fault";
  }
}

function shortErrorCode(errorCode: ChargePointErrorCode): string {
  if (errorCode === "NoError") {
    return "Fault";
  }
  if (errorCode === "GroundFailure") {
    return "Ground";
  }
  if (errorCode === "PowerMeterFailure") {
    return "Meter";
  }
  if (errorCode === "PowerSwitchFailure") {
    return "Switch";
  }
  if (errorCode === "ReaderFailure") {
    return "Reader";
  }
  if (errorCode === "ResetFailure") {
    return "Reset";
  }
  if (errorCode === "UnderVoltage") {
    return "UnderV";
  }
  if (errorCode === "OverVoltage") {
    return "OverV";
  }
  if (errorCode === "WeakSignal") {
    return "Signal";
  }
  return errorCode;
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

function parseJsonOrString(value: string): unknown {
  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
}

function truncateText(value: string, maxLength: number): string {
  if (value.length <= maxLength) {
    return value;
  }

  return `${value.slice(0, Math.max(0, maxLength - 3))}...`;
}

function truncateTaggedLine(value: string, maxLength: number): string {
  const strippedLength = value.replace(/\{[^}]+}/g, "").length;
  if (strippedLength <= maxLength) {
    return value;
  }

  return truncateText(value, maxLength);
}

function inputContentWidth(input: Widgets.BoxElement): number {
  const width = typeof input.width === "number" ? input.width : 48;
  return Math.max(1, width - 4);
}

function renderFieldValue(value: string, cursor: number, width: number, focused: boolean): string {
  const contentWidth = Math.max(1, width);
  if (!focused) {
    return truncateText(value, contentWidth);
  }

  const safeCursor = clamp(cursor, 0, value.length);
  const visibleCharacters = Math.max(1, contentWidth - 1);
  const start = clamp(safeCursor - visibleCharacters, 0, Math.max(0, value.length - visibleCharacters));
  const before = value.slice(start, safeCursor);
  const current = value[safeCursor] ?? " ";
  const after = value.slice(safeCursor + 1, start + visibleCharacters);
  const prefix = start > 0 ? "<" : "";
  const suffix = start + visibleCharacters < value.length ? ">" : "";
  return `${prefix}${before}{inverse}${current}{/inverse}${after}${suffix}`;
}

function isPrintableInput(value: string): boolean {
  return value.length > 0 && !/[\u0000-\u001f\u007f]/.test(value);
}

function weightedMatchScore(value: string, term: string, base: number): number {
  const normalized = value.toLowerCase();
  const index = normalized.indexOf(term);
  if (index < 0) {
    return Number.POSITIVE_INFINITY;
  }

  const boundaryBonus = index === 0 || /\s|\/|-/.test(normalized[index - 1] ?? "") ? 0 : 10;
  return base + boundaryBonus + index;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

function normalizeActionKey(keyName: string | undefined): string {
  if (keyName === "S-m") {
    return "M";
  }
  if (keyName === "S-d") {
    return "D";
  }
  if (keyName === "S-u") {
    return "U";
  }
  if (keyName === "S-o") {
    return "O";
  }
  return keyName ?? "";
}

function legacyActionTab(key: string): TabId | undefined {
  if (["b", "h", "x"].includes(key)) {
    return "station";
  }
  if (["p", "s", "n", "f"].includes(key)) {
    return "connector";
  }
  if (["a", "t", "e", "m", "M"].includes(key)) {
    return "transaction";
  }
  if (["D", "u", "U"].includes(key)) {
    return "maintenance";
  }
  if (["d", "v"].includes(key)) {
    return "data";
  }
  if (["c"].includes(key)) {
    return "logs";
  }
  if (["o", "O", "w", "i"].includes(key)) {
    return "scenarios";
  }
  return undefined;
}

function deleteRuntimeProperty(target: unknown, key: string): void {
  delete (target as Record<string, unknown>)[key];
}

const STOP_REASONS = [
  "Local",
  "EVDisconnected",
  "DeAuthorized",
  "EmergencyStop",
  "HardReset",
  "Other",
  "PowerLoss",
  "Reboot",
  "Remote",
  "SoftReset",
  "UnlockCommand"
] as const;

function isStopReason(value: string): value is StopReason {
  return STOP_REASONS.includes(value as StopReason);
}

const CHARGE_POINT_ERROR_CODES = [
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
] as const;

function readChargePointErrorCode(value: string): ChargePointErrorCode {
  const normalized = value.trim();
  return isChargePointErrorCode(normalized) && normalized !== "NoError" ? normalized : "OtherError";
}

function isChargePointErrorCode(value: string): value is ChargePointErrorCode {
  return CHARGE_POINT_ERROR_CODES.includes(value as ChargePointErrorCode);
}

function readDiagnosticsOutcome(value: string): "success" | "uploadFailure" {
  return value.trim() === "uploadFailure" ? "uploadFailure" : "success";
}

function readFirmwareOutcome(value: string): "success" | "downloadFailure" | "installationFailure" {
  const normalized = value.trim();
  return normalized === "downloadFailure" || normalized === "installationFailure" ? normalized : "success";
}

function readDiagnosticsStatus(value: string): "Idle" | "Uploading" | "Uploaded" | "UploadFailed" {
  return value === "Uploading" || value === "Uploaded" || value === "UploadFailed" ? value : "Idle";
}

function readFirmwareStatus(
  value: string
): "Idle" | "Downloading" | "Downloaded" | "Installing" | "Installed" | "DownloadFailed" | "InstallationFailed" {
  return value === "Downloading" ||
    value === "Downloaded" ||
    value === "Installing" ||
    value === "Installed" ||
    value === "DownloadFailed" ||
    value === "InstallationFailed"
    ? value
    : "Idle";
}
