export type OcppCall = [2, string, string, Record<string, unknown>];
export type OcppCallResult = [3, string, Record<string, unknown>];
export type OcppCallError = [4, string, string, string, Record<string, unknown>];
export type OcppMessage = OcppCall | OcppCallResult | OcppCallError;

export type ConnectorStatus =
  | "Available"
  | "Preparing"
  | "Charging"
  | "SuspendedEVSE"
  | "SuspendedEV"
  | "Finishing"
  | "Reserved"
  | "Unavailable"
  | "Faulted";

export type ChargePointErrorCode =
  | "ConnectorLockFailure"
  | "EVCommunicationError"
  | "GroundFailure"
  | "HighTemperature"
  | "InternalError"
  | "LocalListConflict"
  | "NoError"
  | "OtherError"
  | "OverCurrentFailure"
  | "PowerMeterFailure"
  | "PowerSwitchFailure"
  | "ReaderFailure"
  | "ResetFailure"
  | "UnderVoltage"
  | "OverVoltage"
  | "WeakSignal";

export type AvailabilityType = "Operative" | "Inoperative";

export type RegistrationStatus = "Accepted" | "Pending" | "Rejected";

export type DiagnosticsStatus = "Idle" | "Uploaded" | "UploadFailed" | "Uploading";

export type FirmwareStatus =
  | "Downloaded"
  | "DownloadFailed"
  | "Downloading"
  | "Idle"
  | "InstallationFailed"
  | "Installing"
  | "Installed";

export type StopReason =
  | "EmergencyStop"
  | "EVDisconnected"
  | "HardReset"
  | "Local"
  | "Other"
  | "PowerLoss"
  | "Reboot"
  | "Remote"
  | "SoftReset"
  | "UnlockCommand"
  | "DeAuthorized";

export interface CallResponse {
  messageId: string;
  payload: Record<string, unknown>;
}

export interface CallErrorResponse {
  messageId: string;
  code: string;
  description: string;
  details: Record<string, unknown>;
}

export interface PendingCall {
  action: string;
  resolve: (response: CallResponse) => void;
  reject: (error: Error) => void;
  timeout: NodeJS.Timeout;
}

export interface StationConfig {
  centralSystemUrl: string;
  chargePointId: string;
  vendor: string;
  model: string;
  connectorId: number;
  connectorCount: number;
  heartbeatIntervalSeconds: number;
  persistState: boolean;
  stateDirectory?: string;
  webSocketSubprotocol: string;
  webSocketPingIntervalSeconds: number;
  webSocketReconnectEnabled: boolean;
  webSocketReconnectInitialDelayMs: number;
  webSocketReconnectMaxDelayMs: number;
  webSocketReconnectMaxAttempts: number;
  callTimeoutMs: number;
  tlsRejectUnauthorized: boolean;
  tlsCaFile?: string;
  tlsCertFile?: string;
  tlsKeyFile?: string;
  tlsServerName?: string;
}

export interface ConnectorState {
  id: number;
  status: ConnectorStatus;
  availability: AvailabilityType;
  errorCode: ChargePointErrorCode;
  info?: string;
  vendorId?: string;
  vendorErrorCode?: string;
  evConnected: boolean;
  transactionId?: number;
  transactionStartedAt?: string;
  reservationId?: number;
  reservationIdTag?: string;
  reservationParentIdTag?: string;
  reservationExpiryDate?: string;
  lastIdTag?: string;
  stopTransactionAtWh?: number;
  pendingStartTransaction?: PendingStartTransaction;
  pendingStopTransaction?: PendingStopTransaction;
  meterWh: number;
}

export interface PendingStartTransaction {
  connectorId: number;
  idTag: string;
  meterStart: number;
  timestamp: string;
  reservationId?: number;
}

export interface PendingStopTransaction {
  meterStop: number;
  timestamp: string;
  reason: StopReason;
  idTag?: string;
  transactionData: Record<string, unknown>[];
}

export interface StationState {
  connected: boolean;
  booted: boolean;
  registrationStatus: RegistrationStatus | "Unknown";
  connectorStatus: ConnectorStatus;
  availability: AvailabilityType;
  transactionId?: number;
  reservationId?: number;
  localListVersion: number;
  meterWh: number;
  connectors: ConnectorState[];
  diagnosticsStatus?: DiagnosticsStatus;
  firmwareStatus?: FirmwareStatus;
  lastHeartbeatAt?: Date;
}

export interface LogEntry {
  at: Date;
  level: "info" | "success" | "warn" | "error" | "in" | "out";
  message: string;
  details?: string;
}
