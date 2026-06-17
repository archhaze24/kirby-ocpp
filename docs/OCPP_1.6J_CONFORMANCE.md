# OCPP 1.6J conformance matrix

This matrix tracks emulator coverage against the OCPP 1.6J JSON schemas bundled in `src/ocpp/schemas/json`.

Legend:

- `schema`: request and response payloads are validated against the official JSON schema.
- `handler`: the emulator has behavior for the action.
- `smoke`: covered by `npm run smoke`.
- `manual`: exposed through the TUI or public `Station` API for interactive testing.
- `partial`: implemented for common testing scenarios, but not yet exhaustive against the prose specification.

## Charge Point to Central System

| Action | Schema | Handler | Smoke | Manual | Notes |
| --- | --- | --- | --- | --- | --- |
| Authorize | yes | yes | yes | yes | Online + local list/cache/offline behavior. More negative edge cases needed. |
| BootNotification | yes | yes | yes | yes | Accepted/Pending/Rejected handling is basic. |
| DataTransfer | yes | yes | yes | yes | Vendor-specific payloads are free-form. |
| DiagnosticsStatusNotification | yes | yes | yes | API | Simulated diagnostics lifecycle only. |
| FirmwareStatusNotification | yes | yes | yes | API | Simulated firmware lifecycle only. |
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
| ClearChargingProfile | yes | partial | yes | Filter support exists; full smart-charging semantics pending. |
| DataTransfer | yes | yes | yes | Accepts emulator vendor, rejects unknown vendor. |
| GetCompositeSchedule | yes | partial | yes | Basic active-profile calculation. |
| GetConfiguration | yes | yes | yes | Returns known/unknown keys. |
| GetDiagnostics | yes | partial | yes | Simulates upload statuses; no real file upload. |
| GetLocalListVersion | yes | yes | yes | Persistent local list version. |
| RemoteStartTransaction | yes | yes | yes | Supports connector selection and optional authorization. |
| RemoteStopTransaction | yes | yes | yes | Stops known transaction. |
| ReserveNow | yes | yes | yes | Handles expiry, occupied/unavailable/faulted statuses. |
| Reset | yes | partial | yes | Stops transactions and reboots; retry behavior pending. |
| SendLocalList | yes | yes | yes | Full/differential list persistence. More malformed-list tests pending. |
| SetChargingProfile | yes | partial | yes | Stores profiles and supports common schedule fields. |
| TriggerMessage | yes | yes | yes | Supports Boot, Diagnostics, Firmware, Heartbeat, MeterValues, Status. |
| UnlockConnector | yes | yes | yes | Stops active transaction with `UnlockCommand`. |
| UpdateFirmware | yes | partial | yes | Simulates status notifications; no download/install flow. |

## Next gaps

- More timeout/connection-loss negative tests for transaction message retry, plus reset retry behavior.
- More exact Smart Charging stack/recurrency/relative-profile semantics.
- Negative conformance tests for each CSMS command response status.
- WSS/TLS certificate options and stricter WebSocket subprotocol handling.
