import EventEmitter from "node:events";
import { OcppClient } from "./ocpp/client.js";
import { StationStore, type PersistedStationState } from "./station-store.js";
import {
  authorizeLocally,
  isAcceptedIdTagInfo,
  normalizeIdTagInfo,
  serializeIdTagInfo,
  type AuthorizationDecision,
  type IdTagInfo
} from "./station/authorization.js";
import {
  faultErrorCode,
  nextConnectorStatus,
  statusAfterAvailabilityChange,
  statusAfterFaultClear,
  statusAfterReservationClear
} from "./station/connector-status.js";
import { ConnectorRegistry } from "./station/connector-registry.js";
import { ChargingProfileRegistry } from "./station/charging-profile-registry.js";
import { changeConfiguration, getConfiguration } from "./station/configuration-commands.js";
import { ConfigurationRegistry } from "./station/configuration-registry.js";
import {
  buildMeterValue,
  isValidSampledDataConfiguration,
  readConfiguredMeasurands,
  type Measurand,
  type MeterValueContext
} from "./station/metering.js";
import { MeterValueScheduler } from "./station/meter-value-scheduler.js";
import { ReservationScheduler } from "./station/reservation-scheduler.js";
import { buildPersistedStationState, restorePersistedStationState } from "./station/persistence.js";
import { readNumber, readObject, readOptionalString, readString } from "./station/payload.js";
import { applyLocalAuthListUpdate } from "./station/local-auth-list.js";
import {
  MaintenanceLifecycle,
  type DiagnosticsOutcome,
  type FirmwareOutcome
} from "./station/maintenance-lifecycle.js";
import { describeOcppMessage } from "./station/ocpp-message.js";
import { StatusNotificationScheduler } from "./station/status-scheduler.js";
import { canTriggerMessage } from "./station/trigger-message.js";
import { reserveNowRejection } from "./station/reservation-rules.js";
import {
  canRemoteStart,
  startTransactionRejection,
  statusAfterStopTransaction
} from "./station/transaction-rules.js";
import { readChargingProfile } from "./station/smart-charging.js";
import { isTransactionMessage, retryDelay } from "./station/transaction-message-retry.js";
import { localTransactionId, OfflineTransactionSync, pendingStopPayload } from "./station/offline-transaction-sync.js";
import type {
  AvailabilityType,
  ChargePointErrorCode,
  ConnectorState,
  ConnectorStatus,
  DiagnosticsStatus,
  FirmwareStatus,
  LogEntry,
  PendingStartTransaction,
  PendingStopTransaction,
  RegistrationStatus,
  StationConfig,
  StationState,
  StopReason
} from "./ocpp/types.js";

interface StationEvents {
  state: [state: StationState];
  log: [entry: LogEntry];
}

export declare interface Station {
  on<K extends keyof StationEvents>(event: K, listener: (...args: StationEvents[K]) => void): this;
  emit<K extends keyof StationEvents>(event: K, ...args: StationEvents[K]): boolean;
}

export class Station extends EventEmitter {
  readonly client: OcppClient;
  readonly state: StationState = {
    connected: false,
    booted: false,
    registrationStatus: "Unknown",
    connectorStatus: "Available",
    availability: "Operative",
    localListVersion: 0,
    meterWh: 0,
    connectors: [],
    diagnosticsStatus: "Idle",
    firmwareStatus: "Idle"
  };

  private heartbeatTimer?: NodeJS.Timeout;
  private resetTimer?: NodeJS.Timeout;
  private bootRetryTimer?: NodeJS.Timeout;
  private reconnectTimer?: NodeJS.Timeout;
  private reconnectAttempts = 0;
  private manuallyDisconnected = true;
  private readonly store?: StationStore;
  private readonly configuration: ConfigurationRegistry;
  private readonly localAuthorizationList = new Map<string, Record<string, unknown> | undefined>();
  private readonly authorizationCache = new Map<string, Record<string, unknown> | undefined>();
  private readonly connectors: ConnectorRegistry;
  private readonly statusScheduler: StatusNotificationScheduler;
  private readonly meterValueScheduler: MeterValueScheduler;
  private readonly reservationScheduler: ReservationScheduler;
  private readonly maintenance: MaintenanceLifecycle;
  private readonly chargingProfiles = new ChargingProfileRegistry();
  private readonly offlineTransactions: OfflineTransactionSync;

  constructor(readonly config: StationConfig) {
    super();
    this.client = new OcppClient(config.centralSystemUrl, config.chargePointId, {
      subprotocol: config.webSocketSubprotocol,
      pingIntervalSeconds: config.webSocketPingIntervalSeconds,
      tlsRejectUnauthorized: config.tlsRejectUnauthorized,
      tlsCaFile: config.tlsCaFile,
      tlsCertFile: config.tlsCertFile,
      tlsKeyFile: config.tlsKeyFile,
      tlsServerName: config.tlsServerName
    });
    this.configuration = new ConfigurationRegistry(config);
    this.connectors = new ConnectorRegistry(config.connectorId, config.connectorCount);
    this.connectors.setOnChange(() => this.refreshConnectorSnapshot());
    this.configuration.setValue("NumberOfConnectors", String(this.connectors.size));
    this.statusScheduler = new StatusNotificationScheduler({
      minimumSeconds: () => this.configuration.integer("MinimumStatusDuration", 0),
      currentStatus: (connectorId) => this.connectors.find(connectorId)?.status,
      send: (connectorId, status) => void this.runSafely(() => this.sendStatusNotification(connectorId, status))
    });
    this.meterValueScheduler = new MeterValueScheduler({
      sampleIntervalSeconds: () => this.configuration.integer("MeterValueSampleInterval", 0),
      clockAlignedIntervalSeconds: () => this.configuration.integer("ClockAlignedDataInterval", 0),
      isConnected: () => this.state.connected,
      activeConnectorIds: () =>
        this.connectors.all()
          .filter((connector) => connector.transactionId && connector.status === "Charging")
          .map((connector) => connector.id),
      isPeriodicConnectorActive: (connectorId) => {
        const connector = this.connectors.find(connectorId);
        return Boolean(connector?.transactionId && connector.status === "Charging");
      },
      sendPeriodic: (connectorId, deltaWh) =>
        this.sendMeterValues(connectorId, "Sample.Periodic", deltaWh, this.configuration.measurands("MeterValuesSampledData")),
      sendClockAligned: (connectorIds, deltaWh) => this.sendClockAlignedMeterValues(connectorIds, deltaWh),
      runSafely: (task) => void this.runSafely(task)
    });
    this.reservationScheduler = new ReservationScheduler({
      expiryDate: (connectorId) => this.connectors.find(connectorId)?.reservationExpiryDate,
      expire: (connectorId) => this.expireReservation(connectorId)
    });
    this.maintenance = new MaintenanceLifecycle({
      run: (task) => void this.runSafely(task),
      diagnosticsStatus: (status) => this.diagnosticsStatusNotification(status),
      firmwareStatus: (status) => this.firmwareStatusNotification(status),
      log: (level, message) => this.log(level, message)
    });
    this.offlineTransactions = new OfflineTransactionSync({
      connectors: this.connectors,
      chargingProfiles: this.chargingProfiles,
      send: (action, payload) => this.send(action, payload),
      readIdTagInfo: (value) => this.readIdTagInfo(value),
      rememberAuthorization: (idTag, idTagInfo) => this.rememberAuthorization(idTag, idTagInfo),
      save: () => this.savePersistedState(),
      log: (level, message) => this.log(level, message),
      statusNotification: (connectorId, status) => this.statusNotification(connectorId, status)
    });
    this.store = config.persistState ? new StationStore(config.chargePointId, config.stateDirectory) : undefined;
    this.loadPersistedState();
    this.refreshConnectorSnapshot();
    this.bindClientEvents();
  }

