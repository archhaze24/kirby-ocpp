import { z } from "zod";
import type { StationConfig } from "./ocpp/types.js";

const configSchema = z.object({
  centralSystemUrl: z
    .string({
      required_error: "Use --url or OCPP_URL to set the Central System WebSocket URL"
    })
    .url(),
  chargePointId: z.string().min(1),
  vendor: z.string().min(1),
  model: z.string().min(1),
  connectorId: z.coerce.number().int().positive(),
  connectorCount: z.coerce.number().int().positive(),
  heartbeatIntervalSeconds: z.coerce.number().int().positive(),
  idTag: z.string().min(1),
  persistState: z.coerce.boolean(),
  stateDirectory: z.string().min(1).optional()
});

export function parseConfig(input: Partial<Record<keyof StationConfig, unknown>>): StationConfig {
  return configSchema.parse({
    centralSystemUrl: input.centralSystemUrl ?? process.env.OCPP_URL,
    chargePointId: input.chargePointId ?? process.env.OCPP_CHARGE_POINT_ID ?? "CP-001",
    vendor: input.vendor ?? process.env.OCPP_VENDOR ?? "archhaze24",
    model: input.model ?? process.env.OCPP_MODEL ?? "KIRBY",
    connectorId: input.connectorId ?? 1,
    connectorCount: input.connectorCount ?? input.connectorId ?? 1,
    heartbeatIntervalSeconds: input.heartbeatIntervalSeconds ?? 30,
    idTag: input.idTag ?? "DEADBEEF",
    persistState: input.persistState ?? process.env.KIRBY_OCPP_NO_PERSIST !== "1",
    stateDirectory: input.stateDirectory ?? process.env.KIRBY_OCPP_STATE_DIR
  });
}
