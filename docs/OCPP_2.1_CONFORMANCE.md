# OCPP 2.1 conformance notes

Coverage of `kirby-ocpp --protocol ocpp2.1` (the station) and of `kirby-csms` for OCPP 2.1 sessions. This page lists what differs from OCPP 2.0.1; everything else is as described in [OCPP_2.0.1_CONFORMANCE.md](OCPP_2.0.1_CONFORMANCE.md). For OCPP 1.6J see [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md).

- The station is an emulator for testing a CSMS, not a certified charge station.
- OCPP 2.1 support is the 2.0.1 behavior on the 2.1 wire: subprotocol `ocpp2.1`, the 2.1 schemas, the 2.1 action catalog and the OCPP-J 2.1 message types. None of the new 2.1 functional blocks is implemented (see [Not implemented](#not-implemented)).

Validation: the 181 official 2.1 schemas (JSON Schema draft-06) are embedded in the build. Every frame of a 2.1 connection is validated against them, so a 2.1-only field or enum value is accepted and a 2.0.1-only constraint is not applied. CALLERROR codes use the 2.x spelling.

Contents: [Catalog](#catalog) · [Message types](#message-types) · [Station](#station) · [Tariffs and cost](#tariffs-and-cost) · [Not implemented](#not-implemented) · [Known limits](#known-limits) · [CSMS side](#csms-side-kirby-csms)

## Catalog

The 2.1 catalog holds 91 actions: the 64 actions of 2.0.1 and 27 new ones. `NotifyPeriodicEventStream` is sent only as a SEND, never as a CALL.

| Direction | New actions |
| --- | --- |
| Station to CSMS (12) | `BatterySwap`, `ClosePeriodicEventStream`, `GetCertificateChainStatus`, `NotifyDERAlarm`, `NotifyDERStartStop`, `NotifyPeriodicEventStream` (SEND), `NotifyPriorityCharging`, `NotifySettlement`, `OpenPeriodicEventStream`, `PullDynamicScheduleUpdate`, `ReportDERControl`, `VatNumberValidation` |
| CSMS to station (15) | `AFRRSignal`, `AdjustPeriodicEventStream`, `ChangeTransactionTariff`, `ClearDERControl`, `ClearTariffs`, `GetDERControl`, `GetPeriodicEventStream`, `GetTariffs`, `NotifyAllowedEnergyTransfer`, `NotifyWebPaymentStarted`, `RequestBatterySwap`, `SetDefaultTariff`, `SetDERControl`, `UpdateDynamicSchedule`, `UsePriorityCharging` |

The station sends none of the new station actions. It handles `SetDefaultTariff`, `GetTariffs`, `ClearTariffs` and `ChangeTransactionTariff` (see [Tariffs and cost](#tariffs-and-cost)) and answers each other new CSMS action with CALLERROR `NotSupported`.

## Message types

OCPP-J 2.1 adds message types 5 (CALLRESULTERROR) and 6 (SEND). This section describes the station; for the CSMS see [CSMS side](#csms-side-kirby-csms).

| Topic | Behavior | Reference |
| --- | --- | --- |
| Scope | Types 5 and 6 are accepted on `ocpp2.1` connections only; on `ocpp1.6` and `ocpp2.0.1` they stay unknown message types | Part 4 section 4.1.3 |
| Received SEND | Validated against its payload schema and delivered; an invalid SEND, or one with an unknown action, is dropped with a protocol warning. A SEND is never answered with a CALLRESULT or CALLERROR | 4.2.4 |
| Received CALLRESULTERROR | Surfaced in the log and never answered | 4.2.3 |
| Invalid CALLRESULT | When a CALLRESULT to one of the station's CALLs violates its schema, the CALL fails and a CALLRESULTERROR with the same message id, the 2.x error code of the violation and a description of at most 255 characters is sent | 4.2.3 |
| Sending SEND | A SEND may be sent while a CALL is outstanding and does not wait for the CALL queue | 4.1.1 |
| Message ids | SEND and CALL message ids come from one generator and never repeat | 4.1.4 |
| Other types | Ignored and recorded without a reply | 4.1.3, 4.4 |

## Station

`--protocol ocpp2.1` runs the 2.0.1 station on the `ocpp2.1` subprotocol. The flags, the device model, the state files (`<id>.station2.json`, shared with 2.0.1), `run`, `suite`, `scenarios`, `check` and `state` work as for 2.0.1. Scenario files declare `ocpp_version = "2.1"` and use the 2.x grammar; see [Scenarios](scenarios.md).

| Area | Behavior |
| --- | --- |
| Typed payloads | The typed enums carry the 2.1 values (`Measurand`, `Location` `Upstream`, `MessageFormat` `QRCODE`, `TriggerReason`, `Reason` `ReqEnergyTransferRejected`); `IdToken.type` is a free string; `SignedMeterValue.publicKey` and `signingMethod` are optional |
| Id tokens | Up to 255 characters (36 on 2.0.1) |
| Smart charging | As for 2.0.1 ([Smart charging](OCPP_2.0.1_CONFORMANCE.md#smart-charging)). A profile is kept as received, so the 2.1 fields the station accepts survive (`operationMode` `ChargingOnly`, `powerTolerance`, price schedules, which have no effect). Refused with the code OCPP 2.1 names: the purposes `PriorityCharging` and `LocalGeneration` (`UnsupportedPurpose`), the kind `Dynamic` (`UnsupportedKind`), other operation modes (`InvalidOperationMode`), `limit_L2` and `limit_L3` (`PhaseConflict`), `limitAtSoC`, `useLocalTime`, `randomizedDelay` and `evseSleep` (`InvalidSchedule`), `maxOfflineDuration`, `invalidAfterOfflineDuration` and the discharge, setpoint and V2X fields (`UnsupportedParam`). The limit sources in `ReportChargingProfiles`, `NotifyChargingLimit` and `ClearedChargingLimit` are `CSO` and `EMS`. A `Relative` schedule of a transaction starts when the driver is authorized and the EV is connected (K01.FR.42), not at the start of the transaction as in 2.0.1 |
| Transaction limits | E16, F07: `maxEnergy`, `maxTime` and `maxCost` (`TxCtrlr.SupportedLimits`, a 2.1-only variable); `maxSoC` is not supported and is left out of the confirmation (E16.FR.13). A `transactionLimit` in a `TransactionEventResponse` is confirmed once with an `Updated` event, `triggerReason` `LimitSet`, carrying the supported fields; a response that repeats the current values sends nothing. The station-side limit is the library command `set_transaction_limit`; a value above the one the CSMS set is refused (E16.FR.04). The time runs from the `Started` event. Cost is the CSMS's `totalCost` (from `TransactionEventResponse` or `CostUpdated`); a transaction with local cost uses its local total including taxes instead and ignores the CSMS's cost (E16.FR.16, see [Tariffs and cost](#tariffs-and-cost)). Limits are checked when they change, on a cost update and every second while charging; an energy limit crossed while charging stops the register at the limit (a limit set below the energy already delivered stops charging and leaves the register as it is). A limit lowered while charging is suspended is checked again, so a `TxStopPoint` with `EnergyTransfer` then ends the transaction. A reached limit suspends energy transfer (`SuspendedEVSE`, `EnergyLimitReached`, `TimeLimitReached` or `CostLimitReached`), or ends the transaction when `TxStopPoint` contains `EnergyTransfer` (stopped reason `EnergyLimitReached`, `TimeLimitReached`, or `LocalOutOfCredit` for cost). A higher limit resumes charging in the same `LimitSet` event (E16.FR.14). Limits are kept in the state file with the transaction |
| Resuming transactions | E17: `TxCtrlr.TxResumptionTimeout` (seconds, default 0: off) and `TxCtrlr.TxAllowEnergyTransferResumption` (default `true`), both 2.1-only variables. The state file records when it was last written; a normal exit writes it. When the process starts again within the timeout, each transaction that was running resumes with its EV plugged in and its charging state (`SuspendedEVSE` instead of `Charging` when energy transfer may not resume, a transaction limit holds it or the connector cannot deliver; `Idle` when no EV was connected) and sends `Updated` with `TxResumed` after the boot; `seqNo`, meter, limits and the relative-schedule start continue. Later, or with the timeout at 0, it ends with `AbnormalCondition` and `PowerLoss` as on 2.0.1. `TxProfile`s are not restored (the CSMS sends them again, E17.FR.15). There is no random resumption delay (`EnergyTransferResumptionRandomRange` is not provided) |
| `Reset` | `ImmediateAndResume` (B13): `Rejected` with `statusInfo.reasonCode` `UnsupportedParam` while `TxResumptionTimeout` is 0, and for an EVSE as other reset types. Without transactions it is an `Immediate` reset. With transactions, each gets `Updated` with `ResetCommand` and `SuspendedEVSE` (when it was charging), the station reboots with boot reason `RemoteReset`, and after the accepted boot each transaction resumes with `TxResumed` in its previous state, or `SuspendedEVSE` when `TxAllowEnergyTransferResumption` is `false` |
| `TriggerMessage` | `CustomTrigger` and `SignV2G20Certificate` are `NotImplemented` |
| New CSMS actions | The four tariff actions as in [Tariffs and cost](#tariffs-and-cost); the others CALLERROR `NotSupported` |
| Sent messages | The 2.0.1 set, which stays valid in 2.1 |

## Tariffs and cost

Local cost calculation, I07 to I12. `TariffCostCtrlr` is a 2.1-only component; local tariffs need `Enabled[Tariff]`, local cost needs `Enabled[Cost]` too (both `false` by default). With `Enabled[Tariff]` `false` the four tariff actions get CALLERROR `NotSupported` and the `tariff` of an `AuthorizeResponse` is ignored.

| Topic | Behavior | Reference |
| --- | --- | --- |
| Device model | `Available[Tariff]` and `Available[Cost]` `true`; `Enabled[Tariff]`, `Enabled[Cost]`, `Enabled[RunningCost]` (default `false`); `Interval[Cost]` (default 60 s, at least 1); `Interval[Tariff]` (default 0, stored without effect); `MaxElements[Tariff]` 10; `ConditionsSupported[Tariff]` `false`; `HandleFailedTariff[Tariff]` (`Deauthorize`, the default, `UseDefaultTariff`, `CentralCost`); `TariffFallbackMessage` and `TotalCostFallbackMessage` (stored only); `Problem` | 2.11 of the referenced components |
| Checking a tariff | First match: a tariff over 16 KiB of JSON is `TooManyElements`; a negative price, tax or cost bound, or no price field at all, is `InvalidValue`; more than `MaxElements` prices in one field is `TooManyElements`; any `conditions` is `ConditionNotSupported`. With conditions unsupported, the first price of a field applies | I07.FR.02/03/05 |
| `SetDefaultTariff` | First match: `Rejected` with `UnknownEVSE`; the result of the check above (`InvalidValue` as `Rejected` with that reason code); `DuplicateTariffId` when the id belongs to an installed default tariff, a stored driver tariff or the tariff of a running transaction; `Rejected` with `OutOfMemory` past 4 tariffs with a future `validFrom` per EVSE; otherwise `Accepted`. `evseId` 0 installs the tariff on every EVSE. Default tariffs survive a restart | I07.FR.01-13 |
| `validFrom` | The tariff in use on an EVSE is the valid one (no `validFrom`, or one in the past) with the latest `validFrom`; a tariff without `validFrom` replaces the valid ones at once, tariffs with a future `validFrom` wait for their time | I07.FR.19-23 |
| Driver tariff | The `tariff` of an `Accepted` `AuthorizeResponse` is kept for the token (`validFrom` ignored) and in the authorization cache with the entry. One that fails the check sets `Problem` and, by `HandleFailedTariff`, rejects the token as `Invalid`, falls back to the default tariff, or leaves the cost to the CSMS. Removed when the transaction that used it ends or the authorization lapses without a transaction | I08 |
| `Problem` | Set when a driver tariff fails the check, cleared by the next good one and at the end of a transaction with local cost. Each change sends `NotifyEvent` (`Alerting`, `HardWiredNotification`, `TariffCostCtrlr`/`Problem`, `techCode` with the check result when `true`); the station has no variable monitors | I08.FR.30-37, I12.FR.43-44 |
| Tariff of a transaction | Chosen when the transaction is authorized: the driver tariff, else the default tariff of the EVSE. The transaction keeps it until it ends, also when the default changes or is cleared. Acceptance is implicit: `transactionInfo.tariffId` goes once into the next `TransactionEvent`. `TariffNotAccepted` is not sent | I07.FR.14/24/32, I08.FR.22 |
| `GetTariffs` | Default tariffs with the EVSEs they are installed on and `validFrom`; driver tariffs (and tariffs set by `ChangeTransactionTariff`) with their `idTokens` and the EVSEs of transactions that use them. `NoTariff` without assignments, `Rejected` with `UnknownEVSE` | I09 |
| `ClearTariffs` | Clears default tariffs only, filtered by `tariffIds` and `evseId`: `Accepted` per cleared id, `NoTariff` for a requested id that matched nothing, a single `NoTariff` when nothing was cleared, `Rejected` with `UnknownEVSE`. A running transaction keeps a cleared tariff | I10 |
| `ChangeTransactionTariff` | First match: `TxNotFound`; the result of the check; `NoCurrencyChange`; `Rejected` with `OutOfMemory` after 100 periods; otherwise `Accepted`, an `Updated` event with `TariffChanged` and the new `tariffId`, and the new tariff applies from then on (without its fixed fee) | I11 |
| Cost | Fixed fee once when the tariff is chosen; `reservationFixed`, and `reservationTime` from the `ReserveNow` to the transaction start, when the transaction used a reservation. Energy per kWh, charging time per minute while `Charging`, idle time per minute while the EV is connected and not charging. Time is billed in whole seconds, energy in Wh rounded to 3 decimals, money rounded to 4 decimals. Taxes are applied stack by stack, rates of one stack added up | I12.FR.05-13, I12.FR.30 |
| Min and max cost | `maxCost` of the tariff freezes the amounts (`MaxCost`) while usage keeps counting; `minCost` replaces a lower total at the end (`MinCost`) | I12.FR.38-41 |
| `costDetails` | In the `Ended` event of a transaction with local cost: one charging period per tariff used, with dimensions `Energy`, `ChargingTime` and `IdleTIme` (the spelling of the 2.1 schema) for the fields the tariff prices, `totalCost` and `totalUsage`. With `Enabled[RunningCost]`, the event that names the tariff and an `Updated` event with `RunningCost` every `Interval[Cost]` seconds carry `costDetails` without periods. `failureToCalculate` is not used | I12.FR.01-04, I12.FR.16/17 |
| Resume | Tariff and accumulated cost stay with the transaction across E17 and B13 resumption | E17 |

## Not implemented

The 2.1 functional blocks beyond 2.0.1: bidirectional power transfer (V2X), DER control, tariff conditions and the tariff information use cases I01 to I06 beyond the 2.0.1 behavior, dynamic charging profiles, battery swap, priority charging, periodic event streams and payment (`NotifySettlement`, `VatNumberValidation`, `NotifyWebPaymentStarted`).

## Known limits

- The emulator's terminal UI offers the eight 2.0.1 token types in the token form; another type can be sent with `send_raw`.
- `replay` and `scenarios from-recording` are not available for 2.1, and a fleet (`--count` above 1) runs headless only, as for 2.0.1.
- A station id used with 2.0.1 and 2.1 shares one file set; the files record the protocol that wrote them, which is not checked on load.

## CSMS side (`kirby-csms`)

A 2.1 session is handled by the same listener as 1.6 and 2.0.1 and like a 2.0.1 session, except as listed here.

| Topic | Behavior |
| --- | --- |
| Subprotocol | The default policy is `require:ocpp1.6,ocpp2.0.1,ocpp2.1`; the client's order of preference decides |
| Station id | The 2.0.1 rule: 1 to 48 `identifierString` characters, no colon |
| Inbound validation | Station CALLs are validated against the 2.1 schemas with 2.x error codes; the 91-action catalog decides between `NotSupported` (defined, but not a station CALL) and `NotImplemented` (unknown name). `NotifyPeriodicEventStream` is a SEND message, so a CALL of that name gets `NotSupported` |
| Commands | `SendLocalList`, `GetLocalListVersion`, `ReserveNow`, `CancelReservation`, `GetReport`, `SetChargingProfile`, `ClearChargingProfile`, `GetChargingProfiles` and `GetCompositeSchedule` have forms, as for 2.0.1 |
| Replies | The 36 station CALLs get answers valid against the 2.1 response schemas: the 2.0.1 answers for the shared actions; `OpenPeriodicEventStream` and `PullDynamicScheduleUpdate` `Accepted`; `VatNumberValidation` `Accepted`, echoing the request; `GetCertificateChainStatus` `Failed` for each certificate; an empty result for the other new actions. Rules and faults can replace them |
| State | The 2.0.1 fold (EVSEs, transactions, meter values); `state.v2.protocol` is `ocpp2.1` |
| Commands | Typed commands and forms are the 2.0.1 ones; the 15 CSMS actions new in 2.1 are sent as raw payloads, checked against the 2.1 schemas |
| Scripts | `ocpp_version = "2.1"`; JUnit class names read `ocpp2.1.<script>` |
| Message types 5 and 6 | A station SEND (such as `NotifyPeriodicEventStream`) is recorded in the frame history and events and never answered; with inbound validation on, one that violates its 2.1 schema, or whose action is not a 2.1 SEND message, also emits a `malformed` event. A station CALLRESULTERROR is recorded, never answered, and completes or fails no CSMS CALL. The CSMS never sends a SEND. With inbound validation on, a station CALLRESULT that violates the 2.1 response schema is answered with a CALLRESULTERROR (same message id, the 2.x error code of the violation, a description of at most 255 characters) and fails the call with an invalid-response error; with validation off, and on 1.6 and 2.0.1 sessions, no CALLRESULTERROR is sent |
| Replay | `--replay` stays 1.6-only |
