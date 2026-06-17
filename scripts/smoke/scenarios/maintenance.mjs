export async function runMaintenanceScenario(context) {
  const {
    station,
    callPayloads,
    bootResponses,
    expectResponseStatus,
    sendCentralSystemCall,
    waitForCallAfter,
    waitForCallCountAfter,
    waitForStationState
  } = context;

  // Firmware and diagnostics lifecycle outcomes, including retry attempts.
  await waitForCallAfter(
    "DiagnosticsStatusNotification",
    0,
    (payload) => payload.status === "Uploading"
  );
  await waitForCallAfter(
    "DiagnosticsStatusNotification",
    0,
    (payload) => payload.status === "Uploaded"
  );
  for (const status of ["Downloading", "Downloaded", "Installing", "Installed"]) {
    await waitForCallAfter(
      "FirmwareStatusNotification",
      0,
      (payload) => payload.status === status
    );
  }
  
  station.setDiagnosticsOutcome("uploadFailure");
  const failedDiagnosticsIndex = callPayloads.length;
  await sendCentralSystemCall("GetDiagnostics", { location: "https://example.invalid/diag-fail", retries: 2, retryInterval: 0 });
  await waitForCallAfter(
    "DiagnosticsStatusNotification",
    failedDiagnosticsIndex,
    (payload) => payload.status === "UploadFailed"
  );
  await waitForCallCountAfter(
    "DiagnosticsStatusNotification",
    failedDiagnosticsIndex,
    3,
    (payload) => payload.status === "Uploading"
  );
  
  station.setFirmwareOutcome("downloadFailure");
  const failedFirmwareIndex = callPayloads.length;
  await sendCentralSystemCall("UpdateFirmware", {
    location: "https://example.invalid/fw-fail",
    retrieveDate: new Date().toISOString(),
    retries: 2,
    retryInterval: 0
  });
  await waitForCallAfter(
    "FirmwareStatusNotification",
    failedFirmwareIndex,
    (payload) => payload.status === "DownloadFailed"
  );
  await waitForCallCountAfter(
    "FirmwareStatusNotification",
    failedFirmwareIndex,
    3,
    (payload) => payload.status === "Downloading"
  );
  
  station.setFirmwareOutcome("installationFailure");
  const failedInstallationIndex = callPayloads.length;
  await sendCentralSystemCall("UpdateFirmware", {
    location: "https://example.invalid/fw-install-fail",
    retrieveDate: new Date().toISOString(),
    retries: 2,
    retryInterval: 0
  });
  await waitForCallAfter(
    "FirmwareStatusNotification",
    failedInstallationIndex,
    (payload) => payload.status === "InstallationFailed"
  );
  await waitForCallCountAfter(
    "FirmwareStatusNotification",
    failedInstallationIndex,
    3,
    (payload) => payload.status === "Installing"
  );
  station.setFirmwareOutcome("success");

  const pendingBootIndex = callPayloads.length;
  bootResponses.push(
    { status: "Pending", currentTime: new Date().toISOString(), interval: 1 },
    { status: "Accepted", currentTime: new Date().toISOString(), interval: 5 }
  );
  await expectResponseStatus("TriggerMessage", { requestedMessage: "BootNotification" }, "Accepted", "trigger boot for Pending retry");
  await waitForCallCountAfter("BootNotification", pendingBootIndex, 2);
  await waitForStationState(() => station.state.booted && station.state.registrationStatus === "Accepted", "Pending boot retry accepted");
}
