import { readNumber, readObject, readString } from "./payload.js";

export interface LocalAuthListUpdateResult {
  status: "Accepted" | "Failed" | "NotSupported" | "VersionMismatch";
  listVersion: number;
}

export function applyLocalAuthListUpdate(
  localAuthorizationList: Map<string, Record<string, unknown> | undefined>,
  currentVersion: number,
  payload: Record<string, unknown>,
  options: {
    enabled: boolean;
    maxLength: number;
  }
): LocalAuthListUpdateResult {
  const listVersion = readNumber(payload.listVersion, currentVersion);
  const updateType = readString(payload.updateType, "Full");
  const entries = Array.isArray(payload.localAuthorizationList) ? payload.localAuthorizationList : [];

  if (!options.enabled) {
    return { status: "NotSupported", listVersion: currentVersion };
  }

  if (entries.length > options.maxLength) {
    return { status: "Failed", listVersion: currentVersion };
  }

  if (updateType === "Differential" && listVersion <= currentVersion) {
    return { status: "VersionMismatch", listVersion: currentVersion };
  }

  if (updateType === "Full") {
    localAuthorizationList.clear();
  }

  for (const item of entries) {
    const entry = readObject(item);
    const idTag = readString(entry.idTag, "");
    if (!idTag) {
      continue;
    }

    if (updateType === "Differential" && !("idTagInfo" in entry)) {
      localAuthorizationList.delete(idTag);
      continue;
    }

    localAuthorizationList.set(idTag, "idTagInfo" in entry ? readObject(entry.idTagInfo) : undefined);
  }

  return { status: "Accepted", listVersion };
}