  connect(): void {
    this.manuallyDisconnected = false;
    this.clearReconnectTimer();
    this.log("info", `Connecting to ${this.config.centralSystemUrl}`);
    this.client.connect();
  }

  disconnect(): void {
    this.manuallyDisconnected = true;
    this.clearReconnectTimer();
    this.stopHeartbeatTimer();
    this.clearResetTimer();
    this.clearBootRetryTimer();
    this.meterValueScheduler.stopAllPeriodic();
    this.meterValueScheduler.stopClockAligned();
    this.reservationScheduler.clearAll();
    this.statusScheduler.clearAll();
    this.maintenance.clearAll();
    this.client.close();
    this.patchState({ connected: false });
  }

  reconnect(): void {
    this.disconnect();
    this.connect();
  }

  async boot(options: { scheduleRetry?: boolean } = {}): Promise<void> {
    const scheduleRetry = options.scheduleRetry ?? true;
    this.clearBootRetryTimer();
    const response = await this.send("BootNotification", {
      chargePointVendor: this.config.vendor,
      chargePointModel: this.config.model
    });

    const status = readString(response.payload.status, "Unknown") as RegistrationStatus | "Unknown";
    const interval = readNumber(response.payload.interval, this.config.heartbeatIntervalSeconds);

    this.patchState({
      booted: status === "Accepted",
      registrationStatus: status
    });

    if (status === "Accepted") {
      this.reconnectAttempts = 0;
      this.configuration.setValue("HeartbeatInterval", String(interval));
      this.savePersistedState();
      this.startHeartbeatTimer(interval);
      await this.offlineTransactions.syncPendingTransactions();
      this.meterValueScheduler.reschedulePeriodic(this.connectors.all().map((connector) => connector.id));
      this.meterValueScheduler.startClockAligned();
      await Promise.all(this.connectors.all().map((connector) => this.statusNotification(connector.id, connector.status, true)));
      return;
    }

    this.stopHeartbeatTimer();
    if (scheduleRetry) {
      this.scheduleBootRetry(interval);
    }
  }

  async heartbeat(): Promise<void> {
    const response = await this.send("Heartbeat", {});
    this.patchState({ lastHeartbeatAt: new Date() });

    if (response.payload.currentTime) {
      this.log("success", `Heartbeat accepted, Central System time: ${String(response.payload.currentTime)}`);
    }
  }

  async statusNotification(
    connectorId = this.config.connectorId,
    status = this.connectors.get(connectorId).status,
    force = false
  ): Promise<void> {
    if (!this.client.isConnected) {
      return;
    }

    if (!force && this.statusScheduler.schedule(connectorId, status)) {
      return;
    }

    this.statusScheduler.clear(connectorId);
    await this.sendStatusNotification(connectorId, status);
  }

  private async sendStatusNotification(connectorId: number, status: ConnectorStatus): Promise<void> {
    const connector = this.connectors.get(connectorId);
    const payload: Record<string, unknown> = {
      connectorId,
      errorCode: status === "Faulted" ? faultErrorCode(connector) : "NoError",
      status,
      timestamp: new Date().toISOString()
    };

    if (connector.info) {
      payload.info = connector.info;
    }
    if (connector.vendorId) {
      payload.vendorId = connector.vendorId;
    }
    if (connector.vendorErrorCode) {
      payload.vendorErrorCode = connector.vendorErrorCode;
    }

    await this.send("StatusNotification", payload);
    this.statusScheduler.markSent(connectorId, status);
  }

  async authorize(idTag?: string): Promise<AuthorizationDecision> {
    if (!idTag) {
      this.log("warn", "Authorize requires idTag");
      return { accepted: false, idTagInfo: { status: "Invalid" }, source: "None" };
    }

    if (this.client.isConnected) {
      const response = await this.send("Authorize", { idTag });
      const idTagInfo = this.readIdTagInfo(response.payload.idTagInfo);
      this.rememberAuthorization(idTag, idTagInfo);

      const accepted = isAcceptedIdTagInfo(idTagInfo);
      this.log(accepted ? "success" : "warn", `Authorization status: ${idTagInfo.status} (${idTag})`);
      if (!accepted) {
        await this.stopTransactionsForInvalidIdTag(idTag);
      }
      return { accepted, idTagInfo, source: "CentralSystem" };
    }

    const decision = this.authorizeLocally(idTag, true);
    this.log(
      decision.accepted ? "success" : "warn",
      `Offline authorization status: ${decision.idTagInfo.status} (${idTag}, ${decision.source})`
    );
    return decision;
  }

  async toggleTransaction(connectorId = this.config.connectorId, idTag?: string): Promise<void> {
    if (!idTag) {
      this.log("warn", "StartTransaction requires idTag");
      return;
    }

    const connector = this.connectors.get(connectorId);
    if (connector.transactionId) {
      await this.stopTransaction(connectorId, "Local", idTag);
      return;
    }

    await this.startTransaction(connectorId, idTag);
  }

