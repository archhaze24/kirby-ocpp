# OCPP 2.0.1 conformance notes

Coverage of `kirby-ocpp --protocol ocpp2.0.1` (the station) and of `kirby-csms` for OCPP 2.0.1 sessions. For OCPP 1.6J see [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md).
- The station is an emulator for testing a CSMS, not a certified charge station; it is not OCA-certified and deliberately simulates hardware (no real metering, firmware or file transfer).
- Where the specification and the 1.6 emulator differ, the 2.0.1 station follows the specification; deviations are listed in [Known deviations](#known-deviations).

Summary: the catalog holds 64 actions (24 only the station sends, 39 only the CSMS sends, `DataTransfer` in both directions). Of these 29 are fully implemented, 5 partly, 1 is simulated and 29 are not implemented. The two tables below list 65 rows because `DataTransfer` appears in both directions; it is counted once, as full.

Legend: **full** = implemented as specified for emulator purposes; **partial** = implemented for common cases, see notes; **simulated** = the messages are real, the underlying action (power flow, connector lock) is not; **not implemented** = the station never sends the message, or answers the CSMS CALL with `NotSupported`.

Validation: the 128 official 2.0.1 schemas (JSON Schema draft-06) are embedded in the build. Outgoing and incoming CALL and CALLRESULT payloads are validated against them. An incoming CALL that fails validation, or that is schema-valid but does not decode into the typed request, gets a CALLERROR (`OccurrenceConstraintViolation`, `TypeConstraintViolation`, `PropertyConstraintViolation` or `FormatViolation`, the 2.0.1 spelling). Behavior is covered by the unit and integration tests in `crates/kirby-ocpp-core` and `crates/kirby-ocpp/tests`, driven by the in-process mock CSMS in `crates/kirby-ocpp-testkit`.

Contents: [Profiles](#profiles) · [Station to CSMS](#station-to-csms) · [CSMS to station](#csms-to-station) · [Device model](#device-model) · [Transactions](#transactions) · [Testing aids](#testing-aids-deliberate-non-conformance) · [Known deviations](#known-deviations) · [Known limits](#known-limits) · [Conformance scenarios](#conformance-scenarios) · [CSMS side](#csms-side-kirby-csms)

## Profiles

Functional blocks of OCPP 2.0.1 Part 2 (the numbers are the use-case letters):

| Block | Status |
| --- | --- |
| A Security | partial: `SecurityEventNotification`, Basic Auth (profile 1) and TLS (profiles 2 and 3) at the transport; no certificate management |
| B Provisioning | full for boot, `GetVariables`, `SetVariables`, `GetBaseReport`, `GetReport` (B08), `Reset` (station level) |
| C Authorization | full for `Authorize`, the cache and the Local Authorization List (C13, C14) |
| D Local Authorization List Management | full: `SendLocalList` (full and differential), `GetLocalListVersion`, `LocalAuthListCtrlr`; the list survives a restart |
| E Transactions | full for `TransactionEvent`, offline queue and `GetTransactionStatus` (see deviations) |
| F Remote Control | `RequestStartTransaction`, `RequestStopTransaction`, `TriggerMessage` (partial), `UnlockConnector` (simulated) |
| G Availability | full for `ChangeAvailability`; connector status through `StatusNotification` |
| H Reservation | full: `ReserveNow` (H01, a specific EVSE or any EVSE), `CancelReservation` (H02), use of a reserved EVSE (H03), `ReservationStatusUpdate` `Expired` and `Removed` (H04); `ReservationCtrlr`; reservations survive a restart |
| I TariffAndCost, J MeterValues | `MeterValues` and the metering inside `TransactionEvent`; no tariff and cost messages |
| K Smart Charging | partial: `SetChargingProfile` (K01), `ClearChargingProfile` (K10), `GetChargingProfiles` (K09), `GetCompositeSchedule` (K08), a `TxProfile` in `RequestStartTransaction` (K05), external limits (K11 to K13, driven by the `set_external_limit` and `clear_external_limit` commands); `SmartChargingCtrlr`; profiles survive a restart. Not implemented: ISO 15118 (K15 to K17), `ChargingRateChanged` events (K01.FR.61, a MAY), no offline-specific behavior beyond the stored profiles (K06, K07), see [Smart charging](#smart-charging) |
| L Firmware Management, M ISO 15118, N Diagnostics, O Display Message, P DataTransfer | `DataTransfer` only; the rest is not implemented |

## Station to CSMS

| Action | Status | Notes |
| --- | --- | --- |
| Authorize | full | Online; offline decision order: `AuthCtrlr.LocalAuthorizeOffline` with the cache, then `OfflineTxForUnknownIdEnabled`. Tokens of type `Central` and `NoAuthorization` need no `Authorize`. |
| BootNotification | full | Sent at power-up and after a reset, not after every reconnect. `Pending` and `Rejected` are repeated after the interval of the answer (or after `max(1 s, HeartbeatInterval)` when that is 0). The reason is `PowerUp`, `RemoteReset`, `ScheduledReset`, `Triggered`, `Unknown` (the `boot` command) or the reason of the last boot that was not accepted. |
| ClearedChargingLimit | full | `chargingLimitSource` `EMS` (and `evseId` above 0) when the simulated external system releases its limit (`clear_external_limit`), see [Smart charging](#smart-charging). |
| DataTransfer | full | Free-form payload. |
| FirmwareStatusNotification | partial | Sent only with status `Idle`, as the answer to `TriggerMessage`. |
| Heartbeat | full | Fixed period (see deviations); the interval of an accepted boot replaces `HeartbeatInterval` when positive (B01.FR.04). |
| LogStatusNotification | partial | Sent only with status `Idle`, as the answer to `TriggerMessage`. |
| MeterValues | full (simulated meter) | Sent outside a transaction on `TriggerMessage` (`AlignedDataCtrlr.Measurands`) and when the meter is advanced by command; never queued offline. Values inside a transaction travel in `TransactionEvent`. |
| NotifyChargingLimit | full | `chargingLimitSource` `EMS` when the simulated external system sets a limit (`set_external_limit`); carries the schedule while `SmartChargingCtrlr.NotifyChargingLimitWithSchedules` is `true`, and `evseId` for an EVSE above 0. |
| NotifyReport | full | Chunked answer to `GetBaseReport` and `GetReport`, one chunk per `ItemsPerMessage`/`BytesPerMessage` limit; a failed chunk abandons the report. |
| ReportChargingProfiles | full | The answer to `GetChargingProfiles`: one message per EVSE and limit source, `tbc` on all but the last; the profiles are the ones that were set, unchanged. A failed message abandons the rest of the report. |
| ReservationStatusUpdate | full | `Expired` at the expiry of a reservation, `Removed` when its EVSE becomes `Faulted` or `Unavailable`; an end while offline is reported after the next accepted boot or reconnect. |
| SecurityEventNotification | full | Sent on command, and by the station itself for two critical events (A04.FR.01): `StartupOfTheDevice` once per station start and `ResetOrReboot` after every reboot caused by a `Reset`, each after the boot was accepted. A plain reconnect raises none. An event that was cut short by a closed link is sent again after the reconnect (A04.FR.02), so the CSMS may see it twice; events are not persisted, so one still undelivered when the process stops is lost, and so is one whose CALL timed out or was answered with an error. |
| StatusNotification | full | One per connector; all connectors after an accepted boot, and after a reconnect without a boot (all of them when the offline period exceeded `OfflineThreshold`, otherwise those that changed). |
| TransactionEvent | partial | Start and stop points, `seqNo`, offline queue and retries, see [Transactions](#transactions). |
| the other 10 | not implemented | `Get15118EVCertificate`, `GetCertificateStatus`, `NotifyCustomerInformation`, `NotifyDisplayMessages`, `NotifyEVChargingNeeds`, `NotifyEVChargingSchedule`, `NotifyEvent`, `NotifyMonitoringReport`, `PublishFirmwareStatusNotification`, `SignCertificate` |

## CSMS to station

Every CALL is answered exactly once. An action without a handler gets CALLERROR `NotSupported`; a name that 2.0.1 does not define gets `NotImplemented`. While the CSMS has rejected the boot, every CALL gets `SecurityError` (B03.FR.08); while the registration is `Pending` the station accepts every action it implements.

| Action | Status | Notes |
| --- | --- | --- |
| CancelReservation | full | `Accepted` removes the reservation and frees its EVSE; an unknown id is `Rejected`. |
| ChangeAvailability | full | Station, EVSE or connector scope; `Scheduled` while a transaction runs on an affected EVSE, applied when it ends; a waiting `Inoperative` at any scope (and its cancellation by `Operative`) is kept in the state file and applied after a restart too; EVSE 0 with a connector id is `Rejected`. |
| ClearCache | full | `Rejected` while `AuthCacheCtrlr.Enabled` is `false`. |
| ClearChargingProfile | full | By `chargingProfileId`, by the criteria `evseId`, `chargingProfilePurpose` and `stackLevel` (all of them must match), or without a selection (every profile). `Unknown` when nothing matches. Constraints of an external system are never cleared (K10.FR.04, FR.08, FR.09). |
| CostUpdated | full | Answered with an empty confirmation. The `totalCost` of an ongoing transaction is kept, as is `totalCost` in a `TransactionEventResponse`; an unknown transaction id changes nothing. |
| DataTransfer | full | Data for the station's own vendor id is accepted and echoed (`messageId` is not interpreted); other vendor ids get `UnknownVendorId`. |
| GetBaseReport | full | `FullInventory`, `ConfigurationInventory` and `SummaryInventory` (the `AvailabilityState` variables only). A second request while a report is being sent is `Rejected`. |
| GetChargingProfiles | full | The criteria `evseId` (0 is the station's own profiles), `chargingProfilePurpose`, `stackLevel`, `chargingProfileId` and `chargingLimitSource` combine with AND. `Accepted` is followed by `ReportChargingProfiles`; `NoProfiles` (reason `NotFound`) when nothing matches, or (reason `Unspecified`) while an earlier report is still being sent. |
| GetCompositeSchedule | full | See [Smart charging](#smart-charging). `Rejected` for an unknown EVSE (`UnknownEVSE`), a rate unit that `SmartChargingCtrlr.RateUnit` does not list (`UnsupportedRateUnit`) or a duration of 0 or less. |
| GetLocalListVersion | full | `0` while no list was sent or while `LocalAuthListCtrlr.Enabled` is `false` (D02.FR.03). |
| GetTransactionStatus | full | With an id: `ongoingIndicator` and `messagesInQueue` for that transaction (an unknown id is not ongoing); without an id only `messagesInQueue`, for the whole queue (E14). |
| GetReport | full | B08: `componentCriteria` (`Active`, `Available`, `Enabled`, `Problem`; one of them must hold) and `componentVariable` both limit the report. A `componentVariable` field that is left out matches everything at its level (B08.FR.20-25): without a variable, every variable of the component; without a variable instance, every instance; without `evse`, the component at every level; an `evse` without `connectorId`, the EVSE and each of its connectors. `Accepted` with the report in `NotifyReport` chunks of `DeviceDataCtrlr.ItemsPerMessage` (GetReport); `EmptyResultSet` when nothing matches; `Rejected` while another report is being sent; more `componentVariable` entries than `ItemsPerMessage` get CALLERROR `OccurrenceConstraintViolation`. |
| GetVariables | full | `Actual` attribute only (`NotSupportedAttributeType` otherwise); `UnknownComponent`, `UnknownVariable`; a write-only variable is `Rejected` with reason `WriteOnly`. More entries than `DeviceDataCtrlr.ItemsPerMessage` (GetVariables) get CALLERROR `OccurrenceConstraintViolation`. |
| RequestStartTransaction | full | Authorizes the token first when `AuthCtrlr.AuthorizeRemoteStart` is `true` (F01.FR.01). A `chargingProfile` must be a valid `TxProfile` without a `transactionId` (K05.FR.02, FR.05: otherwise `Rejected`, reason `InvalidProfile`, `InvalidSchedule` and so on); it is installed for the transaction that the request starts. With `SmartChargingCtrlr.Enabled` `false` the profile is ignored (K05.FR.04). An unknown, unavailable or faulted EVSE is `Rejected`. |
| RequestStopTransaction | full | An unknown transaction id is `Rejected` with reason `UnknownTransaction`. |
| ReserveNow | full | A specific EVSE or any EVSE, with `connectorType` `cType2` when given; a known reservation id is replaced. `Accepted`, or `Occupied`, `Faulted`, `Unavailable` by the state of the targeted EVSEs; `Rejected` while `ReservationCtrlr.Enabled` is `false`, for an unknown EVSE, another connector type or an expiry in the past. The connectors of a held EVSE report `Reserved`; only the reserving `idToken` or its `groupIdToken` starts there, and the first `TransactionEvent` carries the `reservationId`. Reservations are kept in the state file. |
| Reset | partial | `Immediate` and `OnIdle` at station level, see [Reset](#reset). A request with an `evseId` is `Rejected` with reason `UnsupportedParam`. |
| SetChargingProfile | full | See [Smart charging](#smart-charging) for the rules and the reason codes. |
| SendLocalList | full | `Full` and `Differential`. A version of 0 or less, a request over 1000 entries or 262144 bytes, a full update with an entry without `idTokenInfo` or a list over 1000 tokens is `Failed`; a differential update whose version is not above the current one is `VersionMismatch`. A refused update leaves the list unchanged. The list is kept in the state file. |
| SetVariables | full | Type, range and value-list validation; read-only variables are `Rejected` with reason `ReadOnly`. Limit as for `GetVariables`. |
| TriggerMessage | partial | `BootNotification` (`Rejected` once the boot is accepted), `Heartbeat`, `StatusNotification` (needs an EVSE and a connector), `MeterValues`, `TransactionEvent` (`Rejected` without a running transaction), `FirmwareStatusNotification` and `LogStatusNotification` (status `Idle`). The certificate signing triggers and `PublishFirmwareStatusNotification` are `NotImplemented`; an unknown EVSE is `Rejected`. |
| UnlockConnector | simulated | `Unlocked`, `UnknownConnector`, or `OngoingAuthorizedTransaction` while an authorized transaction runs; no hardware, no transaction is stopped. |
| the other 18 | not implemented | `NotSupported`: `CertificateSigned`, `ClearDisplayMessage`, `ClearVariableMonitoring`, `CustomerInformation`, `DeleteCertificate`, `GetDisplayMessages`, `GetInstalledCertificateIds`, `GetLog`, `GetMonitoringReport`, `InstallCertificate`, `PublishFirmware`, `SetDisplayMessage`, `SetMonitoringBase`, `SetMonitoringLevel`, `SetNetworkProfile`, `SetVariableMonitoring`, `UnpublishFirmware`, `UpdateFirmware` |

### Smart charging

The CSMS shapes the power with charging profiles. The limit math is the one of the 1.6 emulator ([OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md)); a 2.x profile is converted to it for evaluation. The raw profile is kept and reported back unchanged, so fields the station does not evaluate survive.

`SetChargingProfile` is `Accepted` or `Rejected` with a `reasonCode`; the first rule that fails decides.

| Rule | `reasonCode` |
| --- | --- |
| The EVSE does not exist (EVSE 0 is the station) | `UnknownEVSE` |
| Purpose `PriorityCharging` or `LocalGeneration` (2.1) | `UnsupportedPurpose` |
| Purpose `ChargingStationExternalConstraints`: only an external system sets it (K01.FR.22) | `InvalidProfile` |
| Kind `Dynamic` (2.1) | `UnsupportedKind` |
| `ChargingStationMaxProfile` on an EVSE above 0, or `Relative` (K01.FR.38); `TxProfile` on EVSE 0 (K01.FR.16) | `InvalidProfile` |
| An OCPP 2.1 field that is not implemented: `limitAtSoC`, `useLocalTime` true, `randomizedDelay` above 0, `evseSleep` true, a period without `limit` | `InvalidSchedule` |
| `operationMode` other than `ChargingOnly` (2.1) | `InvalidOperationMode` |
| `limit_L2` or `limit_L3` (2.1) | `PhaseConflict` |
| `maxOfflineDuration`, `invalidAfterOfflineDuration`, the discharge, setpoint and V2X fields (2.1) | `UnsupportedParam` |
| `dynUpdateInterval` or `dynUpdateTime` without a `Dynamic` profile | `InvalidProfile` |
| Not exactly one `chargingSchedule` (K01.FR.34; no ISO 15118) | `InvalidSchedule` |
| `stackLevel` below 0 or above `SmartChargingCtrlr.ProfileStackLevel` | `InvalidStackLevel` |
| More periods than `SmartChargingCtrlr.PeriodsPerSchedule` | `TooManyElements` |
| The first period does not start at 0, starts do not strictly increase, a limit is below 0, `duration` is 0 or less (K01.FR.31, FR.35) | `InvalidSchedule` |
| `Absolute` or `Recurring` without `startSchedule`, `Relative` with one (K01.FR.40, FR.41) | `InvalidSchedule` |
| `Recurring` without `recurrencyKind` (K01.FR.60) | `InvalidProfile` |
| A `chargingRateUnit` that `SmartChargingCtrlr.RateUnit` does not list (K01.FR.26) | `UnsupportedRateUnit` |
| `phaseToUse` (the station cannot switch phases, K01.FR.48), or `numberPhases` above the station's phases or below 1 (K01.FR.43) | `InvalidSchedule` |
| A `TxProfile` without `transactionId` | `InvalidProfile` |
| A `TxProfile` of a transaction that does not run on the EVSE (K01.FR.09, FR.33) | `TxNotFound` |
| An id of a profile of an external system (K01.FR.05) | `InvalidProfileId` |
| A `TxDefaultProfile` whose stack level another id holds on EVSE 0 or on an EVSE (K01.FR.52, FR.53) | `DuplicateProfile` |
| More than 10 profiles after the replacements (`SmartChargingCtrlr.Entries`); the `TxProfile` of an accepted `RequestStartTransaction` holds its place until its transaction starts or the start fails | `TooManyElements` |
| A profile larger than 64 KiB of JSON | `TooLarge` |

- A profile replaces the one with the same id, on any EVSE (K01.FR.05), and the one with the same EVSE, purpose and stack level and another id. A `validTo` in the past is accepted and the profile is dropped at the next request.
- Profiles survive a restart (K01.FR.27), except `TxProfile`s: a `TxProfile` ends with its transaction, and a restored transaction ends at once. Constraints of an external system are kept. Profiles stored by the station under the other 2.x version (2.0.1 or 2.1) are not restored.
- `SmartChargingCtrlr.Enabled` `false` answers the four CSMS requests with CALLERROR `NotSupported` (K01.FR.29) and makes every profile inert.
- Evaluation: the highest stack level wins within a purpose; a `TxProfile` of the running transaction wins over the `TxDefaultProfile`; a `TxDefaultProfile` of EVSE 0 applies to every EVSE; the result is capped by `ChargingStationMaxProfile` and `ChargingStationExternalConstraints`. `validFrom` is inclusive, `validTo` exclusive (K01.FR.36, FR.37). The simulated meter converts `A` to `W` with the station's voltage and phases; the energy register and the `Power.Active.Import` and `Power.Offered` samples follow the limit.
- `GetCompositeSchedule` starts at the whole second of the request, as the 1.6 composite does. It answers for EVSE 0 (the profiles of the station itself, not a sum over the EVSEs) and for every EVSE as if the TxDefault profiles applied (K08.FR.06). Without a profile it is one period at the nominal power (K08, TC_K_39), in the requested unit, or `W`. Without a requested unit, the unit of the highest-priority profile that applies anywhere in the window is used. Only profiles in the requested unit take part: a profile in the other unit is left out of the schedule, although the meter honors it. A duration above 7 days (604800 s) or a schedule of more than 1000 periods is `Rejected` with `UnsupportedRequest`.
- External limits: `set_external_limit` (an EVSE and a `ChargingScheduleType`, or a whole profile) installs a `ChargingStationExternalConstraints` profile with source `EMS` and a negative id that no other profile uses (an explicit id of a CSMS profile is refused), and sends `NotifyChargingLimit`; `clear_external_limit` removes the profiles of an EVSE and sends `ClearedChargingLimit`. They need `SmartChargingCtrlr.ExternalControlSignalsEnabled` (default `true`); a limit that is not a valid charging profile is refused. The profile is installed even when the notification fails.
- Not implemented: `TransactionEvent` with `ChargingRateChanged` (K01.FR.61, K11.FR.04, K13.FR.03, all MAY or tied to `LimitChangeSignificance`, which is stored without effect), ISO 15118 (K15 to K17), the ACPhaseSwitching variables beyond `ACPhaseSwitchingSupported` (`false`).

### Reset

`Reset` replies first; the work follows on a timer.
- `Immediate`: every running transaction ends with trigger `ResetCommand` and reason `ImmediateReset` (B12.FR.04). The `Ended` events get the call timeout to reach the CSMS; after that they stay queued and go out after the reboot (B12.FR.05).
- `OnIdle`: answered `Scheduled` while a transaction runs (`Accepted` otherwise); the reset continues once the last transaction has ended on its own (B12.FR.03).
- The simulated reboot closes the link and opens it again; the next `BootNotification` carries `RemoteReset`, or `ScheduledReset` when the reset was answered `Scheduled`. Once that boot is accepted the station sends the security event `ResetOrReboot`. A request that arrives while a reset is in progress joins it.

## Device model

Components and variables the station reports and accepts. Only the `Actual` attribute exists. A variable is station scoped, EVSE scoped or connector scoped (each EVSE has one connector); the EVSE and connector variables exist once per EVSE. Values that differ from the default persist in the state file.

| Component | Variables | Notes |
| --- | --- | --- |
| `ChargingStation` | `AvailabilityState`, `Available`, `SupplyPhases` | Availability is live; `SupplyPhases` defaults to `--phases` |
| `EVSE` | `AvailabilityState`, `Available`, `Power`, `SupplyPhases`, `AllowReset` | `Power` is live (W); `AllowReset` is `false` |
| `Connector` | `AvailabilityState`, `Available`, `ConnectorType`, `SupplyPhases` | `ConnectorType` is `cType2` |
| `ClockCtrlr` | `DateTime` (live), `TimeSource` | |
| `OCPPCommCtrlr` | `HeartbeatInterval`, `MessageTimeout` (`Default`), `MessageAttempts` and `MessageAttemptInterval` (`TransactionEvent`), `OfflineThreshold`, `WebSocketPingInterval`, `ResetRetries`, `UnlockOnEVSideDisconnect`, `FileTransferProtocols`, `NetworkConfigurationPriority`, `NetworkProfileConnectionAttempts`, `QueueAllMessages` | `MessageTimeout` is read-only and follows `--call-timeout-ms`; `ResetRetries`, `UnlockOnEVSideDisconnect`, `QueueAllMessages` and the two network variables are stored and reported without further effect |
| `AuthCtrlr` | `Enabled`, `AuthorizeRemoteStart`, `LocalAuthorizeOffline`, `LocalPreAuthorize`, `OfflineTxForUnknownIdEnabled` | |
| `SmartChargingCtrlr` | `Enabled`, `Available`, `Entries` (`ChargingProfiles`, live), `LimitChangeSignificance`, `PeriodsPerSchedule`, `ProfileStackLevel`, `RateUnit`, `ExternalControlSignalsEnabled`, `NotifyChargingLimitWithSchedules`, `ACPhaseSwitchingSupported` | `Available`, `Entries` and `ACPhaseSwitchingSupported` (`false`) are read-only. Defaults: `ProfileStackLevel` 10, `PeriodsPerSchedule` 24, `RateUnit` `A,W`, at most 10 `Entries`. `LimitChangeSignificance` is stored without effect |
| `AuthCacheCtrlr` | `Enabled`, `LifeTime`, `Storage` | `Storage` is read-only |
| `TxCtrlr` | `TxStartPoint`, `TxStopPoint`, `EVConnectionTimeOut`, `StopTxOnEVSideDisconnect`, `StopTxOnInvalidId` | |
| `SampledDataCtrlr` | `Enabled`, `TxStartedMeasurands`, `TxUpdatedMeasurands`, `TxEndedMeasurands`, `TxUpdatedInterval`, `TxEndedInterval` | Measurands: `Energy.Active.Import.Register`, `Power.Active.Import`, `Current.Import`, `Voltage` |
| `AlignedDataCtrlr` | `Measurands`, `Interval`, `TxEndedMeasurands`, `TxEndedInterval` | `Interval` drives clock-aligned values; the `TxEnded` pair adds clock-aligned samples to the `Ended` event |
| `DeviceDataCtrlr` | `ItemsPerMessage` and `BytesPerMessage` for `GetReport`, `GetVariables`, `SetVariables` | Read-only; the limits for `GetVariables` and `SetVariables` are enforced |
| `SecurityCtrlr` | `SecurityProfile`, `OrganizationName`, `CertificateEntries`, `BasicAuthPassword` | `SecurityProfile` is read-only |

- `BasicAuthPassword` is write-only: a read answers `Rejected` with reason `WriteOnly`, and it never appears in a report, a snapshot, a log or the state file (it is kept in the separate secrets file). A new value set by the CSMS (`SetVariables`) is put to use at once when the URL carries no password (A01): after the answer the station closes the link and connects with the new password, keeping its registration (no new `BootNotification`, queued messages kept). A value set locally (terminal UI, scenario step) applies at the next connect. A value below 16 characters is accepted with a warning (see deviations); the longest accepted value is 64 characters.
- `HeartbeatInterval` takes effect at once on a running timer.
- The other variables are read when the next transaction, queue delivery or message needs them (for example `TxUpdatedInterval` arms the periodic timer when a transaction starts).

## Transactions

- Each EVSE has at most one transaction. `TxCtrlr.TxStartPoint` (default `PowerPathClosed`) and `TxStopPoint` (default `EVConnected,Authorized`) decide when it starts and ends; the values are `Authorized`, `EVConnected`, `PowerPathClosed` and `EnergyTransfer`.
- Transaction ids are UUID v4. `seqNo` counts from 0 per transaction and is assigned when the event is generated. Optional fields follow the merge rule of Part 2 section E.1.5: `evse` goes out once after the EV connected, `idToken` once when the transaction is authorized (and once more when authorization ends by a token), `remoteStartId` once.
- When a trigger also lets the energy offer start, the trigger event reports the state before the offer (`EVConnected`) and a separate `Updated` event with `ChargingStateChanged` and `Charging` follows (E03.FR.12).
- Stop events: unplugging the cable ends the transaction with `StopTxOnEVSideDisconnect=true` or when a stop point is lost (`EVCommunicationLost`, reason `EVDisconnected`), otherwise an `Updated` event follows (`EVCommunicationLost`, charging state `SuspendedEV`). A second token ends it only when a stop point ends on authorization (`StopAuthorized`, reason `Local`), otherwise an `Updated` event with `StopAuthorized` follows. `Ended` reports `chargingState` `Idle` (EV gone) or `EVConnected`. Other reasons in use: `Remote`, `DeAuthorized`, `Timeout` (`EVConnectionTimeOut`), `ImmediateReset`, `PowerLoss`, `StoppedByEV`, `Other`.
- Metering: a charging EVSE draws its nominal power, or the limit of the charging profiles in force when that is lower ([Smart charging](#smart-charging)); the energy register per EVSE is the integral of that power over monotonic time and also counts outside transactions.
  - `Transaction.Begin` sample (`TxStartedMeasurands`): taken at start, carried to the CSMS in the `Ended` event (or earlier in an `Updated` event when the transaction starts before the EV connects).
  - `Sample.Periodic`: an `Updated` event with trigger `MeterValuePeriodic` every `TxUpdatedInterval` seconds (`TxUpdatedMeasurands`). A changed interval re-arms the timer of a running transaction.
  - `Sample.Clock`: an `Updated` event with trigger `MeterValueClock` for every running transaction when `AlignedDataCtrlr.Interval` is above 0.
  - `Ended` event (J02.FR.10): a `Transaction.Begin` and a `Transaction.End` sample of `TxEndedMeasurands`, one `Sample.Periodic` sample per `SampledDataCtrlr.TxEndedInterval` and one `Sample.Clock` sample per `AlignedDataCtrlr.TxEndedInterval` (`TxEndedMeasurands` of that component), merged by time. At most 120 intermediate samples of each kind are kept: when the list is full, every second one is dropped and the spacing doubles. An empty `TxEndedMeasurands` sends no sampled values.
  - `SampleMeter` command: takes a periodic sample on request (an `Updated` event with `MeterValuePeriodic`, or `MeterValues` outside a transaction).
  - The energy register of each EVSE survives a restart.
- Queue: events go out in generation order, one at a time; only the head is sent, so a stalled head blocks the events behind it.
  - A failed delivery with the link up (CALLERROR, timeout, invalid answer) counts as an attempt: the head waits attempt number times `MessageAttemptInterval` and is sent again unchanged, up to `MessageAttempts` times, then it is discarded (E13).
  - A closed link is not a failed attempt. Events created or still waiting while the link is down are flagged `offline: true` (E11.FR.07) and delivered, in order, after the boot or reconnect has been accepted.
  - The queue holds 4096 events. When it is full, every second `Updated` event is dropped (E12.FR.04, E12.FR.05); `Started` and `Ended` events, and an `Updated` event that carries a once-only field, are kept. Close to twice that (8192 events, less the two a new transaction adds) nothing can be thinned any more: `RequestStartTransaction` answers `Rejected` and a local start does not start a transaction until the CSMS takes events again; events of running transactions keep being queued.
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
| Reservation | All connectors are `cType2`, so another `connectorType` is `Rejected`. A token that does not match the reservation of an EVSE gets `NotAtThisLocation`. A remote start with a group token is checked against the reservation after `Authorize`, when the request was already answered `Accepted`. `ReservationStatusUpdate` `NoTransaction` (2.1, recommended) is not sent; a used reservation whose cable never arrives ends without a report. `RebootRequired` is not used | H01, H03, H04 |
| Local Authorization List | At most 1000 tokens, 1000 entries and 262144 bytes per `SendLocalList`; a larger update is `Failed`. Entry expiry is honored (`SupportsExpiryDateTime` is `true`). With `LocalPreAuthorize` an entry that is not `Accepted` is sent to the CSMS, or refused locally when `DisablePostAuthorize` is `true` | C14.FR.03, D01 |
| EVSE reset | `Reset` with an `evseId` is `Rejected` (`EVSE.AllowReset` is `false`) | B11.FR.09 |
| `BasicAuthPassword` | Below 16 characters accepted with a warning | A00.FR.205, A00.FR.304 |
| Unreadable frames | A CALL with the wrong number of elements or a non-string action, whose message id is readable, gets `RpcFrameworkError` (station and CSMS); other broken CALLs keep `FormatViolation`; a frame without a readable message id is dropped and logged instead of answered with message id `"-1"`, which the specification allows | OCPP-J 2.0.1 section 4.2.3 |
| Smart charging | A `TxProfile` of the CSMS needs its `transactionId`; a profile with the same EVSE, purpose and stack level as another id replaces it instead of being refused; `numberPhases` above the station's phases is `Rejected` instead of lowered; a composite schedule is not converted between `A` and `W`, and EVSE 0 shows the station's own profiles; `ChargingRateChanged` events are not sent; the OCPP 2.1 profile features are refused, see [Smart charging](#smart-charging) | K01.FR.06, FR.43, FR.45, FR.61, K08.FR.03 |
| `GetReport` | The model has no `Active` or `Problem` variables: `Active` matches every component, `Problem` none. A variable selector without an instance matches only variables without an instance. `BytesPerMessage` is not checked on the request. No monitoring, certificate management, firmware or log operations; `SecurityProfile` is read-only | B08, Advanced Device Management |
| Reconnect back-off | `OCPPCommCtrlr.RetryBackOffWaitMinimum`, `RetryBackOffRandomRange` and `RetryBackOffRepeatTimes` are not in the device model; the back-off follows the `--reconnect-*` flags | B01, B02 |
| `MaxEnergyOnInvalidId` | Not modeled | C15 |
| Authorization cache | The lifetime counts from the last update of an entry | C10.FR.10 |
| `TxEndedInterval` | Intermediate `Ended` samples are capped at 120 per kind; past the cap the spacing doubles | J02.FR.10 |
| Transactions | The trigger event of a cable or an authorization reports the state before the energy offer; a separate `Updated` event carries `Charging`. `seqNo` counts per transaction from 0. A restored transaction ends with `PowerLoss` after the boot | E03.FR.12 |
| Transport | No `permessage-deflate` | Part 4 section 3.3 |

## Known limits

- The emulator's terminal UI browses only station-level device model variables and counts the security events it saw sent instead of listing them.
- `--count` above 1, `replay` and `scenarios from-recording` are not available for 2.0.1; `run` and `suite` are. The 1.6 response rules (`--rule`, `set_rule`, `clear_rules`) and `request_certificate` do not exist for 2.0.1.
- No state is shared with 1.6: a station id used with both protocols keeps two independent file sets (`<id>.station2.json`, `<id>.station2.secrets.json`, own format version 2).
- OCPP 2.1 is covered in [OCPP_2.1_CONFORMANCE.md](OCPP_2.1_CONFORMANCE.md).
- Master pass (C16) has no transaction selection: a token of the `AuthCtrlr.MasterPassGroupId` group always ends all running transactions with `MasterPass`. The group comes from the `Authorize` answer, the Local Authorization List entry or the authorization cache, in that order.
- The hard limit of the `TransactionEvent` queue refuses new transactions only. `Updated` events that carry a once-only field are never thinned, so a long offline period with many local plug and unplug cycles on running transactions can still grow the queue past it.

## Conformance scenarios

The seventeen scenarios in `conformance/v201/` exercise the station through a scripted `kirby-csms`; how to run them is in [Scenarios](scenarios.md#ocpp-201-conformance-family). The table maps them to the test cases of OCPP 2.0.1 Part 6 (edition 4, the `_CS` cases for a charging station). The runs are not a certification: a scenario follows a test case where the station supports it and says in the last column where it does not.

| Scenario | Test cases | Covers | Differences from the test case |
| --- | --- | --- | --- |
| `boot-pending` | `TC_B_02_CS` | all steps | The connector state is checked through `StatusNotification` only (the station sends no `NotifyEvent` for it); the content of the report is not inspected |
| `boot-rejected` | `TC_B_03_CS`, `TC_B_30_CS` | all steps | That the station sends nothing while rejected is not asserted; the `SecurityError` code of the refused command is checked in the Rust test, the script checks that it is a CALLERROR |
| `reset` | `TC_B_20_CS`, `TC_B_21_CS`, `TC_B_22_CS` | all steps | The `Ended` event is checked for trigger, reason and charging state, not for the absence of `idToken`; the connector is `Occupied` after the immediate reset because the cable stays plugged in. For `TC_B_21_CS` the transaction is stopped locally once the CSMS script has seen `Scheduled`, and the station reports the connector through `StatusNotification` only |
| `remote-start-stop` | `TC_F_01_CS`, `TC_F_02_CS`, `TC_F_08_CS`, `TC_F_09_CS` | the remote start (cable first and remote start first), the remote stop and the rejected stop | The `EnergyTransferStarted` and `StopAuthorized` sub-steps of the reusable states are reduced to the `TransactionEvent` of the start (trigger `RemoteStart`) and of the stop (`Ended`, trigger `RemoteStop`, reason `Remote`); the charging state is not asserted |
| `unlock-connector` | `TC_F_06_CS`, `TC_F_07_CS`, `TC_F_10_CS` | all steps | None |
| `trigger-message` | `TC_F_11_CS`, `TC_F_13_CS`, `TC_F_20_CS`, `TC_F_23_CS`, `TC_F_24_CS`, `TC_F_26_CS`, `TC_F_27_CS` | all steps | The connector state is checked through `StatusNotification` only; the `meterValue` of the `TC_F_13_CS` event is not inspected |
| `device-model` | `TC_B_06_CS`, `TC_B_07_CS`, `TC_B_09_CS`, `TC_B_10_CS`, `TC_B_32_CS` to `TC_B_37_CS`, `TC_B_39_CS`, `TC_B_12_CS`, `TC_B_13_CS` | all steps for the variable cases; the base report cases only in part | A base report is checked for the answer and for the first `NotifyReport` of its `requestId`, not for its items |
| `change-availability` | `TC_G_03_CS`, `TC_G_04_CS`, `TC_G_05_CS`, `TC_G_07_CS`, `TC_G_09_CS`, `TC_G_11_CS`, `TC_G_14_CS`, and by title `TC_G_06_CS`, `TC_G_08_CS`, `TC_G_10_CS` | the commands, the answers and the `StatusNotification` of each change | The connector state is checked through `StatusNotification` only. With a transaction, the connector is reported `Unavailable` once the transaction ended; the later cable and parking states of `TC_G_11_CS` and `TC_G_14_CS` are not stepped through. With a cable plugged in the connector is `Occupied`, not `Available`, after the EVSE is operative again |
| `transaction-status` | `TC_E_28_CS`, `TC_E_30_CS`, `TC_E_32_CS`, `TC_E_34_CS`; `TC_E_29_CS`, `TC_E_31_CS`, `TC_E_33_CS` in part | all steps of the first four | The three cases with a queued message build the queue from a delivery the CSMS refuses with a CALLERROR (the event waits for its retry), not from a closed connection: the flags `ongoingIndicator` and `messagesInQueue` are checked, the `offline` flag and the delivery after reconnecting are not |
| `transaction-retry` | `TC_E_50_CS`, `TC_E_51_CS` | the number of attempts | The wait between attempts is not asserted (a scenario sees no timestamps); the station sends the event `MessageAttempts` times and no more, which the script proves by the unused refusal |
| `meter-values` | `TC_J_07_CS`, `TC_J_09_CS`, `TC_J_10_CS` | one sample per configured measurand with the contexts `Transaction.Begin`, `Sample.Periodic` and `Transaction.End` | The interval of the updates is not asserted; `TxEndedInterval` stays 0, so the periodic samples of the `Ended` event are not checked (a match compares arrays at their full length); no sample is checked for `location` |
| `scripted-replies` | none | the [`reply`](scenarios.md#reply) step | Scenario feature, not a test case |
| `local-list` | `TC_D_01_CS` to `TC_D_06_CS`, `TC_D_08_CS`, `TC_D_10_CS`, `TC_C_31_CS` | all steps | The lists use the `SendLocalList` schema: an empty list is sent without `localAuthorizationList`, as the schema requires at least one item. The versions start at 5 on a fresh station (the cases use a configured number). `TC_C_31_CS` lists the token as `Invalid` with `LocalPreAuthorize` on, starts the transaction on the `Accepted` answer of the CSMS and stops it; the energy transfer is not checked |
| `reservation` | `TC_H_01_CS` to `TC_H_04_CS`, `TC_H_06_CS`, `TC_H_08_CS` to `TC_H_10_CS`, `TC_H_12_CS`, `TC_H_14_CS` to `TC_H_19_CS`, `TC_H_21_CS` to `TC_H_24_CS` | all steps | The station has one EVSE: `TC_H_14_CS` and `TC_H_24_CS` run with one reservation that holds it. `TC_H_02_CS` runs the remote start branch only. The use of a reservation is checked on the `reservationId` of the `Started` event with the cable plugged in first, so the connector goes `Occupied`, not `Available`. Connector states are checked on `StatusNotification` only. `TC_H_22_CS` sets `ReservationCtrlr.Enabled` to `false` (`Available` is read-only). Expiry dates are fixed (2099). `ReservationStatusUpdate` `Removed` (`TC_H_21_CS`) is sent before the `StatusNotification`, and the scenario does not fix their order |
| `get-report` | `TC_B_16_CS`, `TC_B_17_CS`, `TC_B_18_CS`, `TC_B_54_CS`, `TC_B_55_CS`, `TC_B_56_CS` | all steps | `TC_B_16_CS` checks the answer, `requestId` and `seqNo`, not the items: the station reports every variable of a component that meets the criterion, the case lists only `Enabled`. `TC_B_18_CS` uses the criteria `Available` and `Problem` with the variable `EVSE.AvailabilityState` |
| `master-pass` | `TC_C_14_CS`, `TC_C_45_CS`, `TC_C_49_CS` | all steps | One EVSE. `TC_C_45_CS` shows that the master pass started no transaction by starting one with another token afterwards. `TC_C_14_CS` has the CSMS answer the confirming `Authorize` without a group, so only the cache can name it. The `Ended` event carries the master pass token |
| `local-list-auth` | `TC_D_07_CS`, `TC_C_29_CS`, `TC_C_30_CS`, `TC_C_40_CS`, `TC_C_43_CS` | all steps | The reboot of `TC_D_07_CS` is a `Reset`; the scenario station keeps no state file, so persistence on disk is not exercised here. `TC_C_43_CS` checks that an `Invalid` token of the group of the running transaction is answered `Invalid` and sends no event. The absence of an event is not asserted directly |

Test cases listed by title (`TC_G_06_CS`, `TC_G_08_CS`, `TC_G_10_CS`) follow the title only; the other cases were compared with their steps and validations.

Not in the family:

- `TC_E_41_CS`, `TC_E_42_CS`: the CSMS does not answer, so each attempt waits for the call timeout (30 s by default); `TC_E_50_CS` and `TC_E_51_CS` cover the same retry rule with CALLERROR answers.
- Authorization cache cases and the other Local Authorization List cases, `GetReport` case `TC_B_19_CS`, reservation cases `TC_H_07_CS` (the script cannot send a relative expiry) and `TC_H_13_CS` (`NonEvseSpecific` is true), and the master pass cases with a user interface (`TC_C_47_CS`, `TC_C_48_CS`): not in the family.
- Smart charging (part K): implemented, no scenario yet.
- Certificates, firmware, monitoring and display messages: the station answers `NotSupported` or does not send the message.

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
