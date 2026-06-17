export async function runNegativeCsmsScenario(context) {
  const {
    smoke,
    station,
    callPayloads,
    futureDate,
    sendCentralSystemCall,
    expectResponseStatus,
    expectCallError,
    waitForCallAfter
  } = context;

  // Negative CSMS command matrix and connector-targeted TriggerMessage coverage.
  await expectResponseStatus("ClearCache", {}, "Accepted", "clear cache accepted");
  await expectResponseStatus(
    "ChangeConfiguration",
    { key: "LightIntensity", value: "10" },
    "Accepted",
    "valid ChangeConfiguration accepted"
  );
  await expectResponseStatus("UnlockConnector", { connectorId: 1 }, "Unlocked", "unlock idle connector");
  await expectResponseStatus("CancelReservation", { reservationId: 404 }, "Rejected", "unknown reservation cancellation");
  await expectResponseStatus("ChangeAvailability", { connectorId: 404, type: "Inoperative" }, "Rejected", "unknown connector availability");
  await expectResponseStatus("DataTransfer", { vendorId: "OtherVendor", data: "ping" }, "UnknownVendorId", "unknown vendor DataTransfer");
  await expectResponseStatus("RemoteStartTransaction", { connectorId: 404, idTag: "TAG1" }, "Rejected", "remote start on unknown connector");
  await expectResponseStatus("RemoteStopTransaction", { transactionId: 404 }, "Rejected", "remote stop of unknown transaction");
  await expectResponseStatus(
    "ReserveNow",
    { connectorId: 1, expiryDate: new Date(Date.now() - 60_000).toISOString(), idTag: "TAG1", reservationId: 401 },
    "Rejected",
    "expired reservation"
  );
  await expectResponseStatus(
    "ReserveNow",
    { connectorId: 404, expiryDate: futureDate, idTag: "TAG1", reservationId: 402 },
    "Unavailable",
    "reservation on unknown connector"
  );
  const reserveNegativeConnectorIndex = callPayloads.length;
  const reserveNegativeConnectorId = station.addConnector();
  await waitForCallAfter(
    "StatusNotification",
    reserveNegativeConnectorIndex,
    (payload) => payload.connectorId === reserveNegativeConnectorId && payload.status === "Available"
  );
  await expectResponseStatus(
    "ReserveNow",
    { connectorId: reserveNegativeConnectorId, expiryDate: futureDate, idTag: "TAG1", reservationId: 403 },
    "Accepted",
    "temporary reservation for occupied check"
  );
  await expectResponseStatus(
    "ReserveNow",
    { connectorId: reserveNegativeConnectorId, expiryDate: futureDate, idTag: "TAG2", reservationId: 404 },
    "Occupied",
    "reservation on occupied connector"
  );
  await expectResponseStatus("CancelReservation", { reservationId: 403 }, "Accepted", "clear temporary occupied reservation");
  await expectResponseStatus(
    "ChangeAvailability",
    { connectorId: reserveNegativeConnectorId, type: "Inoperative" },
    "Accepted",
    "make reservation test connector unavailable"
  );
  await expectResponseStatus(
    "ReserveNow",
    { connectorId: reserveNegativeConnectorId, expiryDate: futureDate, idTag: "TAG1", reservationId: 405 },
    "Unavailable",
    "reservation on unavailable connector"
  );
  await expectResponseStatus(
    "ChangeAvailability",
    { connectorId: reserveNegativeConnectorId, type: "Operative" },
    "Accepted",
    "restore reservation test connector"
  );
  await station.setFault(reserveNegativeConnectorId, "GroundFailure", "reservation fault");
  await expectResponseStatus(
    "ReserveNow",
    { connectorId: reserveNegativeConnectorId, expiryDate: futureDate, idTag: "TAG1", reservationId: 406 },
    "Faulted",
    "reservation on faulted connector"
  );
  await station.clearFault(reserveNegativeConnectorId);
  await expectCallError(
    "TriggerMessage",
    { requestedMessage: "Authorize" },
    "PropertyConstraintViolation",
    "unsupported trigger message"
  );
  const triggerStatusIndex = callPayloads.length;
  await expectResponseStatus(
    "TriggerMessage",
    { requestedMessage: "StatusNotification", connectorId: 2 },
    "Accepted",
    "trigger status notification for connector 2"
  );
  await waitForCallAfter(
    "StatusNotification",
    triggerStatusIndex,
    (payload) => payload.connectorId === 2
  );
  const triggerMeterIndex = callPayloads.length;
  await expectResponseStatus(
    "TriggerMessage",
    { requestedMessage: "MeterValues", connectorId: 2 },
    "Accepted",
    "trigger meter values for connector 2"
  );
  await waitForCallAfter(
    "MeterValues",
    triggerMeterIndex,
    (payload) => payload.connectorId === 2
  );
  await expectResponseStatus(
    "TriggerMessage",
    { requestedMessage: "StatusNotification", connectorId: 404 },
    "Rejected",
    "trigger status notification for unknown connector"
  );
  await expectResponseStatus(
    "TriggerMessage",
    { requestedMessage: "MeterValues", connectorId: 404 },
    "Rejected",
    "trigger meter values for unknown connector"
  );
  await expectResponseStatus("UnlockConnector", { connectorId: 404 }, "NotSupported", "unlock unknown connector");
  const localListVersionResponse = await sendCentralSystemCall("GetLocalListVersion", {});
  if (localListVersionResponse[0] !== 3 || typeof localListVersionResponse[2]?.listVersion !== "number") {
    throw new Error(`GetLocalListVersion did not return a numeric listVersion: ${JSON.stringify(localListVersionResponse)}`);
  }
  smoke.addEdgeCheck();
  await expectResponseStatus(
    "SendLocalList",
    {
      listVersion: 1,
      updateType: "Differential",
      localAuthorizationList: [{ idTag: "TAG1", idTagInfo: { status: "Accepted" } }]
    },
    "VersionMismatch",
    "stale local auth list differential"
  );
  await expectCallError("Reset", { type: "Warm" }, "PropertyConstraintViolation", "invalid Reset enum");
  await expectCallError("GetDiagnostics", {}, "OccurenceConstraintViolation", "missing diagnostics location");
  await expectCallError(
    "UpdateFirmware",
    { location: "https://example.invalid/fw-missing-date" },
    "OccurenceConstraintViolation",
    "missing firmware retrieveDate"
  );
  await expectCallError("DefinitelyNotOcpp16", {}, "NotImplemented", "unknown OCPP action");
  await expectCallError("ChangeAvailability", { connectorId: 1 }, "OccurenceConstraintViolation", "missing ChangeAvailability type");
  await expectCallError("RemoteStartTransaction", { connectorId: 1 }, "OccurenceConstraintViolation", "missing RemoteStartTransaction idTag");
  await expectCallError("ReserveNow", { connectorId: 1, idTag: "TAG1" }, "OccurenceConstraintViolation", "missing ReserveNow fields");
  await expectCallError("SetChargingProfile", { connectorId: 1 }, "OccurenceConstraintViolation", "missing SetChargingProfile profile");
  await expectCallError("TriggerMessage", {}, "OccurenceConstraintViolation", "missing TriggerMessage requestedMessage");
  await expectCallError("UnlockConnector", { connectorId: "1" }, "TypeConstraintViolation", "wrong UnlockConnector connectorId type");
}