  async startTransaction(connectorId = this.config.connectorId, idTag?: string): Promise<number | undefined> {
    if (!idTag) {
      this.log("warn", "StartTransaction requires idTag");
      return undefined;
    }

    const connector = this.connectors.get(connectorId);
    const rejection = startTransactionRejection(connector, idTag);
    if (rejection) {
      this.log("warn", rejection);
      return undefined;
    }

    if (!connector.evConnected && connector.status === "Available") {
      this.connectors.patch(connectorId, { evConnected: true, status: "Preparing" });
      void this.runSafely(() => this.statusNotification(connectorId, "Preparing"));
    } else if (connector.status === "Available") {
      this.connectors.patch(connectorId, { status: "Preparing" });
      void this.runSafely(() => this.statusNotification(connectorId, "Preparing"));
    }

    const payload: Record<string, unknown> = {
      connectorId,
      idTag,
      meterStart: Math.round(connector.meterWh),
      timestamp: new Date().toISOString()
    };

    if (connector.reservationId) {
      payload.reservationId = connector.reservationId;
    }

    const localPreAuthorize = this.configuration.boolean("LocalPreAuthorize", false);
    if (localPreAuthorize) {
      const decision = this.authorizeLocally(idTag, false);
      if (!decision.accepted) {
        this.log("warn", `StartTransaction blocked by local authorization: ${decision.idTagInfo.status} (${idTag})`);
        return undefined;
      }
    }

    if (!this.client.isConnected) {
      const decision = this.authorizeLocally(idTag, true);
      if (!decision.accepted) {
        this.log("warn", `Offline StartTransaction blocked: ${decision.idTagInfo.status} (${idTag}, ${decision.source})`);
        return undefined;
      }

      const transactionId = localTransactionId(connectorId);
      this.reservationScheduler.clear(connectorId);
      this.connectors.patch(connectorId, {
        transactionId,
        transactionStartedAt: readString(payload.timestamp, new Date().toISOString()),
        reservationId: undefined,
        reservationIdTag: undefined,
        reservationParentIdTag: undefined,
        reservationExpiryDate: undefined,
        lastIdTag: idTag,
        stopTransactionAtWh: undefined,
        pendingStartTransaction: payload as unknown as PendingStartTransaction,
        pendingStopTransaction: undefined,
        status: "Charging"
      });
      this.savePersistedState();
      this.meterValueScheduler.startPeriodic(connectorId);
      this.log("warn", `Started local offline transaction ${transactionId} on connector ${connectorId}; will sync after reconnect`);
      void this.runSafely(() => this.statusNotification(connectorId, "Charging"));
      return transactionId;
    }

    const response = await this.send("StartTransaction", payload);
    const idTagInfo = this.readIdTagInfo(response.payload.idTagInfo);
    this.rememberAuthorization(idTag, idTagInfo);

    if (!isAcceptedIdTagInfo(idTagInfo)) {
      this.log("warn", `StartTransaction rejected by Central System: ${idTagInfo.status} (${idTag})`);
      return undefined;
    }

    const transactionId = readNumber(response.payload.transactionId, Date.now());
    this.reservationScheduler.clear(connectorId);
    this.connectors.patch(connectorId, {
      transactionId,
      transactionStartedAt: new Date().toISOString(),
      reservationId: undefined,
      reservationIdTag: undefined,
      reservationParentIdTag: undefined,
      reservationExpiryDate: undefined,
      lastIdTag: idTag,
      stopTransactionAtWh: undefined,
      pendingStartTransaction: undefined,
      pendingStopTransaction: undefined,
      status: "Charging"
    });
    this.savePersistedState();
    this.meterValueScheduler.startPeriodic(connectorId);
    this.meterValueScheduler.startClockAligned();
    void this.runSafely(() => this.statusNotification(connectorId, "Charging"));
    return transactionId;
  }

  async stopTransaction(connectorId = this.config.connectorId, reason: StopReason = "Local", idTag?: string): Promise<void> {
    const connector = this.connectors.get(connectorId);

    if (!connector.transactionId) {
      this.log("warn", `Connector ${connectorId} has no active transaction`);
      return;
    }

    this.meterValueScheduler.stopPeriodic(connectorId);

    const transactionId = connector.transactionId;
    const pendingStop: PendingStopTransaction = {
      meterStop: Math.round(connector.meterWh),
      timestamp: new Date().toISOString(),
      reason,
      transactionData: this.buildStopTransactionData(connectorId)
    };

    if (idTag) {
      pendingStop.idTag = idTag;
    }

    if (!this.client.isConnected || connector.pendingStartTransaction) {
      const { evConnected, status } = statusAfterStopTransaction(reason, connector.evConnected, connector.availability);
      this.connectors.patch(connectorId, {
        pendingStopTransaction: pendingStop,
        evConnected,
        status
      });
      this.savePersistedState();
      this.log(
        "warn",
        `Queued ${connector.pendingStartTransaction ? "offline" : "disconnected"} StopTransaction for connector ${connectorId}; will sync after reconnect`
      );
      void this.runSafely(() => this.statusNotification(connectorId, status));
      return;
    }

    const payload: Record<string, unknown> = { ...pendingStopPayload(pendingStop), transactionId };

    this.connectors.patch(connectorId, { status: "Finishing" });
    void this.runSafely(() => this.statusNotification(connectorId, "Finishing"));

    try {
      await this.send("StopTransaction", payload);
    } catch (error) {
      const restoredStatus: ConnectorStatus = connector.evConnected ? "Charging" : "SuspendedEV";
      this.connectors.patch(connectorId, { status: restoredStatus });
      if (restoredStatus === "Charging") {
        this.meterValueScheduler.startPeriodic(connectorId);
        this.meterValueScheduler.startClockAligned();
      }
      void this.runSafely(() => this.statusNotification(connectorId, restoredStatus));
      throw error;
    }

    const { evConnected, status } = statusAfterStopTransaction(reason, connector.evConnected, connector.availability);
    this.connectors.patch(connectorId, {
      transactionId: undefined,
      transactionStartedAt: undefined,
      stopTransactionAtWh: undefined,
      pendingStartTransaction: undefined,
      pendingStopTransaction: undefined,
      evConnected,
      status
    });
    this.chargingProfiles.clearForTransaction(transactionId);
    this.savePersistedState();
    if (status !== "Finishing") {
      void this.runSafely(() => this.statusNotification(connectorId, status));
    }
  }

  async plugIn(connectorId = this.config.connectorId): Promise<void> {
    const connector = this.connectors.get(connectorId);

    if (connector.evConnected) {
      this.log("warn", `Connector ${connectorId} already has EV connected`);
      return;
    }

    if (connector.availability === "Inoperative" || connector.status === "Unavailable" || connector.status === "Faulted") {
      this.log("warn", `Connector ${connectorId} cannot accept EV while ${connector.status}`);
      return;
    }

    if (connector.transactionId) {
      this.connectors.patch(connectorId, { evConnected: true, status: "Charging" });
      this.meterValueScheduler.startPeriodic(connectorId);
      this.meterValueScheduler.startClockAligned();
      await this.statusNotification(connectorId, "Charging");
      return;
    }

    const status: ConnectorStatus = connector.reservationId ? "Reserved" : "Preparing";
    this.connectors.patch(connectorId, { evConnected: true, status });
    await this.statusNotification(connectorId, status);
  }

