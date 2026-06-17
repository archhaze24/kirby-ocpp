import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { WebSocketServer } from "ws";
import { Station } from "../dist/station.js";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const calls = [];
const callPayloads = [];
const responses = new Map();
const callErrorsRemaining = new Map();
const wss = new WebSocketServer({ host: "127.0.0.1", port: 0 });

while (!wss.address()) {
  await sleep(5);
}

const address = wss.address();
if (!address || typeof address === "string") {
  throw new Error("Could not determine smoke CSMS port");
}

let stationSocket;
const stateDirectory = mkdtempSync(join(tmpdir(), "kirby-ocpp-smoke-"));

wss.on("connection", (socket) => {
  stationSocket = socket;

  socket.on("message", (data) => {
    const message = JSON.parse(data.toString());

    if (message[0] === 2) {
      const [, messageId, action, payload] = message;
      calls.push(action);
      callPayloads.push({ action, payload });
      const remainingCallErrors = callErrorsRemaining.get(action) ?? 0;
      if (remainingCallErrors > 0) {
        callErrorsRemaining.set(action, remainingCallErrors - 1);
        socket.send(JSON.stringify([4, messageId, "InternalError", `smoke retry ${action}`, {}]));
        return;
      }
      socket.send(JSON.stringify([3, messageId, responseFor(action, payload)]));
      return;
    }

    if (message[0] === 3 || message[0] === 4) {
      responses.set(message[1], message);
    }
  });
});

const stationConfig = {
  centralSystemUrl: `ws://127.0.0.1:${address.port}/ocpp`,
  chargePointId: "CP-001",
  vendor: "Kirby",
  model: "TUI-16",
  connectorId: 1,
  connectorCount: 2,
  heartbeatIntervalSeconds: 5,
  idTag: "TAG1",
  persistState: true,
  stateDirectory
};

const station = new Station(stationConfig);

station.connect();

const bootDeadline = Date.now() + 3_000;
while (!calls.includes("StatusNotification") && Date.now() < bootDeadline) {
  await sleep(20);
}

if (!calls.includes("StatusNotification")) {
  throw new Error(`Boot sequence did not reach StatusNotification; calls=${calls.join(",")}`);
}

const onlineDecision = await station.authorize("CACHEME");
if (!onlineDecision.accepted || !station.authorizationCache.has("CACHEME")) {
  throw new Error("Authorize did not populate authorization cache");
}

const futureDate = new Date(Date.now() + 60_000).toISOString();
const commands = [
  ["GetConfiguration", {}],
  ["ChangeConfiguration", { key: "HeartbeatInterval", value: "6" }],
  ["ChangeConfiguration", { key: "LocalAuthorizeOffline", value: "true" }],
  ["ChangeConfiguration", { key: "StopTransactionOnEVSideDisconnect", value: "false" }],
  ["ChangeConfiguration", { key: "StopTransactionOnInvalidId", value: "true" }],
  ["ChangeConfiguration", { key: "TransactionMessageAttempts", value: "2" }],
  ["ChangeConfiguration", { key: "TransactionMessageRetryInterval", value: "1" }],
  ["ChangeConfiguration", { key: "MeterValueSampleInterval", value: "1" }],
  ["ChangeConfiguration", { key: "ClockAlignedDataInterval", value: "1" }],
  ["ChangeConfiguration", { key: "MeterValuesAlignedData", value: "Energy.Active.Import.Register,Power.Active.Import" }],
  [
    "ChangeConfiguration",
    {
      key: "MeterValuesSampledData",
      value: "Energy.Active.Import.Register,Power.Active.Import,Current.Import,Voltage,SoC"
    }
  ],
  ["ChangeConfiguration", { key: "StopTxnSampledData", value: "Energy.Active.Import.Register,Power.Active.Import" }],
  ["ChangeConfiguration", { key: "StopTxnAlignedData", value: "Voltage,SoC" }],
  ["ClearCache", {}],
  ["GetLocalListVersion", {}],
  [
    "SendLocalList",
    {
      listVersion: 1,
      updateType: "Full",
      localAuthorizationList: [{ idTag: "TAG1", idTagInfo: { status: "Accepted" } }]
    }
  ],
  ["ClearChargingProfile", {}],
  [
    "SetChargingProfile",
    {
      connectorId: 1,
      csChargingProfiles: {
        chargingProfileId: 1,
        stackLevel: 1,
        chargingProfilePurpose: "TxDefaultProfile",
        chargingProfileKind: "Absolute",
        chargingSchedule: {
          chargingRateUnit: "A",
          chargingSchedulePeriod: [{ startPeriod: 0, limit: 16 }]
        }
      }
    }
  ],
  ["ReserveNow", { connectorId: 1, expiryDate: futureDate, idTag: "TAG1", reservationId: 77 }],
  ["CancelReservation", { reservationId: 77 }],
  ["ChangeAvailability", { connectorId: 1, type: "Inoperative" }],
  ["ChangeAvailability", { connectorId: 1, type: "Operative" }],
  ["RemoteStartTransaction", { connectorId: 2, idTag: "TAG2" }],
  ["RemoteStopTransaction", { transactionId: 123 }],
  ["TriggerMessage", { requestedMessage: "Heartbeat" }],
  ["UnlockConnector", { connectorId: 1 }],
  ["GetDiagnostics", { location: "https://example.invalid/diag" }],
  ["UpdateFirmware", { location: "https://example.invalid/fw", retrieveDate: futureDate }],
  ["DataTransfer", { vendorId: "Kirby", data: "ping" }],
  ["GetCompositeSchedule", { connectorId: 1, duration: 60 }],
  ["Reset", { type: "Soft" }]
];

