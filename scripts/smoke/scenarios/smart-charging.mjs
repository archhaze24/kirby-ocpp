import { assertCompositeLimits, assertCompositeSchedule } from "../assertions.mjs";

export async function runSmartChargingScenario(context) {
  const {
    sendCentralSystemCall,
    expectResponseStatus
  } = context;

  // Smart Charging storage, filtering, priority, recurrency, and composite schedule behavior.
  const invalidChargingConnectorResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 99,
    csChargingProfiles: {
      chargingProfileId: 991,
      stackLevel: 1,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Absolute",
      chargingSchedule: {
        chargingRateUnit: "A",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 8 }]
      }
    }
  });
  if (invalidChargingConnectorResponse[2]?.status !== "Rejected") {
    throw new Error(`SetChargingProfile accepted unknown connector: ${JSON.stringify(invalidChargingConnectorResponse)}`);
  }
  
  const txProfileWithoutTransactionResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 992,
      stackLevel: 1,
      chargingProfilePurpose: "TxProfile",
      chargingProfileKind: "Absolute",
      chargingSchedule: {
        chargingRateUnit: "A",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 8 }]
      }
    }
  });
  if (txProfileWithoutTransactionResponse[2]?.status !== "Rejected") {
    throw new Error(`SetChargingProfile accepted TxProfile without transaction: ${JSON.stringify(txProfileWithoutTransactionResponse)}`);
  }
  
  const unknownCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 99, duration: 60 });
  if (unknownCompositeResponse[2]?.status !== "Rejected") {
    throw new Error(`GetCompositeSchedule accepted unknown connector: ${JSON.stringify(unknownCompositeResponse)}`);
  }
  await expectResponseStatus("GetCompositeSchedule", { connectorId: 1, duration: 0 }, "Rejected", "zero-duration composite schedule");
  
  const unknownClearProfileResponse = await sendCentralSystemCall("ClearChargingProfile", { id: 424242 });
  if (unknownClearProfileResponse[2]?.status !== "Unknown") {
    throw new Error(`ClearChargingProfile unknown profile did not return Unknown: ${JSON.stringify(unknownClearProfileResponse)}`);
  }
  
  const cpMaxProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 0,
    csChargingProfiles: {
      chargingProfileId: 2,
      stackLevel: 1,
      chargingProfilePurpose: "ChargePointMaxProfile",
      chargingProfileKind: "Absolute",
      chargingSchedule: {
        chargingRateUnit: "A",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 20 }]
      }
    }
  });
  if (cpMaxProfileResponse[2]?.status !== "Accepted") {
    throw new Error(`ChargePointMaxProfile was not accepted: ${JSON.stringify(cpMaxProfileResponse)}`);
  }
  
  const highStackDefaultProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 3,
      stackLevel: 2,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Absolute",
      chargingSchedule: {
        chargingRateUnit: "A",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 32 }]
      }
    }
  });
  if (highStackDefaultProfileResponse[2]?.status !== "Accepted") {
    throw new Error(`Higher stack TxDefaultProfile was not accepted: ${JSON.stringify(highStackDefaultProfileResponse)}`);
  }
  
  const expiredProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 4,
      stackLevel: 100,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Absolute",
      validTo: new Date(Date.now() - 60_000).toISOString(),
      chargingSchedule: {
        chargingRateUnit: "A",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 1 }]
      }
    }
  });
  if (expiredProfileResponse[2]?.status !== "Accepted") {
    throw new Error(`Expired TxDefaultProfile was not accepted for pruning test: ${JSON.stringify(expiredProfileResponse)}`);
  }
  
  const priorityCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 1, duration: 60 });
  const priorityLimit = priorityCompositeResponse[2]?.chargingSchedule?.chargingSchedulePeriod?.[0]?.limit;
  if (priorityCompositeResponse[2]?.status !== "Accepted" || priorityLimit !== 20) {
    throw new Error(`Composite schedule did not honor purpose/stack priority: ${JSON.stringify(priorityCompositeResponse)}`);
  }
  
  await expectResponseStatus(
    "GetCompositeSchedule",
    { connectorId: 1, duration: 60, chargingRateUnit: "W" },
    "Rejected",
    "composite schedule for unavailable requested unit"
  );
  
  const multiPeriodProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 6,
      stackLevel: 3,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Absolute",
      chargingSchedule: {
        duration: 90,
        chargingRateUnit: "A",
        chargingSchedulePeriod: [
          { startPeriod: 0, limit: 18 },
          { startPeriod: 30, limit: 12 }
        ]
      }
    }
  });
  if (multiPeriodProfileResponse[2]?.status !== "Accepted") {
    throw new Error(`Multi-period TxDefaultProfile was not accepted: ${JSON.stringify(multiPeriodProfileResponse)}`);
  }
  
  const multiPeriodCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 1, duration: 90 });
  assertCompositeLimits(multiPeriodCompositeResponse, [18, 12], "absolute multi-period composite schedule");
  
  await expectResponseStatus("ClearChargingProfile", { id: 6 }, "Accepted", "clear multi-period charging profile");
  
  const recurringStart = new Date(Date.now() - (24 * 60 * 60 - 20) * 1000).toISOString();
  const recurringProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 7,
      stackLevel: 3,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Recurring",
      recurrencyKind: "Daily",
      chargingSchedule: {
        startSchedule: recurringStart,
        chargingRateUnit: "A",
        chargingSchedulePeriod: [
          { startPeriod: 0, limit: 14 },
          { startPeriod: 86390, limit: 11 }
        ]
      }
    }
  });
  if (recurringProfileResponse[2]?.status !== "Accepted") {
    throw new Error(`Recurring TxDefaultProfile was not accepted: ${JSON.stringify(recurringProfileResponse)}`);
  }
  
  const recurringCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 1, duration: 30 });
  assertCompositeLimits(recurringCompositeResponse, [14, 11, 14], "daily recurring composite schedule");
  
  await expectResponseStatus("ClearChargingProfile", { id: 7 }, "Accepted", "clear recurring charging profile");
  
  const futureValidFromProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 9,
      stackLevel: 99,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Absolute",
      validFrom: new Date(Date.now() + 60_000).toISOString(),
      chargingSchedule: {
        chargingRateUnit: "A",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 1 }]
      }
    }
  });
  if (futureValidFromProfileResponse[2]?.status !== "Accepted") {
    throw new Error(`Future validFrom TxDefaultProfile was not accepted: ${JSON.stringify(futureValidFromProfileResponse)}`);
  }
  
  const futureValidFromCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 1, duration: 60 });
  assertCompositeLimits(futureValidFromCompositeResponse, [20], "future validFrom profile should not affect current composite schedule");
  
  await expectResponseStatus("ClearChargingProfile", { id: 9 }, "Accepted", "clear future validFrom charging profile");
  
  const activatingValidFromProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 12,
      stackLevel: 3,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Absolute",
      validFrom: new Date(Date.now() + 20_000).toISOString(),
      chargingSchedule: {
        chargingRateUnit: "A",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 10 }]
      }
    }
  });
  if (activatingValidFromProfileResponse[2]?.status !== "Accepted") {
    throw new Error(`Activating validFrom TxDefaultProfile was not accepted: ${JSON.stringify(activatingValidFromProfileResponse)}`);
  }
  
  const activatingValidFromCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 1, duration: 40 });
  assertCompositeLimits(activatingValidFromCompositeResponse, [20, 10], "validFrom activation inside composite duration");
  
  await expectResponseStatus("ClearChargingProfile", { id: 12 }, "Accepted", "clear activating validFrom charging profile");
  
  const phasesProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 13,
      stackLevel: 3,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Absolute",
      chargingSchedule: {
        chargingRateUnit: "A",
        minChargingRate: 6,
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 18, numberPhases: 3 }]
      }
    }
  });
  if (phasesProfileResponse[2]?.status !== "Accepted") {
    throw new Error(`numberPhases/minChargingRate TxDefaultProfile was not accepted: ${JSON.stringify(phasesProfileResponse)}`);
  }
  
  const phasesCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 1, duration: 60 });
  assertCompositeSchedule(phasesCompositeResponse, {
    limits: [18],
    minChargingRate: 6,
    numberPhases: [3],
    label: "numberPhases and minChargingRate propagation"
  });
  
  await expectResponseStatus("ClearChargingProfile", { id: 13 }, "Accepted", "clear phases charging profile");
  
  const sameStackFirstResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 14,
      stackLevel: 3,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Absolute",
      chargingSchedule: {
        chargingRateUnit: "A",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 17 }]
      }
    }
  });
  const sameStackSecondResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 15,
      stackLevel: 3,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Absolute",
      chargingSchedule: {
        chargingRateUnit: "A",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 15 }]
      }
    }
  });
  if (sameStackFirstResponse[2]?.status !== "Accepted" || sameStackSecondResponse[2]?.status !== "Accepted") {
    throw new Error(`Same-stack TxDefaultProfiles were not accepted: ${JSON.stringify([sameStackFirstResponse, sameStackSecondResponse])}`);
  }
  
  const sameStackCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 1, duration: 60 });
  assertCompositeLimits(sameStackCompositeResponse, [15], "same-stack latest profile tie behavior");
  
  await expectResponseStatus("ClearChargingProfile", { id: 14 }, "Accepted", "clear first same-stack charging profile");
  await expectResponseStatus("ClearChargingProfile", { id: 15 }, "Accepted", "clear second same-stack charging profile");
  
  const clearFilterConnectorOneResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 16,
      stackLevel: 4,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Absolute",
      chargingSchedule: {
        chargingRateUnit: "A",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 19 }]
      }
    }
  });
  const clearFilterConnectorTwoResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 2,
    csChargingProfiles: {
      chargingProfileId: 17,
      stackLevel: 4,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Absolute",
      chargingSchedule: {
        chargingRateUnit: "A",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 18 }]
      }
    }
  });
  if (clearFilterConnectorOneResponse[2]?.status !== "Accepted" || clearFilterConnectorTwoResponse[2]?.status !== "Accepted") {
    throw new Error(`ClearChargingProfile filter setup failed: ${JSON.stringify([clearFilterConnectorOneResponse, clearFilterConnectorTwoResponse])}`);
  }
  await expectResponseStatus(
    "ClearChargingProfile",
    { connectorId: 1, chargingProfilePurpose: "TxDefaultProfile", stackLevel: 4 },
    "Accepted",
    "clear charging profile by connector/purpose/stack"
  );
  const connectorOneAfterFilteredClear = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 1, duration: 60 });
  assertCompositeLimits(connectorOneAfterFilteredClear, [20], "filtered clear removed connector 1 profile");
  const connectorTwoAfterFilteredClear = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 2, duration: 60 });
  assertCompositeLimits(connectorTwoAfterFilteredClear, [18], "filtered clear preserved connector 2 profile");
  await expectResponseStatus(
    "ClearChargingProfile",
    { connectorId: 2, chargingProfilePurpose: "TxDefaultProfile", stackLevel: 4 },
    "Accepted",
    "clear remaining connector 2 profile"
  );
  
  const weeklyStart = new Date(Date.now() - (7 * 24 * 60 * 60 - 20) * 1000).toISOString();
  const weeklyProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 10,
      stackLevel: 3,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Recurring",
      recurrencyKind: "Weekly",
      chargingSchedule: {
        startSchedule: weeklyStart,
        chargingRateUnit: "A",
        chargingSchedulePeriod: [
          { startPeriod: 0, limit: 15 },
          { startPeriod: 604790, limit: 13 }
        ]
      }
    }
  });
  if (weeklyProfileResponse[2]?.status !== "Accepted") {
    throw new Error(`Weekly recurring TxDefaultProfile was not accepted: ${JSON.stringify(weeklyProfileResponse)}`);
  }
  
  const weeklyCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 1, duration: 30 });
  assertCompositeLimits(weeklyCompositeResponse, [15, 13, 15], "weekly recurring composite schedule");
  
  await expectResponseStatus("ClearChargingProfile", { id: 10 }, "Accepted", "clear weekly recurring charging profile");

  const longDailyStart = new Date(Date.now() - (24 * 60 * 60 - 10) * 1000).toISOString();
  const longDailyProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 12,
      stackLevel: 3,
      chargingProfilePurpose: "TxDefaultProfile",
      chargingProfileKind: "Recurring",
      recurrencyKind: "Daily",
      chargingSchedule: {
        startSchedule: longDailyStart,
        chargingRateUnit: "A",
        chargingSchedulePeriod: [
          { startPeriod: 0, limit: 17 },
          { startPeriod: 86395, limit: 12 }
        ]
      }
    }
  });
  if (longDailyProfileResponse[2]?.status !== "Accepted") {
    throw new Error(`Long daily recurring TxDefaultProfile was not accepted: ${JSON.stringify(longDailyProfileResponse)}`);
  }

  const longDailyCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", {
    connectorId: 1,
    duration: 2 * 24 * 60 * 60 + 20
  });
  assertCompositeLimits(longDailyCompositeResponse, [17, 12, 17, 12, 17, 12, 17], "long daily recurring composite schedule");

  await expectResponseStatus("ClearChargingProfile", { id: 12 }, "Accepted", "clear long daily recurring charging profile");
  
  const wattProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 0,
    csChargingProfiles: {
      chargingProfileId: 11,
      stackLevel: 2,
      chargingProfilePurpose: "ChargePointMaxProfile",
      chargingProfileKind: "Absolute",
      chargingSchedule: {
        chargingRateUnit: "W",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 7000 }]
      }
    }
  });
  if (wattProfileResponse[2]?.status !== "Accepted") {
    throw new Error(`W ChargePointMaxProfile was not accepted: ${JSON.stringify(wattProfileResponse)}`);
  }
  
  const wattCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", {
    connectorId: 1,
    duration: 60,
    chargingRateUnit: "W"
  });
  assertCompositeLimits(wattCompositeResponse, [7000], "W unit composite schedule");
  
  const ampCompositeWithWProfileResponse = await sendCentralSystemCall("GetCompositeSchedule", {
    connectorId: 1,
    duration: 60,
    chargingRateUnit: "A"
  });
  assertCompositeLimits(ampCompositeWithWProfileResponse, [20], "A unit composite schedule with higher-stack W profile present");
  
  await expectResponseStatus("ClearChargingProfile", { id: 11 }, "Accepted", "clear W charging profile");
}
