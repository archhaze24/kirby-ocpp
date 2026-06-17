import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { WebSocketServer } from "ws";
import { Station } from "../../dist/station.js";

export const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

export class SmokeHarness {
  static async create() {
    const wss = new WebSocketServer({ host: "127.0.0.1", port: 0 });
    while (!wss.address()) {
      await sleep(5);
    }

    const address = wss.address();
    if (!address || typeof address === "string") {
      throw new Error("Could not determine smoke CSMS port");
    }

    const harness = new SmokeHarness(wss, address.port);
    harness.bindServer();
    harness.station.connect();
    return harness;
  }

  calls = [];
  callPayloads = [];
  responses = new Map();
  callErrorsRemaining = new Map();
  silentTimeoutsRemaining = new Map();
  closeOnCallRemaining = new Map();
  bootResponses = [];
  edgeChecks = 0;
  stationSocket;

  constructor(wss, port) {
    this.wss = wss;
    this.stateDirectory = mkdtempSync(join(tmpdir(), "kirby-ocpp-smoke-"));
    this.stationConfig = {
      centralSystemUrl: `ws://127.0.0.1:${port}/ocpp`,
      chargePointId: "CP-001",
      vendor: "Kirby",
      model: "TUI-16",
      connectorId: 1,
      connectorCount: 2,
      heartbeatIntervalSeconds: 5,
      idTag: "TAG1",
      persistState: true,
      stateDirectory: this.stateDirectory,
      callTimeoutMs: 80,
      webSocketReconnectEnabled: true,
      webSocketReconnectInitialDelayMs: 50,
      webSocketReconnectMaxDelayMs: 100,
      webSocketReconnectMaxAttempts: 3
    };
    this.station = new Station(this.stationConfig);
  }

  bindServer() {
    this.wss.on("connection", (socket) => {
      this.stationSocket = socket;

      socket.on("message", (data) => {
        const message = JSON.parse(data.toString());

        if (message[0] === 2) {
          const [, messageId, action, payload] = message;
          this.calls.push(action);
          this.callPayloads.push({ action, payload });

          if (this.consumeRemaining(this.silentTimeoutsRemaining, action)) {
            return;
          }
          if (this.consumeRemaining(this.closeOnCallRemaining, action)) {
            socket.close(1011, `smoke disconnect during ${action}`);
            return;
          }
          if (this.consumeRemaining(this.callErrorsRemaining, action)) {
            socket.send(JSON.stringify([4, messageId, "InternalError", `smoke retry ${action}`, {}]));
            return;
          }

          socket.send(JSON.stringify([3, messageId, this.responseFor(action, payload)]));
          return;
        }

        if (message[0] === 3 || message[0] === 4) {
          this.responses.set(message[1], message);
        }
      });
    });
  }

  close() {
    this.station.disconnect();
    this.wss.close();
  }

  addEdgeCheck() {
    this.edgeChecks += 1;
  }

  async waitForInitialBoot() {
    const bootDeadline = Date.now() + 3_000;
    while (!this.calls.includes("StatusNotification") && Date.now() < bootDeadline) {
      await sleep(20);
    }

    if (!this.calls.includes("StatusNotification")) {
      throw new Error(`Boot sequence did not reach StatusNotification; calls=${this.calls.join(",")}`);
    }
  }

