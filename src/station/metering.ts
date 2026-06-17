import type { ConnectorStatus } from "../ocpp/types.js";

export type MeterValueContext = "Sample.Clock" | "Sample.Periodic" | "Transaction.End";

export type Measurand =
  | "Energy.Active.Export.Register"
  | "Energy.Active.Import.Register"
  | "Energy.Reactive.Export.Register"
  | "Energy.Reactive.Import.Register"
  | "Energy.Active.Export.Interval"
  | "Energy.Active.Import.Interval"
  | "Energy.Reactive.Export.Interval"
  | "Energy.Reactive.Import.Interval"
  | "Power.Active.Export"
  | "Power.Active.Import"
  | "Power.Offered"
  | "Power.Reactive.Export"
  | "Power.Reactive.Import"
  | "Power.Factor"
  | "Current.Import"
  | "Current.Export"
  | "Current.Offered"
  | "Voltage"
  | "Frequency"
  | "Temperature"
  | "SoC"
  | "RPM";

export type MeteringConfigurationKey =
  | "MeterValuesAlignedData"
  | "MeterValuesSampledData"
  | "StopTxnAlignedData"
  | "StopTxnSampledData";

export interface MeteringConnector {
  status: ConnectorStatus;
  meterWh: number;
}

const DEFAULT_SAMPLED_DATA: Measurand[] = ["Energy.Active.Import.Register"];

export const MEASURANDS = new Set<Measurand>([
  "Energy.Active.Export.Register",
  "Energy.Active.Import.Register",
  "Energy.Reactive.Export.Register",
  "Energy.Reactive.Import.Register",
  "Energy.Active.Export.Interval",
  "Energy.Active.Import.Interval",
  "Energy.Reactive.Export.Interval",
  "Energy.Reactive.Import.Interval",
  "Power.Active.Export",
  "Power.Offered",
  "Power.Active.Import",
  "Power.Reactive.Export",
  "Power.Reactive.Import",
  "Power.Factor",
  "Current.Import",
  "Current.Export",
  "Current.Offered",
  "Voltage",
  "Frequency",
  "Temperature",
  "SoC",
  "RPM"
]);

export function buildMeterValue(
  connector: MeteringConnector,
  context: MeterValueContext,
  measurands: Measurand[]
): Record<string, unknown> {
  return {
    timestamp: new Date().toISOString(),
    sampledValue: measurands.map((measurand) => buildSampledValue(connector, context, measurand))
  };
}

export function readConfiguredMeasurands(value: string | undefined): Measurand[] {
  const measurands = (value ?? "")
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean)
    .filter((item): item is Measurand => MEASURANDS.has(item as Measurand));

  return measurands.length > 0 ? measurands : DEFAULT_SAMPLED_DATA;
}

export function isValidSampledDataConfiguration(value: string): boolean {
  const items = value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);

  return items.length > 0 && items.every((item) => MEASURANDS.has(item as Measurand));
}

export function periodicMeterDeltaWh(intervalSeconds: number): number {
  return Math.max(0, Math.round((7200 * intervalSeconds) / 3600));
}

export function clockAlignedDelayMs(intervalSeconds: number): number {
  const intervalMs = intervalSeconds * 1000;
  const remainder = Date.now() % intervalMs;
  return remainder === 0 ? intervalMs : intervalMs - remainder;
}

function buildSampledValue(
  connector: MeteringConnector,
  context: MeterValueContext,
  measurand: Measurand
): Record<string, unknown> {
  const sample = sampleForMeasurand(connector, measurand);
  const sampledValue: Record<string, unknown> = {
    value: sample.value,
    context,
    format: "Raw",
    measurand
  };

  if (sample.location) {
    sampledValue.location = sample.location;
  }
  if (sample.phase) {
    sampledValue.phase = sample.phase;
  }
  if (sample.unit) {
    sampledValue.unit = sample.unit;
  }

  return sampledValue;
}

function sampleForMeasurand(
  connector: MeteringConnector,
  measurand: Measurand
): { value: string; unit?: string; location?: string; phase?: string } {
  const charging = connector.status === "Charging";
  const activePowerW = charging ? 7200 : 0;
  const currentA = charging ? 32 : 0;

  switch (measurand) {
    case "Energy.Active.Import.Register":
      return { value: String(Math.round(connector.meterWh)), unit: "Wh", location: "Outlet" };
    case "Energy.Active.Export.Register":
      return { value: "0", unit: "Wh", location: "Outlet" };
    case "Energy.Reactive.Import.Register":
    case "Energy.Reactive.Export.Register":
      return { value: "0", unit: "varh", location: "Outlet" };
    case "Energy.Active.Import.Interval":
      return { value: charging ? "120" : "0", unit: "Wh", location: "Outlet" };
    case "Energy.Active.Export.Interval":
      return { value: "0", unit: "Wh", location: "Outlet" };
    case "Energy.Reactive.Import.Interval":
    case "Energy.Reactive.Export.Interval":
      return { value: "0", unit: "varh", location: "Outlet" };
    case "Power.Active.Import":
      return { value: String(activePowerW), unit: "W", location: "Outlet" };
    case "Power.Active.Export":
      return { value: "0", unit: "W", location: "Outlet" };
    case "Power.Offered":
      return { value: "7400", unit: "W", location: "Outlet" };
    case "Power.Reactive.Import":
    case "Power.Reactive.Export":
      return { value: "0", unit: "var", location: "Outlet" };
    case "Power.Factor":
      return { value: charging ? "1" : "0", location: "Outlet" };
    case "Current.Import":
      return { value: String(currentA), unit: "A", location: "Outlet", phase: "L1" };
    case "Current.Export":
      return { value: "0", unit: "A", location: "Outlet", phase: "L1" };
    case "Current.Offered":
      return { value: "32", unit: "A", location: "Outlet", phase: "L1" };
    case "Voltage":
      return { value: "230", unit: "V", location: "Outlet", phase: "L1-N" };
    case "Temperature":
      return { value: charging ? "34" : "25", unit: "Celsius", location: "Body" };
    case "SoC":
      return { value: charging ? "80" : "0", unit: "Percent", location: "EV" };
    case "Frequency":
      return { value: "50", location: "Outlet" };
    case "RPM":
      return { value: "0", location: "Body" };
  }
}
