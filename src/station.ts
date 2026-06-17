import EventEmitter from "node:events";
import { OcppClient } from "./ocpp/client.js";
import { StationStore, type LocalAuthorizationEntry, type PersistedStationState } from "./station-store.js";
import type {
  AvailabilityType,
  ChargePointErrorCode,
  ConnectorState,
  ConnectorStatus,
  LogEntry,
  OcppMessage,
  RegistrationStatus,
  StationConfig,
  StationState,
  StopReason
} from "./ocpp/types.js";

interface StationEvents {
  state: [state: StationState];
  log: [entry: LogEntry];
}

interface ConfigurationKey {
  readonly: boolean;
  value: string;
}

type IdTagStatus = "Accepted" | "Blocked" | "Expired" | "Invalid" | "ConcurrentTx";

interface IdTagInfo {
  status: IdTagStatus;
  expiryDate?: string;
  parentIdTag?: string;
}

interface AuthorizationDecision {
  accepted: boolean;
  idTagInfo: IdTagInfo;
  source: "CentralSystem" | "LocalList" | "AuthorizationCache" | "OfflineUnknown" | "None";
}

type ChargingProfilePurpose = "ChargePointMaxProfile" | "TxDefaultProfile" | "TxProfile";
type ChargingProfileKind = "Absolute" | "Recurring" | "Relative";
type ChargingRateUnit = "A" | "W";
type MeterValueContext = "Sample.Periodic" | "Transaction.End";
type Measurand =
  | "Energy.Active.Export.Register"
  | "Energy.Active.Import.Register"
  | "Energy.Reactive.Export.Register"
  | "Energy.Reactive.Import.Register"
  | "Energy.Active.Export.Interval"
  | "Energy.Active.Import.Interval"
  | "Energy.Reactive.Export.Interval"
  | "Energy.Reactive.Import.Interval"
  | "Power.Active.Export"
  | "Power.Active.Import"
  | "Power.Offered"
  | "Power.Reactive.Export"
  | "Power.Reactive.Import"
  | "Power.Factor"
  | "Current.Import"
  | "Current.Export"
  | "Current.Offered"
  | "Voltage"
  | "Frequency"
  | "Temperature"
  | "SoC"
  | "RPM";

interface ChargingSchedulePeriod {
  startPeriod: number;
  limit: number;
  numberPhases?: number;
}

interface ChargingSchedule {
  duration?: number;
  startSchedule?: string;
  chargingRateUnit: ChargingRateUnit;
  chargingSchedulePeriod: ChargingSchedulePeriod[];
  minChargingRate?: number;
}

interface ChargingProfile {
  chargingProfileId: number;
  transactionId?: number;
  stackLevel: number;
  chargingProfilePurpose: ChargingProfilePurpose;
  chargingProfileKind: ChargingProfileKind;
  recurrencyKind?: "Daily" | "Weekly";
  validFrom?: string;
  validTo?: string;
  chargingSchedule: ChargingSchedule;
}

interface ChargingProfileEntry {
  connectorId: number;
  profile: ChargingProfile;
}

type MutableConnectorState = ConnectorState;