for (const [action, payload] of commands) {
  const response = await sendCentralSystemCall(action, payload);
  if (response[0] !== 3) {
    throw new Error(`${action} returned CALLERROR ${JSON.stringify(response)}`);
  }

  if (action === "GetCompositeSchedule") {
    const responsePayload = response[2];
    const limit = responsePayload?.chargingSchedule?.chargingSchedulePeriod?.[0]?.limit;
    if (responsePayload?.status !== "Accepted" || limit !== 16) {
      throw new Error(`Unexpected composite schedule response ${JSON.stringify(responsePayload)}`);
    }
  }
}

const fullConfiguration = await sendCentralSystemCall("GetConfiguration", {});
const configurationKeys = fullConfiguration[2]?.configurationKey ?? [];
for (const key of ["ClockAlignedDataInterval", "SupportedFeatureProfiles", "StopTransactionOnEVSideDisconnect"]) {
  if (!configurationKeys.some((entry) => entry.key === key)) {
    throw new Error(`GetConfiguration did not include ${key}`);
  }
}

const readonlyResponse = await sendCentralSystemCall("ChangeConfiguration", {
  key: "NumberOfConnectors",
  value: "7"
});
if (readonlyResponse[2]?.status !== "Rejected") {
  throw new Error(`Readonly ChangeConfiguration was not rejected: ${JSON.stringify(readonlyResponse)}`);
}

const invalidBooleanResponse = await sendCentralSystemCall("ChangeConfiguration", {
  key: "StopTransactionOnEVSideDisconnect",
  value: "maybe"
});
if (invalidBooleanResponse[2]?.status !== "Rejected") {
  throw new Error(`Invalid boolean ChangeConfiguration was not rejected: ${JSON.stringify(invalidBooleanResponse)}`);
}

const invalidSampledDataResponse = await sendCentralSystemCall("ChangeConfiguration", {
  key: "MeterValuesSampledData",
  value: "Energy.Active.Import.Register,Totally.Not.Real"
});
if (invalidSampledDataResponse[2]?.status !== "Rejected") {
  throw new Error(`Invalid sampled data ChangeConfiguration was not rejected: ${JSON.stringify(invalidSampledDataResponse)}`);
}

const addConnectorStartIndex = callPayloads.length;
const minimumStatusConnectorId = station.addConnector();
await waitForCallAfter(
  "StatusNotification",
  addConnectorStartIndex,
  (payload) => payload.connectorId === minimumStatusConnectorId && payload.status === "Available"
);
const minimumStatusResponse = await sendCentralSystemCall("ChangeConfiguration", {
  key: "MinimumStatusDuration",
  value: "1"
});
if (minimumStatusResponse[2]?.status !== "Accepted") {
  throw new Error(`MinimumStatusDuration was not accepted: ${JSON.stringify(minimumStatusResponse)}`);
}

