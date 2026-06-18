import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createServer as createHttpsServer } from "node:https";
import { test } from "node:test";
import { WebSocketServer } from "ws";
import { parseConfig } from "../dist/config.js";
import { OcppClient } from "../dist/ocpp/client.js";
import {
  assertServerAddress,
  createSelfSignedCertificate,
  once,
  waitForServer,
  withClient
} from "./support/ocpp-test-utils.mjs";

test("negotiates the ocpp1.6 WebSocket subprotocol", { timeout: 5_000 }, async () => {
  await withClient(async ({ socket }) => {
    assert.equal(socket.protocol, "ocpp1.6");
  });
});

test("sends WebSocket ping frames when configured", { timeout: 5_000 }, async () => {
  await withClient(
    async ({ socket }) => {
      await once(socket, "ping");
    },
    { clientOptions: { pingIntervalSeconds: 0.01 } }
  );
});

test("terminates the connection when pong is not received", { timeout: 5_000 }, async () => {
  const wss = new WebSocketServer({ host: "127.0.0.1", port: 0, autoPong: false });
  await waitForServer(wss);

  const address = wss.address();
  assertServerAddress(address);

  const connection = once(wss, "connection");
  const client = new OcppClient(`ws://127.0.0.1:${address.port}/ocpp`, "CP-001", {
    pingIntervalSeconds: 0.01
  });
  const error = once(client, "error");
  const disconnected = once(client, "disconnected");

  try {
    client.connect();
    const socket = await connection;
    await once(client, "connected");
    await once(socket, "ping");

    const receivedError = await error;
    assert.match(receivedError.message, /pong timed out/);
    await disconnected;
  } finally {
    client.close();
    wss.close();
  }
});

test("closes the connection when ocpp1.6 subprotocol is not negotiated", { timeout: 5_000 }, async () => {
  const wss = new WebSocketServer({
    host: "127.0.0.1",
    port: 0,
    handleProtocols: () => false
  });
  await waitForServer(wss);

  const address = wss.address();
  assertServerAddress(address);

  const client = new OcppClient(`ws://127.0.0.1:${address.port}/ocpp`, "CP-001");
  const error = once(client, "error");

  try {
    client.connect();
    const receivedError = await error;
    assert.match(
      receivedError.message,
      /did not negotiate required WebSocket subprotocol ocpp1\.6|Server sent no subprotocol/
    );
  } finally {
    client.close();
    wss.close();
  }
});

test("closing while the WebSocket handshake is still pending does not throw", () => {
  const client = new OcppClient("ws://10.255.255.1:65535/ocpp", "CP-001", {
    pingIntervalSeconds: 0
  });

  client.on("error", () => {});
  client.connect();
  assert.doesNotThrow(() => client.close());
});

test("connects to self-signed wss when TLS verification is disabled", { timeout: 5_000 }, async () => {
  const certificate = createSelfSignedCertificate();
  const httpsServer = createHttpsServer({
    cert: readFileSync(certificate.certPath),
    key: readFileSync(certificate.keyPath)
  });
  const wss = new WebSocketServer({ server: httpsServer });
  httpsServer.listen(0, "127.0.0.1");
  await once(httpsServer, "listening");

  const address = httpsServer.address();
  assertServerAddress(address);

  const connection = once(wss, "connection");
  const client = new OcppClient(`wss://127.0.0.1:${address.port}/ocpp`, "CP-001", {
    tlsRejectUnauthorized: false
  });

  try {
    client.connect();
    const [socket] = await Promise.all([connection, once(client, "connected")]);
    assert.equal(socket.protocol, "ocpp1.6");
  } finally {
    client.close();
    wss.close();
    httpsServer.close();
  }
});

test("rejects self-signed wss with default TLS verification", { timeout: 5_000 }, async () => {
  const certificate = createSelfSignedCertificate();
  const httpsServer = createHttpsServer({
    cert: readFileSync(certificate.certPath),
    key: readFileSync(certificate.keyPath)
  });
  const wss = new WebSocketServer({ server: httpsServer });
  httpsServer.listen(0, "127.0.0.1");
  await once(httpsServer, "listening");

  const address = httpsServer.address();
  assertServerAddress(address);

  const client = new OcppClient(`wss://127.0.0.1:${address.port}/ocpp`, "CP-001");
  const error = once(client, "error");

  try {
    client.connect();
    const receivedError = await error;
    assert.match(receivedError.message, /self-signed certificate|unable to verify the first certificate/);
  } finally {
    client.close();
    wss.close();
    httpsServer.close();
  }
});

