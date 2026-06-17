import { Station } from "../../../dist/station.js";

export async function runAvailabilityResetPersistenceScenario(context) {
  const {
    smoke,
    station,
    stationConfig,
    callPayloads,
    closeOnCallRemaining,
    bootResponses,
    futureDate,
    expectResponseStatus,
    sendCentralSystemCall,
    waitForCallAfter,
    waitForCallCountAfter,
    waitForStationState
  } = context;

  // Availability scheduling, invalid-id stop behavior, faults, reset, disconnect, and persistence.
  await station.unplug(1);
  if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Available") {
    throw new Error("Unplug did not move connector 1 to Available");
  }
  
  const mixedAvailabilityIdleConnectorId = station.addConnector();
  await station.plugIn(1);
  await station.startTransaction(1, "TAG1");
  const scheduledAvailabilityResponse = await sendCentralSystemCall("ChangeAvailability", {
    connectorId: 0,
    type: "Inoperative"
  });
  if (scheduledAvailabilityResponse[2]?.status !== "Scheduled") {
    throw new Error(`ChangeAvailability during transaction was not scheduled: ${JSON.stringify(scheduledAvailabilityResponse)}`);
  }
  const activeScheduledConnector = station.state.connectors.find((connector) => connector.id === 1);
  if (activeScheduledConnector?.status !== "Charging" || activeScheduledConnector.availability !== "Inoperative") {
    throw new Error(`Scheduled ChangeAvailability did not preserve active connector status: ${JSON.stringify(activeScheduledConnector)}`);
  }
  const idleScheduledConnector = station.state.connectors.find((connector) => connector.id === mixedAvailabilityIdleConnectorId);
  if (idleScheduledConnector?.status !== "Unavailable" || idleScheduledConnector.availability !== "Inoperative") {
    throw new Error(`Scheduled ChangeAvailability did not immediately apply to idle connector: ${JSON.stringify(idleScheduledConnector)}`);
  }
  
  await station.stopTransaction(1, "EVDisconnected", "TAG1");
  const unavailableAfterScheduledChange = station.state.connectors.find((connector) => connector.id === 1);
  if (unavailableAfterScheduledChange?.status !== "Unavailable" || unavailableAfterScheduledChange.availability !== "Inoperative") {
    throw new Error(`Scheduled ChangeAvailability did not apply after transaction: ${JSON.stringify(unavailableAfterScheduledChange)}`);
  }
  
  const operativeAfterScheduledResponse = await sendCentralSystemCall("ChangeAvailability", {
    connectorId: 0,
    type: "Operative"
  });
  if (operativeAfterScheduledResponse[2]?.status !== "Accepted") {
    throw new Error(`ChangeAvailability back to Operative failed: ${JSON.stringify(operativeAfterScheduledResponse)}`);
  }
  if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Available") {
    throw new Error("ChangeAvailability back to Operative did not restore Available");
  }
  if (station.state.connectors.find((connector) => connector.id === mixedAvailabilityIdleConnectorId)?.status !== "Available") {
    throw new Error("ChangeAvailability back to Operative did not restore idle connector");
  }
  
  await station.plugIn(1);
  await station.startTransaction(1, "BADTAG");
  if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Charging") {
    throw new Error("BADTAG transaction did not start for invalid-id stop test");
  }
  
  const invalidIdStopIndex = callPayloads.length;
  const invalidDecision = await station.authorize("BADTAG");
  if (invalidDecision.accepted) {
    throw new Error(`BADTAG authorization unexpectedly accepted: ${JSON.stringify(invalidDecision)}`);
  }
  await waitForCallAfter(
    "StopTransaction",
    invalidIdStopIndex,
    (payload) => payload.idTag === "BADTAG" && payload.reason === "DeAuthorized"
  );
  const invalidStoppedConnector = station.state.connectors.find((connector) => connector.id === 1);
  if (invalidStoppedConnector?.transactionId || invalidStoppedConnector?.status !== "Finishing") {
    throw new Error(`StopTransactionOnInvalidId did not stop BADTAG transaction: ${JSON.stringify(invalidStoppedConnector)}`);
  }
  
  await station.unplug(1);
  if (station.state.connectors.find((connector) => connector.id === 1)?.status !== "Available") {
    throw new Error("Unplug after invalid-id stop did not move connector 1 to Available");
  }
  
  await station.setFault(1, "GroundFailure", "smoke fault");
  const faultedConnector = station.state.connectors.find((connector) => connector.id === 1);
  if (faultedConnector?.status !== "Faulted" || faultedConnector.errorCode !== "GroundFailure") {
    throw new Error(`Fault injection failed: ${JSON.stringify(faultedConnector)}`);
  }
  
  await station.clearFault(1);
  const clearedConnector = station.state.connectors.find((connector) => connector.id === 1);
  if (clearedConnector?.status !== "Available" || clearedConnector.errorCode !== "NoError") {
    throw new Error(`Fault clear failed: ${JSON.stringify(clearedConnector)}`);
  }
  
  const resetReservationConnectorId = station.addConnector();
  await expectResponseStatus(
    "ReserveNow",
    { connectorId: resetReservationConnectorId, expiryDate: futureDate, idTag: "TAG1", reservationId: 880 },
    "Accepted",
    "reserve connector before reset"
  );
  await station.plugIn(1);
  await station.startTransaction(1, "TAG1");
  const resetStartIndex = callPayloads.length;
  bootResponses.push(
    { status: "Rejected", currentTime: new Date().toISOString(), interval: 1 },
    { status: "Accepted", currentTime: new Date().toISOString(), interval: 5 }
  );
  await expectResponseStatus("Reset", { type: "Hard" }, "Accepted", "hard reset with active transaction");
  await waitForCallAfter(
    "StopTransaction",
    resetStartIndex,
    (payload) => payload.transactionId === 123 && payload.reason === "HardReset"
  );
  await waitForCallCountAfter("BootNotification", resetStartIndex, 2);
  await waitForStationState(() => station.state.booted && station.state.registrationStatus === "Accepted", "reset reboot accepted");
  const resetTxConnector = station.state.connectors.find((connector) => connector.id === 1);
  if (resetTxConnector?.transactionId || resetTxConnector?.status !== "Available" || resetTxConnector.evConnected) {
    throw new Error(`Hard Reset did not clear active transaction connector: ${JSON.stringify(resetTxConnector)}`);
  }
  const resetReservationConnector = station.state.connectors.find((connector) => connector.id === resetReservationConnectorId);
  if (resetReservationConnector?.reservationId || resetReservationConnector?.status !== "Available") {
    throw new Error(`Hard Reset did not clear reservation connector: ${JSON.stringify(resetReservationConnector)}`);
  }
  
  await station.plugIn(1);
  const reconnectStartIndex = callPayloads.length;
  closeOnCallRemaining.set("StartTransaction", 2);
  let disconnectStartRejected = false;
  try {
    await station.startTransaction(1, "TAG1");
  } catch {
    disconnectStartRejected = true;
  }
  if (!disconnectStartRejected) {
    throw new Error("StartTransaction did not reject when socket closed during pending call");
  }
  await waitForStationState(() => !station.state.connected, "socket closed during StartTransaction");
  const connectorAfterDisconnectStart = station.state.connectors.find((connector) => connector.id === 1);
  if (connectorAfterDisconnectStart?.transactionId) {
    throw new Error(`Disconnected StartTransaction created phantom transaction: ${JSON.stringify(connectorAfterDisconnectStart)}`);
  }
  await waitForCallAfter("BootNotification", reconnectStartIndex);
  await waitForStationState(() => station.state.connected && station.state.booted, "station reconnected after pending StartTransaction disconnect");
  smoke.addEdgeCheck();
  smoke.close();
  
  const reloadedStation = new Station(stationConfig);
  if (reloadedStation.state.localListVersion !== 6) {
    throw new Error(`Persisted local list version mismatch: ${reloadedStation.state.localListVersion}`);
  }
  
  if (!reloadedStation.localAuthorizationList.has("TAG1")) {
    throw new Error("Persisted local authorization list does not contain TAG1");
  }
  
  if (reloadedStation.chargingProfiles.length !== 3) {
    throw new Error(`Persisted charging profile count mismatch: ${reloadedStation.chargingProfiles.length}`);
  }
  
  const offlineDecision = await reloadedStation.authorize("TAG1");
  if (!offlineDecision.accepted || offlineDecision.source !== "LocalList") {
    throw new Error(`Offline local authorization failed: ${JSON.stringify(offlineDecision)}`);
  }
}
