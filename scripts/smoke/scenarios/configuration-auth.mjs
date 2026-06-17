export async function runConfigurationAuthScenario(context) {
  const {
    smoke,
    station,
    callPayloads,
    sendCentralSystemCall,
    expectResponseStatus,
    expectCallError
  } = context;

  // Configuration, authorization, and local authorization list edge behavior.
  const fullConfiguration = await sendCentralSystemCall("GetConfiguration", {});
  const configurationKeys = fullConfiguration[2]?.configurationKey ?? [];
  for (const key of ["ClockAlignedDataInterval", "SupportedFeatureProfiles", "StopTransactionOnEVSideDisconnect"]) {
    if (!configurationKeys.some((entry) => entry.key === key)) {
      throw new Error(`GetConfiguration did not include ${key}`);
    }
  }
  
  const partialConfiguration = await sendCentralSystemCall("GetConfiguration", {
    key: ["HeartbeatInterval", "TotallyUnknownConfigurationKey"]
  });
  if (!partialConfiguration[2]?.configurationKey?.some((entry) => entry.key === "HeartbeatInterval")) {
    throw new Error(`GetConfiguration did not return requested known key: ${JSON.stringify(partialConfiguration)}`);
  }
  if (!partialConfiguration[2]?.unknownKey?.includes("TotallyUnknownConfigurationKey")) {
    throw new Error(`GetConfiguration did not report requested unknown key: ${JSON.stringify(partialConfiguration)}`);
  }
  
  await expectResponseStatus(
    "ChangeConfiguration",
    { key: "TotallyUnknownConfigurationKey", value: "1" },
    "NotSupported",
    "unknown ChangeConfiguration key"
  );
  
  const readonlyResponse = await sendCentralSystemCall("ChangeConfiguration", {
    key: "NumberOfConnectors",
    value: "7"
  });
  if (readonlyResponse[2]?.status !== "Rejected") {
    throw new Error(`Readonly ChangeConfiguration was not rejected: ${JSON.stringify(readonlyResponse)}`);
  }
  
  const invalidBooleanResponse = await sendCentralSystemCall("ChangeConfiguration", {
    key: "StopTransactionOnEVSideDisconnect",
    value: "maybe"
  });
  if (invalidBooleanResponse[2]?.status !== "Rejected") {
    throw new Error(`Invalid boolean ChangeConfiguration was not rejected: ${JSON.stringify(invalidBooleanResponse)}`);
  }
  
  const invalidSampledDataResponse = await sendCentralSystemCall("ChangeConfiguration", {
    key: "MeterValuesSampledData",
    value: "Energy.Active.Import.Register,Totally.Not.Real"
  });
  if (invalidSampledDataResponse[2]?.status !== "Rejected") {
    throw new Error(`Invalid sampled data ChangeConfiguration was not rejected: ${JSON.stringify(invalidSampledDataResponse)}`);
  }
  
  await sendCentralSystemCall("ChangeConfiguration", { key: "AuthorizationCacheEnabled", value: "false" });
  await station.authorize("NOCACHE");
  if (station.authorizationCache.has("NOCACHE")) {
    throw new Error("AuthorizationCacheEnabled=false still populated authorization cache");
  }
  await sendCentralSystemCall("ChangeConfiguration", { key: "AuthorizationCacheEnabled", value: "true" });
  
  const expiredOnlineDecision = await station.authorize("EXPIREDAUTH");
  if (expiredOnlineDecision.accepted || expiredOnlineDecision.idTagInfo.status !== "Expired") {
    throw new Error(`Expired online Authorize was not rejected locally after normalization: ${JSON.stringify(expiredOnlineDecision)}`);
  }
  const parentOnlineDecision = await station.authorize("PARENTAUTH");
  if (!parentOnlineDecision.accepted || parentOnlineDecision.idTagInfo.parentIdTag !== "PARENTTAG") {
    throw new Error(`Authorize did not preserve parentIdTag: ${JSON.stringify(parentOnlineDecision)}`);
  }
  const concurrentOnlineDecision = await station.authorize("CONCURRENTAUTH");
  if (concurrentOnlineDecision.accepted || concurrentOnlineDecision.idTagInfo.status !== "ConcurrentTx") {
    throw new Error(`ConcurrentTx online Authorize was not rejected: ${JSON.stringify(concurrentOnlineDecision)}`);
  }
  
  await sendCentralSystemCall("ChangeConfiguration", { key: "LocalPreAuthorize", value: "true" });
  const localPreAuthStartIndex = callPayloads.length;
  await station.startTransaction(1, "UNKNOWNLOCAL");
  if (
    callPayloads
      .slice(localPreAuthStartIndex)
      .some((entry) => entry.action === "StartTransaction" && entry.payload.idTag === "UNKNOWNLOCAL")
  ) {
    throw new Error("LocalPreAuthorize=true allowed unknown idTag to reach StartTransaction");
  }
  if (station.state.connectors.find((connector) => connector.id === 1)?.transactionId) {
    throw new Error("LocalPreAuthorize=true started transaction for unknown idTag");
  }
  await sendCentralSystemCall("ChangeConfiguration", { key: "LocalPreAuthorize", value: "false" });
  await station.unplug(1);
  
  await sendCentralSystemCall("ChangeConfiguration", { key: "AuthorizeRemoteTxRequests", value: "true" });
  await expectResponseStatus(
    "RemoteStartTransaction",
    { connectorId: 1, idTag: "BADTAG" },
    "Rejected",
    "remote start rejected by AuthorizeRemoteTxRequests"
  );
  if (station.state.connectors.find((connector) => connector.id === 1)?.transactionId) {
    throw new Error("Rejected RemoteStartTransaction started a transaction");
  }
  await sendCentralSystemCall("ChangeConfiguration", { key: "AuthorizeRemoteTxRequests", value: "false" });
  
  await expectResponseStatus(
    "SendLocalList",
    {
      listVersion: 2,
      updateType: "Differential",
      localAuthorizationList: [{ idTag: "TAG1" }]
    },
    "Accepted",
    "differential local auth delete"
  );
  if (station.localAuthorizationList.has("TAG1")) {
    throw new Error("Differential SendLocalList without idTagInfo did not delete TAG1");
  }
  const deletedLocalListDecision = station.authorizeLocally("TAG1", false);
  if (deletedLocalListDecision.source === "LocalList") {
    throw new Error(`Deleted local auth entry still authorizes from LocalList: ${JSON.stringify(deletedLocalListDecision)}`);
  }
  await expectResponseStatus(
    "SendLocalList",
    {
      listVersion: 3,
      updateType: "Differential",
      localAuthorizationList: [{ idTag: "TAG1", idTagInfo: { status: "Accepted" } }]
    },
    "Accepted",
    "restore local auth entry after differential delete"
  );
  
  await expectResponseStatus(
    "SendLocalList",
    {
      listVersion: 4,
      updateType: "Differential",
      localAuthorizationList: [{ idTag: "DISABLEDLOCAL", idTagInfo: { status: "Accepted" } }]
    },
    "Accepted",
    "add local auth entry for LocalAuthListEnabled=false check"
  );
  await sendCentralSystemCall("ChangeConfiguration", { key: "LocalAuthListEnabled", value: "false" });
  await expectResponseStatus(
    "SendLocalList",
    {
      listVersion: 5,
      updateType: "Differential",
      localAuthorizationList: [{ idTag: "SHOULDNOTLOAD", idTagInfo: { status: "Accepted" } }]
    },
    "NotSupported",
    "SendLocalList while LocalAuthListEnabled=false"
  );
  if (station.localAuthorizationList.has("SHOULDNOTLOAD")) {
    throw new Error("LocalAuthListEnabled=false still mutated local auth list");
  }
  await sendCentralSystemCall("ChangeConfiguration", { key: "LocalPreAuthorize", value: "true" });
  const disabledLocalListStartIndex = callPayloads.length;
  await station.startTransaction(1, "DISABLEDLOCAL");
  if (
    callPayloads
      .slice(disabledLocalListStartIndex)
      .some((entry) => entry.action === "StartTransaction" && entry.payload.idTag === "DISABLEDLOCAL")
  ) {
    throw new Error("LocalAuthListEnabled=false still allowed local-list idTag to reach StartTransaction");
  }
  await sendCentralSystemCall("ChangeConfiguration", { key: "LocalPreAuthorize", value: "false" });
  await sendCentralSystemCall("ChangeConfiguration", { key: "LocalAuthListEnabled", value: "true" });
  await station.unplug(1);
  await expectResponseStatus(
    "SendLocalList",
    {
      listVersion: 5,
      updateType: "Differential",
      localAuthorizationList: [{ idTag: "DISABLEDLOCAL" }]
    },
    "Accepted",
    "delete LocalAuthListEnabled=false test entry"
  );
  
  await expectCallError(
    "SendLocalList",
    { listVersion: 6, updateType: "Differential", localAuthorizationList: [{}] },
    "OccurenceConstraintViolation",
    "malformed local auth list entry"
  );
  await expectResponseStatus(
    "SendLocalList",
    {
      listVersion: 7,
      updateType: "Differential",
      localAuthorizationList: Array.from({ length: 1001 }, (_, index) => ({
        idTag: `BULK${index}`,
        idTagInfo: { status: "Accepted" }
      }))
    },
    "Failed",
    "SendLocalList over SendLocalListMaxLength"
  );
  
  await expectResponseStatus(
    "SendLocalList",
    {
      listVersion: 6,
      updateType: "Differential",
      localAuthorizationList: [
        {
          idTag: "EXPIREDLOCAL",
          idTagInfo: { status: "Accepted", expiryDate: new Date(Date.now() - 60_000).toISOString() }
        },
        {
          idTag: "PARENTLOCAL",
          idTagInfo: { status: "Accepted", parentIdTag: "ROOTLOCAL" }
        },
        {
          idTag: "DUPLOCAL",
          idTagInfo: { status: "Blocked" }
        },
        {
          idTag: "DUPLOCAL",
          idTagInfo: { status: "Accepted" }
        }
      ]
    },
    "Accepted",
    "local auth parent/expiry/duplicate list"
  );
  const expiredLocalDecision = station.authorizeLocally("EXPIREDLOCAL", false);
  if (expiredLocalDecision.accepted || expiredLocalDecision.idTagInfo.status !== "Expired") {
    throw new Error(`Expired local auth entry was not rejected: ${JSON.stringify(expiredLocalDecision)}`);
  }
  const parentLocalDecision = station.authorizeLocally("PARENTLOCAL", false);
  if (!parentLocalDecision.accepted || parentLocalDecision.idTagInfo.parentIdTag !== "ROOTLOCAL") {
    throw new Error(`Local auth parentIdTag was not preserved: ${JSON.stringify(parentLocalDecision)}`);
  }
  const duplicateLocalDecision = station.authorizeLocally("DUPLOCAL", false);
  if (!duplicateLocalDecision.accepted) {
    throw new Error(`Duplicate local auth entry did not use the latest item: ${JSON.stringify(duplicateLocalDecision)}`);
  }
  
  for (const [idTag, expectedStatus] of [
    ["BLOCKSTART", "Blocked"],
    ["EXPIREDSTART", "Expired"],
    ["CONCURRENTSTART", "ConcurrentTx"]
  ]) {
    await station.plugIn(1);
    await station.startTransaction(1, idTag);
    const connector = station.state.connectors.find((item) => item.id === 1);
    if (connector?.transactionId) {
      throw new Error(`StartTransaction with ${expectedStatus} idTagInfo created a transaction: ${JSON.stringify(connector)}`);
    }
    await station.unplug(1);
    smoke.addEdgeCheck();
  }
}
