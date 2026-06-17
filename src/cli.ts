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
    stateDirectory: options.stateDir
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
