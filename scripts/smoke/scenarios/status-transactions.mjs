import { assertCompositeLimits } from "../assertions.mjs";
import { sleep } from "../harness.mjs";

export async function runStatusTransactionsScenario(context) {
  const {
    smoke,
    station,
    callPayloads,
    callErrorsRemaining,
    silentTimeoutsRemaining,
    futureDate,
    sendCentralSystemCall,
    expectResponseStatus,
    waitForCallAfter,
    waitForStationState
  } = context;

  // StatusNotification discipline and transaction message retry behavior.
  const addConnectorStartIndex = callPayloads.length;
  const minimumStatusConnectorId = station.addConnector();
  await waitForCallAfter(
    "StatusNotification",
    addConnectorStartIndex,
    (payload) => payload.connectorId === minimumStatusConnectorId && payload.status === "Available"
  );
  const minimumStatusResponse = await sendCentralSystemCall("ChangeConfiguration", {
    key: "MinimumStatusDuration",
    value: "1"
  });
  if (minimumStatusResponse[2]?.status !== "Accepted") {
    throw new Error(`MinimumStatusDuration was not accepted: ${JSON.stringify(minimumStatusResponse)}`);
  }
  
  const minimumStatusStartIndex = callPayloads.length;
  await station.cycleStatus(minimumStatusConnectorId);
  await station.cycleStatus(minimumStatusConnectorId);
  await sleep(150);
  if (
    callPayloads
      .slice(minimumStatusStartIndex)
      .some((entry) => entry.action === "StatusNotification" && entry.payload.connectorId === minimumStatusConnectorId)
  ) {
    throw new Error("MinimumStatusDuration did not delay rapid StatusNotification changes");
  }
  await waitForCallAfter(
    "StatusNotification",
    minimumStatusStartIndex,
    (payload) => payload.connectorId === minimumStatusConnectorId && payload.status === "Charging"
  );
  await sendCentralSystemCall("ChangeConfiguration", { key: "MinimumStatusDuration", value: "0" });
  
  const retryMeterValuesIndex = callPayloads.length;
  callErrorsRemaining.set("MeterValues", 1);
  await station.meterValues(1, 0);
  const retriedMeterValues = callPayloads
    .slice(retryMeterValuesIndex)
    .filter((entry) => entry.action === "MeterValues" && entry.payload.connectorId === 1);
  if (retriedMeterValues.length < 2) {
    throw new Error(`MeterValues was not retried after CALLERROR: ${JSON.stringify(retriedMeterValues)}`);
  }
  const triggeredMeterValues = retriedMeterValues.at(-1);
  const sampledValues = triggeredMeterValues?.payload?.meterValue?.[0]?.sampledValue ?? [];
  const sampledMeasurands = sampledValues.map((sample) => sample.measurand).sort();
  for (const measurand of ["Current.Import", "Energy.Active.Import.Register", "Power.Active.Import", "SoC", "Voltage"]) {
    if (!sampledMeasurands.includes(measurand)) {
      throw new Error(`MeterValues did not include configured measurand ${measurand}: ${JSON.stringify(sampledValues)}`);
    }
  }
  
  const timeoutMeterValuesIndex = callPayloads.length;
  silentTimeoutsRemaining.set("MeterValues", 1);
  await station.meterValues(1, 0);
  const timeoutRetriedMeterValues = callPayloads
    .slice(timeoutMeterValuesIndex)
    .filter((entry) => entry.action === "MeterValues" && entry.payload.connectorId === 1);
  if (timeoutRetriedMeterValues.length < 2) {
    throw new Error(`MeterValues was not retried after timeout: ${JSON.stringify(timeoutRetriedMeterValues)}`);
  }
  
  if (station.authorizationCache.has("CACHEME")) {
    throw new Error("ClearCache did not clear authorization cache");
  }
  
  // Reservations, active transaction lifecycle, TxProfile lifecycle, and reconnect continuity.
  const reservationResponse = await sendCentralSystemCall("ReserveNow", {
    connectorId: 1,
    expiryDate: futureDate,
    idTag: "TAG1",
    reservationId: 99
  });
  if (reservationResponse[2]?.status !== "Accepted") {
    throw new Error(`Reservation was not accepted: ${JSON.stringify(reservationResponse)}`);
  }
  
  await station.startTransaction(1, "WRONGTAG");
  if (station.state.connectors.find((connector) => connector.id === 1)?.transactionId) {
    throw new Error("Reserved connector started transaction for wrong idTag");
  }
  
  const retryStartIndex = callPayloads.length;
  callErrorsRemaining.set("StartTransaction", 1);
  await station.startTransaction(1, "TAG1");
  const retriedStartTransactions = callPayloads
    .slice(retryStartIndex)
    .filter((entry) => entry.action === "StartTransaction" && entry.payload.connectorId === 1 && entry.payload.idTag === "TAG1");
  if (retriedStartTransactions.length < 2) {
    throw new Error(`StartTransaction was not retried after CALLERROR: ${JSON.stringify(retriedStartTransactions)}`);
  }
  const reservedStartConnector = station.state.connectors.find((connector) => connector.id === 1);
  if (reservedStartConnector?.status !== "Charging" || reservedStartConnector.reservationId) {
    throw new Error(`Reserved start did not consume reservation: ${JSON.stringify(reservedStartConnector)}`);
  }
  
  const wrongTransactionTxProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 995,
      transactionId: 999,
      stackLevel: 1,
      chargingProfilePurpose: "TxProfile",
      chargingProfileKind: "Absolute",
      chargingSchedule: {
        chargingRateUnit: "A",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 8 }]
      }
    }
  });
  if (wrongTransactionTxProfileResponse[2]?.status !== "Rejected") {
    throw new Error(`TxProfile for another transaction was accepted: ${JSON.stringify(wrongTransactionTxProfileResponse)}`);
  }
  
  const activeTxProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 5,
      transactionId: 123,
      stackLevel: 1,
      chargingProfilePurpose: "TxProfile",
      chargingProfileKind: "Absolute",
      chargingSchedule: {
        chargingRateUnit: "A",
        chargingSchedulePeriod: [{ startPeriod: 0, limit: 10 }]
      }
    }
  });
  if (activeTxProfileResponse[2]?.status !== "Accepted") {
    throw new Error(`TxProfile for active transaction was not accepted: ${JSON.stringify(activeTxProfileResponse)}`);
  }
  
  const txProfileCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 1, duration: 60 });
  const txProfileLimit = txProfileCompositeResponse[2]?.chargingSchedule?.chargingSchedulePeriod?.[0]?.limit;
  if (txProfileCompositeResponse[2]?.status !== "Accepted" || txProfileLimit !== 10) {
    throw new Error(`TxProfile did not affect composite schedule: ${JSON.stringify(txProfileCompositeResponse)}`);
  }
  
  const activeTransactionStartedAt = station.state.connectors.find((connector) => connector.id === 1)?.transactionStartedAt;
  const relativeElapsedSeconds = activeTransactionStartedAt
    ? Math.floor((Date.now() - Date.parse(activeTransactionStartedAt)) / 1000)
    : 0;
  const relativeTransition = relativeElapsedSeconds + 4;
  const relativeTxProfileResponse = await sendCentralSystemCall("SetChargingProfile", {
    connectorId: 1,
    csChargingProfiles: {
      chargingProfileId: 8,
      transactionId: 123,
      stackLevel: 2,
      chargingProfilePurpose: "TxProfile",
      chargingProfileKind: "Relative",
      chargingSchedule: {
        duration: relativeTransition + 10,
        chargingRateUnit: "A",
        chargingSchedulePeriod: [
          { startPeriod: 0, limit: 9 },
          { startPeriod: relativeTransition, limit: 7 }
        ]
      }
    }
  });
  if (relativeTxProfileResponse[2]?.status !== "Accepted") {
    throw new Error(`Relative TxProfile for active transaction was not accepted: ${JSON.stringify(relativeTxProfileResponse)}`);
  }
  
  const relativeTxProfileCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 1, duration: 8 });
  assertCompositeLimits(relativeTxProfileCompositeResponse, [9, 7], "relative TxProfile composite schedule");
  
  const activeReconnectStartIndex = callPayloads.length;
  smoke.stationSocket.close(1011, "smoke active transaction reconnect");
  await waitForCallAfter("BootNotification", activeReconnectStartIndex);
  await waitForCallAfter(
    "StatusNotification",
    activeReconnectStartIndex,
    (payload) => payload.connectorId === 1 && payload.status === "Charging"
  );
  await waitForStationState(() => station.state.connected && station.state.booted, "active transaction reconnect booted");
  const activeReconnectConnector = station.state.connectors.find((connector) => connector.id === 1);
  if (activeReconnectConnector?.transactionId !== 123 || activeReconnectConnector.status !== "Charging") {
    throw new Error(`Active transaction did not survive reconnect: ${JSON.stringify(activeReconnectConnector)}`);
  }
  smoke.addEdgeCheck();
  
  const periodicStartIndex = callPayloads.length;
  await waitForCallAfter("MeterValues", periodicStartIndex, (payload) => payload.connectorId === 1 && payload.transactionId === 123);
  await waitForCallAfter(
    "MeterValues",
    periodicStartIndex,
    (payload) =>
      payload.connectorId === 1 &&
      payload.transactionId === 123 &&
      payload.meterValue?.[0]?.sampledValue?.some((sample) => sample.context === "Sample.Clock")
  );
  
  const stopCountBeforeSuspendedEv = callPayloads.filter((entry) => entry.action === "StopTransaction").length;
  await station.unplug(1);
  const suspendedConnector = station.state.connectors.find((connector) => connector.id === 1);
  if (suspendedConnector?.status !== "SuspendedEV" || suspendedConnector.transactionId !== 123 || suspendedConnector.evConnected) {
    throw new Error(`EV disconnect did not suspend active transaction: ${JSON.stringify(suspendedConnector)}`);
  }
  const stopCountAfterSuspendedEv = callPayloads.filter((entry) => entry.action === "StopTransaction").length;
  if (stopCountAfterSuspendedEv !== stopCountBeforeSuspendedEv) {
    throw new Error("EV disconnect sent StopTransaction despite StopTransactionOnEVSideDisconnect=false");
  }
  
  await station.plugIn(1);
  const resumedConnector = station.state.connectors.find((connector) => connector.id === 1);
  if (resumedConnector?.status !== "Charging" || resumedConnector.transactionId !== 123 || !resumedConnector.evConnected) {
    throw new Error(`EV reconnect did not resume charging transaction: ${JSON.stringify(resumedConnector)}`);
  }
  
  const retryStopTransactionIndex = callPayloads.length;
  callErrorsRemaining.set("StopTransaction", 1);
  await station.stopTransaction(1, "Local", "TAG1");
  const retriedStopTransactions = callPayloads
    .slice(retryStopTransactionIndex)
    .filter((entry) => entry.action === "StopTransaction" && entry.payload.transactionId === 123);
  if (retriedStopTransactions.length < 2) {
    throw new Error(`StopTransaction was not retried after CALLERROR: ${JSON.stringify(retriedStopTransactions)}`);
  }
  const clearedTxProfileCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", { connectorId: 1, duration: 60 });
  const clearedTxProfileLimit = clearedTxProfileCompositeResponse[2]?.chargingSchedule?.chargingSchedulePeriod?.[0]?.limit;
  if (clearedTxProfileCompositeResponse[2]?.status !== "Accepted" || clearedTxProfileLimit !== 20) {
    throw new Error(`TxProfile was not cleared after StopTransaction: ${JSON.stringify(clearedTxProfileCompositeResponse)}`);
  }
  await station.unplug(1);
  
  await station.plugIn(1);
  const timeoutRetryStartIndex = callPayloads.length;
  silentTimeoutsRemaining.set("StartTransaction", 1);
  await station.startTransaction(1, "TAG1");
  const timeoutRetriedStartTransactions = callPayloads
    .slice(timeoutRetryStartIndex)
    .filter((entry) => entry.action === "StartTransaction" && entry.payload.connectorId === 1 && entry.payload.idTag === "TAG1");
  if (timeoutRetriedStartTransactions.length < 2) {
    throw new Error(`StartTransaction was not retried after timeout: ${JSON.stringify(timeoutRetriedStartTransactions)}`);
  }
  if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Charging") {
    throw new Error("StartTransaction timeout retry did not move connector 1 to Charging");
  }
  const timeoutRetryStopIndex = callPayloads.length;
  silentTimeoutsRemaining.set("StopTransaction", 1);
  await station.stopTransaction(1, "Local", "TAG1");
  const timeoutRetriedStopTransactions = callPayloads
    .slice(timeoutRetryStopIndex)
    .filter((entry) => entry.action === "StopTransaction" && entry.payload.transactionId === 123);
  if (timeoutRetriedStopTransactions.length < 2) {
    throw new Error(`StopTransaction was not retried after timeout: ${JSON.stringify(timeoutRetriedStopTransactions)}`);
  }
  await station.unplug(1);

  const remoteProfileConnectorId = station.addConnector();
  const remoteProfileStartIndex = callPayloads.length;
  await expectResponseStatus(
    "RemoteStartTransaction",
    {
      connectorId: remoteProfileConnectorId,
      idTag: "TAG1",
      chargingProfile: {
        chargingProfileId: 45,
        stackLevel: 5,
        chargingProfilePurpose: "TxProfile",
        chargingProfileKind: "Absolute",
        chargingSchedule: {
          chargingRateUnit: "A",
          chargingSchedulePeriod: [{ startPeriod: 0, limit: 6 }]
        }
      }
    },
    "Accepted",
    "remote start with TxProfile"
  );
  await waitForCallAfter(
    "StartTransaction",
    remoteProfileStartIndex,
    (payload) => payload.connectorId === remoteProfileConnectorId && payload.idTag === "TAG1"
  );
  const remoteProfileCompositeResponse = await sendCentralSystemCall("GetCompositeSchedule", {
    connectorId: remoteProfileConnectorId,
    duration: 60
  });
  assertCompositeLimits(remoteProfileCompositeResponse, [6], "RemoteStartTransaction TxProfile composite schedule");
  await expectResponseStatus("RemoteStopTransaction", { transactionId: 123 }, "Accepted", "remote stop after remote-start TxProfile");
  await station.unplug(remoteProfileConnectorId);

  await expectResponseStatus(
    "RemoteStartTransaction",
    {
      connectorId: remoteProfileConnectorId,
      idTag: "TAG1",
      chargingProfile: {
        chargingProfileId: 46,
        stackLevel: 5,
        chargingProfilePurpose: "TxDefaultProfile",
        chargingProfileKind: "Absolute",
        chargingSchedule: {
          chargingRateUnit: "A",
          chargingSchedulePeriod: [{ startPeriod: 0, limit: 6 }]
        }
      }
    },
    "Rejected",
    "remote start with non-TxProfile chargingProfile"
  );

  await sendCentralSystemCall("ChangeConfiguration", { key: "MaxEnergyOnInvalidId", value: "5" });
  await station.plugIn(1);
  await station.startTransaction(1, "BADTAG");
  if (station.state.connectors.find((connector) => connector.id === 1)?.transactionId !== 123) {
    throw new Error("BADTAG transaction did not start for MaxEnergyOnInvalidId check");
  }
  const maxEnergyStopIndex = callPayloads.length;
  const maxEnergyDecision = await station.authorize("BADTAG");
  if (maxEnergyDecision.accepted) {
    throw new Error(`BADTAG authorization unexpectedly accepted during MaxEnergyOnInvalidId check: ${JSON.stringify(maxEnergyDecision)}`);
  }
  if (station.state.connectors.find((connector) => connector.id === 1)?.transactionId !== 123) {
    throw new Error("MaxEnergyOnInvalidId stopped transaction immediately instead of waiting for energy limit");
  }
  await station.meterValues(1, 3);
  if (
    callPayloads
      .slice(maxEnergyStopIndex)
      .some((entry) => entry.action === "StopTransaction" && entry.payload.reason === "DeAuthorized")
  ) {
    throw new Error("MaxEnergyOnInvalidId stopped transaction before the energy limit");
  }
  await station.meterValues(1, 2);
  await waitForCallAfter(
    "StopTransaction",
    maxEnergyStopIndex,
    (payload) => payload.transactionId === 123 && payload.reason === "DeAuthorized" && payload.idTag === "BADTAG"
  );
  if (station.state.connectors.find((connector) => connector.id === 1)?.transactionId) {
    throw new Error("MaxEnergyOnInvalidId did not clear transaction after energy limit");
  }
  await sendCentralSystemCall("ChangeConfiguration", { key: "MaxEnergyOnInvalidId", value: "0" });
  await station.unplug(1);
  
  await station.plugIn(1);
  if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Preparing") {
    throw new Error("Plug in did not move connector 1 to Preparing");
  }
  
  await station.startTransaction(1, "TAG1");
  if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Charging") {
    throw new Error("StartTransaction did not move connector 1 to Charging");
  }
  
  silentTimeoutsRemaining.set("StopTransaction", 2);
  let stopTimeoutRejected = false;
  try {
    await station.stopTransaction(1, "Local", "TAG1");
  } catch {
    stopTimeoutRejected = true;
  }
  if (!stopTimeoutRejected) {
    throw new Error("StopTransaction did not reject after all timeout retry attempts");
  }
  const restoredAfterStopTimeout = station.state.connectors.find((connector) => connector.id === 1);
  if (restoredAfterStopTimeout?.status !== "Charging" || restoredAfterStopTimeout.transactionId !== 123) {
    throw new Error(`StopTransaction timeout failure did not restore active transaction: ${JSON.stringify(restoredAfterStopTimeout)}`);
  }
  
  await station.stopTransaction(1, "Local", "TAG1");
  if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Finishing") {
    throw new Error("StopTransaction did not move connector 1 to Finishing");
  }
  
  const stopTransactionPayload = [...callPayloads].reverse().find((entry) => entry.action === "StopTransaction")?.payload;
  const stopEndValues =
    stopTransactionPayload?.transactionData
      ?.find((meterValue) => meterValue.sampledValue?.some((sample) => sample.context === "Transaction.End"))
      ?.sampledValue ?? [];
  const stopMeasurands = stopEndValues.map((sample) => sample.measurand).sort();
  if (stopMeasurands.join(",") !== "Energy.Active.Import.Register,Power.Active.Import") {
    throw new Error(`StopTransaction did not include configured sampled data: ${JSON.stringify(stopEndValues)}`);
  }
  
  const stopAlignedValues =
    stopTransactionPayload?.transactionData
      ?.find((meterValue) => meterValue.sampledValue?.some((sample) => sample.context === "Sample.Clock"))
      ?.sampledValue ?? [];
  const stopAlignedMeasurands = stopAlignedValues.map((sample) => sample.measurand).sort();
  if (stopAlignedMeasurands.join(",") !== "SoC,Voltage") {
    throw new Error(`StopTransaction did not include configured aligned data: ${JSON.stringify(stopAlignedValues)}`);
  }
  
  await station.dataTransfer({ vendorId: "Kirby", messageId: "smoke", data: "ping" });
}
