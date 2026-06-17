# kirby-ocpp

Terminal OCPP charge point emulator for quick local testing of Central Systems.

## Install

```bash
npm install -g kirby-ocpp
```

During local development:

```bash
npm install
npm run dev -- --url ws://localhost:9000/ocpp --id CP-001
```

Run protocol smoke checks:

```bash
npm run smoke
```

## Usage

```bash
kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001
```

Useful flags:

```bash
kirby-ocpp \
  --url ws://localhost:9000/ocpp \
  --id CP-001 \
  --vendor Kirby \
  --model TUI-16 \
  --connector 1 \
  --connectors 2 \
  --heartbeat 30 \
  --ws-ping 30 \
  --reconnect-initial-ms 1000 \
  --reconnect-max-ms 30000 \
  --call-timeout-ms 30000
```

Environment variables mirror the main flags:

- `OCPP_URL`
- `OCPP_CHARGE_POINT_ID`
- `OCPP_VENDOR`
- `OCPP_MODEL`
- `OCPP_WS_SUBPROTOCOL`
- `OCPP_WS_PING_INTERVAL`
- `OCPP_RECONNECT=0`
- `OCPP_RECONNECT_INITIAL_MS`
- `OCPP_RECONNECT_MAX_MS`
- `OCPP_RECONNECT_MAX_ATTEMPTS`
- `OCPP_CALL_TIMEOUT_MS`
- `OCPP_TLS_CA_FILE`
- `OCPP_TLS_CERT_FILE`
- `OCPP_TLS_KEY_FILE`
- `OCPP_TLS_SERVER_NAME`
- `OCPP_TLS_SKIP_VERIFY=1`

## TUI controls

- `b` sends `BootNotification`
- `h` sends `Heartbeat`
- `s` cycles connector status
- `a` asks for an `idTag` and sends `Authorize`
- `t` asks for an `idTag` and starts or stops a transaction on the selected connector
- `m` sends `MeterValues` for the selected connector
- `M` sends `MeterValues` with editable comma-separated measurands
- `d` sends `DataTransfer` from editable JSON
- `D` sets the diagnostics outcome (`success` or `uploadFailure`)
- `U` sets the firmware outcome (`success`, `downloadFailure`, or `installationFailure`)
- `f` injects or clears a connector fault
- `p` plugs or unplugs an EV on the selected connector
- `j` / `k` scrolls the OCPP log
- `G` / `g` jumps the OCPP log to the bottom
- `l` opens a full-screen plain log view for cleaner text selection
- `y` copies the current OCPP log buffer to the clipboard
- `[` / `]` selects a connector
- `+` adds a connector at runtime
- `r` reconnects
- `q` exits

## Scope

The implementation targets OCPP 1.6 JSON over WebSocket. Incoming and outgoing action payloads are validated against the official OCPP 1.6J JSON schemas in `src/ocpp/schemas/json`.
The WebSocket client requests the `ocpp1.6` subprotocol by default, sends periodic ping frames, and supports `wss://` TLS options through `--tls-ca`, `--tls-cert`, `--tls-key`, `--tls-server-name`, and `--tls-skip-verify`.
Unexpected WebSocket disconnects are retried by default with exponential backoff. Use `--no-reconnect`, `--reconnect-initial-ms`, `--reconnect-max-ms`, and `--reconnect-max-attempts` to tune that behavior.
Use `--call-timeout-ms` to tune how long the emulator waits for a CALLRESULT/CALLERROR before treating the OCPP request as timed out.

Implementation coverage is tracked in `docs/OCPP_1.6J_CONFORMANCE.md`.

Station state is persisted by default per `chargePointId`, including local authorization lists, charging profiles, mutable configuration values, and meter value. Use `--no-persist` for an ephemeral station, or `--state-dir <path>` to choose the storage directory.

Authorization behavior now uses the persisted local authorization list and authorization cache. The emulator supports `LocalAuthorizeOffline`, `LocalPreAuthorize`, `AllowOfflineTxForUnknownId`, `AuthorizationCacheEnabled`, and `AuthorizeRemoteTxRequests` through `ChangeConfiguration`.

Charging profiles are persisted structurally per connector. `SetChargingProfile`, filtered `ClearChargingProfile`, `RemoteStartTransaction` with a `TxProfile`, and `GetCompositeSchedule` are supported for common `ChargePointMaxProfile`, `TxDefaultProfile`, and active `TxProfile` scenarios.

`MeterValuesSampledData`, `MeterValuesAlignedData`, `StopTxnSampledData`, and `StopTxnAlignedData` are honored when building metering payloads. Invalid measurands sent through `ChangeConfiguration` are rejected. `MeterValueSampleInterval` starts periodic `MeterValues` while a transaction is charging, and `ClockAlignedDataInterval` sends `Sample.Clock` values on wall-clock aligned intervals; `0` disables either timer.

Connector lifecycle tracks EV plug state and moves through `Preparing`, `Charging`, `Finishing`, and `Available` for manual start/stop and EV disconnect scenarios.
`MinimumStatusDuration` delays rapid connector status changes and sends only the latest stable `StatusNotification`.
If `StopTransactionOnEVSideDisconnect=false`, unplugging an EV during an active transaction moves the connector to `SuspendedEV` and keeps the transaction active until an explicit stop or reconnect.
If `StopTransactionOnInvalidId=true`, an online `Authorize` response with a non-accepted `idTagInfo.status` stops matching active transactions with reason `DeAuthorized`. `MaxEnergyOnInvalidId` delays that stop until the configured Wh budget is consumed.

Connector faults track OCPP `errorCode`, `info`, `vendorId`, and `vendorErrorCode`, and emit schema-valid `StatusNotification` messages.

Reservations track reserved `idTag`, optional parent id tag, expiry time, and reservation id. Expired reservations clear automatically, and `StartTransaction` only consumes a reservation when the `idTag` matches.

It supports common station calls and Central System commands:

- `BootNotification`
- `Heartbeat`
- `StatusNotification`
- `Authorize`
- `StartTransaction`
- `StopTransaction`
- `MeterValues`
- `DataTransfer`
- `RemoteStartTransaction`
- `RemoteStopTransaction`
- `ChangeAvailability`
- `Reset`
- `TriggerMessage`
- `UnlockConnector`
- `GetConfiguration`
- `ChangeConfiguration`