  async unplug(connectorId = this.config.connectorId): Promise<void> {
    const connector = this.connectors.get(connectorId);

    if (connector.transactionId) {
      if (!this.configuration.boolean("StopTransactionOnEVSideDisconnect", true)) {
        this.meterValueScheduler.stopPeriodic(connectorId);
        this.connectors.patch(connectorId, { evConnected: false, status: "SuspendedEV" });
        await this.statusNotification(connectorId, "SuspendedEV");
        return;
      }

      await this.stopTransaction(connectorId, "EVDisconnected", connector.lastIdTag);
      return;
    }

    if (!connector.evConnected && connector.status !== "Finishing") {
      this.log("warn", `Connector ${connectorId} has no EV connected`);
      return;
    }

    const status: ConnectorStatus =
      connector.availability === "Inoperative" || connector.status === "Unavailable" ? "Unavailable" : "Available";
    this.connectors.patch(connectorId, { evConnected: false, status });
    await this.statusNotification(connectorId, status);
  }

  async setFault(
    connectorId = this.config.connectorId,
    errorCode: ChargePointErrorCode = "OtherError",
    info?: string,
    vendorErrorCode?: string
  ): Promise<void> {
    const connector = this.connectors.get(connectorId);

    if (connector.transactionId) {
      await this.stopTransaction(connectorId, "Other", connector.lastIdTag);
    }

    this.connectors.patch(connectorId, {
      status: "Faulted",
      errorCode: errorCode === "NoError" ? "OtherError" : errorCode,
      info: trimOptional(info, 50),
      vendorId: this.config.vendor,
      vendorErrorCode: trimOptional(vendorErrorCode, 50)
    });
    await this.statusNotification(connectorId, "Faulted");
  }

  async clearFault(connectorId = this.config.connectorId): Promise<void> {
    const connector = this.connectors.get(connectorId);
    const status = statusAfterFaultClear(connector);

    this.connectors.patch(connectorId, {
      status,
      errorCode: "NoError",
      info: undefined,
      vendorId: undefined,
      vendorErrorCode: undefined
    });
    await this.statusNotification(connectorId, status);
  }

  async meterValues(connectorId = this.config.connectorId, deltaWh = 120, sampledData?: string): Promise<void> {
    if (sampledData !== undefined && !isValidSampledDataConfiguration(sampledData)) {
      this.log("warn", `Invalid MeterValues sampled data: ${sampledData}`);
      return;
    }

    const connector = this.connectors.get(connectorId);
    const measurands =
      sampledData === undefined ? this.configuration.measurands("MeterValuesSampledData") : readConfiguredMeasurands(sampledData);
    if (!connector) {
      return;
    }

    await this.sendMeterValues(connectorId, "Sample.Periodic", deltaWh, measurands);
  }

  async dataTransfer(payload: Record<string, unknown> = { vendorId: this.config.vendor }): Promise<void> {
    const response = await this.send("DataTransfer", payload);
    const status = readString(response.payload.status, "Unknown");
    this.log(status === "Accepted" ? "success" : "warn", `DataTransfer status: ${status}`);

    if (response.payload.data) {
      this.log("info", `DataTransfer response data: ${String(response.payload.data)}`);
    }
  }

  async firmwareStatusNotification(status: FirmwareStatus = "Installed"): Promise<void> {
    this.patchState({ firmwareStatus: status });
    await this.send("FirmwareStatusNotification", { status });
  }

  async diagnosticsStatusNotification(status: DiagnosticsStatus = "Uploaded"): Promise<void> {
    this.patchState({ diagnosticsStatus: status });
    await this.send("DiagnosticsStatusNotification", { status });
  }

  setDiagnosticsOutcome(outcome: DiagnosticsOutcome): void {
    this.maintenance.setDiagnosticsOutcome(outcome);
    this.log("info", `Diagnostics outcome set to ${outcome}`);
  }

  setFirmwareOutcome(outcome: FirmwareOutcome): void {
    this.maintenance.setFirmwareOutcome(outcome);
    this.log("info", `Firmware outcome set to ${outcome}`);
  }

  async cycleStatus(connectorId = this.config.connectorId): Promise<void> {
    const connector = this.connectors.get(connectorId);
    const next = nextConnectorStatus(connector.status);
    this.connectors.patch(connectorId, {
      status: next,
      errorCode: next === "Faulted" ? "OtherError" : "NoError",
      info: next === "Faulted" ? "Manual fault" : undefined,
      vendorId: next === "Faulted" ? this.config.vendor : undefined,
      vendorErrorCode: undefined
    });
    await this.statusNotification(connectorId, next);
  }

  addConnector(): number {
    const { id: nextId } = this.connectors.add();
    this.configuration.setValue("NumberOfConnectors", String(this.connectors.size));
    this.savePersistedState();
    void this.runSafely(() => this.statusNotification(nextId, "Available"));
    return nextId;
  }

  private bindClientEvents(): void {
    this.client.on("connected", () => {
      this.clearReconnectTimer();
      this.patchState({ connected: true });
      this.log("success", "Connected");
      void this.runSafely(() => this.boot());
    });

    this.client.on("disconnected", (code, reason) => {
      this.stopHeartbeatTimer();
      this.clearResetTimer();
      this.clearBootRetryTimer();
      this.patchState({ connected: false, booted: false });
      this.log("warn", `Disconnected${code ? ` (${code})` : ""}${reason ? `: ${reason}` : ""}`);
      this.scheduleReconnect();
    });

    this.client.on("error", (error) => this.log("error", error.message));

    this.client.on("log", (direction, message) => {
      this.log(direction, describeOcppMessage(message), JSON.stringify(message));
    });

    this.client.on("call", (messageId, action, payload) => {
      void this.handleCentralSystemCall(messageId, action, payload);
    });
  }

