# OCPP 1.6J conformance matrix

This matrix tracks emulator coverage against the OCPP 1.6J JSON schemas bundled in `src/ocpp/schemas/json`.
The bundled JSON schemas are sourced from the official OCPP 1.6J archive and are treated as the conformance baseline for schema validation.

Legend:

- `schema`: request and response payloads are validated against the official JSON schema.
- `handler`: the emulator has behavior for the action.
- `smoke`: covered by `npm run smoke`.
- `manual`: exposed through the TUI or public `Station` API for interactive testing.
- `partial`: implemented for common testing scenarios, but not yet exhaustive against the prose specification.

`npm run smoke` also covers representative edge/negative CSMS command responses, including unknown connectors, readonly/unknown configuration keys, unknown charging profiles, stale local auth list versions, local auth differential delete/restore/duplicate handling, SendLocalList `NotSupported`/`Failed` behavior, malformed local auth list schema errors, disabled local auth list/cache behavior, parent/expired/concurrent idTagInfo handling, remote-start authorization rejection, remote-start TxProfile handling, unknown reservations/transactions, unknown vendors, reservation Occupied/Unavailable/Faulted statuses, TriggerMessage connector-specific routing/rejection, schema-level CALLERROR responses for malformed/unknown commands, transaction-message retry for StartTransaction/StopTransaction/MeterValues on CALLERROR and timeout, disconnect during pending StartTransaction without creating a phantom transaction, automatic reconnect/reboot after disconnect, active-transaction reconnect continuity, BootNotification Pending retry, MaxEnergyOnInvalidId delayed stop, and Reset with active transaction/reservation cleanup plus ResetRetries reboot retry.
`npm test` covers WebSocket protocol validation, pending-call timeout/disconnect rejection, ping/pong watchdog behavior, local `wss://` TLS success/failure behavior, CA/server-name validation, mTLS client certificate options, station reconnect after unexpected WebSocket close, and max reconnect attempt limiting.

## Charge Point to Central System

| Action | Schema | Handler | Smoke | Manual | Notes |
| --- | --- | --- | --- | --- | --- |
| Authorize | yes | yes | yes | yes | Online + local list/cache/offline behavior, cache disable behavior, LocalAuthListEnabled, LocalPreAuthorize blocking, parentIdTag preservation, expired/blocked/concurrent idTagInfo rejection, immediate invalid-id transaction stop, and MaxEnergyOnInvalidId delayed stop. |
| BootNotification | yes | yes | yes | yes | Accepted handling, Pending retry by response interval, and reset-specific rejected retry coverage. |
| DataTransfer | yes | yes | yes | yes | Vendor-specific payloads are free-form. |
| DiagnosticsStatusNotification | yes | yes | yes | yes | Simulates upload lifecycle with success, UploadFailed, and retry attempts. |
| FirmwareStatusNotification | yes | yes | yes | yes | Simulates download/install lifecycle with success, DownloadFailed, InstallationFailed, and retry attempts. |
| Heartbeat | yes | yes | yes | yes | Updates last heartbeat timestamp. |
| MeterValues | yes | yes | yes | yes | Supports configurable sampled measurands, periodic/clock-aligned sampling, and transaction-message retry settings with CALLERROR/timeout smoke coverage. |
| StartTransaction | yes | yes | yes | yes | Handles reservation consumption, rejected idTagInfo statuses, and transaction-message retry settings with CALLERROR/timeout smoke coverage. |
| StatusNotification | yes | yes | yes | yes | Includes fault metadata, honors MinimumStatusDuration for rapid status changes, and suppresses duplicate status spam. |
| StopTransaction | yes | yes | yes | yes | Uses configurable StopTxnSampledData/StopTxnAlignedData, honors StopTransactionOnEVSideDisconnect, uses transaction-message retry settings with CALLERROR/timeout smoke coverage, and restores active transaction state after final delivery failure. |

## Central System to Charge Point

| Action | Schema | Handler | Smoke | Notes |
| --- | --- | --- | --- | --- |
| CancelReservation | yes | yes | yes | Accepts known reservation, rejects unknown. |
| ChangeAvailability | yes | yes | yes | Supports connector `0`, mixed immediate/scheduled changes, and applies scheduled `Inoperative` after transaction end. |
| ChangeConfiguration | yes | yes | yes | Core keys seeded with type validation for common boolean/integer/list values. |
| ClearCache | yes | yes | yes | Clears authorization cache. |
| ClearChargingProfile | yes | partial | yes | Supports id, connectorId, purpose, and stackLevel filters, including unknown-profile response coverage. |
| DataTransfer | yes | yes | yes | Accepts emulator vendor, rejects unknown vendor. |
| GetCompositeSchedule | yes | partial | yes | Applies purpose/stack priority, same-stack latest-profile tie behavior, ChargePointMaxProfile limiting, TxProfile lifecycle, expired/future validity windows including activation inside requested duration, multi-period schedules, daily/weekly recurring schedules, unit-specific profile selection without A/W conversion, `numberPhases`, `minChargingRate`, and relative TxProfile timing for common cases. |
| GetConfiguration | yes | yes | yes | Returns known/unknown keys. |
| GetDiagnostics | yes | partial | yes | Simulates upload lifecycle, retries, and failure outcome; no real file upload. |
| GetLocalListVersion | yes | yes | yes | Persistent local list version. |
| RemoteStartTransaction | yes | yes | yes | Supports connector selection, optional authorization, rejected Authorize response, and remote-start TxProfile application. |
| RemoteStopTransaction | yes | yes | yes | Stops known transaction. |
| ReserveNow | yes | yes | yes | Handles expiry, occupied/unavailable/faulted statuses with smoke coverage. |
| Reset | yes | partial | yes | Stops active transactions with SoftReset/HardReset reason, clears reservations, cancels maintenance timers, reboots with BootNotification, and honors ResetRetries for rejected reboot registration. |
| SendLocalList | yes | yes | yes | Full/differential list persistence, version mismatch, differential delete, duplicate idTag overwrite, parent/expiry idTagInfo behavior, malformed-list CALLERROR, `NotSupported` when LocalAuthListEnabled=false, and `Failed` over SendLocalListMaxLength. |
| SetChargingProfile | yes | partial | yes | Stores profiles, replaces duplicate profile ids, rejects invalid connector, inactive TxProfile, and wrong-transaction TxProfile cases, supports common absolute/recurring/relative multi-period schedule fields, unit-specific profiles, `numberPhases`, and `minChargingRate`. |
| TriggerMessage | yes | yes | yes | Supports Boot, Diagnostics, Firmware, Heartbeat, MeterValues, Status; connector-specific MeterValues/StatusNotification and unknown connector rejection are smoke-covered. |
| UnlockConnector | yes | yes | yes | Stops active transaction with `UnlockCommand`. |
| UpdateFirmware | yes | partial | yes | Simulates download/install lifecycle, retries, and failure outcomes; no real firmware download. |

## Next gaps

- More station-level resume tests for offline command behavior during reconnect windows.
- More Smart Charging edge cases around ambiguous vendor-specific tie policies and long recurring schedules.
- Expand edge/negative conformance tests toward full per-command response-status matrices.
- More WebSocket failure-mode tests around handshake errors mixed with reconnect/backoff.
