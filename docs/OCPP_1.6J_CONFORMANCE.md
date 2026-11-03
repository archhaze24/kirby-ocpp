# OCPP 1.6J conformance notes

Coverage of `kirby-ocpp` against OCPP 1.6J. All 28 actions have schemas bundled in `crates/kirby-ocpp-core/schemas` (official Open Charge Alliance files, unmodified); the 11 actions of the OCPP 1.6 Security Whitepaper (edition 3) have typed messages and bundled schemas too (see Known limits). The station is an emulator for testing a CSMS, not a certified charge point; it is not OCA-certified and deliberately simulates hardware (no real metering, firmware or file transfer).

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
| Smart Charging | partial: profiles are validated, stored, reported and applied to the simulated power and energy (see below) |

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
| MeterValues | full (simulated meter) | `MeterValuesSampledData`, `MeterValuesAlignedData`, `MeterValueSampleInterval`, `ClockAlignedDataInterval` honoured; values come from a simulated energy meter that follows the power model below. |
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
| GetCompositeSchedule | partial | Purpose/stack precedence, validity windows, absolute/recurring/relative schedules, `ChargePointMaxProfile` limiting; uncovered stretches show the station's nominal power; edge cases with ambiguous tie policies are not exhaustive. |
| GetConfiguration | full | More keys than `GetConfigurationMaxKeys` is answered with CALLERROR `OccurenceConstraintViolation`. |
| GetDiagnostics | simulated | Starts the status lifecycle; no file is uploaded. |
| GetLocalListVersion | full | |
| RemoteStartTransaction | full | Optional authorization (`AuthorizeRemoteTxRequests`), reservation rules, `TxProfile` without `transactionId` stored for the new transaction. |
| RemoteStopTransaction | full | |
| ReserveNow | full | Occupied/Unavailable/Faulted/Rejected; an existing reservationId is replaced, also when it is on another connector (the reservation moves; a refused move keeps the original). |
| Reset | partial | Replies `Accepted`, stops transactions (reason `SoftReset`/`HardReset`), releases reservations, re-registers; `ResetRetries`. Soft reboots on the same WebSocket; Hard closes it and reconnects (also with auto-reconnect off), then boots. No real restart. |
| SendLocalList | full | Full and differential updates, version rules, `NotSupported` when disabled, `Failed` over `SendLocalListMaxLength`. |
| SetChargingProfile | partial | Stored and validated against `ChargeProfileMaxStackLevel`, `ChargingScheduleMaxPeriods`, `MaxChargingProfilesInstalled`, allowed rate unit; limits the simulated power. |
| TriggerMessage | full | BootNotification, DiagnosticsStatusNotification, FirmwareStatusNotification, Heartbeat, MeterValues, StatusNotification; connector-specific where applicable. |
| UnlockConnector | partial | Stops a running transaction with `UnlockCommand`; no hardware. |
| UpdateFirmware | simulated | Download/install lifecycle only. |

## Configuration keys

Registry keys, seeded from the station config: AllowOfflineTxForUnknownId, AuthorizationCacheEnabled, AuthorizeRemoteTxRequests, BlinkRepeat, ChargePointModel, ChargePointSerialNumber, ChargePointVendor, ChargeProfileMaxStackLevel, ChargingScheduleAllowedChargingRateUnit, ChargingScheduleMaxPeriods, ClockAlignedDataInterval, ConnectionTimeOut, ConnectorPhaseRotation, ConnectorPhaseRotationMaxLength, GetConfigurationMaxKeys, HeartbeatInterval, LightIntensity, LocalAuthListEnabled, LocalAuthListMaxLength, LocalAuthorizeOffline, LocalPreAuthorize, MaxChargingProfilesInstalled, MaxEnergyOnInvalidId, MeterValuesAlignedData(+MaxLength), MeterValuesSampledData(+MaxLength), MeterValueSampleInterval, MinimumStatusDuration, NumberOfConnectors, ReserveConnectorZeroSupported, ResetRetries, SendLocalListMaxLength, StopTransactionOnEVSideDisconnect, StopTransactionOnInvalidId, StopTxnAlignedData(+MaxLength), StopTxnSampledData(+MaxLength), SupportedFeatureProfiles(+MaxLength), TransactionMessageAttempts, TransactionMessageRetryInterval, UnlockConnectorOnEVSideDisconnect, WebSocketPingInterval; and the Security Whitepaper keys: SecurityProfile (writable, 0-3, can only be raised), AuthorizationKey (write-only), CpoName, CertificateStoreMaxLength (8), CertificateSignedMaxChainSize (10000), AdditionalRootCertificateCheck (false).