const STATUS_SEQUENCE: ConnectorStatus[] = [
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

const DEFAULT_SAMPLED_DATA: Measurand[] = ["Energy.Active.Import.Register"];
const SAMPLED_DATA_CONFIGURATION_KEYS = new Set(["MeterValuesSampledData", "StopTxnSampledData"]);
const MEASURANDS = new Set<Measurand>([
  "Energy.Active.Export.Register",
  "Energy.Active.Import.Register",
  "Energy.Reactive.Export.Register",
  "Energy.Reactive.Import.Register",
  "Energy.Active.Export.Interval",
  "Energy.Active.Import.Interval",
  "Energy.Reactive.Export.Interval",
  "Energy.Reactive.Import.Interval",
  "Power.Active.Export",
  "Power.Offered",
  "Power.Active.Import",
  "Power.Reactive.Export",
  "Power.Reactive.Import",
  "Power.Factor",
  "Current.Import",
  "Current.Export",
  "Current.Offered",
  "Voltage",
  "Frequency",
  "Temperature",
  "SoC",
  "RPM"
]);

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
    connectors: []
  };

  private heartbeatTimer?: NodeJS.Timeout;
  private readonly store?: StationStore;
  private readonly configuration = new Map<string, ConfigurationKey>();
  private readonly localAuthorizationList = new Map<string, Record<string, unknown> | undefined>();
  private readonly authorizationCache = new Map<string, Record<string, unknown> | undefined>();
  private readonly connectors = new Map<number, MutableConnectorState>();
  private readonly reservationTimers = new Map<number, NodeJS.Timeout>();
  private chargingProfiles: ChargingProfileEntry[] = [];

  constructor(readonly config: StationConfig) {
    super();
    this.client = new OcppClient(config.centralSystemUrl, config.chargePointId);
    this.seedConfiguration();
    this.initializeConnectors(config.connectorCount);
    this.store = config.persistState ? new StationStore(config.chargePointId, config.stateDirectory) : undefined;
    this.loadPersistedState();
    this.refreshConnectorSnapshot();
    this.bindClientEvents();
  }

  connect(): void {
    this.log("info", `Connecting to ${this.config.centralSystemUrl}`);
    this.client.connect();
  }

  disconnect(): void {
    this.stopHeartbeatTimer();
    this.client.close();
    this.patchState({ connected: false });
  }

  reconnect(): void {
    this.disconnect();
    this.connect();
  }

  async boot(): Promise<void> {
    const response = await this.send("BootNotification", {
      chargePointVendor: this.config.vendor,
      chargePointModel: this.config.model
    });

    const status = this.readString(response.payload.status, "Unknown") as RegistrationStatus | "Unknown";
    const interval = this.readNumber(response.payload.interval, this.config.heartbeatIntervalSeconds);

    this.patchState({
      booted: status === "Accepted",
      registrationStatus: status
    });

    if (status === "Accepted") {
      this.setConfigurationValue("HeartbeatInterval", String(interval));
      this.savePersistedState();
      this.startHeartbeatTimer(interval);
      await Promise.all(this.getConnectors().map((connector) => this.statusNotification(connector.id)));
    }
  }

  async heartbeat(): Promise<void> {
    const response = await this.send("Heartbeat", {});
    this.patchState({ lastHeartbeatAt: new Date() });

    if (response.payload.currentTime) {
      this.log("success", `Heartbeat accepted, Central System time: ${String(response.payload.currentTime)}`);
    }
  }

  async statusNotification(connectorId = this.config.connectorId, status = this.getConnector(connectorId).status): Promise<void> {
    const connector = this.getConnector(connectorId);
    const payload: Record<string, unknown> = {
      connectorId,
      errorCode: status === "Faulted" ? this.faultErrorCode(connector) : "NoError",
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
  }

  async authorize(idTag = this.config.idTag): Promise<AuthorizationDecision> {
    if (this.client.isConnected) {
      const response = await this.send("Authorize", { idTag });
      const idTagInfo = this.readIdTagInfo(response.payload.idTagInfo);
      this.rememberAuthorization(idTag, idTagInfo);

      const accepted = this.isAcceptedIdTagInfo(idTagInfo);
      this.log(accepted ? "success" : "warn", `Authorization status: ${idTagInfo.status} (${idTag})`);
      return { accepted, idTagInfo, source: "CentralSystem" };
    }

    const decision = this.authorizeLocally(idTag, true);
    this.log(
      decision.accepted ? "success" : "warn",
      `Offline authorization status: ${decision.idTagInfo.status} (${idTag}, ${decision.source})`
    );
    return decision;
  }

  async toggleTransaction(connectorId = this.config.connectorId, idTag = this.config.idTag): Promise<void> {
    const connector = this.getConnector(connectorId);
    if (connector.transactionId) {
      await this.stopTransaction(connectorId, "Local", idTag);
      return;
    }

    await this.startTransaction(connectorId, idTag);
  }

  async startTransaction(connectorId = this.config.connectorId, idTag = this.config.idTag): Promise<void> {
    const connector = this.getConnector(connectorId);

    if (connector.transactionId) {
      this.log("warn", `Connector ${connectorId} already has transaction ${connector.transactionId}`);
      return;
    }

    if (connector.availability === "Inoperative" || connector.status === "Unavailable") {
      this.log("warn", `Connector ${connectorId} is unavailable`);
      return;
    }

    if (connector.status === "Faulted") {
      this.log("warn", `Connector ${connectorId} is faulted`);
      return;
    }

    if (connector.status === "Finishing") {
      this.log("warn", `Connector ${connectorId} is finishing; unplug before starting a new transaction`);
      return;
    }

    if (connector.reservationId && connector.reservationIdTag !== idTag) {
      this.log("warn", `Connector ${connectorId} is reserved for another idTag`);
      return;
    }

    if (!connector.evConnected && connector.status === "Available") {
      this.patchConnector(connectorId, { evConnected: true, status: "Preparing" });
      void this.runSafely(() => this.statusNotification(connectorId, "Preparing"));
    } else if (connector.status === "Available") {
      this.patchConnector(connectorId, { status: "Preparing" });
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

    const localPreAuthorize = this.readBooleanConfiguration("LocalPreAuthorize", false);
    if (localPreAuthorize) {
      const decision = this.authorizeLocally(idTag, false);
      if (!decision.accepted && decision.source !== "None") {
        this.log("warn", `StartTransaction blocked by local authorization: ${decision.idTagInfo.status} (${idTag})`);
        return;
      }
    }

    const response = await this.send("StartTransaction", payload);
    const idTagInfo = this.readIdTagInfo(response.payload.idTagInfo);
    this.rememberAuthorization(idTag, idTagInfo);

    if (!this.isAcceptedIdTagInfo(idTagInfo)) {
      this.log("warn", `StartTransaction rejected by Central System: ${idTagInfo.status} (${idTag})`);
      return;
    }

    const transactionId = this.readNumber(response.payload.transactionId, Date.now());
    this.clearReservationTimer(connectorId);
    this.patchConnector(connectorId, {
      transactionId,
      reservationId: undefined,
      reservationIdTag: undefined,
      reservationParentIdTag: undefined,
      reservationExpiryDate: undefined,
      lastIdTag: idTag,
      status: "Charging"
    });
    this.savePersistedState();
    void this.runSafely(() => this.statusNotification(connectorId, "Charging"));
  }

  async stopTransaction(connectorId = this.config.connectorId, reason: StopReason = "Local", idTag?: string): Promise<void> {
    const connector = this.getConnector(connectorId);

    if (!connector.transactionId) {
      this.log("warn", `Connector ${connectorId} has no active transaction`);
      return;
    }

    const transactionId = connector.transactionId;
    const payload: Record<string, unknown> = {
      meterStop: Math.round(connector.meterWh),
      timestamp: new Date().toISOString(),
      transactionId,
      reason,
      transactionData: [this.buildMeterValue(connectorId, "Transaction.End", this.configuredMeasurands("StopTxnSampledData"))]
    };

    if (idTag) {
      payload.idTag = idTag;
    }

    this.patchConnector(connectorId, { status: "Finishing" });
    void this.runSafely(() => this.statusNotification(connectorId, "Finishing"));

    await this.send("StopTransaction", payload);

    const evConnected = reason !== "EVDisconnected" && connector.evConnected;
    const status: ConnectorStatus = evConnected ? "Finishing" : "Available";
    this.patchConnector(connectorId, { transactionId: undefined, evConnected, status });
    void this.runSafely(() => this.statusNotification(connectorId, status));
  }

  async plugIn(connectorId = this.config.connectorId): Promise<void> {
    const connector = this.getConnector(connectorId);

    if (connector.evConnected) {
      this.log("warn", `Connector ${connectorId} already has EV connected`);
      return;
    }

    if (connector.availability === "Inoperative" || connector.status === "Unavailable" || connector.status === "Faulted") {
      this.log("warn", `Connector ${connectorId} cannot accept EV while ${connector.status}`);
      return;
    }

    const status: ConnectorStatus = connector.reservationId ? "Reserved" : "Preparing";
    this.patchConnector(connectorId, { evConnected: true, status });
    await this.statusNotification(connectorId, status);
  }

  async unplug(connectorId = this.config.connectorId): Promise<void> {
    const connector = this.getConnector(connectorId);

    if (connector.transactionId) {
      await this.stopTransaction(connectorId, "EVDisconnected", connector.lastIdTag);
      return;
    }

    if (!connector.evConnected && connector.status !== "Finishing") {
      this.log("warn", `Connector ${connectorId} has no EV connected`);
      return;
    }

    const status: ConnectorStatus =
      connector.availability === "Inoperative" || connector.status === "Unavailable" ? "Unavailable" : "Available";
    this.patchConnector(connectorId, { evConnected: false, status });
    await this.statusNotification(connectorId, status);
  }

  async setFault(
    connectorId = this.config.connectorId,
    errorCode: ChargePointErrorCode = "OtherError",
    info?: string,
    vendorErrorCode?: string
  ): Promise<void> {
    const connector = this.getConnector(connectorId);

    if (connector.transactionId) {
      await this.stopTransaction(connectorId, "Other", connector.lastIdTag);
    }

    this.patchConnector(connectorId, {
      status: "Faulted",
      errorCode: errorCode === "NoError" ? "OtherError" : errorCode,
      info: trimOptional(info, 50),
      vendorId: this.config.vendor,
      vendorErrorCode: trimOptional(vendorErrorCode, 50)
    });
    await this.statusNotification(connectorId, "Faulted");
  }

  async clearFault(connectorId = this.config.connectorId): Promise<void> {
    const connector = this.getConnector(connectorId);
    const status = this.statusAfterFaultClear(connector);

    this.patchConnector(connectorId, {
      status,
      errorCode: "NoError",
      info: undefined,
      vendorId: undefined,
      vendorErrorCode: undefined
    });
    await this.statusNotification(connectorId, status);
  }

  async meterValues(connectorId = this.config.connectorId, deltaWh = 120, sampledData?: string): Promise<void> {
    if (sampledData !== undefined && !this.isValidSampledDataConfiguration(sampledData)) {
      this.log("warn", `Invalid MeterValues sampled data: ${sampledData}`);
      return;
    }

    const connector = this.getConnector(connectorId);
    const nextMeter = connector.meterWh + deltaWh;
    const measurands =
      sampledData === undefined ? this.configuredMeasurands("MeterValuesSampledData") : this.readConfiguredMeasurands(sampledData);
    this.patchConnector(connectorId, { meterWh: nextMeter });
    this.savePersistedState();

    const payload: Record<string, unknown> = {
      connectorId,
      meterValue: [this.buildMeterValue(connectorId, "Sample.Periodic", measurands)]
    };

    if (connector.transactionId) {
      payload.transactionId = connector.transactionId;
    }

    await this.send("MeterValues", payload);
  }

  async dataTransfer(payload: Record<string, unknown> = { vendorId: this.config.vendor }): Promise<void> {
    const response = await this.send("DataTransfer", payload);
    const status = this.readString(response.payload.status, "Unknown");
    this.log(status === "Accepted" ? "success" : "warn", `DataTransfer status: ${status}`);

    if (response.payload.data) {
      this.log("info", `DataTransfer response data: ${String(response.payload.data)}`);
    }
  }

  async firmwareStatusNotification(status = "Installed"): Promise<void> {
    await this.send("FirmwareStatusNotification", { status });
  }

  async diagnosticsStatusNotification(status = "Uploaded"): Promise<void> {
    await this.send("DiagnosticsStatusNotification", { status });
  }

  async cycleStatus(connectorId = this.config.connectorId): Promise<void> {
    const connector = this.getConnector(connectorId);
    const currentIndex = STATUS_SEQUENCE.indexOf(connector.status);
    const next = STATUS_SEQUENCE[(currentIndex + 1) % STATUS_SEQUENCE.length] ?? "Available";
    this.patchConnector(connectorId, {
      status: next,
      errorCode: next === "Faulted" ? "OtherError" : "NoError",
      info: next === "Faulted" ? "Manual fault" : undefined,
      vendorId: next === "Faulted" ? this.config.vendor : undefined,
      vendorErrorCode: undefined
    });
    await this.statusNotification(connectorId, next);
  }

  addConnector(): number {
    const nextId = Math.max(0, ...this.connectors.keys()) + 1;
    this.connectors.set(nextId, this.createConnector(nextId));
    this.setConfigurationValue("NumberOfConnectors", String(this.connectors.size));
    this.refreshConnectorSnapshot();
    this.savePersistedState();
    void this.runSafely(() => this.statusNotification(nextId, "Available"));
    return nextId;
  }

  private bindClientEvents(): void {
    this.client.on("connected", () => {
      this.patchState({ connected: true });
      this.log("success", "Connected");
      void this.runSafely(() => this.boot());
    });

    this.client.on("disconnected", (code, reason) => {
      this.stopHeartbeatTimer();
      this.patchState({ connected: false, booted: false });
      this.log("warn", `Disconnected${code ? ` (${code})` : ""}${reason ? `: ${reason}` : ""}`);
    });

    this.client.on("error", (error) => this.log("error", error.message));

    this.client.on("log", (direction, message) => {
      this.log(direction, this.describeMessage(message), JSON.stringify(message));
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
          this.client.reply(messageId, { fileName: "kirby-ocpp-diagnostics.log" });
          setTimeout(() => void this.runSafely(() => this.diagnosticsStatusNotification("Uploading")), 100);
          setTimeout(() => void this.runSafely(() => this.diagnosticsStatusNotification("Uploaded")), 500);
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
          setTimeout(() => void this.runSafely(() => this.firmwareStatusNotification("Downloading")), 100);
          setTimeout(() => void this.runSafely(() => this.firmwareStatusNotification("Downloaded")), 500);
          setTimeout(() => void this.runSafely(() => this.firmwareStatusNotification("Installed")), 900);
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
    const reservationId = this.readNumber(payload.reservationId, -1);
    const connector = this.findConnectorByReservation(reservationId);
    if (!connector) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    this.clearReservation(connector.id);
    this.client.reply(messageId, { status: "Accepted" });
    void this.runSafely(() => this.statusNotification(connector.id, this.getConnector(connector.id).status));
  }

  private async handleChangeAvailability(messageId: string, payload: Record<string, unknown>): Promise<void> {
    const availability = this.readString(payload.type, "Operative") as AvailabilityType;
    const connectorId = this.readNumber(payload.connectorId, 0);
    const requestedConnector = connectorId === 0 ? undefined : this.findConnector(connectorId);
    if (connectorId !== 0 && !requestedConnector) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }
    const connectors: MutableConnectorState[] = connectorId === 0 ? this.getConnectors() : [requestedConnector ?? this.getConnector(connectorId)];

    if (connectors.some((connector) => connector.transactionId)) {
      for (const connector of connectors) {
        this.patchConnector(connector.id, { availability });
      }
      this.client.reply(messageId, { status: "Scheduled" });
      return;
    }

    for (const connector of connectors) {
      this.patchConnector(connector.id, {
        availability,
        status: availability === "Operative" ? "Available" : "Unavailable"
      });
    }

    this.client.reply(messageId, { status: "Accepted" });
    for (const connector of connectors) {
      void this.runSafely(() => this.statusNotification(connector.id, this.getConnector(connector.id).status));
    }
  }

  private handleChangeConfiguration(messageId: string, payload: Record<string, unknown>): void {
    const key = this.readString(payload.key, "");
    const value = this.readString(payload.value, "");
    const existing = this.configuration.get(key);

    if (!existing) {
      this.client.reply(messageId, { status: "NotSupported" });
      return;
    }

    if (existing.readonly) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    if (SAMPLED_DATA_CONFIGURATION_KEYS.has(key) && !this.isValidSampledDataConfiguration(value)) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    this.setConfigurationValue(key, value);
    this.savePersistedState();
    this.client.reply(messageId, { status: "Accepted" });

    if (key === "HeartbeatInterval") {
      const interval = Number.parseInt(value, 10);
      if (Number.isFinite(interval) && interval > 0) {
        this.startHeartbeatTimer(interval);
      }
    }
  }

  private handleDataTransfer(messageId: string, payload: Record<string, unknown>): void {
    const vendorId = this.readString(payload.vendorId, "");
    if (vendorId !== this.config.vendor) {
      this.client.reply(messageId, { status: "UnknownVendorId" });
      return;
    }

    this.client.reply(messageId, { status: "Accepted", data: this.readString(payload.data, "") });
  }

  private handleGetConfiguration(messageId: string, payload: Record<string, unknown>): void {
    const requestedKeys = Array.isArray(payload.key) ? payload.key.filter((key): key is string => typeof key === "string") : [];
    const keys = requestedKeys.length > 0 ? requestedKeys : [...this.configuration.keys()];
    const configurationKey = [];
    const unknownKey = [];

    for (const key of keys) {
      const entry = this.configuration.get(key);
      if (!entry) {
        unknownKey.push(key);
        continue;
      }

      configurationKey.push({ key, readonly: entry.readonly, value: entry.value });
    }

    this.client.reply(messageId, { configurationKey, unknownKey });
  }

  private handleSetChargingProfile(messageId: string, payload: Record<string, unknown>): void {
    const connectorId = this.readNumber(payload.connectorId, -1);
    const profile = this.readChargingProfile(payload.csChargingProfiles);

    if (!profile) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    if (!this.canStoreChargingProfile(connectorId, profile)) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    this.chargingProfiles = this.chargingProfiles.filter(
      (entry) => entry.profile.chargingProfileId !== profile.chargingProfileId
    );
    this.chargingProfiles.push({ connectorId, profile });
    this.savePersistedState();
    this.client.reply(messageId, { status: "Accepted" });
  }

  private handleClearChargingProfile(messageId: string, payload: Record<string, unknown>): void {
    const before = this.chargingProfiles.length;
    this.chargingProfiles = this.chargingProfiles.filter((entry) => !this.matchesChargingProfileFilter(entry, payload));
    const removed = before - this.chargingProfiles.length;

    if (removed > 0) {
      this.savePersistedState();
    }

    this.client.reply(messageId, { status: removed > 0 ? "Accepted" : "Unknown" });
  }

  private handleGetCompositeSchedule(messageId: string, payload: Record<string, unknown>): void {
    const connectorId = this.readNumber(payload.connectorId, -1);
    const duration = this.readNumber(payload.duration, 0);
    const requestedUnit = this.readChargingRateUnit(payload.chargingRateUnit);
    const connector = this.findConnector(connectorId);

    if (!connector || duration <= 0) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    const scheduleStart = new Date();
    const activeProfiles = this.applicableChargingProfiles(connectorId, scheduleStart);
    const unit = requestedUnit ?? activeProfiles[0]?.profile.chargingSchedule.chargingRateUnit ?? "A";
    const limits = activeProfiles
      .filter((entry) => entry.profile.chargingSchedule.chargingRateUnit === unit)
      .map((entry) => this.limitAt(entry.profile, scheduleStart))
      .filter((limit): limit is number => typeof limit === "number" && Number.isFinite(limit));

    if (limits.length === 0) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    const limit = Math.min(...limits);
    this.client.reply(messageId, {
      status: "Accepted",
      connectorId,
      scheduleStart: scheduleStart.toISOString(),
      chargingSchedule: {
        duration,
        startSchedule: scheduleStart.toISOString(),
        chargingRateUnit: unit,
        chargingSchedulePeriod: [{ startPeriod: 0, limit }]
      }
    });
  }

  private handleSendLocalList(messageId: string, payload: Record<string, unknown>): void {
    const listVersion = this.readNumber(payload.listVersion, this.state.localListVersion);
    const updateType = this.readString(payload.updateType, "Full");
    const entries = Array.isArray(payload.localAuthorizationList) ? payload.localAuthorizationList : [];

    if (updateType === "Differential" && listVersion <= this.state.localListVersion) {
      this.client.reply(messageId, { status: "VersionMismatch" });
      return;
    }

    if (updateType === "Full") {
      this.localAuthorizationList.clear();
    }

    for (const item of entries) {
      const entry = this.readObject(item);
      const idTag = this.readString(entry.idTag, "");
      if (!idTag) {
        continue;
      }

      if (updateType === "Differential" && !("idTagInfo" in entry)) {
        this.localAuthorizationList.delete(idTag);
        continue;
      }

      this.localAuthorizationList.set(
        idTag,
        "idTagInfo" in entry ? this.readObject(entry.idTagInfo) : undefined
      );
    }

    this.patchState({ localListVersion: listVersion });
    this.savePersistedState();
    this.client.reply(messageId, { status: "Accepted" });
  }

  private async handleRemoteStartTransaction(messageId: string, payload: Record<string, unknown>): Promise<void> {
    const requestedConnector = this.readNumber(payload.connectorId, 0);
    const connector = requestedConnector > 0 ? this.findConnector(requestedConnector) : this.findAvailableConnector();
    const idTag = this.readString(payload.idTag, this.config.idTag);
    const canStart =
      Boolean(connector) &&
      !connector?.transactionId &&
      connector?.availability === "Operative" &&
      connector.status !== "Unavailable" &&
      connector.status !== "Faulted" &&
      connector.status !== "Reserved";

    if (!canStart) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    if (this.readBooleanConfiguration("AuthorizeRemoteTxRequests", false)) {
      const decision = await this.authorize(idTag);
      if (!decision.accepted) {
        this.client.reply(messageId, { status: "Rejected" });
        return;
      }
    }

    this.client.reply(messageId, { status: "Accepted" });

    if (connector) {
      await this.startTransaction(connector.id, idTag);
    }
  }

  private async handleRemoteStopTransaction(messageId: string, payload: Record<string, unknown>): Promise<void> {
    const transactionId = this.readNumber(payload.transactionId, -1);
    const connector = this.findConnectorByTransaction(transactionId);
    const accepted = Boolean(connector);
    this.client.reply(messageId, { status: accepted ? "Accepted" : "Rejected" });

    if (accepted && connector) {
      await this.stopTransaction(connector.id, "Remote");
    }
  }

  private async handleReserveNow(messageId: string, payload: Record<string, unknown>): Promise<void> {
    const requestedConnectorId = this.readNumber(payload.connectorId, this.config.connectorId);
    const connector = requestedConnectorId === 0 ? this.findAvailableConnector() : this.findConnector(requestedConnectorId);
    const connectorId = connector?.id ?? requestedConnectorId;

    if (!connector) {
      this.client.reply(messageId, { status: "Unavailable" });
      return;
    }

    if (connector.transactionId) {
      this.client.reply(messageId, { status: "Occupied" });
      return;
    }

    if (connector.reservationId || connector.status === "Reserved") {
      this.client.reply(messageId, { status: "Occupied" });
      return;
    }

    if (connector.status === "Faulted") {
      this.client.reply(messageId, { status: "Faulted" });
      return;
    }

    if (connector.availability === "Inoperative" || connector.status === "Unavailable") {
      this.client.reply(messageId, { status: "Unavailable" });
      return;
    }

    const expiryDate = this.readString(payload.expiryDate, "");
    if (!expiryDate || Date.parse(expiryDate) <= Date.now()) {
      this.client.reply(messageId, { status: "Rejected" });
      return;
    }

    this.patchConnector(connectorId, {
      reservationId: this.readNumber(payload.reservationId, Date.now()),
      reservationIdTag: this.readString(payload.idTag, ""),
      reservationParentIdTag: this.readOptionalString(payload.parentIdTag),
      reservationExpiryDate: expiryDate,
      status: "Reserved"
    });
    this.scheduleReservationExpiry(connectorId);
    this.savePersistedState();
    this.client.reply(messageId, { status: "Accepted" });
    void this.runSafely(() => this.statusNotification(connectorId, "Reserved"));
  }

  private handleReset(messageId: string, payload: Record<string, unknown>): void {
    const resetType = this.readString(payload.type, "Soft");
    const reason: StopReason = resetType === "Hard" ? "HardReset" : "SoftReset";

    this.client.reply(messageId, { status: "Accepted" });

    void this.runSafely(async () => {
      for (const connector of this.getConnectors()) {
        if (connector.transactionId) {
          await this.stopTransaction(connector.id, reason);
        }
      }

      for (const connector of this.getConnectors()) {
        this.clearReservation(connector.id);
      }
      this.patchState({ booted: false });
      setTimeout(() => void this.runSafely(() => this.boot()), 500);
    });
  }

  private async handleTriggerMessage(messageId: string, payload: Record<string, unknown>): Promise<void> {
    const requestedMessage = this.readString(payload.requestedMessage, "");

    if (!this.canTriggerMessage(requestedMessage)) {
      this.client.reply(messageId, { status: "NotImplemented" });
      return;
    }

    this.client.reply(messageId, { status: "Accepted" });
    await this.triggerMessage(requestedMessage);
  }

  private async handleUnlockConnector(messageId: string, payload: Record<string, unknown> = {}): Promise<void> {
    const connectorId = this.readNumber(payload.connectorId, this.config.connectorId);
    const connector = this.findConnector(connectorId);

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

  private async triggerMessage(requestedMessage: string): Promise<void> {
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
        await this.meterValues(this.config.connectorId, 0);
        break;
      case "StatusNotification":
        await this.statusNotification(this.config.connectorId);
        break;
      default:
        this.log("warn", `TriggerMessage requested unsupported message: ${requestedMessage}`);
    }
  }

  private canTriggerMessage(requestedMessage: string): boolean {
    return [
      "BootNotification",
      "DiagnosticsStatusNotification",
      "FirmwareStatusNotification",
      "Heartbeat",
      "MeterValues",
      "StatusNotification"
    ].includes(requestedMessage);
  }

  private async send(action: string, payload: Record<string, unknown>) {
    try {
      const response = await this.client.call(action, payload);
      this.log("success", `${action} accepted`);
      return response;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log("error", message);
      throw error;
    }
  }

  private buildMeterValue(connectorId: number, context: MeterValueContext, measurands: Measurand[]): Record<string, unknown> {
    return {
      timestamp: new Date().toISOString(),
      sampledValue: measurands.map((measurand) => this.buildSampledValue(connectorId, context, measurand))
    };
  }

  private buildSampledValue(connectorId: number, context: MeterValueContext, measurand: Measurand): Record<string, unknown> {
    const sample = this.sampleForMeasurand(connectorId, measurand);
    const sampledValue: Record<string, unknown> = {
      value: sample.value,
      context,
      format: "Raw",
      measurand
    };

    if (sample.location) {
      sampledValue.location = sample.location;
    }
    if (sample.phase) {
      sampledValue.phase = sample.phase;
    }
    if (sample.unit) {
      sampledValue.unit = sample.unit;
    }

    return sampledValue;
  }

  private sampleForMeasurand(
    connectorId: number,
    measurand: Measurand
  ): { value: string; unit?: string; location?: string; phase?: string } {
    const connector = this.getConnector(connectorId);
    const charging = connector.status === "Charging";
    const activePowerW = charging ? 7200 : 0;
    const currentA = charging ? 32 : 0;

    switch (measurand) {
      case "Energy.Active.Import.Register":
        return { value: String(Math.round(connector.meterWh)), unit: "Wh", location: "Outlet" };
      case "Energy.Active.Export.Register":
        return { value: "0", unit: "Wh", location: "Outlet" };
      case "Energy.Reactive.Import.Register":
      case "Energy.Reactive.Export.Register":
        return { value: "0", unit: "varh", location: "Outlet" };
      case "Energy.Active.Import.Interval":
        return { value: charging ? "120" : "0", unit: "Wh", location: "Outlet" };
      case "Energy.Active.Export.Interval":
        return { value: "0", unit: "Wh", location: "Outlet" };
      case "Energy.Reactive.Import.Interval":
      case "Energy.Reactive.Export.Interval":
        return { value: "0", unit: "varh", location: "Outlet" };
      case "Power.Active.Import":
        return { value: String(activePowerW), unit: "W", location: "Outlet" };
      case "Power.Active.Export":
        return { value: "0", unit: "W", location: "Outlet" };
      case "Power.Offered":
        return { value: "7400", unit: "W", location: "Outlet" };
      case "Power.Reactive.Import":
      case "Power.Reactive.Export":
        return { value: "0", unit: "var", location: "Outlet" };
      case "Power.Factor":
        return { value: charging ? "1" : "0", location: "Outlet" };
      case "Current.Import":
        return { value: String(currentA), unit: "A", location: "Outlet", phase: "L1" };
      case "Current.Export":
        return { value: "0", unit: "A", location: "Outlet", phase: "L1" };
      case "Current.Offered":
        return { value: "32", unit: "A", location: "Outlet", phase: "L1" };
      case "Voltage":
        return { value: "230", unit: "V", location: "Outlet", phase: "L1-N" };
      case "Temperature":
        return { value: charging ? "34" : "25", unit: "Celsius", location: "Body" };
      case "SoC":
        return { value: charging ? "80" : "0", unit: "Percent", location: "EV" };
      case "Frequency":
        return { value: "50", location: "Outlet" };
      case "RPM":
        return { value: "0", location: "Body" };
    }
  }

  private seedConfiguration(): void {
    this.configuration.set("HeartbeatInterval", {
      readonly: false,
      value: String(this.config.heartbeatIntervalSeconds)
    });
    this.configuration.set("MeterValuesSampledData", {
      readonly: false,
      value: "Energy.Active.Import.Register"
    });
    this.configuration.set("StopTxnSampledData", {
      readonly: false,
      value: "Energy.Active.Import.Register"
    });
    this.configuration.set("MeterValueSampleInterval", {
      readonly: false,
      value: "60"
    });
    this.configuration.set("NumberOfConnectors", {
      readonly: true,
      value: String(this.connectors.size || this.config.connectorCount)
    });
    this.configuration.set("AuthorizeRemoteTxRequests", {
      readonly: false,
      value: "false"
    });
    this.configuration.set("AuthorizationCacheEnabled", {
      readonly: false,
      value: "true"
    });
    this.configuration.set("AllowOfflineTxForUnknownId", {
      readonly: false,
      value: "false"
    });
    this.configuration.set("LocalAuthorizeOffline", {
      readonly: false,
      value: "false"
    });
    this.configuration.set("LocalPreAuthorize", {
      readonly: false,
      value: "false"
    });
    this.configuration.set("ChargePointVendor", {
      readonly: true,
      value: this.config.vendor
    });
    this.configuration.set("ChargePointModel", {
      readonly: true,
      value: this.config.model
    });
  }

  private setConfigurationValue(key: string, value: string): void {
    const existing = this.configuration.get(key);
    if (!existing) {
      return;
    }

    this.configuration.set(key, { ...existing, value });
  }

  private configuredMeasurands(key: "MeterValuesSampledData" | "StopTxnSampledData"): Measurand[] {
    return this.readConfiguredMeasurands(this.configuration.get(key)?.value);
  }

  private readConfiguredMeasurands(value: string | undefined): Measurand[] {
    const measurands = (value ?? "")
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean)
      .filter((item): item is Measurand => MEASURANDS.has(item as Measurand));

    return measurands.length > 0 ? measurands : DEFAULT_SAMPLED_DATA;
  }

  private isValidSampledDataConfiguration(value: string): boolean {
    const items = value
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean);

    return items.length > 0 && items.every((item) => MEASURANDS.has(item as Measurand));
  }

  private readBooleanConfiguration(key: string, fallback: boolean): boolean {
    const value = this.configuration.get(key)?.value;
    if (value === undefined) {
      return fallback;
    }

    return value.toLowerCase() === "true";
  }

  private authorizeLocally(idTag: string, requireOfflinePermission: boolean): AuthorizationDecision {
    if (requireOfflinePermission && !this.readBooleanConfiguration("LocalAuthorizeOffline", false)) {
      return {
        accepted: false,
        idTagInfo: { status: "Invalid" },
        source: "None"
      };
    }

    const localListInfo = this.localAuthorizationList.get(idTag);
    if (this.localAuthorizationList.has(idTag)) {
      const idTagInfo = this.normalizeIdTagInfo(localListInfo);
      return {
        accepted: this.isAcceptedIdTagInfo(idTagInfo),
        idTagInfo,
        source: "LocalList"
      };
    }

    if (this.readBooleanConfiguration("AuthorizationCacheEnabled", true) && this.authorizationCache.has(idTag)) {
      const idTagInfo = this.normalizeIdTagInfo(this.authorizationCache.get(idTag));
      return {
        accepted: this.isAcceptedIdTagInfo(idTagInfo),
        idTagInfo,
        source: "AuthorizationCache"
      };
    }

    if (requireOfflinePermission && this.readBooleanConfiguration("AllowOfflineTxForUnknownId", false)) {
      return {
        accepted: true,
        idTagInfo: { status: "Accepted" },
        source: "OfflineUnknown"
      };
    }

    return {
      accepted: false,
      idTagInfo: { status: "Invalid" },
      source: "None"
    };
  }

  private rememberAuthorization(idTag: string, idTagInfo: IdTagInfo): void {
    if (!this.readBooleanConfiguration("AuthorizationCacheEnabled", true)) {
      return;
    }

    this.authorizationCache.set(idTag, this.serializeIdTagInfo(idTagInfo));
    this.savePersistedState();
  }

  private readIdTagInfo(value: unknown): IdTagInfo {
    return this.normalizeIdTagInfo(this.readObject(value));
  }

  private normalizeIdTagInfo(value: Record<string, unknown> | undefined): IdTagInfo {
    const status = this.readIdTagStatus(value?.status, "Accepted");
    const expiryDate = this.readOptionalString(value?.expiryDate);
    const parentIdTag = this.readOptionalString(value?.parentIdTag);
    const idTagInfo: IdTagInfo = {
      status: expiryDate && Date.parse(expiryDate) <= Date.now() ? "Expired" : status
    };

    if (expiryDate) {
      idTagInfo.expiryDate = expiryDate;
    }

    if (parentIdTag) {
      idTagInfo.parentIdTag = parentIdTag;
    }

    return idTagInfo;
  }

  private serializeIdTagInfo(idTagInfo: IdTagInfo): Record<string, unknown> {
    const output: Record<string, unknown> = { status: idTagInfo.status };
    if (idTagInfo.expiryDate) {
      output.expiryDate = idTagInfo.expiryDate;
    }
    if (idTagInfo.parentIdTag) {
      output.parentIdTag = idTagInfo.parentIdTag;
    }
    return output;
  }

  private isAcceptedIdTagInfo(idTagInfo: IdTagInfo): boolean {
    return idTagInfo.status === "Accepted";
  }

  private readIdTagStatus(value: unknown, fallback: IdTagStatus): IdTagStatus {
    return value === "Accepted" ||
      value === "Blocked" ||
      value === "Expired" ||
      value === "Invalid" ||
      value === "ConcurrentTx"
      ? value
      : fallback;
  }

  private readChargingProfile(value: unknown): ChargingProfile | undefined {
    const payload = this.readObject(value);
    const chargingSchedule = this.readChargingSchedule(payload.chargingSchedule);
    const chargingProfilePurpose = this.readChargingProfilePurpose(payload.chargingProfilePurpose);
    const chargingProfileKind = this.readChargingProfileKind(payload.chargingProfileKind);

    if (!chargingSchedule || !chargingProfilePurpose || !chargingProfileKind) {
      return undefined;
    }

    const profile: ChargingProfile = {
      chargingProfileId: this.readNumber(payload.chargingProfileId, -1),
      stackLevel: this.readNumber(payload.stackLevel, -1),
      chargingProfilePurpose,
      chargingProfileKind,
      chargingSchedule
    };

    const transactionId = this.readOptionalNumber(payload.transactionId);
    const recurrencyKind = this.readRecurrencyKind(payload.recurrencyKind);
    const validFrom = this.readOptionalString(payload.validFrom);
    const validTo = this.readOptionalString(payload.validTo);

    if (transactionId !== undefined) {
      profile.transactionId = transactionId;
    }
    if (recurrencyKind) {
      profile.recurrencyKind = recurrencyKind;
    }
    if (validFrom) {
      profile.validFrom = validFrom;
    }
    if (validTo) {
      profile.validTo = validTo;
    }

    return profile.chargingProfileId >= 0 && profile.stackLevel >= 0 ? profile : undefined;
  }

  private readChargingSchedule(value: unknown): ChargingSchedule | undefined {
    const payload = this.readObject(value);
    const chargingRateUnit = this.readChargingRateUnit(payload.chargingRateUnit);
    const periods = Array.isArray(payload.chargingSchedulePeriod)
      ? payload.chargingSchedulePeriod.map((period) => this.readChargingSchedulePeriod(period)).filter(isDefined)
      : [];

    if (!chargingRateUnit || periods.length === 0) {
      return undefined;
    }

    const schedule: ChargingSchedule = {
      chargingRateUnit,
      chargingSchedulePeriod: periods.sort((left, right) => left.startPeriod - right.startPeriod)
    };
    const duration = this.readOptionalNumber(payload.duration);
    const startSchedule = this.readOptionalString(payload.startSchedule);
    const minChargingRate = this.readOptionalNumber(payload.minChargingRate);

    if (duration !== undefined) {
      schedule.duration = duration;
    }
    if (startSchedule) {
      schedule.startSchedule = startSchedule;
    }
    if (minChargingRate !== undefined) {
      schedule.minChargingRate = minChargingRate;
    }

    return schedule;
  }

  private readChargingSchedulePeriod(value: unknown): ChargingSchedulePeriod | undefined {
    const payload = this.readObject(value);
    const startPeriod = this.readOptionalNumber(payload.startPeriod);
    const limit = this.readOptionalNumber(payload.limit);

    if (startPeriod === undefined || limit === undefined) {
      return undefined;
    }

    const period: ChargingSchedulePeriod = { startPeriod, limit };
    const numberPhases = this.readOptionalNumber(payload.numberPhases);
    if (numberPhases !== undefined) {
      period.numberPhases = numberPhases;
    }

    return period;
  }

  private readChargingProfileEntry(value: unknown): ChargingProfileEntry | undefined {
    const payload = this.readObject(value);
    const connectorId = this.readOptionalNumber(payload.connectorId);
    const profile = this.readChargingProfile(payload.profile);

    if (connectorId === undefined || !profile) {
      return undefined;
    }

    return { connectorId, profile };
  }

  private readLegacyChargingProfileEntry(value: unknown): ChargingProfileEntry | undefined {
    const profile = this.readChargingProfile(value);
    return profile ? { connectorId: this.config.connectorId, profile } : undefined;
  }

  private readChargingProfilePurpose(value: unknown): ChargingProfilePurpose | undefined {
    return value === "ChargePointMaxProfile" || value === "TxDefaultProfile" || value === "TxProfile" ? value : undefined;
  }

  private readChargingProfileKind(value: unknown): ChargingProfileKind | undefined {
    return value === "Absolute" || value === "Recurring" || value === "Relative" ? value : undefined;
  }

  private readChargingRateUnit(value: unknown): ChargingRateUnit | undefined {
    return value === "A" || value === "W" ? value : undefined;
  }

  private readRecurrencyKind(value: unknown): "Daily" | "Weekly" | undefined {
    return value === "Daily" || value === "Weekly" ? value : undefined;
  }

  private canStoreChargingProfile(connectorId: number, profile: ChargingProfile): boolean {
    if (!Number.isInteger(connectorId) || connectorId < 0) {
      return false;
    }

    if (profile.chargingProfilePurpose === "ChargePointMaxProfile") {
      return connectorId === 0;
    }

    if (connectorId !== 0 && !this.findConnector(connectorId)) {
      return false;
    }

    if (profile.chargingProfilePurpose === "TxProfile") {
      const connector = connectorId > 0 ? this.findConnector(connectorId) : undefined;
      if (!connector?.transactionId) {
        return false;
      }

      return profile.transactionId === undefined || profile.transactionId === connector.transactionId;
    }

    return true;
  }

  private matchesChargingProfileFilter(entry: ChargingProfileEntry, payload: Record<string, unknown>): boolean {
    const hasFilter =
      "id" in payload || "connectorId" in payload || "chargingProfilePurpose" in payload || "stackLevel" in payload;

    if (!hasFilter) {
      return true;
    }

    const id = this.readOptionalNumber(payload.id);
    const connectorId = this.readOptionalNumber(payload.connectorId);
    const purpose = this.readChargingProfilePurpose(payload.chargingProfilePurpose);
    const stackLevel = this.readOptionalNumber(payload.stackLevel);

    return (
      (id === undefined || entry.profile.chargingProfileId === id) &&
      (connectorId === undefined || entry.connectorId === connectorId) &&
      (purpose === undefined || entry.profile.chargingProfilePurpose === purpose) &&
      (stackLevel === undefined || entry.profile.stackLevel === stackLevel)
    );
  }

  private applicableChargingProfiles(connectorId: number, at: Date): ChargingProfileEntry[] {
    const connector = this.getConnector(connectorId);

    return this.chargingProfiles
      .filter((entry) => entry.connectorId === 0 || entry.connectorId === connectorId)
      .filter((entry) => this.isChargingProfileActive(entry.profile, at))
      .filter((entry) => {
        if (entry.profile.chargingProfilePurpose !== "TxProfile") {
          return true;
        }

        return (
          Boolean(connector.transactionId) &&
          (entry.profile.transactionId === undefined || entry.profile.transactionId === connector.transactionId)
        );
      })
      .sort((left, right) => right.profile.stackLevel - left.profile.stackLevel);
  }

  private isChargingProfileActive(profile: ChargingProfile, at: Date): boolean {
    const validFrom = profile.validFrom ? Date.parse(profile.validFrom) : undefined;
    const validTo = profile.validTo ? Date.parse(profile.validTo) : undefined;
    const time = at.getTime();

    return (
      (validFrom === undefined || Number.isNaN(validFrom) || time >= validFrom) &&
      (validTo === undefined || Number.isNaN(validTo) || time <= validTo)
    );
  }

  private limitAt(profile: ChargingProfile, at: Date): number | undefined {
    const schedule = profile.chargingSchedule;
    const elapsedSeconds = this.scheduleElapsedSeconds(profile, at);

    if (schedule.duration !== undefined && elapsedSeconds > schedule.duration) {
      return undefined;
    }

    let activePeriod: ChargingSchedulePeriod | undefined;
    for (const period of schedule.chargingSchedulePeriod) {
      if (period.startPeriod <= elapsedSeconds) {
        activePeriod = period;
      }
    }

    return activePeriod?.limit;
  }

  private scheduleElapsedSeconds(profile: ChargingProfile, at: Date): number {
    if (profile.chargingProfileKind === "Relative") {
      return 0;
    }

    const start = profile.chargingSchedule.startSchedule
      ? Date.parse(profile.chargingSchedule.startSchedule)
      : profile.validFrom
        ? Date.parse(profile.validFrom)
        : at.getTime();

    const rawElapsed = Math.max(0, Math.floor((at.getTime() - (Number.isNaN(start) ? at.getTime() : start)) / 1000));

    if (profile.chargingProfileKind !== "Recurring") {
      return rawElapsed;
    }

    const periodSeconds = profile.recurrencyKind === "Weekly" ? 7 * 24 * 60 * 60 : 24 * 60 * 60;
    return rawElapsed % periodSeconds;
  }

  private initializeConnectors(count: number): void {
    const requestedCount = Number.isInteger(count) && count > 0 ? count : 1;
    const connectorCount = Math.max(requestedCount, this.config.connectorId);
    for (let connectorId = 1; connectorId <= connectorCount; connectorId += 1) {
      this.connectors.set(connectorId, this.createConnector(connectorId));
    }
    this.setConfigurationValue("NumberOfConnectors", String(this.connectors.size));
  }

  private createConnector(id: number): MutableConnectorState {
    return {
      id,
      status: "Available",
      availability: "Operative",
      errorCode: "NoError",
      evConnected: false,
      meterWh: 0
    };
  }

  private getConnector(id: number): MutableConnectorState {
    const connector = this.connectors.get(id);
    if (!connector) {
      throw new Error(`Connector ${id} does not exist`);
    }

    return connector;
  }

  private findConnector(id: number): MutableConnectorState | undefined {
    return this.connectors.get(id);
  }

  private getConnectors(): MutableConnectorState[] {
    return [...this.connectors.values()].sort((left, right) => left.id - right.id);
  }

  private patchConnector(id: number, patch: Partial<MutableConnectorState>): void {
    const connector = this.getConnector(id);
    Object.assign(connector, patch);
    this.refreshConnectorSnapshot();
  }

  private refreshConnectorSnapshot(): void {
    const connectors = this.getConnectors().map((connector) => ({ ...connector }));
    const primary = this.connectors.get(this.config.connectorId) ?? connectors[0];

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

  private findAvailableConnector(): MutableConnectorState | undefined {
    return this.getConnectors().find(
      (connector) =>
        !connector.transactionId &&
        connector.availability === "Operative" &&
        connector.status === "Available"
    );
  }

  private findConnectorByTransaction(transactionId: number): MutableConnectorState | undefined {
    return this.getConnectors().find((connector) => connector.transactionId === transactionId);
  }

  private findConnectorByReservation(reservationId: number): MutableConnectorState | undefined {
    return this.getConnectors().find((connector) => connector.reservationId === reservationId);
  }

  private clearReservation(connectorId: number): void {
    const connector = this.getConnector(connectorId);
    this.clearReservationTimer(connectorId);
    this.patchConnector(connectorId, {
      reservationId: undefined,
      reservationIdTag: undefined,
      reservationParentIdTag: undefined,
      reservationExpiryDate: undefined,
      status: this.statusAfterReservationClear(connector)
    });
    this.savePersistedState();
  }

  private scheduleReservationExpiry(connectorId: number): void {
    const connector = this.getConnector(connectorId);
    this.clearReservationTimer(connectorId);

    if (!connector.reservationExpiryDate) {
      return;
    }

    const delay = Date.parse(connector.reservationExpiryDate) - Date.now();
    if (delay <= 0) {
      this.expireReservation(connectorId);
      return;
    }

    this.reservationTimers.set(
      connectorId,
      setTimeout(() => this.expireReservation(connectorId), delay)
    );
  }

  private clearReservationTimer(connectorId: number): void {
    const timer = this.reservationTimers.get(connectorId);
    if (!timer) {
      return;
    }

    clearTimeout(timer);
    this.reservationTimers.delete(connectorId);
  }

  private expireReservation(connectorId: number): void {
    const connector = this.findConnector(connectorId);
    if (!connector?.reservationId) {
      return;
    }

    this.log("warn", `Reservation ${connector.reservationId} expired on connector ${connectorId}`);
    this.clearReservation(connectorId);
    void this.runSafely(() => this.statusNotification(connectorId, this.getConnector(connectorId).status));
  }

  private statusAfterReservationClear(connector: MutableConnectorState): ConnectorStatus {
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

  private faultErrorCode(connector: MutableConnectorState): ChargePointErrorCode {
    return connector.errorCode === "NoError" ? "OtherError" : connector.errorCode;
  }

  private statusAfterFaultClear(connector: MutableConnectorState): ConnectorStatus {
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

    const persistedConnectorCount = Math.max(persisted.connectorCount ?? 1, this.config.connectorId);
    while (this.connectors.size < persistedConnectorCount) {
      const nextId = Math.max(0, ...this.connectors.keys()) + 1;
      this.connectors.set(nextId, this.createConnector(nextId));
    }
    this.setConfigurationValue("NumberOfConnectors", String(this.connectors.size));

    this.state.localListVersion = persisted.localListVersion;
    this.chargingProfiles = persisted.chargingProfiles
      .map((entry) => this.readChargingProfileEntry(entry) ?? this.readLegacyChargingProfileEntry(entry))
      .filter(isDefined);

    for (const [connectorId, meterWh] of Object.entries(persisted.connectorMeterValues ?? {})) {
      const numericConnectorId = Number.parseInt(connectorId, 10);
      if (Number.isInteger(numericConnectorId) && this.connectors.has(numericConnectorId)) {
        this.patchConnector(numericConnectorId, { meterWh });
      }
    }

    for (const [connectorId, reservation] of Object.entries(persisted.connectorReservations ?? {})) {
      const numericConnectorId = Number.parseInt(connectorId, 10);
      if (!Number.isInteger(numericConnectorId) || !this.connectors.has(numericConnectorId)) {
        continue;
      }

      if (Date.parse(reservation.expiryDate) <= Date.now()) {
        continue;
      }

      this.patchConnector(numericConnectorId, {
        reservationId: reservation.reservationId,
        reservationIdTag: reservation.idTag,
        reservationParentIdTag: reservation.parentIdTag,
        reservationExpiryDate: reservation.expiryDate,
        status: "Reserved"
      });
      this.scheduleReservationExpiry(numericConnectorId);
    }

    if (Object.keys(persisted.connectorMeterValues ?? {}).length === 0 && this.connectors.has(this.config.connectorId)) {
      this.patchConnector(this.config.connectorId, { meterWh: persisted.meterWh });
    }

    this.localAuthorizationList.clear();
    for (const entry of persisted.localAuthorizationList) {
      this.localAuthorizationList.set(entry.idTag, entry.idTagInfo);
    }

    this.authorizationCache.clear();
    for (const entry of persisted.authorizationCache ?? []) {
      this.authorizationCache.set(entry.idTag, entry.idTagInfo);
    }

    for (const [key, value] of Object.entries(persisted.configurationValues)) {
      this.setConfigurationValue(key, value);
    }
  }

  private savePersistedState(): void {
    if (!this.store) {
      return;
    }

    try {
      this.store.save({
        version: 1,
        chargePointId: this.config.chargePointId,
        connectorCount: this.connectors.size,
        connectorMeterValues: this.serializeConnectorMeterValues(),
        connectorReservations: this.serializeConnectorReservations(),
        localListVersion: this.state.localListVersion,
        localAuthorizationList: this.serializeLocalAuthorizationList(),
        authorizationCache: this.serializeAuthorizationCache(),
        chargingProfiles: this.chargingProfiles,
        configurationValues: this.serializeMutableConfiguration(),
        meterWh: this.getConnector(this.config.connectorId).meterWh
      });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.log("error", `Could not save station state: ${message}`);
    }
  }

  private serializeLocalAuthorizationList(): LocalAuthorizationEntry[] {
    return [...this.localAuthorizationList.entries()].map(([idTag, idTagInfo]) => {
      if (!idTagInfo) {
        return { idTag };
      }

      return { idTag, idTagInfo };
    });
  }

  private serializeAuthorizationCache(): LocalAuthorizationEntry[] {
    return [...this.authorizationCache.entries()].map(([idTag, idTagInfo]) => {
      if (!idTagInfo) {
        return { idTag };
      }

      return { idTag, idTagInfo };
    });
  }

  private serializeMutableConfiguration(): Record<string, string> {
    return Object.fromEntries(
      [...this.configuration.entries()]
        .filter(([, value]) => !value.readonly)
        .map(([key, value]) => [key, value.value])
    );
  }

  private serializeConnectorMeterValues(): Record<string, number> {
    return Object.fromEntries(this.getConnectors().map((connector) => [String(connector.id), connector.meterWh]));
  }

  private serializeConnectorReservations(): Record<string, { reservationId: number; idTag: string; parentIdTag?: string; expiryDate: string }> {
    return Object.fromEntries(
      this.getConnectors()
        .filter((connector) => connector.reservationId && connector.reservationIdTag && connector.reservationExpiryDate)
        .map((connector) => {
          const reservation: { reservationId: number; idTag: string; parentIdTag?: string; expiryDate: string } = {
            reservationId: connector.reservationId ?? 0,
            idTag: connector.reservationIdTag ?? "",
            expiryDate: connector.reservationExpiryDate ?? ""
          };
          if (connector.reservationParentIdTag) {
            reservation.parentIdTag = connector.reservationParentIdTag;
          }
          return [String(connector.id), reservation];
        })
    );
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

  private patchState(patch: Partial<StationState>): void {
    Object.assign(this.state, patch);
    this.emit("state", { ...this.state });
  }

  private log(level: LogEntry["level"], message: string, details?: string): void {
    this.emit("log", { at: new Date(), level, message, details });
  }

  private describeMessage(message: OcppMessage): string {
    if (message[0] === 2) {
      return `CALL ${message[2]} (${message[1]})`;
    }

    if (message[0] === 3) {
      return `CALLRESULT (${message[1]})`;
    }

    return `CALLERROR ${message[2]} (${message[1]})`;
  }

  private async runSafely(task: () => Promise<void>): Promise<void> {
    try {
      await task();
    } catch {
      // Errors are logged in send(); keeping async UI handlers quiet avoids unhandled rejections.
    }
  }

  private readString(value: unknown, fallback: string): string {
    return typeof value === "string" ? value : fallback;
  }

  private readOptionalString(value: unknown): string | undefined {
    return typeof value === "string" ? value : undefined;
  }

  private readNumber(value: unknown, fallback: number): number {
    return typeof value === "number" && Number.isFinite(value) ? value : fallback;
  }

  private readOptionalNumber(value: unknown): number | undefined {
    return typeof value === "number" && Number.isFinite(value) ? value : undefined;
  }

  private readObject(value: unknown): Record<string, unknown> {
    return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
  }

  private readNestedString(payload: Record<string, unknown>, path: string[], fallback: string): string {
    let value: unknown = payload;

    for (const key of path) {
      if (!value || typeof value !== "object" || !(key in value)) {
        return fallback;
      }

      value = (value as Record<string, unknown>)[key];
    }

    return this.readString(value, fallback);
  }
}

function isDefined<T>(value: T | undefined): value is T {
  return value !== undefined;
}

function trimOptional(value: string | undefined, maxLength: number): string | undefined {
  const trimmed = value?.trim();
  if (!trimmed) {
    return undefined;
  }

  return trimmed.slice(0, maxLength);
}
