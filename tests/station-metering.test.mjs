import assert from "node:assert/strict";
import { test } from "node:test";
import { createDefaultConfiguration } from "../dist/station/configuration.js";
import { ConfigurationRegistry } from "../dist/station/configuration-registry.js";
import { ConnectorRegistry } from "../dist/station/connector-registry.js";
import { buildMeterValue, DEFAULT_SAMPLED_DATA } from "../dist/station/metering.js";
import { restorePersistedStationState } from "../dist/station/persistence.js";

test("default metering configuration reports charging-like sampled values", () => {
  assert.deepEqual(DEFAULT_SAMPLED_DATA, [
    "Energy.Active.Import.Register",
    "Power.Active.Import",
    "Current.Import",
    "Voltage",
    "Temperature",
    "SoC"
  ]);

  const configuration = createDefaultConfiguration({
    chargePointId: "CP-1",
    centralSystemUrl: "ws://127.0.0.1",
    connectorId: 1,
    connectorCount: 1,
    vendor: "kirby",
    model: "test",
    idTag: "TAG",
    heartbeatIntervalSeconds: 60,
    callTimeoutMs: 30_000,
    reconnect: true,
    reconnectInitialDelayMs: 1_000,
    reconnectMaxDelayMs: 30_000,
    reconnectMaxAttempts: 0,
    webSocketPingIntervalSeconds: 0,
    persistState: false
  });

  assert.equal(configuration.get("MeterValuesSampledData")?.value, DEFAULT_SAMPLED_DATA.join(","));
  assert.equal(configuration.get("StopTxnSampledData")?.value, DEFAULT_SAMPLED_DATA.join(","));

  const meterValue = buildMeterValue({ status: "Charging", meterWh: 1440 }, "Sample.Periodic", DEFAULT_SAMPLED_DATA);
  const sampledValues = meterValue.sampledValue;
  assert.equal(sampledValues.length, 6);
  assert.deepEqual(sampledValues.map((sample) => sample.measurand), DEFAULT_SAMPLED_DATA);
  assert.ok(Number(sampledValues.find((sample) => sample.measurand === "Power.Active.Import")?.value) > 0);
  assert.ok(Number(sampledValues.find((sample) => sample.measurand === "Current.Import")?.value) > 0);
  assert.ok(Number(sampledValues.find((sample) => sample.measurand === "Temperature")?.value) > 25);
});

test("persisted legacy metering defaults are upgraded to rich sampled values", () => {
  const configuration = new ConfigurationRegistry(testConfig());
  const connectors = new ConnectorRegistry(1, 1);

  restorePersistedStationState({
    persisted: {
      version: 1,
      chargePointId: "CP-1",
      connectorCount: 1,
      connectorMeterValues: {},
      connectorReservations: {},
      connectorTransactions: {},
      localListVersion: 0,
      localAuthorizationList: [],
      authorizationCache: [],
      chargingProfiles: [],
      configurationValues: {
        MeterValuesSampledData: "Energy.Active.Import.Register",
        StopTxnSampledData: "Energy.Active.Import.Register"
      },
      meterWh: 0
    },
    connectorId: 1,
    connectors,
    configuration,
    localAuthorizationList: new Map(),
    authorizationCache: new Map(),
    scheduleReservation: () => {}
  });

  assert.equal(configuration.get("MeterValuesSampledData")?.value, DEFAULT_SAMPLED_DATA.join(","));
  assert.equal(configuration.get("StopTxnSampledData")?.value, DEFAULT_SAMPLED_DATA.join(","));
});

function testConfig() {
  return {
    chargePointId: "CP-1",
    centralSystemUrl: "ws://127.0.0.1",
    connectorId: 1,
    connectorCount: 1,
    vendor: "kirby",
    model: "test",
    idTag: "TAG",
    heartbeatIntervalSeconds: 60,
    callTimeoutMs: 30_000,
    reconnect: true,
    reconnectInitialDelayMs: 1_000,
    reconnectMaxDelayMs: 30_000,
    reconnectMaxAttempts: 0,
    webSocketPingIntervalSeconds: 0,
    persistState: false
  };
}