const minimumStatusStartIndex = callPayloads.length;
await station.cycleStatus(minimumStatusConnectorId);
await station.cycleStatus(minimumStatusConnectorId);
await sleep(150);
if (
  callPayloads
    .slice(minimumStatusStartIndex)
    .some((entry) => entry.action === "StatusNotification" && entry.payload.connectorId === minimumStatusConnectorId)
) {
  throw new Error("MinimumStatusDuration did not delay rapid StatusNotification changes");
}
await waitForCallAfter(
  "StatusNotification",
  minimumStatusStartIndex,
  (payload) => payload.connectorId === minimumStatusConnectorId && payload.status === "Charging"
);
await sendCentralSystemCall("ChangeConfiguration", { key: "MinimumStatusDuration", value: "0" });

await station.meterValues(1, 0);
await waitForCall("MeterValues");
const triggeredMeterValues = [...callPayloads].reverse().find((entry) => entry.action === "MeterValues");
const sampledValues = triggeredMeterValues?.payload?.meterValue?.[0]?.sampledValue ?? [];
const sampledMeasurands = sampledValues.map((sample) => sample.measurand).sort();
for (const measurand of ["Current.Import", "Energy.Active.Import.Register", "Power.Active.Import", "SoC", "Voltage"]) {
  if (!sampledMeasurands.includes(measurand)) {
    throw new Error(`MeterValues did not include configured measurand ${measurand}: ${JSON.stringify(sampledValues)}`);
  }
}

if (station.authorizationCache.has("CACHEME")) {
  throw new Error("ClearCache did not clear authorization cache");
}

const reservationResponse = await sendCentralSystemCall("ReserveNow", {
  connectorId: 1,
  expiryDate: futureDate,
  idTag: "TAG1",
  reservationId: 99
});
if (reservationResponse[2]?.status !== "Accepted") {
  throw new Error(`Reservation was not accepted: ${JSON.stringify(reservationResponse)}`);
}

await station.startTransaction(1, "WRONGTAG");
if (station.state.connectors.find((connector) => connector.id === 1)?.transactionId) {
  throw new Error("Reserved connector started transaction for wrong idTag");
}

const retryStartIndex = callPayloads.length;
callErrorsRemaining.set("StartTransaction", 1);
await station.startTransaction(1, "TAG1");
const retriedStartTransactions = callPayloads
  .slice(retryStartIndex)
  .filter((entry) => entry.action === "StartTransaction" && entry.payload.connectorId === 1 && entry.payload.idTag === "TAG1");
if (retriedStartTransactions.length < 2) {
  throw new Error(`StartTransaction was not retried after CALLERROR: ${JSON.stringify(retriedStartTransactions)}`);
}
const reservedStartConnector = station.state.connectors.find((connector) => connector.id === 1);
if (reservedStartConnector?.status !== "Charging" || reservedStartConnector.reservationId) {
  throw new Error(`Reserved start did not consume reservation: ${JSON.stringify(reservedStartConnector)}`);
}

const periodicStartIndex = callPayloads.length;
await waitForCallAfter("MeterValues", periodicStartIndex, (payload) => payload.connectorId === 1 && payload.transactionId === 123);
await waitForCallAfter(
  "MeterValues",
  periodicStartIndex,
  (payload) =>
    payload.connectorId === 1 &&
    payload.transactionId === 123 &&
    payload.meterValue?.[0]?.sampledValue?.some((sample) => sample.context === "Sample.Clock")
);

const stopCountBeforeSuspendedEv = callPayloads.filter((entry) => entry.action === "StopTransaction").length;
await station.unplug(1);
const suspendedConnector = station.state.connectors.find((connector) => connector.id === 1);
if (suspendedConnector?.status !== "SuspendedEV" || suspendedConnector.transactionId !== 123 || suspendedConnector.evConnected) {
  throw new Error(`EV disconnect did not suspend active transaction: ${JSON.stringify(suspendedConnector)}`);
}
const stopCountAfterSuspendedEv = callPayloads.filter((entry) => entry.action === "StopTransaction").length;
if (stopCountAfterSuspendedEv !== stopCountBeforeSuspendedEv) {
  throw new Error("EV disconnect sent StopTransaction despite StopTransactionOnEVSideDisconnect=false");
}

await station.plugIn(1);
const resumedConnector = station.state.connectors.find((connector) => connector.id === 1);
if (resumedConnector?.status !== "Charging" || resumedConnector.transactionId !== 123 || !resumedConnector.evConnected) {
  throw new Error(`EV reconnect did not resume charging transaction: ${JSON.stringify(resumedConnector)}`);
}

await station.stopTransaction(1, "Local", "TAG1");
await station.unplug(1);

