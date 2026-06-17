#!/usr/bin/env node
import { Command } from "commander";
import { ZodError } from "zod";
import { parseConfig } from "./config.js";
import { Station } from "./station.js";
import { Tui } from "./tui.js";

const program = new Command();

program
  .name("kirby-ocpp")
  .description("Terminal OCPP charge point emulator")
  .option("-u, --url <url>", "Central System WebSocket URL, e.g. ws://localhost:9000/ocpp")
  .option("-i, --id <id>", "charge point id", process.env.OCPP_CHARGE_POINT_ID ?? "CP-001")
  .option("--vendor <vendor>", "charge point vendor", process.env.OCPP_VENDOR ?? "archhaze24")
  .option("--model <model>", "charge point model", process.env.OCPP_MODEL ?? "KIRBY")
  .option("-c, --connector <id>", "initial connector id", "1")
  .option("--connectors <count>", "number of station connectors", "1")
  .option("--heartbeat <seconds>", "fallback heartbeat interval in seconds", "30")
  .option("--id-tag <idTag>", "default RFID/idTag", "DEADBEEF")
  .option("--no-persist", "do not save station state such as local auth list and charging profiles")
  .option("--state-dir <path>", "directory for persisted station state")
  .option("--ws-subprotocol <protocol>", "WebSocket subprotocol to request", "ocpp1.6")
  .option("--ws-ping <seconds>", "WebSocket ping interval in seconds, 0 disables pings", "30")
  .option("--no-reconnect", "do not automatically reconnect after an unexpected WebSocket disconnect")
  .option("--reconnect-initial-ms <milliseconds>", "initial automatic reconnect delay in milliseconds", "1000")
  .option("--reconnect-max-ms <milliseconds>", "maximum automatic reconnect delay in milliseconds", "30000")
  .option("--reconnect-max-attempts <attempts>", "maximum automatic reconnect attempts, 0 means unlimited", "0")
  .option("--call-timeout-ms <milliseconds>", "OCPP CALL response timeout in milliseconds", "30000")
  .option("--tls-ca <path>", "CA certificate bundle for wss:// connections")
  .option("--tls-cert <path>", "client certificate for wss:// mutual TLS")
  .option("--tls-key <path>", "client private key for wss:// mutual TLS")
  .option("--tls-server-name <name>", "TLS server name override for wss:// connections")
  .option("--tls-skip-verify", "disable TLS certificate verification for local/self-signed wss:// testing")
  .parse();

const options = program.opts<{
  url: string;
  id: string;
  vendor: string;
  model: string;
  connector: string;
  connectors: string;
  heartbeat: string;
  idTag: string;
  persist: boolean;
  stateDir?: string;
  wsSubprotocol: string;
  wsPing: string;
  reconnect: boolean;
  reconnectInitialMs: string;
  reconnectMaxMs: string;
  reconnectMaxAttempts: string;
  callTimeoutMs: string;
  tlsCa?: string;
  tlsCert?: string;
  tlsKey?: string;
  tlsServerName?: string;
  tlsSkipVerify?: boolean;
}>();

try {
  const config = parseConfig({
    centralSystemUrl: options.url,
    chargePointId: options.id,
    vendor: options.vendor,
    model: options.model,
    connectorId: options.connector,
    connectorCount: Math.max(Number.parseInt(options.connectors, 10), Number.parseInt(options.connector, 10)),
    heartbeatIntervalSeconds: options.heartbeat,
    idTag: options.idTag,
    persistState: options.persist,
    stateDirectory: options.stateDir,
    webSocketSubprotocol: options.wsSubprotocol,
    webSocketPingIntervalSeconds: options.wsPing,
    webSocketReconnectEnabled: options.reconnect,
    webSocketReconnectInitialDelayMs: options.reconnectInitialMs,
    webSocketReconnectMaxDelayMs: options.reconnectMaxMs,
    webSocketReconnectMaxAttempts: options.reconnectMaxAttempts,
    callTimeoutMs: options.callTimeoutMs,
    tlsRejectUnauthorized: !options.tlsSkipVerify,
    tlsCaFile: options.tlsCa,
    tlsCertFile: options.tlsCert,
    tlsKeyFile: options.tlsKey,
    tlsServerName: options.tlsServerName
  });

  const station = new Station(config);
  const tui = new Tui(station);
  tui.start();
} catch (error) {
  if (error instanceof ZodError) {
    console.error(error.errors.map((issue) => `${issue.path.join(".")}: ${issue.message}`).join("\n"));
    process.exit(1);
  }

  throw error;
}
