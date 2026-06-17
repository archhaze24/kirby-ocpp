import type { StationConfig } from "../ocpp/types.js";
import { createDefaultConfiguration, type ConfigurationKey } from "./configuration.js";
import { readConfiguredMeasurands, type Measurand, type MeteringConfigurationKey } from "./metering.js";

export class ConfigurationRegistry {
  readonly values: Map<string, ConfigurationKey>;

  constructor(config: StationConfig) {
    this.values = createDefaultConfiguration(config);
  }

  get(key: string): ConfigurationKey | undefined {
    return this.values.get(key);
  }

  keys(): string[] {
    return [...this.values.keys()];
  }

  setValue(key: string, value: string): void {
    const existing = this.values.get(key);
    if (!existing) {
      return;
    }

    this.values.set(key, { ...existing, value });
  }

  boolean(key: string, fallback: boolean): boolean {
    const value = this.values.get(key)?.value;
    if (value === undefined) {
      return fallback;
    }

    return value.toLowerCase() === "true";
  }

  integer(key: string, fallback: number): number {
    const value = this.values.get(key)?.value;
    if (value === undefined) {
      return fallback;
    }

    const parsed = Number.parseInt(value, 10);
    return Number.isSafeInteger(parsed) ? parsed : fallback;
  }

  measurands(key: MeteringConfigurationKey): Measurand[] {
    return readConfiguredMeasurands(this.values.get(key)?.value);
  }
}
