import type { ConfigurationRegistry } from "./configuration-registry.js";
import { readString } from "./payload.js";

export interface ChangeConfigurationResult {
  status: "Accepted" | "Rejected" | "NotSupported";
  key: string;
  value: string;
}

export function changeConfiguration(
  configuration: ConfigurationRegistry,
  payload: Record<string, unknown>
): ChangeConfigurationResult {
  const key = readString(payload.key, "");
  const value = readString(payload.value, "");
  const existing = configuration.get(key);

  if (!existing) {
    return { status: "NotSupported", key, value };
  }

  if (existing.readonly || (existing.validate && !existing.validate(value))) {
    return { status: "Rejected", key, value };
  }

  configuration.setValue(key, value);
  return { status: "Accepted", key, value };
}

export function getConfiguration(
  configuration: ConfigurationRegistry,
  payload: Record<string, unknown>
): { configurationKey: Record<string, unknown>[]; unknownKey: string[] } {
  const requestedKeys = Array.isArray(payload.key) ? payload.key.filter((key): key is string => typeof key === "string") : [];
  const keys = requestedKeys.length > 0 ? requestedKeys : configuration.keys();
  const configurationKey = [];
  const unknownKey = [];

  for (const key of keys) {
    const entry = configuration.get(key);
    if (!entry) {
      unknownKey.push(key);
      continue;
    }

    configurationKey.push({ key, readonly: entry.readonly, value: entry.value });
  }

  return { configurationKey, unknownKey };
}
