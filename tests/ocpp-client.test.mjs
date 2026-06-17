import assert from "node:assert/strict";
import { test } from "node:test";
import { WebSocketServer } from "ws";
import { parseConfig } from "../dist/config.js";
import { OcppClient } from "../dist/ocpp/client.js";

test("responds with OccurenceConstraintViolation for missing required payload fields", { timeout: 5_000 }, async () => {
  await withClient(async ({ socket, nextClientMessage }) => {
    socket.send(JSON.stringify([2, "csms-1", "RemoteStartTransaction", {}]));

    const message = await nextClientMessage();
    assert.equal(message[0], 4);
    assert.equal(message[1], "csms-1");
    assert.equal(message[2], "OccurenceConstraintViolation");
  });
});

test("responds with PropertyConstraintViolation for enum violations", { timeout: 5_000 }, async () => {
  await withClient(async ({ socket, nextClientMessage }) => {
    socket.send(JSON.stringify([2, "csms-2", "Reset", { type: "Warm" }]));

    const message = await nextClientMessage();
    assert.equal(message[0], 4);
    assert.equal(message[1], "csms-2");
    assert.equal(message[2], "PropertyConstraintViolation");
  });
});

test("responds with FormationViolation for malformed CALL frames with message id", { timeout: 5_000 }, async () => {
  await withClient(async ({ socket, nextClientMessage }) => {
    socket.send(JSON.stringify([2, "csms-3", "Reset"]));

    const message = await nextClientMessage();
    assert.equal(message[0], 4);
    assert.equal(message[1], "csms-3");
    assert.equal(message[2], "FormationViolation");
  });
});

test("rejects pending calls when CALLRESULT violates response schema", { timeout: 5_000 }, async () => {
  await withClient(async ({ client, socket, nextServerMessage }) => {
    const promise = client.call("BootNotification", {
      chargePointVendor: "Kirby",
      chargePointModel: "TUI-16"
    });

    const call = await nextServerMessage();
    assert.equal(call[0], 2);
    assert.equal(call[2], "BootNotification");

    socket.send(JSON.stringify([3, call[1], { status: "Accepted" }]));

    await assert.rejects(promise, /OccurenceConstraintViolation/);
  });
});

test("preserves schema properties named id when stripping metadata", { timeout: 5_000 }, async () => {
  await withClient(async ({ client, socket, nextClientMessage }) => {
    const call = new Promise((resolve) => {
      client.once("call", (messageId, action, payload) => {
        client.reply(messageId, { status: "Unknown" });
        resolve({ action, payload });
      });
    });

    socket.send(JSON.stringify([2, "csms-clear-profile", "ClearChargingProfile", { id: 424242 }]));

    const received = await call;
    assert.deepEqual(received, { action: "ClearChargingProfile", payload: { id: 424242 } });

    const message = await nextClientMessage();
    assert.equal(message[0], 3);
    assert.equal(message[1], "csms-clear-profile");
    assert.equal(message[2].status, "Unknown");
  });
});

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

test("parses WebSocket and TLS security config", () => {
  const config = parseConfig({
    centralSystemUrl: "wss://central.example/ocpp",
    tlsRejectUnauthorized: false,
    tlsCaFile: "/tmp/ca.pem",
    tlsCertFile: "/tmp/client.pem",
    tlsKeyFile: "/tmp/client-key.pem",
    tlsServerName: "central.example",
    webSocketSubprotocol: "ocpp1.6",
    webSocketPingIntervalSeconds: 45
  });

  assert.equal(config.tlsRejectUnauthorized, false);
  assert.equal(config.tlsCaFile, "/tmp/ca.pem");
  assert.equal(config.tlsCertFile, "/tmp/client.pem");
  assert.equal(config.tlsKeyFile, "/tmp/client-key.pem");
  assert.equal(config.tlsServerName, "central.example");
  assert.equal(config.webSocketSubprotocol, "ocpp1.6");
  assert.equal(config.webSocketPingIntervalSeconds, 45);
});

async function withClient(run, options = {}) {
  const wss = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await waitForServer(wss);

  const address = wss.address();
  assert(address && typeof address !== "string");

  const serverMessages = [];
  const clientMessages = [];
  let resolveServerMessage;
  let resolveClientMessage;

  const connection = new Promise((resolve) => {
    wss.once("connection", (socket) => {
      socket.on("message", (data) => {
        serverMessages.push(JSON.parse(data.toString()));
        resolveServerMessage?.();
      });
      resolve(socket);
    });
  });

  const client = new OcppClient(`ws://127.0.0.1:${address.port}/ocpp`, "CP-001", options.clientOptions);
  client.on("log", (direction, message) => {
    if (direction === "out") {
      clientMessages.push(message);
      resolveClientMessage?.();
    }
  });
  client.on("error", () => {});
  client.connect();

  const [socket] = await Promise.all([connection, once(client, "connected")]);

  try {
    await run({
      client,
      socket,
      nextClientMessage: () => nextMessage(clientMessages, (resolve) => {
        resolveClientMessage = resolve;
      }),
      nextServerMessage: () => nextMessage(serverMessages, (resolve) => {
        resolveServerMessage = resolve;
      })
    });
  } finally {
    client.close();
    socket.close();
    wss.close();
  }
}

async function nextMessage(messages, setResolver) {
  if (messages.length > 0) {
    return messages.shift();
  }

  await new Promise((resolve) => setResolver(resolve));
  return messages.shift();
}

async function waitForServer(wss) {
  while (!wss.address()) {
    await delay(5);
  }
}

function once(emitter, event) {
  return new Promise((resolve) => emitter.once(event, resolve));
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