Capability and limit keys (for example `NumberOfConnectors`, the `*MaxLength` keys, `SupportedFeatureProfiles`) are read-only. Writable values are persisted per charge point. Some keys (for example `BlinkRepeat`, `LightIntensity`) are stored and reported but drive no behaviour. Vendor-specific keys are not supported (`NotSupported`).

## Testing aids (deliberate non-conformance)

Response rules (`--rule`, `KIRBY_OCPP_RULES`, TUI, scenario steps `set_rule` / `clear_rules`) make the station answer a CSMS command in a way the specification forbids, on purpose: `reject` (the action's rejection status, or CALLERROR `NotSupported` where the response has none: `GetConfiguration`, `GetLocalListVersion`, `ClearChargingProfile`, `GetDiagnostics`, `UpdateFirmware`), `delay:MS`, `error:CODE[:TEXT]`, `drop` (close the socket without a reply) and `ignore` (no reply, so the CSMS times out; the "every CALL gets one reply" rule is deliberately waived, bypassing the `InternalError` drop fallback). Without a rule the station behaves as described above. Rules are runtime only; except for `delay` the command is not executed. See the README for the syntax.

Local edits (TUI data browser, `Command::SetConfiguration`, `SetLocalListEntry`, `RemoveLocalListEntry`, `ClearLocalList`, `InstallChargingProfile`, `ClearChargingProfiles`) are not messages and have no spec equivalent; they reuse the rules of `ChangeConfiguration`, `SendLocalList`, `SetChargingProfile` and `ClearChargingProfile`. The local list version is the one thing they decide themselves: an edit is a differential update with version `current + 1` (a CSMS that tracks versions sees a mismatch, which is the point of testing it), and a local clear is a full update with an empty list and version `0` ("no list"). Nothing is sent to the CSMS after a local edit.

## Security Whitepaper behaviour (certificates, logs, extended trigger)

- **Certificate store**: Central System and Manufacturer root certificates (PEM, one per `InstallCertificate`, at most 5500 characters, at most `CertificateStoreMaxLength` = 8 in total). Only self-signed (root) certificates that are currently valid are accepted; anything else (garbage, expired, not yet valid, several certificates, not a root, store full) is `Rejected` (deviation: a full store is `Rejected`, as OCPP 2.0.1 says; the whitepaper text is ambiguous). Signatures are not verified. Installing a certificate that is already there is `Accepted` and changes nothing. Certificate hashes follow the OCSP CertID rules (hash of the issuer name DER and of the issuer key bits, serial in lowercase hex without leading zeros, compared case-insensitively); `GetInstalledCertificateIds` answers SHA-256 data, `DeleteCertificate` accepts SHA-256/384/512. With `SecurityProfile` 2 or 3 the last Central System root cannot be deleted (`Failed`); this is an assumption, the whitepaper wording was not checked against the source. Installed Central System roots become the TLS trust anchors (in addition to `--tls-ca`); once one is installed the system roots are no longer used.
- **Certificate signing**: `RequestCertificate` creates an ECDSA P-256 key and a CSR (`CN` = charge point id, `O` = `CpoName` or `Kirby`) and sends `SignCertificate`. `CertificateSigned` is `Accepted` only if a request is pending, the chain parses, fits `CertificateSignedMaxChainSize`, every certificate is currently valid and the leaf carries the pending public key; otherwise it is `Rejected` and an `InvalidChargePointCertificate` security event follows. Chain trust is not verified. A rejected `SignCertificate` forgets the key. `ExtendedTriggerMessage` (`SignChargePointCertificate`) is not implemented yet.
- **Keys on disk**: certificates are public and live in the main state file (format version 4; older files load with an empty store). The private key of the station certificate, a pending request key and the `AuthorizationKey` live in `<id>.station.secrets.json` next to it (mode `0600` on Unix, never in the main file, never in logs or the snapshot; GetConfiguration lists `AuthorizationKey` without a value and the snapshot shows `********`). Deviation: the raw `ChangeConfiguration` CALL that carries the key is still visible in the frame log (`--log-file`, `--json`, TUI log view).
- **Profiles**: `SecurityProfile` can only be raised (lowering is `Rejected`). 1 needs credentials (an `AuthorizationKey` or a password in the URL), 2 needs a `wss://` URL as well, 3 additionally a client certificate (`--tls-cert` or a stored certificate). Changes apply at the next (re)connect. Profile 1 and 2 send HTTP Basic Auth (user = charge point id, password = `AuthorizationKey` as given, 16-40 printable ASCII characters; deviation: not hex-decoded) unless the URL already carries a password; profile 3 uses the stored certificate unless `--tls-cert` is given. Profile 2 does not require TLS verification to be on.
- **Security events**: sent as `SecurityEventNotification` once registered, queued offline (at most 100, persisted, oldest dropped) and re-sent in order after the next accepted boot. A CSMS answer with an error drops the event. Emitted: `StartupOfTheDevice` (once per process, after the first accepted boot), `ResetOrReboot`, `ReconfigurationOfSecurityParameters` (SecurityProfile/AuthorizationKey changes), `InvalidChargePointCertificate`, `FailedToAuthenticateAtCentralSystem` (HTTP 401/403 on the upgrade), `InvalidCentralSystemCertificate` (TLS peer certificate refused) and anything valid sent with `Command::SendSecurityEvent`. `SettingSystemTime` is not emitted because the station never sets its clock.
- **Response rules** can target the four certificate commands (`reject` answers `Rejected`, `Failed`, `NotFound` and `Rejected` respectively).
- **GetLog / LogStatusNotification**: simulated, nothing is read or sent anywhere. `remoteLocation` must be an absolute URL with a host and scheme ftp, ftps, http, https or sftp, else `Rejected` (no `filename`). `Accepted` carries a generated `filename` (`diagnostics-log-<id>-<time>.log` or `security-log-...`; `logType` only changes the name). The reply comes first, then `LogStatusNotification` `Uploading` then `Uploaded` (100 ms apart) with the `requestId`; the diagnostics outcome knob (`SetDiagnosticsOutcome`) selects `UploadFailure` instead, with `retries` / `retryInterval` (at most 5 retries, at most 1 s apart) as for `GetDiagnostics`. A `GetLog` while an upload runs is `AcceptedCanceled`; the cancelled upload sends no further status (`UploadLogStatusEnumType` has no cancelled value; deviation: no `Idle`/other final report). A disconnect or `Reset` drops the remaining statuses. `PermissionDenied`, `BadMessage` and `NotSupportedOperation` are never produced. Snapshot: `log_status`, `log_request_id`.
- **ExtendedTriggerMessage**: same gate as `TriggerMessage` (only `BootNotification` before the station is `Accepted` or `Pending`, else `Rejected`; `connectorId` of `StatusNotification` / `MeterValues` must exist, else `Rejected`; it is ignored for the other messages). `BootNotification`, `Heartbeat`, `StatusNotification`, `MeterValues`, `FirmwareStatusNotification` behave as in `TriggerMessage` (firmware status is the current one). `LogStatusNotification` reports `Uploading` (with `requestId`) while a `GetLog` runs, otherwise `Idle` without `requestId`. `SignChargePointCertificate` is `Accepted` and starts the `SignCertificate` flow (as `RequestCertificate`). `NotImplemented` is never answered. The triggered message follows the reply.
- **Response rules** can also target `GetLog` and `ExtendedTriggerMessage` (`reject` answers `Rejected`).

## Known limits

- Security Whitepaper (edition 3), certificate, log and extended-trigger parts implemented; signed firmware is wire layer only. Implemented: `GetLog`, `LogStatusNotification`, `ExtendedTriggerMessage`, `InstallCertificate`, `DeleteCertificate`, `GetInstalledCertificateIds`, `CertificateSigned`, `SignCertificate` (`Command::RequestCertificate`), `SecurityEventNotification` (`Command::SendSecurityEvent`), the security configuration keys and the profile 1-3 transport rules. Not yet (CALLERROR `NotSupported`): `SignedUpdateFirmware`; `SignedFirmwareStatusNotification` is never sent. See "Security Whitepaper behaviour" below.
- No OCPP 2.x, no JSON-over-SOAP; one WebSocket connection, subprotocol `ocpp1.6` by default.
- Meter, power, SoC and temperature values are simulated.
- Diagnostics and firmware flows emit the right status sequences but transfer nothing.
- The simulated meter is deterministic: temperature and SoC are decorative functions of the energy.

## Intentional differences from the earlier TypeScript version

Strict boolean parsing and CLI > env > default precedence; `GetConfiguration` key limit; `TriggerMessage` reports the real diagnostics/firmware status and honours the registration gate; maintenance timers are cancelled on disconnect; `ChangeAvailability` on connector 0 also changes all connectors; the reply to a CSMS command is sent before follow-up messages; atomic, versioned state files (directory fsynced, long ids get a hashed file name, a `Scheduled` `ChangeAvailability` survives a restart) with `.bak` recovery; `LocalAuthorizeOffline` and `AllowOfflineTxForUnknownId` default to `true` (TS and the spec leave the default to the implementation; TS used `false`, which made the built-in `offline-sync` scenario fail on a stock station). Both stay writable via `ChangeConfiguration`.

### Power model and smart charging (differences from TS, spec behaviour wins)

- **Power and energy.** A `Charging` connector draws `min(nominal power, charging limit)`; energy is the integral of that power over monotonic elapsed time, integrated piecewise across schedule period boundaries and re-evaluated when profiles, status or transactions change. Nominal power, voltage and phases are configurable (`--nominal-power-w` 7200, `--voltage-v` 230, `--phases` 1; single phase 7.2 kW is about 31.3 A). TS added a fixed `2 * interval` Wh per tick and reported unrelated power values.
- **Readings** come from the same numbers: `Power.Active.Import` is the power, `Current.Import` is power / (voltage x phases), `Voltage` is the configured voltage, `Power.Offered` / `Current.Offered` are the limit in force (also when idle), `Energy.Active.Import.Interval` is the energy since the previous sample. The `MeterValues` command still adds its `wh_delta` to the register.
- **Limit resolution.** Profiles in `A` are converted to watts (`A x voltage x numberPhases`, `numberPhases` defaulting to and capped by the station's phases) before they are compared, so `A` and `W` profiles combine. Precedence per instant: the bound `TxProfile`, else the highest stack level of `TxDefaultProfile` (connector's own wins a tie with connector 0), then the minimum with `ChargePointMaxProfile`. The `ChargePointMaxProfile` limits the whole station, so it is split equally between the connectors that are charging; `GetCompositeSchedule` still reports it per connector.
- **Gaps (composite schedule).** A profile limits only the instants it covers. Where no profile covers an instant there is no limit, so the composite schedule always spans the requested window, starts at the requested time, and shows the station's nominal power (in the requested unit) for uncovered stretches. `Rejected` only if no profile of the requested unit covers any instant of the window. The earlier port shifted `scheduleStart` after a leading gap and ended the schedule at a later gap.
- **Absolute and recurring profiles without `startSchedule` and `validFrom`** count their schedule from the moment they were installed (state file version 3 stores `installed_at`; profiles restored from older files count from the restore time). TS anchored them at the composite request time.
- **Reset.** A Hard reset waits briefly for in-flight CALLs (the `StopTransaction`s), closes the WebSocket and reconnects at once, then boots again; a Soft reset reboots on the same connection (TS rebooted on the same connection for both).
