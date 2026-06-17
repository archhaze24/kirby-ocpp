export function assertCompositeLimits(response, expectedLimits, label) {
  const periods = response[2]?.chargingSchedule?.chargingSchedulePeriod ?? [];
  const limits = periods.map((period) => period.limit);
  if (response[2]?.status !== "Accepted" || limits.join(",") !== expectedLimits.join(",")) {
    throw new Error(`${label} expected limits ${expectedLimits.join(",")}: ${JSON.stringify(response)}`);
  }
}

export function assertCompositeSchedule(response, expected) {
  const schedule = response[2]?.chargingSchedule ?? {};
  const periods = schedule.chargingSchedulePeriod ?? [];
  const limits = periods.map((period) => period.limit);
  const numberPhases = periods.map((period) => period.numberPhases ?? null);

  if (response[2]?.status !== "Accepted" || limits.join(",") !== expected.limits.join(",")) {
    throw new Error(`${expected.label} expected limits ${expected.limits.join(",")}: ${JSON.stringify(response)}`);
  }

  if ("minChargingRate" in expected && schedule.minChargingRate !== expected.minChargingRate) {
    throw new Error(`${expected.label} expected minChargingRate ${expected.minChargingRate}: ${JSON.stringify(response)}`);
  }

  if (expected.numberPhases && numberPhases.join(",") !== expected.numberPhases.join(",")) {
    throw new Error(`${expected.label} expected numberPhases ${expected.numberPhases.join(",")}: ${JSON.stringify(response)}`);
  }
}
