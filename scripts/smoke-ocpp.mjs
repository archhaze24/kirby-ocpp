import { SmokeHarness } from "./smoke/harness.mjs";
import { runAvailabilityResetPersistenceScenario } from "./smoke/scenarios/availability-reset-persistence.mjs";
import { runBaselineScenario } from "./smoke/scenarios/baseline.mjs";
import { runConfigurationAuthScenario } from "./smoke/scenarios/configuration-auth.mjs";
import { runMaintenanceScenario } from "./smoke/scenarios/maintenance.mjs";
import { runNegativeCsmsScenario } from "./smoke/scenarios/negative-csms.mjs";
import { runSmartChargingScenario } from "./smoke/scenarios/smart-charging.mjs";
import { runStatusTransactionsScenario } from "./smoke/scenarios/status-transactions.mjs";

const smoke = await SmokeHarness.create();
const context = createSmokeContext(smoke);

const baseline = await runBaselineScenario(context);
Object.assign(context, baseline);
await runMaintenanceScenario(context);
await runConfigurationAuthScenario(context);
await runNegativeCsmsScenario(context);
await runSmartChargingScenario(context);
await runStatusTransactionsScenario(context);
await runAvailabilityResetPersistenceScenario(context);

console.log(
  `ok ${context.commands.length} CSMS commands; edge checks=${smoke.edgeChecks}; charge point calls=${[...new Set(smoke.calls)].sort().join(",")}`
);
process.exit(0);

function createSmokeContext(smoke) {
  return {
    smoke,
    station: smoke.station,
    stationConfig: smoke.stationConfig,
    calls: smoke.calls,
    callPayloads: smoke.callPayloads,
    callErrorsRemaining: smoke.callErrorsRemaining,
    silentTimeoutsRemaining: smoke.silentTimeoutsRemaining,
    closeOnCallRemaining: smoke.closeOnCallRemaining,
    bootResponses: smoke.bootResponses,
    sendCentralSystemCall: smoke.sendCentralSystemCall.bind(smoke),
    expectResponseStatus: smoke.expectResponseStatus.bind(smoke),
    expectCallError: smoke.expectCallError.bind(smoke),
    waitForCallAfter: smoke.waitForCallAfter.bind(smoke),
    waitForCallCountAfter: smoke.waitForCallCountAfter.bind(smoke),
    waitForStationState: smoke.waitForStationState.bind(smoke)
  };
}
