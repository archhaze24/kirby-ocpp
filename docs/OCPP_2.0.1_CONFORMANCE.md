# OCPP 2.0.1 conformance notes

Coverage of `kirby-ocpp --protocol ocpp2.0.1` (the station) and of `kirby-csms` for OCPP 2.0.1 sessions. For OCPP 1.6J see [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md).
- The station is an emulator for testing a CSMS, not a certified charge station; it is not OCA-certified and deliberately simulates hardware (no real metering, firmware or file transfer).
- Where the specification and the 1.6 emulator differ, the 2.0.1 station follows the specification; deviations are listed in [Known deviations](#known-deviations).

Summary: the catalog holds 64 actions (24 only the station sends, 39 only the CSMS sends, `DataTransfer` in both directions). Of these 15 are fully implemented, 6 partly, 1 is simulated and 42 are not implemented. The two tables below list 65 rows because `DataTransfer` appears in both directions; it is counted once, as full.

Legend: **full** = implemented as specified for emulator purposes; **partial** = implemented for common cases, see notes; **simulated** = the messages are real, the underlying action (power flow, connector lock) is not; **not implemented** = the station never sends the message, or answers the CSMS CALL with `NotSupported`.

Validation: the 128 official 2.0.1 schemas (JSON Schema draft-06) are embedded in the build. Outgoing and incoming CALL and CALLRESULT payloads are validated against them. An incoming CALL that fails validation, or that is schema-valid but does not decode into the typed request, gets a CALLERROR (`OccurrenceConstraintViolation`, `TypeConstraintViolation`, `PropertyConstraintViolation` or `FormatViolation`, the 2.0.1 spelling). Behavior is covered by the unit and integration tests in `crates/kirby-ocpp-core` and `crates/kirby-ocpp/tests`, driven by the in-process mock CSMS in `crates/kirby-ocpp-testkit`.

Contents: [Profiles](#profiles) · [Station to CSMS](#station-to-csms) · [CSMS to station](#csms-to-station) · [Device model](#device-model) · [Transactions](#transactions) · [Testing aids](#testing-aids-deliberate-non-conformance) · [Known deviations](#known-deviations) · [Known limits](#known-limits) · [CSMS side](#csms-side-kirby-csms)

## Profiles

Functional blocks of OCPP 2.0.1 Part 2 (the numbers are the use-case letters):

| Block | Status |
| --- | --- |
| A Security | partial: `SecurityEventNotification`, Basic Auth (profile 1) and TLS (profiles 2 and 3) at the transport; no certificate management |
| B Provisioning | full for boot, `GetVariables`, `SetVariables`, `GetBaseReport`, `Reset` (station level); `GetReport` is not supported |
| C Authorization | full for `Authorize` and the cache; no Local Authorization List |
| D Local Authorization List Management | not implemented |
| E Transactions | full for `TransactionEvent`, offline queue and `GetTransactionStatus` (see deviations) |
| F Remote Control | `RequestStartTransaction`, `RequestStopTransaction`, `TriggerMessage` (partial), `UnlockConnector` (simulated) |
| G Availability | full for `ChangeAvailability`; connector status through `StatusNotification` |
| H Reservation | not implemented |
| I TariffAndCost, J MeterValues | `MeterValues` and the metering inside `TransactionEvent`; no tariff and cost messages |
| K Smart Charging, L Firmware Management, M ISO 15118, N Diagnostics, O Display Message, P DataTransfer | `DataTransfer` only; the rest is not implemented |

## Station to CSMS

| Action | Status | Notes |
| --- | --- | --- |
| Authorize | full | Online; offline decision order: `AuthCtrlr.LocalAuthorizeOffline` with the cache, then `OfflineTxForUnknownIdEnabled`. Tokens of type `Central` and `NoAuthorization` need no `Authorize`. |
| BootNotification | full | Sent at power-up and after a reset, not after every reconnect. `Pending` and `Rejected` are repeated after the interval of the answer (or after `max(1 s, HeartbeatInterval)` when that is 0). The reason is `PowerUp`, `RemoteReset`, `Triggered` or the reason of the last boot that was not accepted. |
| DataTransfer | full | Free-form payload. |
| FirmwareStatusNotification | partial | Sent only with status `Idle`, as the answer to `TriggerMessage`. |
| Heartbeat | full | Fixed period (see deviations); the interval of an accepted boot replaces `HeartbeatInterval` when positive (B01.FR.04). |
| LogStatusNotification | partial | Sent only with status `Idle`, as the answer to `TriggerMessage`. |
| MeterValues | full (simulated meter) | Sent outside a transaction on `TriggerMessage` (`AlignedDataCtrlr.Measurands`) and when the meter is advanced by command; never queued offline. Values inside a transaction travel in `TransactionEvent`. |
| NotifyReport | full | Chunked answer to `GetBaseReport`, one chunk per `ItemsPerMessage`/`BytesPerMessage` limit; a failed chunk abandons the report. |
| SecurityEventNotification | full | Sent on command only; the station raises no security events by itself. |
| StatusNotification | full | One per connector; all connectors after an accepted boot, and after a reconnect without a boot (all of them when the offline period exceeded `OfflineThreshold`, otherwise those that changed). |
| TransactionEvent | partial | Start and stop points, `seqNo`, offline queue and retries, see [Transactions](#transactions). |
| the other 14 | not implemented | `ClearedChargingLimit`, `Get15118EVCertificate`, `GetCertificateStatus`, `NotifyChargingLimit`, `NotifyCustomerInformation`, `NotifyDisplayMessages`, `NotifyEVChargingNeeds`, `NotifyEVChargingSchedule`, `NotifyEvent`, `NotifyMonitoringReport`, `PublishFirmwareStatusNotification`, `ReportChargingProfiles`, `ReservationStatusUpdate`, `SignCertificate` |

## CSMS to station

Every CALL is answered exactly once. An action without a handler gets CALLERROR `NotSupported`; a name that 2.0.1 does not define gets `NotImplemented`. While the CSMS has rejected the boot, every CALL gets `SecurityError` (B03.FR.08); while the registration is `Pending` the station accepts every action it implements.

| Action | Status | Notes |
| --- | --- | --- |
| ChangeAvailability | full | Station, EVSE or connector scope; `Scheduled` while a transaction runs on an affected EVSE, applied when it ends; EVSE 0 with a connector id is `Rejected`. |
| ClearCache | full | `Rejected` while `AuthCacheCtrlr.Enabled` is `false`. |
| DataTransfer | full | Data for the station's own vendor id is accepted and echoed (`messageId` is not interpreted); other vendor ids get `UnknownVendorId`. |
| GetBaseReport | full | `FullInventory`, `ConfigurationInventory` and `SummaryInventory` (the `AvailabilityState` variables only). A second request while a report is being sent is `Rejected`. |
| GetTransactionStatus | full | With an id: `ongoingIndicator` and `messagesInQueue` for that transaction (an unknown id is not ongoing); without an id only `messagesInQueue`, for the whole queue (E14). |
| GetReport | not implemented | Answered with status `NotSupported` and reason code `UnsupportedRequest`. |
| GetVariables | full | `Actual` attribute only (`NotSupportedAttributeType` otherwise); `UnknownComponent`, `UnknownVariable`; a write-only variable is `Rejected` with reason `WriteOnly`. More entries than `DeviceDataCtrlr.ItemsPerMessage` (GetVariables) get CALLERROR `OccurrenceConstraintViolation`. |
| RequestStartTransaction | partial | Authorizes the token first when `AuthCtrlr.AuthorizeRemoteStart` is `true` (F01.FR.01). A `chargingProfile` is ignored with a warning. An unknown, unavailable or faulted EVSE is `Rejected`. |
| RequestStopTransaction | full | An unknown transaction id is `Rejected` with reason `UnknownTransaction`. |
| Reset | partial | `Immediate` and `OnIdle` at station level, see [Reset](#reset). A request with an `evseId` is `Rejected` with reason `UnsupportedParam`. |
| SetVariables | full | Type, range and value-list validation; read-only variables are `Rejected` with reason `ReadOnly`. Limit as for `GetVariables`. |
| TriggerMessage | partial | `BootNotification` (`Rejected` once the boot is accepted), `Heartbeat`, `StatusNotification` (needs an EVSE and a connector), `MeterValues`, `TransactionEvent` (`Rejected` without a running transaction), `FirmwareStatusNotification` and `LogStatusNotification` (status `Idle`). The certificate signing triggers and `PublishFirmwareStatusNotification` are `NotImplemented`; an unknown EVSE is `Rejected`. |
| UnlockConnector | simulated | `Unlocked`, `UnknownConnector`, or `OngoingAuthorizedTransaction` while an authorized transaction runs; no hardware, no transaction is stopped. |
| the other 28 | not implemented | `NotSupported`: `CancelReservation`, `CertificateSigned`, `ClearChargingProfile`, `ClearDisplayMessage`, `ClearVariableMonitoring`, `CostUpdated`, `CustomerInformation`, `DeleteCertificate`, `GetChargingProfiles`, `GetCompositeSchedule`, `GetDisplayMessages`, `GetInstalledCertificateIds`, `GetLocalListVersion`, `GetLog`, `GetMonitoringReport`, `InstallCertificate`, `PublishFirmware`, `ReserveNow`, `SendLocalList`, `SetChargingProfile`, `SetDisplayMessage`, `SetMonitoringBase`, `SetMonitoringLevel`, `SetNetworkProfile`, `SetVariableMonitoring`, `UnpublishFirmware`, `UpdateFirmware` (27) and `GetReport` (answered as above) |

### Reset

`Reset` replies first; the work follows on a timer.
- `Immediate`: every running transaction ends with trigger `ResetCommand` and reason `ImmediateReset` (B12.FR.04). The `Ended` events get the call timeout to reach the CSMS; after that they stay queued and go out after the reboot (B12.FR.05).
- `OnIdle`: answered `Scheduled` while a transaction runs (`Accepted` otherwise); the reset continues once the last transaction has ended on its own (B12.FR.03).
- The simulated reboot closes the link and opens it again; the next `BootNotification` carries `RemoteReset`. A request that arrives while a reset is in progress joins it.

## Device model

Components and variables the station reports and accepts. Only the `Actual` attribute exists. A variable is station scoped, EVSE scoped or connector scoped (each EVSE has one connector); the EVSE and connector variables exist once per EVSE. Values that differ from the default persist in the state file.

| Component | Variables | Notes |
| --- | --- | --- |
| `ChargingStation` | `AvailabilityState`, `Available`, `SupplyPhases` | Availability is live; `SupplyPhases` defaults to `--phases` |
| `EVSE` | `AvailabilityState`, `Available`, `Power`, `SupplyPhases`, `AllowReset` | `Power` is live (W); `AllowReset` is `false` |
| `Connector` | `AvailabilityState`, `Available`, `ConnectorType`, `SupplyPhases` | `ConnectorType` is `cType2` |
| `ClockCtrlr` | `DateTime` (live), `TimeSource` | |
| `OCPPCommCtrlr` | `HeartbeatInterval`, `MessageTimeout` (`Default`), `MessageAttempts` and `MessageAttemptInterval` (`TransactionEvent`), `OfflineThreshold`, `WebSocketPingInterval`, `ResetRetries`, `UnlockOnEVSideDisconnect`, `FileTransferProtocols`, `NetworkConfigurationPriority`, `NetworkProfileConnectionAttempts`, `QueueAllMessages` | `ResetRetries`, `UnlockOnEVSideDisconnect`, `QueueAllMessages` and the two network variables are stored and reported without further effect |
| `AuthCtrlr` | `Enabled`, `AuthorizeRemoteStart`, `LocalAuthorizeOffline`, `LocalPreAuthorize`, `OfflineTxForUnknownIdEnabled` | |
| `AuthCacheCtrlr` | `Enabled`, `LifeTime`, `Storage` | `Storage` is read-only |
| `TxCtrlr` | `TxStartPoint`, `TxStopPoint`, `EVConnectionTimeOut`, `StopTxOnEVSideDisconnect`, `StopTxOnInvalidId` | |
| `SampledDataCtrlr` | `Enabled`, `TxStartedMeasurands`, `TxUpdatedMeasurands`, `TxEndedMeasurands`, `TxUpdatedInterval`, `TxEndedInterval` | Measurands: `Energy.Active.Import.Register`, `Power.Active.Import`, `Current.Import`, `Voltage` |
| `AlignedDataCtrlr` | `Measurands`, `Interval`, `TxEndedMeasurands`, `TxEndedInterval` | `Interval` drives clock-aligned values; the `TxEnded` pair adds clock-aligned samples to the `Ended` event |
| `DeviceDataCtrlr` | `ItemsPerMessage` and `BytesPerMessage` for `GetReport`, `GetVariables`, `SetVariables` | Read-only; the limits for `GetVariables` and `SetVariables` are enforced |
| `SecurityCtrlr` | `SecurityProfile`, `OrganizationName`, `CertificateEntries`, `BasicAuthPassword` | `SecurityProfile` is read-only |

- `BasicAuthPassword` is write-only: a read answers `Rejected` with reason `WriteOnly`, and it never appears in a report, a snapshot, a log or the state file (it is kept in the separate secrets file). A new value applies at the next connect. A value below 16 characters is accepted with a warning (see deviations); the longest accepted value is 64 characters.
- `HeartbeatInterval` takes effect at once on a running timer.
- The other variables are read when the next transaction, queue delivery or message needs them (for example `TxUpdatedInterval` arms the periodic timer when a transaction starts).

## Transactions

- Each EVSE has at most one transaction. `TxCtrlr.TxStartPoint` (default `PowerPathClosed`) and `TxStopPoint` (default `EVConnected,Authorized`) decide when it starts and ends; the values are `Authorized`, `EVConnected`, `PowerPathClosed` and `EnergyTransfer`.
- Transaction ids are UUID v4. `seqNo` counts from 0 per transaction and is assigned when the event is generated. Optional fields follow the merge rule of Part 2 section E.1.5: `evse` goes out once after the EV connected, `idToken` once when the transaction is authorized (and once more when authorization ends by a token), `remoteStartId` once.
- When a trigger also lets the energy offer start, the trigger event reports the state before the offer (`EVConnected`) and a separate `Updated` event with `ChargingStateChanged` and `Charging` follows (E03.FR.12).
- Stop events: unplugging the cable ends the transaction with `StopTxOnEVSideDisconnect=true` or when a stop point is lost (`EVCommunicationLost`, reason `EVDisconnected`), otherwise an `Updated` event follows (`EVCommunicationLost`, charging state `SuspendedEV`). A second token ends it only when a stop point ends on authorization (`StopAuthorized`, reason `Local`), otherwise an `Updated` event with `StopAuthorized` follows. `Ended` reports `chargingState` `Idle` (EV gone) or `EVConnected`. Other reasons in use: `Remote`, `DeAuthorized`, `Timeout` (`EVConnectionTimeOut`), `ImmediateReset`, `PowerLoss`, `StoppedByEV`, `Other`.
- Metering: a charging EVSE draws its nominal power; the energy register per EVSE is the integral of that power over monotonic time and also counts outside transactions.
  - `Transaction.Begin` sample (`TxStartedMeasurands`): taken at start, carried to the CSMS in the `Ended` event (or earlier in an `Updated` event when the transaction starts before the EV connects).
  - `Sample.Periodic`: an `Updated` event with trigger `MeterValuePeriodic` every `TxUpdatedInterval` seconds (`TxUpdatedMeasurands`). A changed interval re-arms the timer of a running transaction.
  - `Sample.Clock`: an `Updated` event with trigger `MeterValueClock` for every running transaction when `AlignedDataCtrlr.Interval` is above 0.
  - `Ended` event (J02.FR.10): a `Transaction.Begin` and a `Transaction.End` sample of `TxEndedMeasurands`, one `Sample.Periodic` sample per `SampledDataCtrlr.TxEndedInterval` and one `Sample.Clock` sample per `AlignedDataCtrlr.TxEndedInterval` (`TxEndedMeasurands` of that component), merged by time. At most 120 intermediate samples of each kind are kept: when the list is full, every second one is dropped and the spacing doubles. An empty `TxEndedMeasurands` sends no sampled values.
  - `SampleMeter` command: takes a periodic sample on request (an `Updated` event with `MeterValuePeriodic`, or `MeterValues` outside a transaction).
  - The energy register of each EVSE survives a restart.
- Queue: events go out in generation order, one at a time; only the head is sent, so a stalled head blocks the events behind it.
  - A failed delivery with the link up (CALLERROR, timeout, invalid answer) counts as an attempt: the head waits attempt number times `MessageAttemptInterval` and is sent again unchanged, up to `MessageAttempts` times, then it is discarded (E13).
  - A closed link is not a failed attempt. Events created or still waiting while the link is down are flagged `offline: true` (E11.FR.07) and delivered, in order, after the boot or reconnect has been accepted.
  - The queue holds 4096 events. When it is full, every second `Updated` event is dropped (E12.FR.04, E12.FR.05); `Started` and `Ended` events, and an `Updated` event that carries a once-only field, are kept.
  - The queue survives a restart.
- A transaction that was running when the process stopped ends with reason `PowerLoss` and trigger `AbnormalCondition` after the next accepted boot; its `seqNo` continues from the stored value.
- Authorization: the `idTokenInfo` of a `TransactionEventResponse` updates the cache. A token that is no longer accepted ends the transaction when `StopTxOnInvalidId` is `true` (default); with `false` the transaction continues with charging state `SuspendedEVSE` (C15.FR.06). The cache holds at most 1000 entries (entries that are not `Accepted` go first, then the oldest, C10.FR.07); an entry expires at the earlier of the CSMS `cacheExpiryDateTime` and `LifeTime` after its last update (C10.FR.10; `LifeTime` 0 adds no limit). With `LocalPreAuthorize` the transaction starts from the cache and the CSMS is asked in parallel.

## Testing aids (deliberate non-conformance)

| Aid | Effect |
| --- | --- |
| `--ws-subprotocol` | Offers a WebSocket subprotocol different from `ocpp2.0.1`; the messages still follow `--protocol` and the station prints a `note:` |
| Station id | An id that is longer than 48 characters, has a colon or characters outside `identifierString` prints a `note:` and is used as given |
| `send_raw` command | Sends any action with any JSON payload; validation can be switched off |
| `set_variable` command | Sets a device-model variable locally, without a CSMS request; read-only variables are refused |
| `--show-secrets` | Shows secrets (`BasicAuthPassword`) in logs and recordings |
| `--tls-skip-verify` and the other TLS flags | As for 1.6, see [Security](security.md) |

## Known deviations

The specification wins over 1.6 parity. Each entry is either backed by a requirement or an explicit simplification.

| Area | Behavior | Reference |
| --- | --- | --- |
| Heartbeat | Sent on a fixed period; it does not restart when other messages are sent | B04 |
| Connector status | Reported by `StatusNotification` only; no `NotifyEvent` for `Connector.AvailabilityState` | G01 allows either |
| Boot | At power-up and after a reset only; a reconnect with an accepted registration re-sends the statuses and the queue instead (unlike the 1.6 emulator) | B01.FR.03, B04 |
| Local Authorization List, reservation | Not implemented; `RebootRequired` is not used | C, H |
| EVSE reset | `Reset` with an `evseId` is `Rejected` (`EVSE.AllowReset` is `false`) | B11.FR.09 |
| `BasicAuthPassword` | Below 16 characters accepted with a warning | A00.FR.205, A00.FR.304 |
| Smart charging | `chargingProfile` in `RequestStartTransaction` is ignored with a warning | |
| `GetReport` | Answers `NotSupported` (no custom reports, no component criteria); no monitoring, certificate management, firmware or log operations; `SecurityProfile` is read-only | Advanced Device Management |
| Reconnect back-off | `OCPPCommCtrlr.RetryBackOffWaitMinimum`, `RetryBackOffRandomRange` and `RetryBackOffRepeatTimes` are not in the device model; the back-off follows the `--reconnect-*` flags | B01, B02 |
| `MaxEnergyOnInvalidId` | Not modeled | C15 |
| Authorization cache | The lifetime counts from the last update of an entry | C10.FR.10 |
| `TxEndedInterval` | Intermediate `Ended` samples are capped at 120 per kind; past the cap the spacing doubles | J02.FR.10 |
| Transactions | The trigger event of a cable or an authorization reports the state before the energy offer; a separate `Updated` event carries `Charging`. `seqNo` counts per transaction from 0. A restored transaction ends with `PowerLoss` after the boot | E03.FR.12 |
| Transport | No `permessage-deflate` | Part 4 section 3.3 |

## Known limits

- The terminal UI is not available for 2.0.1; the station runs headless.
- `--count` above 1, `suite`, `replay`, `scenarios from-recording` of a 2.0.1 recording and `run --scenario` are not available for 2.0.1: scenario files are written for 1.6. The 1.6 response rules (`--rule`, `set_rule`, `clear_rules`) and `request_certificate` do not exist for 2.0.1.
- No state is shared with 1.6: a station id used with both protocols keeps two independent file sets (`<id>.station2.json`, `<id>.station2.secrets.json`, own format version 1).
- OCPP 2.1 is not supported (`--protocol ocpp2.1` exits 2).

## CSMS side (`kirby-csms`)

A 2.0.1 session is handled by the same listener as 1.6 (see [CSMS](csms.md#connecting-stations)).

| Topic | Behavior |
| --- | --- |
| Subprotocol | Chosen by the `--subprotocol` policy (default `require:ocpp1.6,ocpp2.0.1`); a session without a known subprotocol is handled as 1.6 |
| Station id | 1 to 48 characters from `a-z A-Z 0-9 * - _ = + | @ .` (OCPP-J section 3.1.1); another id is refused with HTTP 400, before the credentials are checked |
| Authentication | Basic Auth with the literal key only (no hex decoding); security profile 3 compares the certificate Common Name with the station id |
| Inbound validation | Station CALLs are validated against the 2.0.1 schemas; CALLERRORs use the 2.0.1 spelling. An action the station may not send gets `NotSupported` (defined) or `NotImplemented` (unknown name) |
| Replies | Canned replies for the 25 actions a station may send, valid against the response schemas; `Authorize` and `TransactionEvent` answer by token value (`BADTAG`, `EXPIREDAUTH` and so on), rules and faults can replace them |
| Registration | The boot answer is `Accepted` unless a rule or fault changes it; the registration gate (`SecurityError` for CALLs before an accepted boot) is not applied |
| State | `state.v2` tracks EVSEs and connectors, transactions (keyed by the station-made id), the latest meter values and counters; a repeated `seqNo` is a duplicate; events may arrive late, and an event after `Ended` never reopens a transaction; device-model values from `NotifyReport` are not stored |
| Commands | Any 2.0.1 action can be sent with a raw payload (`call`); typed commands use the schemas of the station's protocol |
| Rules and faults | A rule or fault with a `protocol` applies only to stations that negotiated it |
