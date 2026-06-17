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
  persistState: z.coerce.boolean(),
  stateDirectory: z.string().min(1).optional(),
  webSocketSubprotocol: z.string().min(1),
  webSocketPingIntervalSeconds: z.coerce.number().int().nonnegative(),
  webSocketReconnectEnabled: z.coerce.boolean(),
  webSocketReconnectInitialDelayMs: z.coerce.number().int().nonnegative(),
  webSocketReconnectMaxDelayMs: z.coerce.number().int().nonnegative(),
  webSocketReconnectMaxAttempts: z.coerce.number().int().nonnegative(),
  callTimeoutMs: z.coerce.number().int().positive(),
  tlsRejectUnauthorized: z.coerce.boolean(),
  tlsCaFile: z.string().min(1).optional(),
  tlsCertFile: z.string().min(1).optional(),
  tlsKeyFile: z.string().min(1).optional(),
  tlsServerName: z.string().min(1).optional()
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
    persistState: input.persistState ?? process.env.KIRBY_OCPP_NO_PERSIST !== "1",
    stateDirectory: input.stateDirectory ?? process.env.KIRBY_OCPP_STATE_DIR,
    webSocketSubprotocol: input.webSocketSubprotocol ?? process.env.OCPP_WS_SUBPROTOCOL ?? "ocpp1.6",
    webSocketPingIntervalSeconds: input.webSocketPingIntervalSeconds ?? process.env.OCPP_WS_PING_INTERVAL ?? 30,
    webSocketReconnectEnabled: input.webSocketReconnectEnabled ?? process.env.OCPP_RECONNECT !== "0",
    webSocketReconnectInitialDelayMs: input.webSocketReconnectInitialDelayMs ?? process.env.OCPP_RECONNECT_INITIAL_MS ?? 1_000,
    webSocketReconnectMaxDelayMs: input.webSocketReconnectMaxDelayMs ?? process.env.OCPP_RECONNECT_MAX_MS ?? 30_000,
    webSocketReconnectMaxAttempts: input.webSocketReconnectMaxAttempts ?? process.env.OCPP_RECONNECT_MAX_ATTEMPTS ?? 0,
    callTimeoutMs: input.callTimeoutMs ?? process.env.OCPP_CALL_TIMEOUT_MS ?? 30_000,
    tlsRejectUnauthorized: input.tlsRejectUnauthorized ?? process.env.OCPP_TLS_SKIP_VERIFY !== "1",
    tlsCaFile: input.tlsCaFile ?? process.env.OCPP_TLS_CA_FILE,
    tlsCertFile: input.tlsCertFile ?? process.env.OCPP_TLS_CERT_FILE,
    tlsKeyFile: input.tlsKeyFile ?? process.env.OCPP_TLS_KEY_FILE,
    tlsServerName: input.tlsServerName ?? process.env.OCPP_TLS_SERVER_NAME
  });
}
