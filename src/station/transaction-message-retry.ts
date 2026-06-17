export const TRANSACTION_MESSAGE_ACTIONS = new Set(["StartTransaction", "StopTransaction", "MeterValues"]);

export function isTransactionMessage(action: string): boolean {
  return TRANSACTION_MESSAGE_ACTIONS.has(action);
}

export function retryDelay(intervalSeconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, Math.max(0, intervalSeconds) * 1000));
}
