# kirby-ocpp

Terminal OCPP 1.6J charge point emulator for local and staging CSMS testing, written in Rust.

It speaks OCPP 1.6 JSON over WebSocket (`ws://` and `wss://`, including mutual TLS), validates every payload against the bundled official OCPP 1.6J JSON schemas, persists station state per charge point id, and can be used three ways:

- an interactive terminal UI (default when stdout is a terminal);
- a headless event logger (`--no-tui`, `--json`);
- a scripted scenario runner for CI (`kirby-ocpp run --scenario ...`).

## Install

With Node.js (no Rust needed; installs the prebuilt binary for your platform):

```bash
npx kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001
npm install -g kirby-ocpp
```

With Cargo (Rust 1.88 or newer):

```bash
cargo install kirby-ocpp
```

Or download a prebuilt binary for Linux, macOS or Windows from the [GitHub releases](https://github.com/archhaze24/kirby-ocpp/releases). To build from a checkout: `cargo install --path crates/kirby-ocpp`.

## Quick start

```bash
kirby-ocpp --url ws://localhost:9000/ocpp --id CP-001          # TUI
kirby-ocpp --url ws://localhost:9000/ocpp --no-tui --json       # headless, JSON lines
kirby-ocpp --url ws://localhost:9000/ocpp run --scenario charge-session
```

`--url` (or `OCPP_URL`) is the only required option. Precedence is flag, then environment variable, then default. Booleans accept `true/false/yes/no/y/n/on/off/1/0`.

## Options

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `-u, --url` | `OCPP_URL` | required | CSMS WebSocket URL (`ws://` or `wss://`) |
| `-i, --id` | `OCPP_CHARGE_POINT_ID` | `CP-001` | Charge point id |
| `--vendor` | `OCPP_VENDOR` | `archhaze24` | BootNotification vendor |
| `--model` | `OCPP_MODEL` | `KIRBY` | BootNotification model |
| `--serial` | `OCPP_SERIAL` | none | Serial number (max 25 chars) |
| `--firmware-version` | `OCPP_FIRMWARE_VERSION` | none | Firmware version (max 50 chars) |
| `-c, --connector` | | `1` | Initial connector |
| `--connectors` | | `1` | Number of connectors |
| `--heartbeat` | | `30` | Fallback heartbeat interval (s) |
| `--nominal-power-w` | | `7200` | Nominal charging power per connector (W); charging profiles can only lower it |
| `--voltage-v` | | `230` | Voltage per phase (V), used for A to W conversion and the `Voltage` reading |
| `--phases` | | `1` | Number of phases (1-3) |
| `--ws-subprotocol` | `OCPP_WS_SUBPROTOCOL` | `ocpp1.6` | Requested subprotocol |
| `--ws-ping` | `OCPP_WS_PING_INTERVAL` | `30` | WebSocket ping interval (s), `0` disables |
| `--reconnect <BOOL>` | `OCPP_RECONNECT` | `true` | Auto-reconnect after unexpected disconnect |
| `--no-reconnect` | | | Disable auto-reconnect (wins over `--reconnect`) |
| `--reconnect-initial-ms` | `OCPP_RECONNECT_INITIAL_MS` | `1000` | Initial backoff; `0` is rejected (exit 2) unless `--reconnect-max-attempts` is set or reconnect is off |
| `--reconnect-max-ms` | `OCPP_RECONNECT_MAX_MS` | `30000` | Maximum backoff |
| `--reconnect-max-attempts` | `OCPP_RECONNECT_MAX_ATTEMPTS` | `0` | `0` = unlimited |
| `--call-timeout-ms` | `OCPP_CALL_TIMEOUT_MS` | `30000` | CALL response timeout |
| `--tls-ca` | `OCPP_TLS_CA_FILE` | system roots | CA bundle (PEM) |
| `--tls-cert`, `--tls-key` | `OCPP_TLS_CERT_FILE`, `OCPP_TLS_KEY_FILE` | | Client certificate and key for mTLS (both or neither) |
| `--tls-server-name` | `OCPP_TLS_SERVER_NAME` | URL host | SNI / verification name override |
| `--tls-skip-verify` | `OCPP_TLS_SKIP_VERIFY` | off | Skip certificate verification (testing only) |
| `--no-persist` | `KIRBY_OCPP_NO_PERSIST` | off | Do not load or save station state |
| `--state-dir` | `KIRBY_OCPP_STATE_DIR` | `~/.kirby-ocpp/stations` | State directory (`%USERPROFILE%` on Windows) |
| `--rule ACTION=BEHAVIOR[*N]` | `KIRBY_OCPP_RULES` (`;`-separated) | none | Response rule for a CSMS command, repeatable; see [Response rules](#response-rules) |
| `--print-config` | | | Print the effective configuration as JSON and exit |
| `--no-tui` | | | Never start the TUI; log to stdout |
| `--json` | | | JSON lines instead of text (events and the `run` report) |
| `--log-file` | | | Also append the headless event log to a file |
| `--licenses` | | | Print licence notices and exit |
| `-V, --version`, `-h, --help` | | | |

Options are global and may be placed before or after a subcommand.

## TLS

`wss://` uses rustls. Without `--tls-ca` the operating system trust store is used. Use `--tls-ca` for a private CA, `--tls-cert`/`--tls-key` for client authentication, `--tls-server-name` when the URL host differs from the certificate name, and `--tls-skip-verify` only against local self-signed servers.

## State persistence

Per charge point id, one JSON file `<state dir>/<id>.station.json` holds the local authorization list and its version, authorization cache, charging profiles, writable configuration values, availability (including a `Scheduled` change waiting for a transaction to end), connector count and per-connector state (status, meter, reservation, transaction) and the offline transaction queue. Ids whose escaped name would exceed 200 bytes are shortened and get a hash suffix (`...~<16 hex>.station.json`) so the name always fits the filesystem limit. Files are versioned and written atomically (temp file plus rename, then the directory is fsynced); a corrupt file or one belonging to another charge point is moved to `<file>.bak` and the station starts clean. `--no-persist` disables both loading and saving.

## Terminal UI

Seven tabs, switched with `1`-`7` or `Tab`/`Shift-Tab`: Station, Connector, Transaction, Maintenance, Data, Logs, Scenarios. Panes: connectors, actions, log (move focus with Left/Right).

| Key | Action |
| --- | --- |
| `Up/Down`, `j/k`, `PageUp/PageDown` | Move in the focused pane |
| `[` `]` | Previous / next connector |
| `Enter` | Run the selected action / open the selected log row |
| letters | Accelerators of the current tab's actions (shown in the list) |
| `/` or `:` or `Ctrl-P` | Command palette (fuzzy search over all actions); `/` searches the log when the log pane is focused on the Logs tab |
| `Esc` | Close dialog / clear log search |
| `?` | Help |
| `q`, `Ctrl-C` | Quit |

Actions: connect/disconnect/reconnect, BootNotification, Heartbeat, add connector, response rules (`m` adds one, `Action=off` removes one, `M` clears all; active rules are listed in the Station pane); plug/unplug, set status, StatusNotification, fault toggle; Authorize, StartTransaction, StopTransaction (with reason), MeterValues; set diagnostics/firmware outcome and send their status notifications; DataTransfer, vendor ping and raw CALLs.

Log view (Logs tab): CALL/RESULT/ERROR correlation, filters (all, calls, errors, station, csms; `f`), search (`s`), pause/follow (`p`), jump to newest (`G`), detail view with scroll (`Enter`; `c`/`y` copies), copy row (`y`) or visible rows (`Y`), export as JSONL (`e`, default `./kirby-ocpp-<id>-<timestamp>.jsonl`), clear with confirmation (`c`). The buffer keeps the last 2000 entries. Copying tries external clipboard tools first and falls back to the terminal's OSC 52 sequence.

Scenarios tab: one `Run: <name>` action per built-in scenario (keys `o`, `w`, `s`, `i` for the first four) and `l` to load a scenario file, `x` to cancel the running one.

## Headless mode and exit codes

Without a TTY (or with `--no-tui`) the tool connects and prints events until SIGINT/SIGTERM, then closes the WebSocket cleanly. With `--json` each line is one JSON object; frames carry a `payload` field.

`kirby-ocpp run --scenario <name|file.toml> [--report report.json] [--timeout 60] [--id-tag TAG]` connects, waits for an accepted BootNotification, runs the scenario and prints a PASS/FAIL/SKIP report (a JSON object with `"type": "report"` under `--json`).

| Code | Meaning |
| --- | --- |
| 0 | Success (scenario passed, normal shutdown) |
| 1 | Scenario failed or timed out, or a runtime error |
| 2 | Usage or configuration error |
| 130 | `run` interrupted by SIGINT (Ctrl-C); SIGTERM still ends it with 1 |

## Scenarios

`kirby-ocpp scenarios list` lists the built-in scenarios (`charge-session`, `fault-recovery`, `finish-session`, `offline-sync`); `kirby-ocpp scenarios show <name>` prints one as TOML. Built-ins target connector 1 unless `--connector` is given. `offline-sync` works on a stock station: `LocalAuthorizeOffline` and `AllowOfflineTxForUnknownId` default to `true` (switch them off with `ChangeConfiguration` to test the CSMS-side rejection path).

```toml
name = "my-session"
description = "Plug in, start, meter, stop"

[[steps]]
action = "plug"
connector = 1

[[steps]]
action = "start_tx"
connector = 1
id_tag = "KIRBY-TAG"        # implicitly expects status Accepted

[[steps]]
action = "meter"
connector = 1
wh_delta = 1000

[[steps]]
action = "expect_call"      # a CALL seen since the last matched expect_call
call = "MeterValues"
timeout_ms = 3000

[[steps]]
action = "stop_tx"
connector = 1
reason = "Local"
expect = { status = "Accepted" }
```

Actions: `connect`, `disconnect`, `reconnect`, `boot`, `heartbeat`, `plug`, `unplug`, `authorize`, `start_tx`, `stop_tx`, `meter`, `status`, `fault`, `clear_fault`, `sleep`, `wait_for`, `expect_call` (`call`, optional `direction`, `match` payload subset), `send_raw` (`call`, `payload`, `validate`), `data_transfer`, `set_rule` (`rule`), `clear_rules` (optional `call`). Every step may have `name` and `expect` (`status`, `payload` subset, `error = true`). `wait_for` takes `condition = { kind = ... }` with kinds `connected`, `disconnected`, `registration`, `connector_status`, `tx_active`, `tx_idle`, `no_pending_messages`, and `timeout_ms` (default 5000). Unknown fields are rejected.

## Response rules

Response rules make the station misbehave on purpose, to test how a CSMS reacts. A rule overrides how one CSMS command (any of the 19 the station handles, e.g. `Reset`, `RemoteStartTransaction`, `ChangeConfiguration`) is answered. Form: `ACTION=BEHAVIOR[*COUNT]`, action names case-insensitive.

| Behavior | Effect |
| --- | --- |
| `reject` | The action's own rejection (`Rejected`; `NotSupported` for `SendLocalList` and `UnlockConnector`). `GetConfiguration`, `GetLocalListVersion`, `ClearChargingProfile`, `GetDiagnostics` and `UpdateFirmware` have no rejection status, so they get CALLERROR `NotSupported`. |
| `delay:MS` | The command is handled normally; only the reply is sent `MS` ms later (0 to 3600000). The station keeps working meanwhile. |
| `error:CODE[:TEXT]` | CALLERROR with an OCPP 1.6 code (`NotImplemented`, `NotSupported`, `InternalError`, `ProtocolError`, `SecurityError`, `FormationViolation`, `PropertyConstraintViolation`, `OccurenceConstraintViolation`, `TypeConstraintViolation`, `GenericError`). |
| `drop` | Close the WebSocket without replying; the normal reconnect lifecycle follows (disabled by `--no-reconnect`). |
| `ignore` | Never reply, so the CSMS call timeout fires. |

`*COUNT` applies the rule that many times and then removes it (default: unlimited). There is one rule per action; setting another replaces it. Except for `delay`, the command is **not executed** (no side effects). Rules are runtime only, never persisted. Set them with `--rule` (repeatable) or `KIRBY_OCPP_RULES` (separated by `;`, so texts must not contain `;`), in the TUI (Station tab, `m`), as scenario steps, or through `Command::SetResponseRule` / `ClearResponseRules` in the core API. Each application is logged (`response rule: Reset -> reject, 2 more`).

```sh
kirby-ocpp --url ws://localhost:9000/ocpp --rule Reset=reject --rule GetConfiguration=delay:5000 \
  --rule ChangeConfiguration=error:InternalError*3 --rule RemoteStartTransaction=ignore*1
```

In scenarios: `action = "set_rule"` with `rule = "Reset=reject*2"`, and `action = "clear_rules"` with an optional `call = "Reset"` (all rules when omitted).

## Testing against a mock

`crates/kirby-ocpp-testkit` is a dev-only scripted mock CSMS (plain and TLS) used by the integration tests; it is not published. Use it from tests of your own, or point `kirby-ocpp` at any real or local CSMS.

## Development

Workspace crates: `kirby-ocpp-core` (protocol, station state machine, scenarios, WebSocket/TLS; no terminal code), `kirby-ocpp-tui` (ratatui UI), `kirby-ocpp` (binary: CLI and headless runner), `kirby-ocpp-testkit` (mock CSMS, not published).

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # includes doc tests and process-level CLI tests
```

Protocol coverage is documented in [docs/OCPP_1.6J_CONFORMANCE.md](docs/OCPP_1.6J_CONFORMANCE.md).

## License

MIT, see [LICENSE](LICENSE). The bundled OCPP JSON schemas belong to the Open Charge Alliance; see [THIRD_PARTY_NOTICES.md](crates/kirby-ocpp/THIRD_PARTY_NOTICES.md) or run `kirby-ocpp --licenses`. Crate licences are recorded in `Cargo.lock` and the crates' metadata.
