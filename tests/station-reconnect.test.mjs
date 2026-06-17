import assert from "node:assert/strict";
import { test } from "node:test";
import { WebSocketServer } from "ws";
import { parseConfig } from "../dist/config.js";
import { Station } from "../dist/station.js";
import {
  assertServerAddress,
  delay,
  waitForCondition,
  waitForServer
} from "./support/ocpp-test-utils.mjs";

test("station reconnects and boots again after unexpected WebSocket disconnect", { timeout: 5_000 }, async () => {
  const wss = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await waitForServer(wss);

  const address = wss.address();
  assertServerAddress(address);

  let activeSocket;
  let connections = 0;
  let bootNotifications = 0;
  wss.on("connection", (socket) => {
    activeSocket = socket;
    connections += 1;
    socket.on("message", (data) => {
      const message = JSON.parse(data.toString());
      if (message[0] !== 2) {
        return;
      }

      const [, messageId, action] = message;
      if (action === "BootNotification") {
        bootNotifications += 1;
        socket.send(JSON.stringify([3, messageId, { status: "Accepted", currentTime: new Date().toISOString(), interval: 1 }]));
        return;
      }
      if (action === "Heartbeat") {
        socket.send(JSON.stringify([3, messageId, { currentTime: new Date().toISOString() }]));
        return;
      }

      socket.send(JSON.stringify([3, messageId, {}]));
    });
  });

  const station = new Station(parseConfig({
    centralSystemUrl: `ws://127.0.0.1:${address.port}/ocpp`,
    persistState: false,
    webSocketPingIntervalSeconds: 0,
    webSocketReconnectInitialDelayMs: 20,
    webSocketReconnectMaxDelayMs: 20,
    webSocketReconnectMaxAttempts: 2,
    callTimeoutMs: 200
  }));
  station.on("log", () => {});

  try {
    station.connect();
    await waitForCondition(() => station.state.booted && bootNotifications === 1, "initial station boot");

    activeSocket.close(1011, "test disconnect");
    await waitForCondition(
      () => connections >= 2 && station.state.connected && station.state.booted && bootNotifications >= 2,
      "station reconnect boot"
    );
  } finally {
    station.disconnect();
    wss.close();
  }
});

test("station stops reconnecting after max reconnect attempts", { timeout: 5_000 }, async () => {
  const wss = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await waitForServer(wss);

  const address = wss.address();
  assertServerAddress(address);

  let connections = 0;
  wss.on("connection", (socket) => {
    connections += 1;
    setTimeout(() => socket.close(1011, "always down"), 5);
  });

  const station = new Station(parseConfig({
    centralSystemUrl: `ws://127.0.0.1:${address.port}/ocpp`,
    persistState: false,
    webSocketPingIntervalSeconds: 0,
    webSocketReconnectInitialDelayMs: 20,
    webSocketReconnectMaxDelayMs: 20,
    webSocketReconnectMaxAttempts: 2,
    callTimeoutMs: 50
  }));
  const logs = [];
  station.on("log", (entry) => logs.push(entry.message));

  try {
    station.connect();
    await waitForCondition(
      () => logs.some((message) => message.includes("Reconnect stopped after 2 failed attempt")),
      "max reconnect attempts"
    );
    await delay(80);
    assert.equal(connections, 3);
  } finally {
    station.disconnect();
    wss.close();
  }
});
