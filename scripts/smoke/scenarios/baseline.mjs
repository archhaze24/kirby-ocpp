export async function runBaselineScenario(context) {
  const {
    smoke,
    station,
    sendCentralSystemCall
  } = context;

  await smoke.waitForInitialBoot();
  
  // Bootstrap and baseline CSMS command coverage.
  const onlineDecision = await station.authorize("CACHEME");
  if (!onlineDecision.accepted || !station.authorizationCache.has("CACHEME")) {
    throw new Error("Authorize did not populate authorization cache");
  }
  
  const futureDate = new Date(Date.now() + 60_000).toISOString();
  const commands = [
    ["GetConfiguration", {}],
    ["ChangeConfiguration", { key: "HeartbeatInterval", value: "6" }],
    ["ChangeConfiguration", { key: "LocalAuthorizeOffline", value: "true" }],
    ["ChangeConfiguration", { key: "StopTransactionOnEVSideDisconnect", value: "false" }],
    ["ChangeConfiguration", { key: "StopTransactionOnInvalidId", value: "true" }],
    ["ChangeConfiguration", { key: "ResetRetries", value: "1" }],
    ["ChangeConfiguration", { key: "TransactionMessageAttempts", value: "2" }],
    ["ChangeConfiguration", { key: "TransactionMessageRetryInterval", value: "1" }],
    ["ChangeConfiguration", { key: "MeterValueSampleInterval", value: "1" }],
    ["ChangeConfiguration", { key: "ClockAlignedDataInterval", value: "1" }],
    ["ChangeConfiguration", { key: "MeterValuesAlignedData", value: "Energy.Active.Import.Register,Power.Active.Import" }],
    [
      "ChangeConfiguration",
      {
        key: "MeterValuesSampledData",
        value: "Energy.Active.Import.Register,Power.Active.Import,Current.Import,Voltage,SoC"
      }
    ],
    ["ChangeConfiguration", { key: "StopTxnSampledData", value: "Energy.Active.Import.Register,Power.Active.Import" }],
    ["ChangeConfiguration", { key: "StopTxnAlignedData", value: "Voltage,SoC" }],
    ["ClearCache", {}],
    ["GetLocalListVersion", {}],
    [
      "SendLocalList",
      {
        listVersion: 1,
        updateType: "Full",
        localAuthorizationList: [{ idTag: "TAG1", idTagInfo: { status: "Accepted" } }]
      }
    ],
    ["ClearChargingProfile", {}],
    [
      "SetChargingProfile",
      {
        connectorId: 1,
        csChargingProfiles: {
          chargingProfileId: 1,
          stackLevel: 1,
          chargingProfilePurpose: "TxDefaultProfile",
          chargingProfileKind: "Absolute",
          chargingSchedule: {
            chargingRateUnit: "A",
            chargingSchedulePeriod: [{ startPeriod: 0, limit: 16 }]
          }
        }
      }
    ],
    ["ReserveNow", { connectorId: 1, expiryDate: futureDate, idTag: "TAG1", reservationId: 77 }],
    ["CancelReservation", { reservationId: 77 }],
    ["ChangeAvailability", { connectorId: 1, type: "Inoperative" }],
    ["ChangeAvailability", { connectorId: 1, type: "Operative" }],
    ["RemoteStartTransaction", { connectorId: 2, idTag: "TAG2" }],
    ["RemoteStopTransaction", { transactionId: 123 }],
    ["TriggerMessage", { requestedMessage: "Heartbeat" }],
    ["UnlockConnector", { connectorId: 1 }],
    ["GetDiagnostics", { location: "https://example.invalid/diag" }],
    ["UpdateFirmware", { location: "https://example.invalid/fw", retrieveDate: futureDate }],
    ["DataTransfer", { vendorId: "Kirby", data: "ping" }],
    ["GetCompositeSchedule", { connectorId: 1, duration: 60 }]
  ];
  
  for (const [action, payload] of commands) {
    const response = await sendCentralSystemCall(action, payload);
    if (response[0] !== 3) {
      throw new Error(`${action} returned CALLERROR ${JSON.stringify(response)}`);
    }
  
    if (action === "GetCompositeSchedule") {
      const responsePayload = response[2];
      const limit = responsePayload?.chargingSchedule?.chargingSchedulePeriod?.[0]?.limit;
      if (responsePayload?.status !== "Accepted" || limit !== 16) {
        throw new Error(`Unexpected composite schedule response ${JSON.stringify(responsePayload)}`);
      }
    }
  }
  
  return { commands, futureDate };
}
