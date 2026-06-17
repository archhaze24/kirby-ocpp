import type { StationConfig } from "../ocpp/types.js";
import { DEFAULT_SAMPLED_DATA, isValidSampledDataConfiguration, MEASURANDS } from "./metering.js";

export interface ConfigurationKey {
  readonly: boolean;
  value: string;
  validate?: (value: string) => boolean;
}

const SUPPORTED_FEATURE_PROFILES = [
  "Core",
  "FirmwareManagement",
  "LocalAuthListManagement",
  "Reservation",
  "RemoteTrigger",
  "SmartCharging"
];

const CONNECTOR_PHASE_ROTATIONS = new Set([
  "NotApplicable",
  "Unknown",
  "RST",
  "RTS",
  "SRT",
  "STR",
  "TRS",
  "TSR"
]);

const DEFAULT_SAMPLED_DATA_CONFIGURATION = DEFAULT_SAMPLED_DATA.join(",");

export function createDefaultConfiguration(config: StationConfig): Map<string, ConfigurationKey> {
  const configuration = new Map<string, ConfigurationKey>();
  const boolean = (value: string) => isBooleanConfigurationValue(value);
  const nonNegativeInteger = (value: string) => isIntegerConfigurationValue(value, 0);
  const positiveInteger = (value: string) => isIntegerConfigurationValue(value, 1);
  const sampledData = (value: string) => isValidSampledDataConfiguration(value);
  const set = (key: string, readonly: boolean, value: string, validate?: (value: string) => boolean) => {
    configuration.set(key, { readonly, value, validate });
  };

  set("AllowOfflineTxForUnknownId", false, "false", boolean);
  set("AuthorizationCacheEnabled", false, "true", boolean);
  set("AuthorizeRemoteTxRequests", false, "false", boolean);
  set("BlinkRepeat", false, "0", nonNegativeInteger);
  set("ChargePointModel", true, config.model);
  set("ChargePointSerialNumber", true, config.chargePointId);
  set("ChargePointVendor", true, config.vendor);
  set("ClockAlignedDataInterval", false, "0", nonNegativeInteger);
  set("ConnectionTimeOut", false, "60", positiveInteger);
  set("ConnectorPhaseRotation", false, "Unknown", isConnectorPhaseRotationConfiguration);
  set("ConnectorPhaseRotationMaxLength", true, String(Math.max(1, config.connectorCount)));
  set("GetConfigurationMaxKeys", true, "50");
  set("HeartbeatInterval", false, String(config.heartbeatIntervalSeconds), positiveInteger);
  set("LightIntensity", false, "0", nonNegativeInteger);
  set("LocalAuthListEnabled", false, "true", boolean);
  set("LocalAuthListMaxLength", true, "1000");
  set("LocalAuthorizeOffline", false, "false", boolean);
  set("LocalPreAuthorize", false, "false", boolean);
  set("MaxEnergyOnInvalidId", false, "0", nonNegativeInteger);
  set("MeterValuesAlignedData", false, DEFAULT_SAMPLED_DATA_CONFIGURATION, sampledData);
  set("MeterValuesAlignedDataMaxLength", true, String(MEASURANDS.size));
  set("MeterValuesSampledData", false, DEFAULT_SAMPLED_DATA_CONFIGURATION, sampledData);
  set("MeterValuesSampledDataMaxLength", true, String(MEASURANDS.size));
  set("MeterValueSampleInterval", false, "60", nonNegativeInteger);
  set("MinimumStatusDuration", false, "0", nonNegativeInteger);
  set("NumberOfConnectors", true, String(config.connectorCount));
  set("ResetRetries", false, "0", nonNegativeInteger);
  set("SendLocalListMaxLength", true, "1000");
  set("StopTransactionOnEVSideDisconnect", false, "true", boolean);
  set("StopTransactionOnInvalidId", false, "false", boolean);
  set("StopTxnAlignedData", false, DEFAULT_SAMPLED_DATA_CONFIGURATION, sampledData);
  set("StopTxnAlignedDataMaxLength", true, String(MEASURANDS.size));
  set("StopTxnSampledData", false, DEFAULT_SAMPLED_DATA_CONFIGURATION, sampledData);
  set("StopTxnSampledDataMaxLength", true, String(MEASURANDS.size));
  set("SupportedFeatureProfiles", true, SUPPORTED_FEATURE_PROFILES.join(","));
  set("SupportedFeatureProfilesMaxLength", true, String(SUPPORTED_FEATURE_PROFILES.length));
  set("TransactionMessageAttempts", false, "1", positiveInteger);
  set("TransactionMessageRetryInterval", false, "30", positiveInteger);
  set("UnlockConnectorOnEVSideDisconnect", false, "false", boolean);
  set("WebSocketPingInterval", false, "0", nonNegativeInteger);

  return configuration;
}

function isBooleanConfigurationValue(value: string): boolean {
  return value === "true" || value === "false";
}

function isIntegerConfigurationValue(value: string, minimum: number): boolean {
  if (!/^\d+$/.test(value)) {
    return false;
  }

  const parsed = Number.parseInt(value, 10);
  return Number.isSafeInteger(parsed) && parsed >= minimum;
}

function isConnectorPhaseRotationConfiguration(value: string): boolean {
  const items = value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);

  return items.length > 0 && items.every((item) => CONNECTOR_PHASE_ROTATIONS.has(item));
}