  private async handleCentralSystemCall(
    messageId: string,
    action: string,
    payload: Record<string, unknown>
  ): Promise<void> {
    try {
      switch (action) {
        case "CancelReservation":
          this.handleCancelReservation(messageId, payload);
          break;

        case "ChangeAvailability":
          await this.handleChangeAvailability(messageId, payload);
          break;

        case "ChangeConfiguration":
          this.handleChangeConfiguration(messageId, payload);
          break;

        case "ClearCache":
          this.authorizationCache.clear();
          this.savePersistedState();
          this.client.reply(messageId, { status: "Accepted" });
          break;

        case "ClearChargingProfile":
          this.handleClearChargingProfile(messageId, payload);
          break;

        case "DataTransfer":
          this.handleDataTransfer(messageId, payload);
          break;

        case "GetCompositeSchedule":
          this.handleGetCompositeSchedule(messageId, payload);
          break;

        case "GetConfiguration":
          this.handleGetConfiguration(messageId, payload);
          break;

        case "GetDiagnostics":
          this.client.reply(messageId, { fileName: this.maintenance.startDiagnostics(payload) });
          break;

        case "GetLocalListVersion":
          this.client.reply(messageId, { listVersion: this.state.localListVersion });
          break;

        case "RemoteStartTransaction":
          await this.handleRemoteStartTransaction(messageId, payload);
          break;

        case "RemoteStopTransaction":
          await this.handleRemoteStopTransaction(messageId, payload);
          break;

        case "ReserveNow":
          await this.handleReserveNow(messageId, payload);
          break;

        case "Reset":
          this.handleReset(messageId, payload);
          break;

        case "SendLocalList":
          this.handleSendLocalList(messageId, payload);
          break;

        case "SetChargingProfile":
          this.handleSetChargingProfile(messageId, payload);
          break;

        case "TriggerMessage":
          await this.handleTriggerMessage(messageId, payload);
          break;

        case "UnlockConnector":
          await this.handleUnlockConnector(messageId, payload);
          break;

        case "UpdateFirmware":
          this.client.reply(messageId, {});
          this.maintenance.startFirmware(payload);
          break;

        default:
          this.client.replyError(messageId, "NotSupported", `${action} is not supported by this emulator`);
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.client.replyError(messageId, "InternalError", message);
    }
  }

  private handleCancelReservation(messageId: string, payload: Record<string, unknown>): void {
    const reservationId = readNumber(payload.reservationId, -1);
    const connector = this.connectors.findByReservation(reservationId);
    if (!connector) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    this.clearReservation(connector.id);
    this.client.reply(messageId, { status: "Accepted" });
    void this.runSafely(() => this.statusNotification(connector.id, this.connectors.get(connector.id).status));
  }

  private async handleChangeAvailability(messageId: string, payload: Record<string, unknown>): Promise<void> {
    const availability = readString(payload.type, "Operative") as AvailabilityType;
    const connectorId = readNumber(payload.connectorId, 0);
    const requestedConnector = connectorId === 0 ? undefined : this.connectors.find(connectorId);
    if (connectorId !== 0 && !requestedConnector) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }
    const connectors: ConnectorState[] = connectorId === 0 ? this.connectors.all() : [requestedConnector ?? this.connectors.get(connectorId)];

    const scheduled = connectors.some((connector) => connector.transactionId);
    for (const connector of connectors) {
      if (connector.transactionId) {
        this.connectors.patch(connector.id, { availability });
        continue;
      }

      this.connectors.patch(connector.id, { availability, status: statusAfterAvailabilityChange(connector, availability) });
    }

    this.client.reply(messageId, { status: scheduled ? "Scheduled" : "Accepted" });
    for (const connector of connectors.filter((connector) => !connector.transactionId)) {
      void this.runSafely(() => this.statusNotification(connector.id, this.connectors.get(connector.id).status));
    }
  }

  private handleChangeConfiguration(messageId: string, payload: Record<string, unknown>): void {
    const result = changeConfiguration(this.configuration, payload);
    this.client.reply(messageId, { status: result.status });

    if (result.status !== "Accepted") {
      return;
    }

    this.savePersistedState();

    if (result.key === "HeartbeatInterval") {
      const interval = Number.parseInt(result.value, 10);
      if (Number.isFinite(interval) && interval > 0) {
        this.startHeartbeatTimer(interval);
      }
    }

    if (result.key === "MeterValueSampleInterval") {
      this.meterValueScheduler.reschedulePeriodic(this.connectors.all().map((connector) => connector.id));
    }

    if (result.key === "ClockAlignedDataInterval") {
      this.meterValueScheduler.startClockAligned();
    }

    if (result.key === "MinimumStatusDuration" && result.value === "0") {
      this.statusScheduler.flush();
    }
  }

  private handleDataTransfer(messageId: string, payload: Record<string, unknown>): void {
    const vendorId = readString(payload.vendorId, "");
    if (vendorId !== this.config.vendor) {
      this.client.reply(messageId, { status: "UnknownVendorId" });
      return;
    }

    this.client.reply(messageId, { status: "Accepted", data: readString(payload.data, "") });
  }

  private handleGetConfiguration(messageId: string, payload: Record<string, unknown>): void {
    this.client.reply(messageId, getConfiguration(this.configuration, payload));
  }

  private handleSetChargingProfile(messageId: string, payload: Record<string, unknown>): void {
    const connectorId = readNumber(payload.connectorId, -1);
    const status = this.chargingProfiles.set(
      connectorId,
      payload.csChargingProfiles,
      {
        hasConnector: (id) => Boolean(this.connectors.find(id)),
        transactionIdForConnector: (id) => this.connectors.find(id)?.transactionId
      }
    );

    if (status !== "Accepted") {
      this.client.reply(messageId, { status });
      return;
    }

    this.savePersistedState();
    this.client.reply(messageId, { status: "Accepted" });
  }

  private handleClearChargingProfile(messageId: string, payload: Record<string, unknown>): void {
    const status = this.chargingProfiles.clear(payload);
    if (status === "Accepted") {
      this.savePersistedState();
    }

    this.client.reply(messageId, { status });
  }

  private handleGetCompositeSchedule(messageId: string, payload: Record<string, unknown>): void {
    const connectorId = readNumber(payload.connectorId, -1);
    const connector = connectorId === 0 ? undefined : this.connectors.find(connectorId);

    if (connectorId !== 0 && !connector) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    const schedule = this.chargingProfiles.compositeSchedulePayload(
      payload,
      connector?.transactionId,
      connector?.transactionStartedAt
    );
    if (!schedule) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    if (schedule.prunedExpired) {
      this.savePersistedState();
    }

    this.client.reply(messageId, schedule.payload);
  }

  private handleSendLocalList(messageId: string, payload: Record<string, unknown>): void {
    const result = applyLocalAuthListUpdate(this.localAuthorizationList, this.state.localListVersion, payload, {
      enabled: this.configuration.boolean("LocalAuthListEnabled", true),
      maxLength: this.configuration.integer("SendLocalListMaxLength", 1000)
    });
    if (result.status !== "Accepted") {
      this.client.reply(messageId, { status: result.status });
      return;
    }

    this.patchState({ localListVersion: result.listVersion });
    this.savePersistedState();
    this.client.reply(messageId, { status: "Accepted" });
  }