test("connects to wss with CA file and server name override", { timeout: 5_000 }, async () => {
  const certificate = createSelfSignedCertificate();
  const httpsServer = createHttpsServer({
    cert: readFileSync(certificate.certPath),
    key: readFileSync(certificate.keyPath)
  });
  const wss = new WebSocketServer({ server: httpsServer });
  httpsServer.listen(0, "127.0.0.1");
  await once(httpsServer, "listening");

  const address = httpsServer.address();
  assertServerAddress(address);

  const connection = once(wss, "connection");
  const client = new OcppClient(`wss://127.0.0.1:${address.port}/ocpp`, "CP-001", {
    tlsCaFile: certificate.certPath,
    tlsServerName: "localhost"
  });

  try {
    client.connect();
    const [socket] = await Promise.all([connection, once(client, "connected")]);
    assert.equal(socket.protocol, "ocpp1.6");
  } finally {
    client.close();
    wss.close();
    httpsServer.close();
  }
});

test("connects to wss with mutual TLS client certificate", { timeout: 5_000 }, async () => {
  const certificate = createSelfSignedCertificate();
  const cert = readFileSync(certificate.certPath);
  const key = readFileSync(certificate.keyPath);
  const httpsServer = createHttpsServer({
    ca: cert,
    cert,
    key,
    rejectUnauthorized: true,
    requestCert: true
  });
  const wss = new WebSocketServer({ server: httpsServer });
  httpsServer.listen(0, "127.0.0.1");
  await once(httpsServer, "listening");

  const address = httpsServer.address();
  assertServerAddress(address);

  const connection = once(wss, "connection");
  const client = new OcppClient(`wss://127.0.0.1:${address.port}/ocpp`, "CP-001", {
    tlsCaFile: certificate.certPath,
    tlsCertFile: certificate.certPath,
    tlsKeyFile: certificate.keyPath,
    tlsServerName: "localhost"
  });

  try {
    client.connect();
    const [socket] = await Promise.all([connection, once(client, "connected")]);
    assert.equal(socket.protocol, "ocpp1.6");
  } finally {
    client.close();
    wss.close();
    httpsServer.close();
  }
});

test("parses WebSocket and TLS security config", () => {
  const config = parseConfig({
    centralSystemUrl: "wss://central.example/ocpp",
    tlsRejectUnauthorized: false,
    tlsCaFile: "/tmp/ca.pem",
    tlsCertFile: "/tmp/client.pem",
    tlsKeyFile: "/tmp/client-key.pem",
    tlsServerName: "central.example",
    webSocketSubprotocol: "ocpp1.6",
    webSocketPingIntervalSeconds: 45,
    webSocketReconnectEnabled: false,
    webSocketReconnectInitialDelayMs: 25,
    webSocketReconnectMaxDelayMs: 250,
    webSocketReconnectMaxAttempts: 3,
    callTimeoutMs: 1234
  });

  assert.equal(config.tlsRejectUnauthorized, false);
  assert.equal(config.tlsCaFile, "/tmp/ca.pem");
  assert.equal(config.tlsCertFile, "/tmp/client.pem");
  assert.equal(config.tlsKeyFile, "/tmp/client-key.pem");
  assert.equal(config.tlsServerName, "central.example");
  assert.equal(config.webSocketSubprotocol, "ocpp1.6");
  assert.equal(config.webSocketPingIntervalSeconds, 45);
  assert.equal(config.webSocketReconnectEnabled, false);
  assert.equal(config.webSocketReconnectInitialDelayMs, 25);
  assert.equal(config.webSocketReconnectMaxDelayMs, 250);
  assert.equal(config.webSocketReconnectMaxAttempts, 3);
  assert.equal(config.callTimeoutMs, 1234);
});
