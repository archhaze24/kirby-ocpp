export const TRIGGERABLE_MESSAGES = [
  "BootNotification",
  "DiagnosticsStatusNotification",
  "FirmwareStatusNotification",
  "Heartbeat",
  "MeterValues",
  "StatusNotification"
];

export function canTriggerMessage(requestedMessage: string): boolean {
  return TRIGGERABLE_MESSAGES.includes(requestedMessage);
}
