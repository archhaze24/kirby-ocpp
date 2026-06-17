# OCPP 1.6J conformance matrix

This matrix tracks emulator coverage against the OCPP 1.6J JSON schemas bundled in `src/ocpp/schemas/json`.

Legend:

- `schema`: request and response payloads are validated against the official JSON schema.
- `handler`: the emulator has behavior for the action.
- `smoke`: covered by `npm run smoke`.
- `manual`: exposed through the TUI or public `Station` API for interactive testing.
- `partial`: implemented for common testing scenarios, but not yet exhaustive against the prose specification.

`npm run smoke` also covers representative negative CSMS command responses, including unknown connectors, readonly/unknown configuration keys, unknown charging profiles, stale local auth list versions, unknown reservations/transactions, unknown vendors, and schema-level CALLERROR responses for malformed commands.

## Charge Point to Central System

| Action | Schema | Handler | Smoke | Manual | Notes |
| --- | --- | --- | --- | --- | --- |
| Authorize | yes | yes | yes | yes | Online + local list/cache/offline behavior. More negative edge cases needed. |
| BootNotification | yes | yes | yes | yes | Accepted/Pending/Rejected handling is basic. |
| DataTransfer | yes | yes | yes | yes | Vendor-specific payloads are free-form. |
| DiagnosticsStatusNotification | yes | yes | yes | yes | Simulates upload lifecycle with success and UploadFailed outcomes. |
| FirmwareStatusNotification | yes | yes | yes | yes | Simulates download/install lifecycle with success, DownloadFailed, and InstallationFailed outcomes. |
| Heartbeat | yes | yes | yes | yes | Updates last heartbeat timestamp. |
| MeterValues | yes | yes | yes | yes | Supports configurable sampled measurands, periodic/clock-aligned sampling, and transaction-message retry settings. |
| StartTransaction | yes | yes | yes | yes | Handles reservation consumption, idTagInfo rejection, and transaction-message retry settings. More auth edge cases pending. |
| StatusNotification | yes | yes | yes | yes | Includes fault metadata, honors MinimumStatusDuration for rapid status changes, and suppresses duplicate status spam. |
| StopTransaction | yes | yes | yes | yes | Uses configurable StopTxnSampledData/StopTxnAlignedData, honors StopTransactionOnEVSideDisconnect, and uses transaction-message retry settings. |

## Central System to Charge Point

| Action | Schema | Handler | Smoke | Notes |
| --- | --- | --- | --- | --- |
| CancelReservation | yes | yes | yes | Accepts known reservation, rejects unknown. |
| ChangeAvailability | yes | yes | yes | Supports connector `0`, mixed immediate/scheduled changes, and applies scheduled `Inoperative` after transaction end. |
| ChangeConfiguration | yes | yes | yes | Core keys seeded with type validation for common boolean/integer/list values. |
| ClearCache | yes | yes | yes | Clears authorization cache. |
| ClearChargingProfile | yes | partial | yes | Filter support exists, including unknown-profile response coverage. |
| DataTransfer | yes | yes | yes | Accepts emulator vendor, rejects unknown vendor. |
| GetCompositeSchedule | yes | partial | yes | Applies purpose/stack priority, ChargePointMaxProfile limiting, TxProfile lifecycle, and expired-profile pruning for common cases. |
| GetConfiguration | yes | yes | yes | Returns known/unknown keys. |
| GetDiagnostics | yes | partial | yes | Simulates upload lifecycle and failure outcome; no real file upload. |
| GetLocalListVersion | yes | yes | yes | Persistent local list version. |
| RemoteStartTransaction | yes | yes | yes | Supports connector selection and optional authorization. |
| RemoteStopTransaction | yes | yes | yes | Stops known transaction. |
| ReserveNow | yes | yes | yes | Handles expiry, occupied/unavailable/faulted statuses. |
| Reset | yes | partial | yes | Stops transactions and reboots; retry behavior pending. |
| SendLocalList | yes | yes | yes | Full/differential list persistence. More malformed-list tests pending. |
| SetChargingProfile | yes | partial | yes | Stores profiles, rejects invalid connector/TxProfile cases, supports common absolute/recurring/relative schedule fields. |
| TriggerMessage | yes | yes | yes | Supports Boot, Diagnostics, Firmware, Heartbeat, MeterValues, Status. |
| UnlockConnector | yes | yes | yes | Stops active transaction with `UnlockCommand`. |
| UpdateFirmware | yes | partial | yes | Simulates download/install lifecycle and failure outcomes; no real firmware download. |

## Next gaps

- More timeout/connection-loss negative tests for transaction message retry, plus reset retry behavior.
- More Smart Charging edge cases around complex multi-period schedules and recurrency boundaries.
- Expand negative conformance tests toward full per-command response-status matrices.
- More WebSocket security tests around mTLS, failed certificate validation, and dropped ping/pong connections.