  private async handleRemoteStartTransaction(messageId: string, payload: Record<string, unknown>): Promise<void> {
    const requestedConnector = readNumber(payload.connectorId, 0);
    const connector = requestedConnector > 0 ? this.connectors.find(requestedConnector) : this.connectors.findAvailable();
    const idTag = readString(payload.idTag, "");

    if (!idTag || !canRemoteStart(connector) || !this.canAcceptRemoteStartChargingProfile(payload)) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    if (this.configuration.boolean("AuthorizeRemoteTxRequests", false)) {
      const decision = await this.authorize(idTag);
      if (!decision.accepted) {
        this.client.reply(messageId, { status: "Rejected" });
        return;
      }
    }

    this.client.reply(messageId, { status: "Accepted" });

    if (connector) {
      const transactionId = await this.startTransaction(connector.id, idTag);
      if (transactionId !== undefined) {
        this.applyRemoteStartChargingProfile(connector.id, payload.chargingProfile, transactionId);
      }
    }
  }

  private async handleRemoteStopTransaction(messageId: string, payload: Record<string, unknown>): Promise<void> {
    const transactionId = readNumber(payload.transactionId, -1);
    const connector = this.connectors.findByTransaction(transactionId);
    const accepted = Boolean(connector);
    this.client.reply(messageId, { status: accepted ? "Accepted" : "Rejected" });

    if (accepted && connector) {
      await this.stopTransaction(connector.id, "Remote");
    }
  }

  private async handleReserveNow(messageId: string, payload: Record<string, unknown>): Promise<void> {
    const requestedConnectorId = readNumber(payload.connectorId, this.config.connectorId);
    const connector = requestedConnectorId === 0 ? this.connectors.findAvailable() : this.connectors.find(requestedConnectorId);
    const connectorId = connector?.id ?? requestedConnectorId;
    const expiryDate = readString(payload.expiryDate, "");
    const rejection = reserveNowRejection(connector, expiryDate);
    if (rejection) {
      this.client.reply(messageId, { status: rejection });
      return;
    }

    this.connectors.patch(connectorId, {
      reservationId: readNumber(payload.reservationId, Date.now()),
      reservationIdTag: readString(payload.idTag, ""),
      reservationParentIdTag: readOptionalString(payload.parentIdTag),
      reservationExpiryDate: expiryDate,
      status: "Reserved"
    });
    this.reservationScheduler.schedule(connectorId);
    this.savePersistedState();
    this.client.reply(messageId, { status: "Accepted" });
    void this.runSafely(() => this.statusNotification(connectorId, "Reserved"));
  }

  private handleReset(messageId: string, payload: Record<string, unknown>): void {
    const resetType = readString(payload.type, "Soft");
    const reason: StopReason = resetType === "Hard" ? "HardReset" : "SoftReset";

    this.client.reply(messageId, { status: "Accepted" });
    void this.runSafely(() => this.performReset(reason, resetType));
  }

  private async handleTriggerMessage(messageId: string, payload: Record<string, unknown>): Promise<void> {
    const requestedMessage = readString(payload.requestedMessage, "");

    if (!canTriggerMessage(requestedMessage)) {
      this.client.reply(messageId, { status: "NotImplemented" });
      return;
    }

    if (!this.canTriggerMessageForPayload(requestedMessage, payload)) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    this.client.reply(messageId, { status: "Accepted" });
    await this.triggerMessage(requestedMessage, payload);
  }

  private async handleUnlockConnector(messageId: string, payload: Record<string, unknown> = {}): Promise<void> {
    const connectorId = readNumber(payload.connectorId, this.config.connectorId);
    const connector = this.connectors.find(connectorId);

    if (!connector) {
      this.client.reply(messageId, { status: "NotSupported" });
      return;
    }

    if (connector.transactionId) {
      this.client.reply(messageId, { status: "Unlocked" });
      await this.stopTransaction(connectorId, "UnlockCommand");
      return;
    }

    this.client.reply(messageId, { status: "Unlocked" });
  }

  private async triggerMessage(requestedMessage: string, payload: Record<string, unknown> = {}): Promise<void> {
    switch (requestedMessage) {
      case "BootNotification":
        await this.boot();
        break;
      case "DiagnosticsStatusNotification":
        await this.diagnosticsStatusNotification("Idle");
        break;
      case "FirmwareStatusNotification":
        await this.firmwareStatusNotification("Idle");
        break;
      case "Heartbeat":
        await this.heartbeat();
        break;
      case "MeterValues":
        await this.meterValues(this.triggerConnectorId(payload), 0);
        break;
      case "StatusNotification":
        await this.statusNotification(this.triggerConnectorId(payload), this.connectors.get(this.triggerConnectorId(payload)).status, true);
        break;
      default:
        this.log("warn", `TriggerMessage requested unsupported message: ${requestedMessage}`);
    }
  }

  private canTriggerMessageForPayload(requestedMessage: string, payload: Record<string, unknown>): boolean {
    if (requestedMessage !== "MeterValues" && requestedMessage !== "StatusNotification") {
      return true;
    }

    return Boolean(this.connectors.find(this.triggerConnectorId(payload)));
  }

  private triggerConnectorId(payload: Record<string, unknown>): number {
    return "connectorId" in payload ? readNumber(payload.connectorId, -1) : this.config.connectorId;
  }

  private canAcceptRemoteStartChargingProfile(payload: Record<string, unknown>): boolean {
    if (!("chargingProfile" in payload)) {
      return true;
    }

    const profile = readChargingProfile(payload.chargingProfile);
    return Boolean(
      profile &&
        profile.chargingProfilePurpose === "TxProfile" &&
        profile.transactionId === undefined
    );
  }

  private applyRemoteStartChargingProfile(connectorId: number, profilePayload: unknown, transactionId: number): void {
    const profile = readChargingProfile(profilePayload);
    if (!profile) {
      return;
    }

    const status = this.chargingProfiles.set(
      connectorId,
      { ...profile, transactionId },
      {
        hasConnector: (id) => Boolean(this.connectors.find(id)),
        transactionIdForConnector: (id) => this.connectors.find(id)?.transactionId
      }
    );

    if (status === "Accepted") {
      this.savePersistedState();
    }
  }

  private async send(action: string, payload: Record<string, unknown>) {
    if (isTransactionMessage(action)) {
      return this.sendTransactionMessage(action, payload);
    }

    try {
      const response = await this.client.call(action, payload, this.config.callTimeoutMs ?? 30_000);
      this.log("success", `${action} accepted`);
      return response;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log("error", message);
      throw error;
    }
  }

