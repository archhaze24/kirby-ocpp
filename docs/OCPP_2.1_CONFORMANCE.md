# OCPP 2.1 conformance notes

Coverage of `kirby-ocpp --protocol ocpp2.1` (the station) and of `kirby-csms` for OCPP 2.1 sessions. This page lists what differs from OCPP 2.0.1; everything else is as described in [OCPP_2.0.1_CONFORMANCE.md](OCPP_2.0.1_CONFORMANCE.md). For OCPP 1.6J see [OCPP_1.6J_CONFORMANCE.md](OCPP_1.6J_CONFORMANCE.md).

- The station is an emulator for testing a CSMS, not a certified charge station.
- OCPP 2.1 support is the 2.0.1 behavior on the 2.1 wire: subprotocol `ocpp2.1`, the 2.1 schemas, the 2.1 action catalog and the OCPP-J 2.1 message types. None of the new 2.1 functional blocks is implemented (see [Not implemented](#not-implemented)).

Validation: the 181 official 2.1 schemas (JSON Schema draft-06) are embedded in the build. Every frame of a 2.1 connection is validated against them, so a 2.1-only field or enum value is accepted and a 2.0.1-only constraint is not applied. CALLERROR codes use the 2.x spelling.

Contents: [Catalog](#catalog) · [Message types](#message-types) · [Station](#station) · [Not implemented](#not-implemented) · [Known limits](#known-limits) · [CSMS side](#csms-side-kirby-csms)

## Catalog

The 2.1 catalog holds 91 actions: the 64 actions of 2.0.1 and 27 new ones. `NotifyPeriodicEventStream` is sent only as a SEND, never as a CALL.

| Direction | New actions |
| --- | --- |
| Station to CSMS (12) | `BatterySwap`, `ClosePeriodicEventStream`, `GetCertificateChainStatus`, `NotifyDERAlarm`, `NotifyDERStartStop`, `NotifyPeriodicEventStream` (SEND), `NotifyPriorityCharging`, `NotifySettlement`, `OpenPeriodicEventStream`, `PullDynamicScheduleUpdate`, `ReportDERControl`, `VatNumberValidation` |
| CSMS to station (15) | `AFRRSignal`, `AdjustPeriodicEventStream`, `ChangeTransactionTariff`, `ClearDERControl`, `ClearTariffs`, `GetDERControl`, `GetPeriodicEventStream`, `GetTariffs`, `NotifyAllowedEnergyTransfer`, `NotifyWebPaymentStarted`, `RequestBatterySwap`, `SetDefaultTariff`, `SetDERControl`, `UpdateDynamicSchedule`, `UsePriorityCharging` |

The station sends none of the new station actions and answers each new CSMS action with CALLERROR `NotSupported`.

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
| `Reset` | `ImmediateAndResume` is `Rejected` with `statusInfo.reasonCode` `UnsupportedParam`; the station does not reboot |
| `TriggerMessage` | `CustomTrigger` and `SignV2G20Certificate` are `NotImplemented` |
| New CSMS actions | CALLERROR `NotSupported` |
| Sent messages | The 2.0.1 set, which stays valid in 2.1 |

## Not implemented

The 2.1 functional blocks beyond 2.0.1: bidirectional power transfer (V2X), DER control, tariffs and cost, local cost calculation, dynamic charging profiles, battery swap, priority charging, periodic event streams and payment (`NotifySettlement`, `VatNumberValidation`, `NotifyWebPaymentStarted`).

## Known limits

- The emulator's terminal UI offers the eight 2.0.1 token types in the token form; another type can be sent with `send_raw`.
- `--count` above 1, `replay` and `scenarios from-recording` are not available for 2.1, as for 2.0.1.
- A station id used with 2.0.1 and 2.1 shares one file set; the files record the protocol that wrote them, which is not checked on load.

## CSMS side (`kirby-csms`)

A 2.1 session is handled by the same listener as 1.6 and 2.0.1 and like a 2.0.1 session, except as listed here.

| Topic | Behavior |
| --- | --- |
| Subprotocol | The default policy is `require:ocpp1.6,ocpp2.0.1,ocpp2.1`; the client's order of preference decides |
| Station id | The 2.0.1 rule: 1 to 48 `identifierString` characters, no colon |
| Inbound validation | Station CALLs are validated against the 2.1 schemas with 2.x error codes; the 91-action catalog decides between `NotSupported` (defined, but not a station CALL) and `NotImplemented` (unknown name). `NotifyPeriodicEventStream` is a SEND message, so a CALL of that name gets `NotSupported` |
| Commands | `SendLocalList` and `GetLocalListVersion` have forms, as for 2.0.1 |
| Replies | The 36 station CALLs get answers valid against the 2.1 response schemas: the 2.0.1 answers for the shared actions; `OpenPeriodicEventStream` and `PullDynamicScheduleUpdate` `Accepted`; `VatNumberValidation` `Accepted`, echoing the request; `GetCertificateChainStatus` `Failed` for each certificate; an empty result for the other new actions. Rules and faults can replace them |
| State | The 2.0.1 fold (EVSEs, transactions, meter values); `state.v2.protocol` is `ocpp2.1` |
| Commands | Typed commands and forms are the 2.0.1 ones; the 15 CSMS actions new in 2.1 are sent as raw payloads, checked against the 2.1 schemas |
| Scripts | `ocpp_version = "2.1"`; JUnit class names read `ocpp2.1.<script>` |
| Message types 5 and 6 | A station SEND (such as `NotifyPeriodicEventStream`) is recorded in the frame history and events and never answered; with inbound validation on, one that violates its 2.1 schema, or whose action is not a 2.1 SEND message, also emits a `malformed` event. A station CALLRESULTERROR is recorded, never answered, and completes or fails no CSMS CALL. The CSMS sends neither type itself: a station reply that violates its schema fails the call without a CALLRESULTERROR |
| Replay | `--replay` stays 1.6-only |
