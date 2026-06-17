# kirby-ocpp

Terminal OCPP 1.6J charge point emulator for local and staging CSMS testing.

It runs as an interactive TUI, speaks OCPP 1.6 JSON over WebSocket, validates payloads against bundled OCPP 1.6J JSON schemas, and can persist station state per charge point id.

## Install

Run without installing:

```bash
npx kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001
```

Or install globally:

```bash
npm install -g kirby-ocpp
kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001
```

## Usage

```bash
kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001
```

Common options:

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

WSS/TLS options:

```bash
kirby-ocpp \
  --url wss://csms.example.com/ocpp \
  --id CP-001 \
  --tls-ca ./ca.pem \
  --tls-cert ./client-cert.pem \
  --tls-key ./client-key.pem
```

Useful runtime flags:

- `--no-persist`: run an ephemeral station.
- `--state-dir <path>`: choose where station state is stored.
- `--ws-subprotocol <protocol>`: defaults to `ocpp1.6`.
- `--ws-ping <seconds>`: WebSocket ping interval, `0` disables pings.
- `--no-reconnect`: disable automatic reconnect.
- `--reconnect-initial-ms <ms>`, `--reconnect-max-ms <ms>`, `--reconnect-max-attempts <count>`: reconnect backoff.
- `--call-timeout-ms <ms>`: OCPP CALL response timeout.
- `--tls-server-name <name>` and `--tls-skip-verify`: local/self-signed WSS testing.

Environment variables mirror the main flags: `OCPP_URL`, `OCPP_CHARGE_POINT_ID`, `OCPP_VENDOR`, `OCPP_MODEL`, `OCPP_WS_SUBPROTOCOL`, `OCPP_WS_PING_INTERVAL`, `OCPP_RECONNECT`, `OCPP_RECONNECT_INITIAL_MS`, `OCPP_RECONNECT_MAX_MS`, `OCPP_RECONNECT_MAX_ATTEMPTS`, `OCPP_CALL_TIMEOUT_MS`, `OCPP_TLS_CA_FILE`, `OCPP_TLS_CERT_FILE`, `OCPP_TLS_KEY_FILE`, `OCPP_TLS_SERVER_NAME`, and `OCPP_TLS_SKIP_VERIFY`.

## TUI

The interface is organized into tabs. Use `1`-`7` or `Tab` / `Shift+Tab` to switch tabs, `Up` / `Down` to select an action, and `Enter` to run it.

- `1` Station
- `2` Connector
- `3` Transaction
- `4` Maintenance
- `5` Data
- `6` Logs
- `7` Scenarios

Global shortcuts:

- `/`: command palette.
- `[` / `]`: previous/next connector.
- `+`: add connector.
- `j` / `k`: scroll logs.
- `G`: jump logs to bottom.
- `F`: cycle log filter.
- `l`: full-screen plain log view.
- `y`: copy current log buffer.
- `?`: help.
- `q`: quit.

Forms prompt for values such as `idTag`, connector id, measurands, and `DataTransfer` payloads. There is no built-in default RFID/idTag.

## Persistence

State is persisted by default per `chargePointId`. This includes mutable configuration, local auth lists/cache, charging profiles, connector meter values, reservations, and pending offline transactions.

```bash
kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001 --no-persist
kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001 --state-dir ./station-state
```

## Scope

kirby-ocpp targets OCPP 1.6J. It supports common Charge Point calls and Central System commands for booting, authorization, transactions, metering, local auth lists, reservations, availability, reset, diagnostics, firmware, smart charging, trigger messages, and data transfer.

Detailed coverage is tracked in [docs/OCPP_1.6J_CONFORMANCE.md](docs/OCPP_1.6J_CONFORMANCE.md).

## Development

```bash
npm install
npm run dev -- --url ws://localhost:9000/ocpp --id CP-001
```

Before publishing:

```bash
npm run typecheck
npm test
npm run smoke
npm run pack:check
```

`npm run pack:check` builds the package, installs the generated tarball into a temporary directory, and verifies the installed CLI and bundled schemas.

## Third-Party Materials

This project is MIT licensed. Bundled OCPP 1.6J JSON schemas are sourced from the official Open Charge Alliance OCPP 1.6 download package and are included for validation/conformance. They are not owned by this project and are not covered by this project's MIT license.

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