  sendCentralSystemCall(action, payload) {
    if (!this.stationSocket) {
      throw new Error("Station socket is not connected");
    }

    return new Promise((resolve, reject) => {
      const messageId = `csms-${this.responses.size}-${action}`;
      this.stationSocket.send(JSON.stringify([2, messageId, action, payload]));

      const started = Date.now();
      const tick = () => {
        if (this.responses.has(messageId)) {
          resolve(this.responses.get(messageId));
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

  async expectResponseStatus(action, payload, expectedStatus, label) {
    const response = await this.sendCentralSystemCall(action, payload);
    if (response[0] !== 3 || response[2]?.status !== expectedStatus) {
      throw new Error(`${label} expected ${expectedStatus}: ${JSON.stringify(response)}`);
    }
    this.addEdgeCheck();
  }

  async expectCallError(action, payload, expectedCode, label) {
    const response = await this.sendCentralSystemCall(action, payload);
    if (response[0] !== 4 || response[2] !== expectedCode) {
      throw new Error(`${label} expected CALLERROR ${expectedCode}: ${JSON.stringify(response)}`);
    }
    this.addEdgeCheck();
  }

  async waitForCall(action) {
    const started = Date.now();
    while (Date.now() - started <= 3_000) {
      const entry = this.callPayloads.find((item) => item.action === action);
      if (entry) {
        return entry.payload;
      }

      await sleep(20);
    }

    throw new Error(`Timed out waiting for charge point ${action}`);
  }

  async waitForCallAfter(action, startIndex, predicate = () => true) {
    const started = Date.now();
    while (Date.now() - started <= 3_000) {
      const match = this.callPayloads
        .slice(startIndex)
        .some((entry) => entry.action === action && predicate(entry.payload));
      if (match) {
        return;
      }

      await sleep(20);
    }

    throw new Error(`Timed out waiting for charge point ${action} after index ${startIndex}`);
  }

  async waitForCallCountAfter(action, startIndex, count, predicate = () => true) {
    const started = Date.now();
    while (Date.now() - started <= 3_000) {
      const matches = this.callPayloads
        .slice(startIndex)
        .filter((entry) => entry.action === action && predicate(entry.payload));
      if (matches.length >= count) {
        return;
      }

      await sleep(20);
    }

    throw new Error(`Timed out waiting for ${count} charge point ${action} calls after index ${startIndex}`);
  }

  async waitForStationState(predicate, label) {
    const started = Date.now();
    while (Date.now() - started <= 3_000) {
      if (predicate()) {
        return;
      }

      await sleep(20);
    }

    throw new Error(`Timed out waiting for station state: ${label}; state=${JSON.stringify(this.station.state)}`);
  }

  responseFor(action, payload = {}) {
    switch (action) {
      case "BootNotification":
        if (this.bootResponses.length > 0) {
          return this.bootResponses.shift();
        }
        return { status: "Accepted", currentTime: new Date().toISOString(), interval: 5 };
      case "Heartbeat":
        return { currentTime: new Date().toISOString() };
      case "Authorize":
        return this.authorizeResponse(payload);
      case "StartTransaction":
        return this.startTransactionResponse(payload);
      case "DataTransfer":
        return { status: "Accepted", data: "pong" };
      default:
        return {};
    }
  }

  authorizeResponse(payload) {
    if (payload.idTag === "BADTAG") {
      return { idTagInfo: { status: "Blocked" } };
    }
    if (payload.idTag === "EXPIREDAUTH") {
      return { idTagInfo: { status: "Accepted", expiryDate: new Date(Date.now() - 60_000).toISOString() } };
    }
    if (payload.idTag === "PARENTAUTH") {
      return { idTagInfo: { status: "Accepted", parentIdTag: "PARENTTAG" } };
    }
    if (payload.idTag === "CONCURRENTAUTH") {
      return { idTagInfo: { status: "ConcurrentTx" } };
    }
    return { idTagInfo: { status: "Accepted" } };
  }

  startTransactionResponse(payload) {
    if (payload.idTag === "BLOCKSTART") {
      return { transactionId: 123, idTagInfo: { status: "Blocked" } };
    }
    if (payload.idTag === "EXPIREDSTART") {
      return {
        transactionId: 123,
        idTagInfo: { status: "Accepted", expiryDate: new Date(Date.now() - 60_000).toISOString() }
      };
    }
    if (payload.idTag === "CONCURRENTSTART") {
      return { transactionId: 123, idTagInfo: { status: "ConcurrentTx" } };
    }
    return { transactionId: 123, idTagInfo: { status: "Accepted" } };
  }

  consumeRemaining(map, key) {
    const remaining = map.get(key) ?? 0;
    if (remaining <= 0) {
      return false;
    }

    map.set(key, remaining - 1);
    return true;
  }
}
