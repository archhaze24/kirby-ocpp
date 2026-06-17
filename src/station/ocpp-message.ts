import type { OcppMessage } from "../ocpp/types.js";

export function describeOcppMessage(message: OcppMessage): string {
  if (message[0] === 2) {
    return `CALL ${message[2]} (${message[1]})`;
  }

  if (message[0] === 3) {
    return `CALLRESULT (${message[1]})`;
  }

  return `CALLERROR ${message[2]} (${message[1]})`;
}
