export type IdTagStatus = "Accepted" | "Blocked" | "Expired" | "Invalid" | "ConcurrentTx";

export interface IdTagInfo {
  status: IdTagStatus;
  expiryDate?: string;
  parentIdTag?: string;
}

export interface AuthorizationDecision {
  accepted: boolean;
  idTagInfo: IdTagInfo;
  source: "CentralSystem" | "LocalList" | "AuthorizationCache" | "OfflineUnknown" | "None";
}

export function authorizeLocally(
  idTag: string,
  requireOfflinePermission: boolean,
  localAuthorizationList: Map<string, Record<string, unknown> | undefined>,
  authorizationCache: Map<string, Record<string, unknown> | undefined>,
  configuration: {
    localAuthorizeOffline: boolean;
    authorizationCacheEnabled: boolean;
    allowOfflineTxForUnknownId: boolean;
  }
): AuthorizationDecision {
  if (requireOfflinePermission && !configuration.localAuthorizeOffline) {
    return {
      accepted: false,
      idTagInfo: { status: "Invalid" },
      source: "None"
    };
  }

  const localListInfo = localAuthorizationList.get(idTag);
  if (localAuthorizationList.has(idTag)) {
    const idTagInfo = normalizeIdTagInfo(localListInfo);
    return {
      accepted: isAcceptedIdTagInfo(idTagInfo),
      idTagInfo,
      source: "LocalList"
    };
  }

  if (configuration.authorizationCacheEnabled && authorizationCache.has(idTag)) {
    const idTagInfo = normalizeIdTagInfo(authorizationCache.get(idTag));
    return {
      accepted: isAcceptedIdTagInfo(idTagInfo),
      idTagInfo,
      source: "AuthorizationCache"
    };
  }

  if (requireOfflinePermission && configuration.allowOfflineTxForUnknownId) {
    return {
      accepted: true,
      idTagInfo: { status: "Accepted" },
      source: "OfflineUnknown"
    };
  }

  return {
    accepted: false,
    idTagInfo: { status: "Invalid" },
    source: "None"
  };
}

export function normalizeIdTagInfo(value: Record<string, unknown> | undefined): IdTagInfo {
  const status = readIdTagStatus(value?.status, "Accepted");
  const expiryDate = readOptionalString(value?.expiryDate);
  const parentIdTag = readOptionalString(value?.parentIdTag);
  const idTagInfo: IdTagInfo = {
    status: expiryDate && Date.parse(expiryDate) <= Date.now() ? "Expired" : status
  };

  if (expiryDate) {
    idTagInfo.expiryDate = expiryDate;
  }
  if (parentIdTag) {
    idTagInfo.parentIdTag = parentIdTag;
  }

  return idTagInfo;
}

export function serializeIdTagInfo(idTagInfo: IdTagInfo): Record<string, unknown> {
  const output: Record<string, unknown> = { status: idTagInfo.status };
  if (idTagInfo.expiryDate) {
    output.expiryDate = idTagInfo.expiryDate;
  }
  if (idTagInfo.parentIdTag) {
    output.parentIdTag = idTagInfo.parentIdTag;
  }
  return output;
}

export function isAcceptedIdTagInfo(idTagInfo: IdTagInfo): boolean {
  return idTagInfo.status === "Accepted";
}

function readIdTagStatus(value: unknown, fallback: IdTagStatus): IdTagStatus {
  return value === "Accepted" ||
    value === "Blocked" ||
    value === "Expired" ||
    value === "Invalid" ||
    value === "ConcurrentTx"
    ? value
    : fallback;
}

function readOptionalString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}