await station.plugIn(1);
if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Preparing") {
  throw new Error("Plug in did not move connector 1 to Preparing");
}

await station.startTransaction(1, "TAG1");
if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Charging") {
  throw new Error("StartTransaction did not move connector 1 to Charging");
}

await station.stopTransaction(1, "Local", "TAG1");
if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Finishing") {
  throw new Error("StopTransaction did not move connector 1 to Finishing");
}

const stopTransactionPayload = [...callPayloads].reverse().find((entry) => entry.action === "StopTransaction")?.payload;
const stopEndValues =
  stopTransactionPayload?.transactionData
    ?.find((meterValue) => meterValue.sampledValue?.some((sample) => sample.context === "Transaction.End"))
    ?.sampledValue ?? [];
const stopMeasurands = stopEndValues.map((sample) => sample.measurand).sort();
if (stopMeasurands.join(",") !== "Energy.Active.Import.Register,Power.Active.Import") {
  throw new Error(`StopTransaction did not include configured sampled data: ${JSON.stringify(stopEndValues)}`);
}

const stopAlignedValues =
  stopTransactionPayload?.transactionData
    ?.find((meterValue) => meterValue.sampledValue?.some((sample) => sample.context === "Sample.Clock"))
    ?.sampledValue ?? [];
const stopAlignedMeasurands = stopAlignedValues.map((sample) => sample.measurand).sort();
if (stopAlignedMeasurands.join(",") !== "SoC,Voltage") {
  throw new Error(`StopTransaction did not include configured aligned data: ${JSON.stringify(stopAlignedValues)}`);
}

await station.dataTransfer({ vendorId: "Kirby", messageId: "smoke", data: "ping" });

await station.unplug(1);
if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Available") {
  throw new Error("Unplug did not move connector 1 to Available");
}

const mixedAvailabilityIdleConnectorId = station.addConnector();
await station.plugIn(1);
await station.startTransaction(1, "TAG1");
const scheduledAvailabilityResponse = await sendCentralSystemCall("ChangeAvailability", {
  connectorId: 0,
  type: "Inoperative"
});
if (scheduledAvailabilityResponse[2]?.status !== "Scheduled") {
  throw new Error(`ChangeAvailability during transaction was not scheduled: ${JSON.stringify(scheduledAvailabilityResponse)}`);
}
const activeScheduledConnector = station.state.connectors.find((connector) => connector.id === 1);
if (activeScheduledConnector?.status !== "Charging" || activeScheduledConnector.availability !== "Inoperative") {
  throw new Error(`Scheduled ChangeAvailability did not preserve active connector status: ${JSON.stringify(activeScheduledConnector)}`);
}
const idleScheduledConnector = station.state.connectors.find((connector) => connector.id === mixedAvailabilityIdleConnectorId);
if (idleScheduledConnector?.status !== "Unavailable" || idleScheduledConnector.availability !== "Inoperative") {
  throw new Error(`Scheduled ChangeAvailability did not immediately apply to idle connector: ${JSON.stringify(idleScheduledConnector)}`);
}

await station.stopTransaction(1, "EVDisconnected", "TAG1");
const unavailableAfterScheduledChange = station.state.connectors.find((connector) => connector.id === 1);
if (unavailableAfterScheduledChange?.status !== "Unavailable" || unavailableAfterScheduledChange.availability !== "Inoperative") {
  throw new Error(`Scheduled ChangeAvailability did not apply after transaction: ${JSON.stringify(unavailableAfterScheduledChange)}`);
}

const operativeAfterScheduledResponse = await sendCentralSystemCall("ChangeAvailability", {
  connectorId: 0,
  type: "Operative"
});
if (operativeAfterScheduledResponse[2]?.status !== "Accepted") {
  throw new Error(`ChangeAvailability back to Operative failed: ${JSON.stringify(operativeAfterScheduledResponse)}`);
}
if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Available") {
  throw new Error("ChangeAvailability back to Operative did not restore Available");
}
if (station.state.connectors.find((connector) => connector.id === mixedAvailabilityIdleConnectorId)?.status !== "Available") {
  throw new Error("ChangeAvailability back to Operative did not restore idle connector");
}

await station.plugIn(1);
await station.startTransaction(1, "BADTAG");
if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Charging") {
  throw new Error("BADTAG transaction did not start for invalid-id stop test");
}