  private async sendTransactionMessage(action: string, payload: Record<string, unknown>) {
    const attempts = this.configuration.integer("TransactionMessageAttempts", 1);
    const retryInterval = this.configuration.integer("TransactionMessageRetryInterval", 30);
    let lastError: unknown;

    for (let attempt = 1; attempt <= attempts; attempt += 1) {
      try {
        const response = await this.client.call(action, payload, this.config.callTimeoutMs ?? 30_000);
        this.log("success", `${action} accepted`);
        return response;
      } catch (error) {
        lastError = error;
        if (attempt >= attempts) {
          break;
        }

        const message = error instanceof Error ? error.message : String(error);
        this.log("warn", `${action} attempt ${attempt}/${attempts} failed: ${message}; retrying in ${retryInterval}s`);
        await retryDelay(retryInterval);
      }
    }

    const message = lastError instanceof Error ? lastError.message : String(lastError);
    this.log("error", message);
    throw lastError instanceof Error ? lastError : new Error(message);
  }

  private async sendMeterValues(
    connectorId: number,
    context: MeterValueContext,
    deltaWh: number,
    measurands: Measurand[]
  ): Promise<void> {
    const connector = this.connectors.get(connectorId);
    const nextMeter = connector.meterWh + deltaWh;
    this.connectors.patch(connectorId, { meterWh: nextMeter });
    this.savePersistedState();

    if (!this.client.isConnected || connector.pendingStartTransaction) {
      await this.stopTransactionIfInvalidEnergyLimitReached(connectorId, nextMeter);
      return;
    }

    const payload: Record<string, unknown> = {
      connectorId,
      meterValue: [buildMeterValue(this.connectors.get(connectorId), context, measurands)]
    };

    if (connector.transactionId) {
      payload.transactionId = connector.transactionId;
    }

    await this.send("MeterValues", payload);
    await this.stopTransactionIfInvalidEnergyLimitReached(connectorId, nextMeter);
  }

  private buildStopTransactionData(connectorId: number): Record<string, unknown>[] {
    return [
      buildMeterValue(this.connectors.get(connectorId), "Transaction.End", this.configuration.measurands("StopTxnSampledData")),
      buildMeterValue(this.connectors.get(connectorId), "Sample.Clock", this.configuration.measurands("StopTxnAlignedData"))
    ];
  }

  private authorizeLocally(idTag: string, requireOfflinePermission: boolean): AuthorizationDecision {
    return authorizeLocally(
      idTag,
      requireOfflinePermission,
      this.localAuthorizationList,
      this.authorizationCache,
      {
        localAuthListEnabled: this.configuration.boolean("LocalAuthListEnabled", true),
        localAuthorizeOffline: this.configuration.boolean("LocalAuthorizeOffline", false),
        authorizationCacheEnabled: this.configuration.boolean("AuthorizationCacheEnabled", true),
        allowOfflineTxForUnknownId: this.configuration.boolean("AllowOfflineTxForUnknownId", false)
      }
    );
  }

  private rememberAuthorization(idTag: string, idTagInfo: IdTagInfo): void {
    if (!this.configuration.boolean("AuthorizationCacheEnabled", true)) {
      return;
    }

    this.authorizationCache.set(idTag, serializeIdTagInfo(idTagInfo));
    this.savePersistedState();
  }

  private readIdTagInfo(value: unknown): IdTagInfo {
    return normalizeIdTagInfo(readObject(value));
  }

  private async stopTransactionsForInvalidIdTag(idTag: string): Promise<void> {
    if (!this.configuration.boolean("StopTransactionOnInvalidId", false)) {
      return;
    }

    const maxEnergyOnInvalidId = this.configuration.integer("MaxEnergyOnInvalidId", 0);
    const connectors = this.connectors.all().filter(
      (connector) => connector.transactionId && connector.lastIdTag === idTag
    );
    for (const connector of connectors) {
      if (maxEnergyOnInvalidId <= 0) {
        await this.stopTransaction(connector.id, "DeAuthorized", idTag);
        continue;
      }

      this.connectors.patch(connector.id, { stopTransactionAtWh: connector.meterWh + maxEnergyOnInvalidId });
      this.log(
        "warn",
        `Connector ${connector.id} will stop after ${maxEnergyOnInvalidId}Wh because idTag ${idTag} is no longer authorized`
      );
    }
  }

  private async stopTransactionIfInvalidEnergyLimitReached(connectorId: number, meterWh: number): Promise<void> {
    const connector = this.connectors.get(connectorId);
    if (!connector.transactionId || connector.stopTransactionAtWh === undefined || meterWh < connector.stopTransactionAtWh) {
      return;
    }

    await this.stopTransaction(connectorId, "DeAuthorized", connector.lastIdTag);
  }

  private refreshConnectorSnapshot(): void {
    const { connectors, primary } = this.connectors.snapshot();

    this.state.connectors = connectors;
    if (primary) {
      this.state.connectorStatus = primary.status;
      this.state.availability = primary.availability;
      this.state.transactionId = primary.transactionId;
      this.state.reservationId = primary.reservationId;
      this.state.meterWh = primary.meterWh;
    }

    this.emit("state", { ...this.state, connectors });
  }

  private clearReservation(connectorId: number): void {
    const connector = this.connectors.get(connectorId);
    this.reservationScheduler.clear(connectorId);
    this.connectors.patch(connectorId, {
      reservationId: undefined,
      reservationIdTag: undefined,
      reservationParentIdTag: undefined,
      reservationExpiryDate: undefined,
      status: statusAfterReservationClear(connector)
    });
    this.savePersistedState();
  }

  private async sendClockAlignedMeterValues(connectorIds: number[], deltaWh: number): Promise<void> {
    const measurands = this.configuration.measurands("MeterValuesAlignedData");
    for (const connectorId of connectorIds) {
      await this.sendMeterValues(connectorId, "Sample.Clock", deltaWh, measurands);
    }
  }

  private expireReservation(connectorId: number): void {
    const connector = this.connectors.find(connectorId);
    if (!connector?.reservationId) {
      return;
    }

    this.log("warn", `Reservation ${connector.reservationId} expired on connector ${connectorId}`);
    this.clearReservation(connectorId);
    void this.runSafely(() => this.statusNotification(connectorId, this.connectors.get(connectorId).status));
  }

