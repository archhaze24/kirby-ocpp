import { execFileSync } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { WebSocketServer } from "ws";
import { OcppClient } from "../../dist/ocpp/client.js";

export async function withClient(run, options = {}) {
  const wss = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await waitForServer(wss);

  const address = wss.address();
  assertServerAddress(address);

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

export async function waitForServer(wss) {
  while (!wss.address()) {
    await delay(5);
  }
}

export function assertServerAddress(address) {
  if (!address || typeof address === "string") {
    throw new Error("Could not determine local WebSocket server address");
  }
}

export function once(emitter, event) {
  return new Promise((resolve) => emitter.once(event, resolve));
}

export function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export async function waitForCondition(predicate, label, timeoutMs = 2_000) {
  const started = Date.now();
  while (Date.now() - started <= timeoutMs) {
    if (predicate()) {
      return;
    }

    await delay(10);
  }

  throw new Error(`Timed out waiting for ${label}`);
}

export function createSelfSignedCertificate() {
  const directory = mkdtempSync(join(tmpdir(), "kirby-ocpp-wss-"));
  const keyPath = join(directory, "key.pem");
  const certPath = join(directory, "cert.pem");

  execFileSync(
    "openssl",
    [
      "req",
      "-x509",
      "-newkey",
      "rsa:2048",
      "-nodes",
      "-keyout",
      keyPath,
      "-out",
      certPath,
      "-days",
      "1",
      "-subj",
      "/CN=localhost"
    ],
    { stdio: "ignore" }
  );

  return { certPath, keyPath };
}

async function nextMessage(messages, setResolver) {
  if (messages.length > 0) {
    return messages.shift();
  }

  await new Promise((resolve) => setResolver(resolve));
  return messages.shift();
}
