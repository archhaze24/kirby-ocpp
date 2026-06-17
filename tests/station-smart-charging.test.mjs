import assert from "node:assert/strict";
import { test } from "node:test";
import { ChargingProfileRegistry } from "../dist/station/charging-profile-registry.js";

test("long daily recurring composite schedules include transitions across all requested cycles", () => {
  const registry = new ChargingProfileRegistry();
  const startSchedule = new Date(Date.now() - (24 * 60 * 60 - 10) * 1000).toISOString();

  assert.equal(
    registry.set(
      1,
      {
        chargingProfileId: 1,
        stackLevel: 1,
        chargingProfilePurpose: "TxDefaultProfile",
        chargingProfileKind: "Recurring",
        recurrencyKind: "Daily",
        chargingSchedule: {
          startSchedule,
          chargingRateUnit: "A",
          chargingSchedulePeriod: [
            { startPeriod: 0, limit: 17 },
            { startPeriod: 86395, limit: 12 }
          ]
        }
      },
      connectorOptions()
    ),
    "Accepted"
  );

  const response = registry.compositeSchedulePayload({ connectorId: 1, duration: 2 * 24 * 60 * 60 + 20 }, undefined);
  assert.deepEqual(limits(response), [17, 12, 17, 12, 17, 12, 17]);
});

test("requested composite schedule unit filters incompatible charging profiles", () => {
  const registry = new ChargingProfileRegistry();
  assert.equal(
    registry.set(
      0,
      {
        chargingProfileId: 2,
        stackLevel: 1,
        chargingProfilePurpose: "ChargePointMaxProfile",
        chargingProfileKind: "Absolute",
        chargingSchedule: {
          chargingRateUnit: "A",
          chargingSchedulePeriod: [{ startPeriod: 0, limit: 20 }]
        }
      },
      connectorOptions()
    ),
    "Accepted"
  );

  assert.equal(registry.compositeSchedulePayload({ connectorId: 1, duration: 60, chargingRateUnit: "W" }, undefined), undefined);
});

test("expired charging profiles are pruned during composite schedule calculation", () => {
  const registry = new ChargingProfileRegistry();
  assert.equal(
    registry.set(
      1,
      {
        chargingProfileId: 3,
        stackLevel: 1,
        chargingProfilePurpose: "TxDefaultProfile",
        chargingProfileKind: "Absolute",
        validTo: new Date(Date.now() - 1_000).toISOString(),
        chargingSchedule: {
          chargingRateUnit: "A",
          chargingSchedulePeriod: [{ startPeriod: 0, limit: 8 }]
        }
      },
      connectorOptions()
    ),
    "Accepted"
  );

  const response = registry.compositeSchedulePayload({ connectorId: 1, duration: 60 }, undefined);
  assert.equal(response, undefined);
  assert.equal(registry.length, 0);
});

function connectorOptions() {
  return {
    hasConnector: (connectorId) => connectorId === 1,
    transactionIdForConnector: () => undefined
  };
}

function limits(response) {
  return response.payload.chargingSchedule.chargingSchedulePeriod.map((period) => period.limit);
}
