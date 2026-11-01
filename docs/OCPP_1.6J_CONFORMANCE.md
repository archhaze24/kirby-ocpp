# OCPP 1.6J conformance notes

Coverage of `kirby-ocpp` against OCPP 1.6J. All 28 actions have schemas bundled in `crates/kirby-ocpp-core/schemas` (official Open Charge Alliance files, unmodified). The station is an emulator for testing a CSMS, not a certified charge point; it is not OCA-certified and deliberately simulates hardware (no real metering, firmware or file transfer).

Legend: **full** = implemented as specified for emulator purposes; **partial** = implemented for common cases, see notes; **simulated** = the state machine and messages are real, the underlying action (upload, download, power flow) is not.

Validation: outgoing and incoming CALL and CALLRESULT payloads are validated against the schemas (the Rust build embeds them). An incoming CALL that fails validation gets a CALLERROR (unknown actions are answered with a CALLERROR). Behaviour is covered by the unit and integration tests in `crates/kirby-ocpp-core` and `crates/kirby-ocpp/tests`, driven by the in-process mock CSMS in `crates/kirby-ocpp-testkit`.

## Profiles

| Profile | Status |
| --- | --- |
| Core | full, apart from the notes below |
| Firmware Management | simulated |
| Local Auth List Management | full |
| Reservation | full |
| Remote Trigger | full (all six messages) |
| Smart Charging | partial: profiles are validated, stored and reported; they do not throttle the simulated power |

`SupportedFeatureProfiles` reports `Core,FirmwareManagement,LocalAuthListManagement,Reservation,RemoteTrigger,SmartCharging`.

## Charge Point to Central System

| Action | Status | Notes |
| --- | --- | --- |
| Authorize | full | Online; offline decision order: `LocalAuthorizeOffline`, local list (if `LocalAuthListEnabled`), cache (if `AuthorizationCacheEnabled`), `AllowOfflineTxForUnknownId`. `LocalPreAuthorize`, parent tags, expiry and `ConcurrentTx` honoured. |
| BootNotification | full | Pending and Rejected: retried after the CSMS interval. Required before other messages are triggered. |
| DataTransfer | full | Free-form payload. |
| DiagnosticsStatusNotification | simulated | Upload lifecycle with selectable outcome and bounded retries. |
| FirmwareStatusNotification | simulated | Download/install lifecycle with selectable outcome and bounded retries. |
| Heartbeat | full | |
| MeterValues | full (simulated meter) | `MeterValuesSampledData`, `MeterValuesAlignedData`, `MeterValueSampleInterval`, `ClockAlignedDataInterval` honoured; values come from a simulated energy meter. |
| StartTransaction | full | Reservation consumption, idTagInfo statuses, `StopTransactionOnInvalidId`/`MaxEnergyOnInvalidId`. |
| StatusNotification | full | Fault metadata, `MinimumStatusDuration`, duplicate suppression. |
| StopTransaction | full | `StopTxnSampledData`/`StopTxnAlignedData` in `transactionData`, `StopTransactionOnEVSideDisconnect`. |

Transaction messages are queued and retried (`TransactionMessageAttempts`, `TransactionMessageRetryInterval`); transactions started or stopped offline are synchronised after reconnect. The queue survives restarts (see persistence in the README).

## Central System to Charge Point