const invalidIdStopIndex = callPayloads.length;
const invalidDecision = await station.authorize("BADTAG");
if (invalidDecision.accepted) {
  throw new Error(`BADTAG authorization unexpectedly accepted: ${JSON.stringify(invalidDecision)}`);
}
await waitForCallAfter(
  "StopTransaction",
  invalidIdStopIndex,
  (payload) => payload.idTag === "BADTAG" && payload.reason === "DeAuthorized"
);
const invalidStoppedConnector = station.state.connectors.find((connector) => connector.id === 1);
if (invalidStoppedConnector?.transactionId || invalidStoppedConnector?.status !== "Finishing") {
  throw new Error(`StopTransactionOnInvalidId did not stop BADTAG transaction: ${JSON.stringify(invalidStoppedConnector)}`);
}

await station.unplug(1);
if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Available") {
  throw new Error("Unplug after invalid-id stop did not move connector 1 to Available");
}

await station.setFault(1, "GroundFailure", "smoke fault");
const faultedConnector = station.state.connectors.find((connector) => connector.id === 1);
if (faultedConnector?.status !== "Faulted" || faultedConnector.errorCode !== "GroundFailure") {
  throw new Error(`Fault injection failed: ${JSON.stringify(faultedConnector)}`);
}

await station.clearFault(1);
const clearedConnector = station.state.connectors.find((connector) => connector.id === 1);
if (clearedConnector?.status !== "Available" || clearedConnector.errorCode !== "NoError") {
  throw new Error(`Fault clear failed: ${JSON.stringify(clearedConnector)}`);
}

station.disconnect();
wss.close();

const reloadedStation = new Station(stationConfig);
if (reloadedStation.state.localListVersion !== 1) {
  throw new Error(`Persisted local list version mismatch: ${reloadedStation.state.localListVersion}`);
}

if (!reloadedStation.localAuthorizationList.has("TAG1")) {
  throw new Error("Persisted local authorization list does not contain TAG1");
}

if (reloadedStation.chargingProfiles.length !== 1) {
  throw new Error(`Persisted charging profile count mismatch: ${reloadedStation.chargingProfiles.length}`);
}

const offlineDecision = await reloadedStation.authorize("TAG1");
if (!offlineDecision.accepted || offlineDecision.source !== "LocalList") {
  throw new Error(`Offline local authorization failed: ${JSON.stringify(offlineDecision)}`);
}

console.log(`ok ${commands.length} CSMS commands; charge point calls=${[...new Set(calls)].sort().join(",")}`);
process.exit(0);

function responseFor(action, payload = {}) {
  switch (action) {
    case "BootNotification":
      return { status: "Accepted", currentTime: new Date().toISOString(), interval: 5 };
    case "Heartbeat":
      return { currentTime: new Date().toISOString() };
    case "Authorize":
      if (payload.idTag === "BADTAG") {
        return { idTagInfo: { status: "Blocked" } };
      }
      return { idTagInfo: { status: "Accepted" } };
    case "StartTransaction":
      return { transactionId: 123, idTagInfo: { status: "Accepted" } };
    case "DataTransfer":
      return { status: "Accepted", data: "pong" };
    default:
      return {};
  }
}

function sendCentralSystemCall(action, payload) {
  if (!stationSocket) {
    throw new Error("Station socket is not connected");
  }

  return new Promise((resolve, reject) => {
    const messageId = `csms-${responses.size}-${action}`;
    stationSocket.send(JSON.stringify([2, messageId, action, payload]));

    const started = Date.now();
    const tick = () => {
      if (responses.has(messageId)) {
        resolve(responses.get(messageId));
        return;
      }

      if (Date.now() - started > 3_000) {
        reject(new Error(`Timed out waiting for ${action}`));
        return;
      }

      setTimeout(tick, 20);
    };

    tick();
  });
}

async function waitForCall(action) {
  const started = Date.now();
  while (Date.now() - started <= 3_000) {
    if (callPayloads.some((entry) => entry.action === action)) {
      return;
    }

    await sleep(20);
  }

  throw new Error(`Timed out waiting for charge point ${action}`);
}

async function waitForCallAfter(action, startIndex, predicate = () => true) {
  const started = Date.now();
  while (Date.now() - started <= 3_000) {
    const match = callPayloads
      .slice(startIndex)
      .some((entry) => entry.action === action && predicate(entry.payload));
    if (match) {
      return;
    }

    await sleep(20);
  }

  throw new Error(`Timed out waiting for charge point ${action} after index ${startIndex}`);
}