  private loadPersistedState(): void {
    if (!this.store) {
      return;
    }

    let persisted: PersistedStationState | undefined;
    try {
      persisted = this.store.load();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log("warn", `Could not load persisted station state: ${message}`);
      return;
    }

    if (!persisted) {
      return;
    }

    const restored = restorePersistedStationState({
      persisted,
      connectorId: this.config.connectorId,
      connectors: this.connectors,
      configuration: this.configuration,
      localAuthorizationList: this.localAuthorizationList,
      authorizationCache: this.authorizationCache,
      scheduleReservation: (connectorId) => this.reservationScheduler.schedule(connectorId)
    });
    this.state.localListVersion = restored.localListVersion;
    this.chargingProfiles.replace(restored.chargingProfiles);
  }

  private savePersistedState(): void {
    if (!this.store) {
      return;
    }

    try {
      this.store.save(
        buildPersistedStationState({
          chargePointId: this.config.chargePointId,
          connectorId: this.config.connectorId,
          connectors: this.connectors.all(),
          localListVersion: this.state.localListVersion,
          localAuthorizationList: this.localAuthorizationList,
          authorizationCache: this.authorizationCache,
          chargingProfiles: this.chargingProfiles.entries(),
          configuration: this.configuration.values
        })
      );
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log("error", `Could not save station state: ${message}`);
    }
  }

  private startHeartbeatTimer(intervalSeconds: number): void {
    this.stopHeartbeatTimer();
    this.heartbeatTimer = setInterval(() => {
      void this.runSafely(() => this.heartbeat());
    }, intervalSeconds * 1000);
  }

  private stopHeartbeatTimer(): void {
    if (!this.heartbeatTimer) {
      return;
    }

    clearInterval(this.heartbeatTimer);
    this.heartbeatTimer = undefined;
  }

  private async performReset(reason: StopReason, resetType: string): Promise<void> {
    this.log("warn", `${resetType} reset accepted; rebooting station`);
    this.stopHeartbeatTimer();
    this.clearResetTimer();
    this.maintenance.clearAll();
    this.meterValueScheduler.stopClockAligned();

    for (const connector of this.connectors.all()) {
      if (connector.transactionId) {
        await this.stopTransaction(connector.id, reason);
        const resetStatus: ConnectorStatus =
          this.connectors.get(connector.id).availability === "Inoperative" ? "Unavailable" : "Available";
        this.connectors.patch(connector.id, { evConnected: false, status: resetStatus });
        void this.runSafely(() => this.statusNotification(connector.id, resetStatus));
      }
    }

    this.meterValueScheduler.stopAllPeriodic();
    for (const connector of this.connectors.all()) {
      if (!connector.reservationId) {
        continue;
      }

      this.clearReservation(connector.id);
      void this.runSafely(() => this.statusNotification(connector.id, this.connectors.get(connector.id).status));
    }

    this.patchState({ booted: false, registrationStatus: "Unknown" });
    this.resetTimer = setTimeout(() => {
      this.resetTimer = undefined;
      if (this.state.connected) {
        void this.runSafely(() => this.bootAfterReset(this.configuration.integer("ResetRetries", 0)));
      }
    }, 500);
  }

  private async bootAfterReset(remainingRetries: number): Promise<void> {
    await this.boot({ scheduleRetry: false });
    if (this.state.registrationStatus === "Accepted" || remainingRetries <= 0 || !this.state.connected) {
      return;
    }

    this.log("warn", `Reset reboot BootNotification was ${this.state.registrationStatus}; retrying (${remainingRetries} left)`);
    this.resetTimer = setTimeout(() => {
      this.resetTimer = undefined;
      if (this.state.connected) {
        void this.runSafely(() => this.bootAfterReset(remainingRetries - 1));
      }
    }, 500);
  }

  private clearResetTimer(): void {
    if (!this.resetTimer) {
      return;
    }

    clearTimeout(this.resetTimer);
    this.resetTimer = undefined;
  }

  private scheduleBootRetry(intervalSeconds: number): void {
    if (this.bootRetryTimer || !this.state.connected) {
      return;
    }

    const delayMs = Math.max(1, intervalSeconds) * 1000;
    this.log("warn", `BootNotification was ${this.state.registrationStatus}; retrying in ${delayMs}ms`);
    this.bootRetryTimer = setTimeout(() => {
      this.bootRetryTimer = undefined;
      if (this.state.connected) {
        void this.runSafely(() => this.boot());
      }
    }, delayMs);
  }

  private clearBootRetryTimer(): void {
    if (!this.bootRetryTimer) {
      return;
    }

    clearTimeout(this.bootRetryTimer);
    this.bootRetryTimer = undefined;
  }

  private scheduleReconnect(): void {
    if (
      this.manuallyDisconnected ||
      !this.config.webSocketReconnectEnabled ||
      this.reconnectTimer ||
      this.state.connected
    ) {
      return;
    }

    const maxAttempts = this.config.webSocketReconnectMaxAttempts;
    if (maxAttempts > 0 && this.reconnectAttempts >= maxAttempts) {
      this.log("error", `Reconnect stopped after ${this.reconnectAttempts} failed attempt(s)`);
      return;
    }

    this.reconnectAttempts += 1;
    const delayMs = this.reconnectDelayMs(this.reconnectAttempts);
    this.log("warn", `Reconnect attempt ${this.reconnectAttempts}${maxAttempts > 0 ? `/${maxAttempts}` : ""} in ${delayMs}ms`);

    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = undefined;
      if (this.manuallyDisconnected || this.state.connected) {
        return;
      }

      this.log("info", `Reconnecting to ${this.config.centralSystemUrl}`);
      this.client.connect();
    }, delayMs);
  }

  private reconnectDelayMs(attempt: number): number {
    const initial = Math.max(0, this.config.webSocketReconnectInitialDelayMs);
    const max = Math.max(initial, this.config.webSocketReconnectMaxDelayMs);
    return Math.min(initial * 2 ** Math.max(0, attempt - 1), max);
  }

  private clearReconnectTimer(): void {
    if (!this.reconnectTimer) {
      return;
    }

    clearTimeout(this.reconnectTimer);
    this.reconnectTimer = undefined;
  }

  private patchState(patch: Partial<StationState>): void {
    Object.assign(this.state, patch);
    this.emit("state", { ...this.state });
  }

  private log(level: LogEntry["level"], message: string, details?: string): void {
    this.emit("log", { at: new Date(), level, message, details });
  }

  private async runSafely(task: () => Promise<void>): Promise<void> {
    try {
      await task();
    } catch {
      // Errors are logged in send(); keeping async UI handlers quiet avoids unhandled rejections.
    }
  }

}

function trimOptional(value: string | undefined, maxLength: number): string | undefined {
  const trimmed = value?.trim();
  if (!trimmed) {
    return undefined;
  }

  return trimmed.slice(0, maxLength);
}
