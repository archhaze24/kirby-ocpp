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
  --heartbeat 30
```

Environment variables mirror the main flags:

- `OCPP_URL`
- `OCPP_CHARGE_POINT_ID`
- `OCPP_VENDOR`
- `OCPP_MODEL`

## TUI controls

- `b` sends `BootNotification`
- `h` sends `Heartbeat`
- `s` cycles connector status
- `a` asks for an `idTag` and sends `Authorize`
- `t` asks for an `idTag` and starts or stops a transaction on the selected connector
- `m` sends `MeterValues` for the selected connector
- `M` sends `MeterValues` with editable comma-separated measurands
- `d` sends `DataTransfer` from editable JSON
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

Station state is persisted by default per `chargePointId`, including local authorization lists, charging profiles, mutable configuration values, and meter value. Use `--no-persist` for an ephemeral station, or `--state-dir <path>` to choose the storage directory.

Authorization behavior now uses the persisted local authorization list and authorization cache. The emulator supports `LocalAuthorizeOffline`, `LocalPreAuthorize`, `AllowOfflineTxForUnknownId`, `AuthorizationCacheEnabled`, and `AuthorizeRemoteTxRequests` through `ChangeConfiguration`.

Charging profiles are persisted structurally per connector. `SetChargingProfile`, filtered `ClearChargingProfile`, and `GetCompositeSchedule` are supported for basic `ChargePointMaxProfile`, `TxDefaultProfile`, and active `TxProfile` scenarios.

`MeterValuesSampledData` and `StopTxnSampledData` are honored when building `MeterValues` and `StopTransaction.transactionData`. Invalid measurands sent through `ChangeConfiguration` are rejected.

Connector lifecycle tracks EV plug state and moves through `Preparing`, `Charging`, `Finishing`, and `Available` for manual start/stop and EV disconnect scenarios.

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
