import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { WebSocketServer } from "ws";
import { parseConfig } from "../dist/config.js";
import { Station } from "../dist/station.js";
import {
  assertServerAddress,
  waitForCondition,
  waitForServer
} from "./support/ocpp-test-utils.mjs";

test("offline StartTransaction and StopTransaction are synchronized after reconnect", { timeout: 5_000 }, async () => {
  const { station, wss, messages } = await createStationSyncHarness();

  try {
    seedOfflineAuthorization(station);
    await station.plugIn(1);
    const localTransactionId = await station.startTransaction(1, "TAG1");
    assert.ok(localTransactionId < 0);

    await station.meterValues(1, 7);
    await station.stopTransaction(1, "Local", "TAG1");

    const offlineConnector = station.state.connectors.find((connector) => connector.id === 1);
    assert.ok(offlineConnector.pendingStartTransaction);
    assert.ok(offlineConnector.pendingStopTransaction);
    assert.equal(messages.length, 0);

    station.connect();
    await waitForCondition(() => messages.some((message) => message.action === "StopTransaction"), "offline transaction sync");

    const start = messages.find((message) => message.action === "StartTransaction");
    const stop = messages.find((message) => message.action === "StopTransaction");
    assert.equal(start.payload.idTag, "TAG1");
    assert.equal(start.payload.connectorId, 1);
    assert.equal(stop.payload.transactionId, 777);
    assert.equal(stop.payload.reason, "Local");
    assert.equal(stop.payload.idTag, "TAG1");

    const syncedConnector = station.state.connectors.find((connector) => connector.id === 1);
    assert.equal(syncedConnector.transactionId, undefined);
    assert.equal(syncedConnector.pendingStartTransaction, undefined);
    assert.equal(syncedConnector.pendingStopTransaction, undefined);
  } finally {
    station.disconnect();
    wss.close();
  }
});

test("persisted offline transaction syncs after station restart", { timeout: 5_000 }, async () => {
  const stateDirectory = mkdtempSync(join(tmpdir(), "kirby-ocpp-offline-test-"));
  const { station, wss, messages, config } = await createStationSyncHarness({ persistState: true, stateDirectory });

  try {
    seedOfflineAuthorization(station);
    await station.plugIn(1);
    await station.startTransaction(1, "TAG1");
    await station.stopTransaction(1, "Local", "TAG1");
    station.disconnect();

    const reloadedStation = new Station(config);
    reloadedStation.on("log", () => {});
    const restoredConnector = reloadedStation.state.connectors.find((connector) => connector.id === 1);
    assert.ok(restoredConnector.pendingStartTransaction);
    assert.ok(restoredConnector.pendingStopTransaction);

    reloadedStation.connect();
    await waitForCondition(() => messages.some((message) => message.action === "StopTransaction"), "persisted offline transaction sync");

    const stop = messages.find((message) => message.action === "StopTransaction");
    assert.equal(stop.payload.transactionId, 777);
    assert.equal(stop.payload.reason, "Local");

    reloadedStation.disconnect();
  } finally {
    station.disconnect();
    wss.close();
  }
});

test("successful connected StopTransaction clears persisted transaction state", { timeout: 5_000 }, async () => {
  const stateDirectory = mkdtempSync(join(tmpdir(), "kirby-ocpp-stop-persist-test-"));
  const { station, wss, messages, config } = await createStationSyncHarness({ persistState: true, stateDirectory });

  try {
    station.connect();
    await waitForCondition(() => messages.some((message) => message.action === "BootNotification"), "boot");
    await station.plugIn(1);
    const transactionId = await station.startTransaction(1, "TAG1");
    assert.equal(transactionId, 777);
    await station.stopTransaction(1, "Local", "TAG1");
    await waitForCondition(() => messages.some((message) => message.action === "StopTransaction"), "stop transaction");

    const stoppedConnector = station.state.connectors.find((connector) => connector.id === 1);
    assert.equal(stoppedConnector.transactionId, undefined);
    station.disconnect();

    const reloadedStation = new Station(config);
    reloadedStation.on("log", () => {});
    const restoredConnector = reloadedStation.state.connectors.find((connector) => connector.id === 1);
    assert.equal(restoredConnector.transactionId, undefined);
    assert.equal(restoredConnector.pendingStartTransaction, undefined);
    assert.equal(restoredConnector.pendingStopTransaction, undefined);
    assert.notEqual(restoredConnector.status, "Charging");
    reloadedStation.disconnect();
  } finally {
    station.disconnect();
    wss.close();
  }
});

async function createStationSyncHarness(overrides = {}) {
  const wss = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await waitForServer(wss);
  const address = wss.address();
  assertServerAddress(address);

  const messages = [];
  wss.on("connection", (socket) => {
    socket.on("message", (data) => {
      const message = JSON.parse(data.toString());
      if (message[0] !== 2) {
        return;
      }

      const [, messageId, action, payload] = message;
      messages.push({ action, payload });
      if (action === "BootNotification") {
        socket.send(JSON.stringify([3, messageId, { status: "Accepted", currentTime: new Date().toISOString(), interval: 1 }]));
        return;
      }
      if (action === "StartTransaction") {
        socket.send(JSON.stringify([3, messageId, { transactionId: 777, idTagInfo: { status: "Accepted" } }]));
        return;
      }

      socket.send(JSON.stringify([3, messageId, action === "Heartbeat" ? { currentTime: new Date().toISOString() } : {}]));
    });
  });

  const config = parseConfig({
    centralSystemUrl: `ws://127.0.0.1:${address.port}/ocpp`,
    persistState: false,
    webSocketPingIntervalSeconds: 0,
    webSocketReconnectEnabled: false,
    callTimeoutMs: 200,
    ...overrides
  });
  const station = new Station(config);
  station.on("log", () => {});

  return { config, messages, station, wss };
}

function seedOfflineAuthorization(station) {
  station.configuration.setValue("LocalAuthorizeOffline", "true");
  station.localAuthorizationList.set("TAG1", { status: "Accepted" });
}