| Action | Status | Notes |
| --- | --- | --- |
| CancelReservation | full | |
| ChangeAvailability | full | Connector 0 affects the charge point and all connectors; with a running transaction the answer is `Scheduled` and the change applies when it ends. |
| ChangeConfiguration | full | Keys matched case-insensitively; type and list-length validation; read-only keys `Rejected`; unknown `NotSupported`. |
| ClearCache | full | |
| ClearChargingProfile | partial | Filters by id, connector, purpose, stack level. |
| DataTransfer | full | Own vendor id echoed, other vendors `UnknownVendorId`. |
| GetCompositeSchedule | partial | Purpose/stack precedence, validity windows, absolute/recurring/relative schedules, `ChargePointMaxProfile` limiting; edge cases with ambiguous tie policies are not exhaustive. |
| GetConfiguration | full | More keys than `GetConfigurationMaxKeys` is answered with CALLERROR `OccurenceConstraintViolation`. |
| GetDiagnostics | simulated | Starts the status lifecycle; no file is uploaded. |
| GetLocalListVersion | full | |
| RemoteStartTransaction | full | Optional authorization (`AuthorizeRemoteTxRequests`), reservation rules, `TxProfile` without `transactionId` stored for the new transaction. |
| RemoteStopTransaction | full | |
| ReserveNow | full | Occupied/Unavailable/Faulted/Rejected; same reservationId on another connector moves the reservation. |
| Reset | partial | Replies `Accepted`, stops transactions, releases reservations, re-registers; `ResetRetries`. No real restart; Soft and Hard behave the same apart from the stop reason. |
| SendLocalList | full | Full and differential updates, version rules, `NotSupported` when disabled, `Failed` over `SendLocalListMaxLength`. |
| SetChargingProfile | partial | Stored and validated against `ChargeProfileMaxStackLevel`, `ChargingScheduleMaxPeriods`, `MaxChargingProfilesInstalled`, allowed rate unit; not applied to simulated power. |
| TriggerMessage | full | BootNotification, DiagnosticsStatusNotification, FirmwareStatusNotification, Heartbeat, MeterValues, StatusNotification; connector-specific where applicable. |
| UnlockConnector | partial | Stops a running transaction with `UnlockCommand`; no hardware. |
| UpdateFirmware | simulated | Download/install lifecycle only. |

## Configuration keys

Registry keys, seeded from the station config: AllowOfflineTxForUnknownId, AuthorizationCacheEnabled, AuthorizeRemoteTxRequests, BlinkRepeat, ChargePointModel, ChargePointSerialNumber, ChargePointVendor, ChargeProfileMaxStackLevel, ChargingScheduleAllowedChargingRateUnit, ChargingScheduleMaxPeriods, ClockAlignedDataInterval, ConnectionTimeOut, ConnectorPhaseRotation, ConnectorPhaseRotationMaxLength, GetConfigurationMaxKeys, HeartbeatInterval, LightIntensity, LocalAuthListEnabled, LocalAuthListMaxLength, LocalAuthorizeOffline, LocalPreAuthorize, MaxChargingProfilesInstalled, MaxEnergyOnInvalidId, MeterValuesAlignedData(+MaxLength), MeterValuesSampledData(+MaxLength), MeterValueSampleInterval, MinimumStatusDuration, NumberOfConnectors, ReserveConnectorZeroSupported, ResetRetries, SendLocalListMaxLength, StopTransactionOnEVSideDisconnect, StopTransactionOnInvalidId, StopTxnAlignedData(+MaxLength), StopTxnSampledData(+MaxLength), SupportedFeatureProfiles(+MaxLength), TransactionMessageAttempts, TransactionMessageRetryInterval, UnlockConnectorOnEVSideDisconnect, WebSocketPingInterval.

Capability and limit keys (for example `NumberOfConnectors`, the `*MaxLength` keys, `SupportedFeatureProfiles`) are read-only. Writable values are persisted per charge point. Some keys (for example `BlinkRepeat`, `LightIntensity`) are stored and reported but drive no behaviour. Vendor-specific keys are not supported (`NotSupported`).

## Known limits

- No Security Whitepaper extensions, no OCPP 2.x, no JSON-over-SOAP; one WebSocket connection, subprotocol `ocpp1.6` by default.
- Meter, power, SoC and temperature values are simulated.
- Diagnostics and firmware flows emit the right status sequences but transfer nothing.
- Smart charging does not influence the simulated power.

## Intentional differences from the earlier TypeScript version

Strict boolean parsing and CLI > env > default precedence; `GetConfiguration` key limit; `TriggerMessage` reports the real diagnostics/firmware status and honours the registration gate; maintenance timers are cancelled on disconnect; `ChangeAvailability` on connector 0 also changes all connectors; the reply to a CSMS command is sent before follow-up messages; atomic, versioned state files (directory fsynced, long ids get a hashed file name, a `Scheduled` `ChangeAvailability` survives a restart) with `.bak` recovery; `LocalAuthorizeOffline` and `AllowOfflineTxForUnknownId` default to `true` (TS and the spec leave the default to the implementation; TS used `false`, which made the built-in `offline-sync` scenario fail on a stock station). Both stay writable via `ChangeConfiguration`.
